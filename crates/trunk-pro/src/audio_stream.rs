//! HTTP audio streams decoded to 8 kHz mono and divided into calls by
//! voice activity, then handed to the recorder's normal call finalizer.

use std::collections::VecDeque;
use std::io::{self, Cursor, Read};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::{AudioStream, AudioStreamChannel};
use crate::runtime::EngineCmd;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;

const SAMPLE_RATE: usize = 8000;
const FRAME_SAMPLES: usize = 160;
const PRE_ROLL_SAMPLES: usize = SAMPLE_RATE / 2;
const ACTIVE_DBFS: f32 = -42.0;
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
static NEXT_CALL_ID: AtomicU32 = AtomicU32::new(1);

pub struct AudioCall {
    pub system: u16,
    pub call_num: u32,
    pub short_name: String,
    pub talkgroup: u32,
    pub samples: Vec<f32>,
    pub started_at: Instant,
}

pub struct AudioFrame {
    pub system: u16,
    pub call_num: u32,
    pub short_name: String,
    pub talkgroup: u32,
    pub samples: Vec<f32>,
}

pub fn spawn(
    stream: AudioStream,
    system: u16,
    tx: Sender<EngineCmd>,
    stop: Arc<AtomicBool>,
    name: &str,
) -> std::io::Result<JoinHandle<()>> {
    let name = name.to_string();
    std::thread::Builder::new().name(name).spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let result = stream_once(&stream, system, &tx, &stop);
            if !stop.load(Ordering::Relaxed) {
                let error = result.as_ref().err().cloned();
                let _ = tx.send(EngineCmd::AudioStreamStatus {
                    short_name: stream.short_name.clone(),
                    connected: false,
                    error,
                });
            }
            if let Err(e) = result {
                if !stop.load(Ordering::Relaxed) {
                    log::warn!(
                        "Internet audio stream {}: {e}; reconnecting",
                        stream.short_name
                    );
                }
            }
            sleep_until_stopped(&stop, RECONNECT_DELAY);
        }
        let _ = tx.send(EngineCmd::AudioStreamEnded);
    })
}

fn stream_once(
    stream: &AudioStream,
    system: u16,
    tx: &Sender<EngineCmd>,
    stop: &Arc<AtomicBool>,
) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_per_read(Some(Duration::from_secs(15)))
        .user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION")))
        .http_status_as_error(false)
        .build()
        .into();
    let response = agent
        .get(&stream.url)
        .call()
        .map_err(|e| format!("HTTP stream request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "HTTP stream server returned status {}",
            response.status()
        ));
    }

    let icy_metaint = response
        .headers()
        .get("icy-metaint")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| "invalid ICY metadata interval".to_string())
        })
        .transpose()?;
    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel(8);
    let stream = stream.clone();
    let tx = tx.clone();
    let stop_reader = stop.clone();
    let reader = std::thread::Builder::new()
        .name("audio-stream-decode".into())
        .spawn(move || {
            decode(
                HttpChunkReader::new(chunk_rx),
                system,
                &stream,
                &tx,
                &stop_reader,
            )
        })
        .map_err(|e| format!("couldn't start audio decoder thread: {e}"))?;

    let mut response = response.into_body().into_reader();
    let mut icy = icy_metaint.map(IcyMetadataStripper::new);
    let mut buffer = [0u8; 16 * 1024];
    let network_result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        match response.read(&mut buffer) {
            Ok(0) => break Err("stream ended".into()),
            Ok(n) => {
                let chunk = &buffer[..n];
                let audio = icy.as_mut().map_or_else(
                    || chunk.to_vec(),
                    |stripper| stripper.strip(chunk),
                );
                if !audio.is_empty() && chunk_tx.blocking_send(audio).is_err() {
                    break Err("audio decoder stopped unexpectedly".into());
                }
            }
            Err(_) if stop.load(Ordering::Relaxed) => break Ok(()),
            Err(e) => break Err(format!("HTTP stream read failed: {e}")),
        }
    };
    drop(chunk_tx);
    let decoded = reader
        .join()
        .map_err(|_| "audio decoder thread panicked".to_string())?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if let Err(error) = &network_result {
        if error != "stream ended" {
            return Err(error.clone());
        }
    }
    decoded?;
    network_result
}

fn decode(
    input: impl Read + Send + Sync + 'static,
    system: u16,
    stream: &AudioStream,
    tx: &Sender<EngineCmd>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut detector = Detector::new(
        stream.silence_timeout_s,
        stream.max_call_s,
        stream.max_continuous_s,
    );
    let mut pcm = Vec::with_capacity(FRAME_SAMPLES * 8);
    let mut stream_samples = 0usize;
    let stream_started = Instant::now();
    let mut connected = false;
    let source = MediaSourceStream::new(
        Box::new(ReadOnlySource::new(input)),
        MediaSourceStreamOptions::default(),
    );
    let mut hint = Hint::new();
    let extension = stream.url.parse::<ureq::http::Uri>()
        .ok()
        .and_then(|url| url.path().rsplit('/').next().map(str::to_owned))
        .and_then(|name| {
            name.rsplit_once('.')
                .map(|(_, extension)| extension.to_owned())
        })
        .filter(|extension| matches!(extension.to_ascii_lowercase().as_str(), "mp3" | "wav"));
    if let Some(extension) = extension {
        hint.with_extension(&extension);
    }
    let probed = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported or invalid audio stream: {e}"))?;
    let mut format = probed;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "audio stream contains no audio track".to_string())?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| "audio stream codec is unknown".to_string())?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("unsupported audio codec: {e}"))?;
    let mut resampler: Option<StreamResampler> = None;
    let decode_result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break Ok(()),
            Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                break Ok(());
            }
            Err(SymphoniaError::ResetRequired) => {
                break Err("audio stream format changed unexpectedly".into());
            }
            Err(e) => break Err(format!("couldn't read audio stream: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => break Err(format!("couldn't decode audio stream: {e}")),
        };
        let sample_rate = decoded.spec().rate();
        let channels = decoded.spec().channels().count();
        if sample_rate == 0 || channels == 0 {
            break Err("audio stream has an invalid sample format".into());
        }
        let mut interleaved = Vec::with_capacity(decoded.samples_interleaved());
        decoded.copy_to_vec_interleaved(&mut interleaved);
        let mono = select_channel(&interleaved, channels, stream.channel);
        let converter = resampler.get_or_insert_with(|| StreamResampler::new(sample_rate));
        if converter.sample_rate != sample_rate {
            *converter = StreamResampler::new(sample_rate);
        }
        converter.process(&mono, &mut pcm);
        let complete = pcm.len() / FRAME_SAMPLES * FRAME_SAMPLES;
        if complete > 0 {
            if !connected {
                tx.send(EngineCmd::AudioStreamStatus {
                    short_name: stream.short_name.clone(),
                    connected: true,
                    error: None,
                })
                .map_err(|_| "recorder stopped".to_string())?;
                connected = true;
            }
            for samples in pcm[..complete].chunks_exact(FRAME_SAMPLES) {
                let samples = samples.to_vec();
                let at = stream_started
                    + Duration::from_secs_f64(stream_samples as f64 / SAMPLE_RATE as f64);
                stream_samples += FRAME_SAMPLES;
                let completed = detector.push(&samples, at);
                let call_num = detector
                    .active_call()
                    .or_else(|| completed.as_ref().map(|(id, _, _)| *id));
                if let Some(call_num) = call_num {
                    tx.send(EngineCmd::AudioFrame(AudioFrame {
                        system,
                        call_num,
                        short_name: stream.short_name.clone(),
                        talkgroup: stream.talkgroup,
                        samples: samples.clone(),
                    }))
                    .map_err(|_| "recorder stopped".to_string())?;
                }
                if let Some((call_num, samples, started_at)) = completed {
                    send_call(stream, system, call_num, tx, samples, started_at)?;
                }
            }
            pcm.drain(..complete);
        }
    };
    if let Some((call_num, samples, started_at)) = detector.flush() {
        send_call(stream, system, call_num, tx, samples, started_at)?;
    }
    decode_result
}

fn select_channel(samples: &[f32], channels: usize, selection: AudioStreamChannel) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| match selection {
            AudioStreamChannel::Left => frame[0],
            AudioStreamChannel::Right => *frame.get(1).unwrap_or(&frame[0]),
            AudioStreamChannel::Mix => frame.iter().sum::<f32>() / channels as f32,
        })
        .collect()
}

struct StreamResampler {
    sample_rate: u32,
    inner: trunk_core::dsp::fm::AudioResampler,
}

impl StreamResampler {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            inner: trunk_core::dsp::fm::AudioResampler::new(
                sample_rate as f64,
                3400.0_f64.min(sample_rate as f64 * 0.45),
            ),
        }
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        for &sample in input {
            self.inner.push(sample, |sample| {
                output.push(sample.clamp(-1.0, 1.0));
            });
        }
    }
}

struct HttpChunkReader {
    receiver: Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    current: Cursor<Vec<u8>>,
}

impl HttpChunkReader {
    fn new(receiver: tokio::sync::mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            receiver: Mutex::new(receiver),
            current: Cursor::new(Vec::new()),
        }
    }
}

impl Read for HttpChunkReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            let n = self.current.read(output)?;
            if n > 0 {
                return Ok(n);
            }
            let receiver = self
                .receiver
                .get_mut()
                .map_err(|_| io::Error::other("HTTP stream reader lock was poisoned"))?;
            match receiver.blocking_recv() {
                Some(chunk) => self.current = Cursor::new(chunk),
                None => return Ok(0),
            }
        }
    }
}

struct IcyMetadataStripper {
    interval: usize,
    audio_remaining: usize,
    metadata_remaining: usize,
    reading_metadata_length: bool,
}

impl IcyMetadataStripper {
    fn new(interval: usize) -> Self {
        Self {
            interval,
            audio_remaining: interval,
            metadata_remaining: 0,
            reading_metadata_length: false,
        }
    }

    fn strip(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len());
        for &byte in input {
            if self.audio_remaining > 0 {
                output.push(byte);
                self.audio_remaining -= 1;
                if self.audio_remaining == 0 {
                    self.reading_metadata_length = true;
                }
            } else if self.reading_metadata_length {
                self.metadata_remaining = usize::from(byte) * 16;
                self.reading_metadata_length = false;
                if self.metadata_remaining == 0 {
                    self.audio_remaining = self.interval;
                }
            } else if self.metadata_remaining > 0 {
                self.metadata_remaining -= 1;
                if self.metadata_remaining == 0 {
                    self.audio_remaining = self.interval;
                }
            } else {
                self.reading_metadata_length = true;
            }
        }
        output
    }
}

fn send_call(
    stream: &AudioStream,
    system: u16,
    call_num: u32,
    tx: &Sender<EngineCmd>,
    samples: Vec<f32>,
    started_at: Instant,
) -> Result<(), String> {
    tx.send(EngineCmd::AudioCall(AudioCall {
        system,
        call_num,
        short_name: stream.short_name.clone(),
        talkgroup: stream.talkgroup,
        samples,
        started_at,
    }))
    .map_err(|_| "recorder stopped".to_string())
}

fn sleep_until_stopped(stop: &AtomicBool, duration: Duration) {
    let until = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct Detector {
    pre_roll: VecDeque<f32>,
    active: Option<Vec<f32>>,
    started_at: Option<Instant>,
    call_num: Option<u32>,
    quiet_samples: usize,
    silence_samples: usize,
    max_call_samples: usize,
    max_continuous_samples: usize,
    continuous_samples: usize,
    continuing: bool,
    waiting_for_silence: bool,
}

impl Detector {
    fn new(silence_timeout_s: f64, max_call_s: f64, max_continuous_s: f64) -> Self {
        Detector {
            pre_roll: VecDeque::with_capacity(PRE_ROLL_SAMPLES),
            active: None,
            started_at: None,
            call_num: None,
            quiet_samples: 0,
            silence_samples: (silence_timeout_s * SAMPLE_RATE as f64).round() as usize,
            max_call_samples: (max_call_s * SAMPLE_RATE as f64).round() as usize,
            max_continuous_samples: (max_continuous_s * SAMPLE_RATE as f64).round() as usize,
            continuous_samples: 0,
            continuing: false,
            waiting_for_silence: false,
        }
    }

    fn push(&mut self, frame: &[f32], at: Instant) -> Option<(u32, Vec<f32>, Instant)> {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
        let voice = rms > 10.0_f32.powf(ACTIVE_DBFS / 20.0);
        if self.waiting_for_silence {
            if voice {
                self.quiet_samples = 0;
            } else {
                self.quiet_samples += frame.len();
                if self.quiet_samples >= self.silence_samples {
                    self.waiting_for_silence = false;
                    self.continuous_samples = 0;
                    self.quiet_samples = 0;
                    self.remember(frame);
                }
            }
            return None;
        }
        if self.active.is_none() {
            if self.continuing {
                if voice {
                    self.active = Some(frame.to_vec());
                    self.started_at = Some(at);
                    self.call_num = Some(Self::next_call_id());
                    self.continuous_samples += frame.len();
                    self.quiet_samples = 0;
                    return None;
                }
                self.quiet_samples += frame.len();
                self.continuous_samples += frame.len();
                if self.quiet_samples >= self.silence_samples {
                    self.continuing = false;
                    self.continuous_samples = 0;
                    self.quiet_samples = 0;
                    self.remember(frame);
                } else if self.continuous_samples >= self.max_continuous_samples {
                    self.continuing = false;
                    self.waiting_for_silence = true;
                }
                return None;
            }
            if voice {
                let keep = self
                    .max_call_samples
                    .saturating_sub(frame.len())
                    .min(self.pre_roll.len());
                let discard = self.pre_roll.len() - keep;
                self.pre_roll.drain(..discard);
                let mut samples: Vec<f32> = self.pre_roll.drain(..).collect();
                let pre_roll = Duration::from_secs_f64(samples.len() as f64 / SAMPLE_RATE as f64);
                samples.extend_from_slice(frame);
                self.started_at = Some(at.checked_sub(pre_roll).unwrap_or(at));
                self.active = Some(samples);
                self.call_num = Some(Self::next_call_id());
                self.quiet_samples = 0;
                self.continuous_samples = frame.len();
                if self
                    .active
                    .as_ref()
                    .is_some_and(|samples| samples.len() >= self.max_call_samples)
                {
                    let samples = self.active.take().expect("active call");
                    let call_num = self.call_num.take().expect("active call id");
                    let started_at = self.started_at.take().expect("active call start");
                    self.continuing = true;
                    return Some((call_num, samples, started_at));
                }
            } else {
                self.remember(frame);
            }
            return None;
        }
        self.active
            .as_mut()
            .expect("active call")
            .extend_from_slice(frame);
        if voice {
            self.quiet_samples = 0;
        } else {
            self.quiet_samples += frame.len();
        }
        self.continuous_samples += frame.len();
        if self.quiet_samples < self.silence_samples {
            if self.continuous_samples < self.max_continuous_samples
                && self
                    .active
                    .as_ref()
                    .is_some_and(|samples| samples.len() < self.max_call_samples)
            {
                return None;
            }
        } else {
            self.continuing = false;
            self.continuous_samples = 0;
        }
        if self.quiet_samples < self.silence_samples {
            self.continuing = self.continuous_samples < self.max_continuous_samples;
            self.waiting_for_silence = !self.continuing;
        }
        let samples = self.active.take().expect("active call");
        let call_num = self.call_num.take().expect("active call id");
        let started_at = self.started_at.take().expect("active call start");
        if !self.continuing {
            self.quiet_samples = 0;
        }
        Some((call_num, samples, started_at))
    }

    fn flush(&mut self) -> Option<(u32, Vec<f32>, Instant)> {
        let samples = self.active.take()?;
        let call_num = self.call_num.take()?;
        let started_at = self.started_at.take()?;
        self.quiet_samples = 0;
        self.continuing = false;
        self.waiting_for_silence = false;
        self.continuous_samples = 0;
        Some((call_num, samples, started_at))
    }

    fn next_call_id() -> u32 {
        0x8000_0000 | (NEXT_CALL_ID.fetch_add(1, Ordering::Relaxed) & 0x7fff_ffff)
    }

    fn active_call(&self) -> Option<u32> {
        self.active.as_ref()?;
        self.call_num
    }

    fn remember(&mut self, frame: &[f32]) {
        self.pre_roll.extend(frame.iter().copied());
        while self.pre_roll.len() > PRE_ROLL_SAMPLES {
            self.pre_roll.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn wav(samples: impl IntoIterator<Item = f32>) -> Vec<u8> {
        let samples: Vec<i16> = samples
            .into_iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let mut wav = Vec::with_capacity(data_len as usize + 44);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&(SAMPLE_RATE as u32).to_le_bytes());
        wav.extend_from_slice(&((SAMPLE_RATE * 2) as u32).to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    #[test]
    fn silence_delimits_audio_and_keeps_preroll() {
        let mut detector = Detector::new(0.25, 120.0, 600.0);
        let now = Instant::now();
        let silence = vec![0.0; FRAME_SAMPLES];
        let voice = vec![0.2; FRAME_SAMPLES];
        for i in 0..30 {
            assert!(
                detector
                    .push(&silence, now + Duration::from_millis(i * 20))
                    .is_none()
            );
        }
        for i in 0..10 {
            assert!(
                detector
                    .push(&voice, now + Duration::from_millis(600 + i * 20))
                    .is_none()
            );
        }
        let mut call = None;
        for i in 0..13 {
            call = detector.push(&silence, now + Duration::from_millis(800 + i * 20));
        }
        let (_, samples, started_at) = call.expect("silence ended the transmission");
        assert_eq!(samples.len(), 4000 + 1600 + 13 * FRAME_SAMPLES);
        assert_eq!(started_at, now + Duration::from_millis(100));
    }

    #[test]
    fn stream_end_flushes_a_call_without_waiting_for_the_hang() {
        let mut detector = Detector::new(3.0, 120.0, 600.0);
        let now = Instant::now();
        assert!(detector.push(&vec![0.2; FRAME_SAMPLES], now).is_none());
        assert_eq!(detector.flush().unwrap().1.len(), FRAME_SAMPLES);
    }

    #[test]
    fn long_audio_is_split_and_detection_waits_for_silence_at_the_continuous_limit() {
        let mut detector = Detector::new(0.04, 0.04, 0.12);
        let now = Instant::now();
        let voice = vec![0.2; FRAME_SAMPLES];
        let silence = vec![0.0; FRAME_SAMPLES];

        let mut completed = Vec::new();
        for i in 0..6 {
            if let Some(call) = detector.push(&voice, now + Duration::from_millis(i * 20)) {
                completed.push(call);
            }
        }
        assert_eq!(completed.len(), 3);
        assert!(
            completed
                .iter()
                .all(|(_, samples, _)| samples.len() == 2 * FRAME_SAMPLES)
        );
        assert!(detector.waiting_for_silence);
        assert!(
            detector
                .push(&voice, now + Duration::from_millis(120))
                .is_none()
        );

        detector.push(&silence, now + Duration::from_millis(140));
        detector.push(&silence, now + Duration::from_millis(160));
        assert!(!detector.waiting_for_silence);
        let next = detector
            .push(&voice, now + Duration::from_millis(180))
            .expect("recording resumes after silence");
        assert_eq!(next.1.len(), 2 * FRAME_SAMPLES);
    }

    #[test]
    fn decoder_emits_live_frames_and_a_finalized_call() {
        let stream = AudioStream {
            url: "https://example.test/live.mp3".into(),
            short_name: "dispatch".into(),
            talkgroup: 123,
            enabled: true,
            silence_timeout_s: 0.25,
            max_call_s: 120.0,
            max_continuous_s: 600.0,
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let samples = std::iter::repeat_n(0.0_f32, 30 * FRAME_SAMPLES)
            .chain(std::iter::repeat_n(0.2_f32, 10 * FRAME_SAMPLES))
            .chain(std::iter::repeat_n(0.0_f32, 13 * FRAME_SAMPLES));
        decode(
            Cursor::new(wav(samples)),
            2,
            &stream,
            &tx,
            &AtomicBool::new(false),
        )
        .unwrap();

        let messages: Vec<_> = rx.try_iter().collect();
        let frames: Vec<_> = messages
            .iter()
            .filter_map(|m| match m {
                EngineCmd::AudioFrame(frame) => Some(frame),
                _ => None,
            })
            .collect();
        let calls: Vec<_> = messages
            .iter()
            .filter_map(|m| match m {
                EngineCmd::AudioCall(call) => Some(call),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 23);
        assert!(
            frames
                .iter()
                .all(|f| f.system == 2 && f.talkgroup == 123 && f.short_name == "dispatch")
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].samples.len(), 4000 + 1600 + 13 * FRAME_SAMPLES);
    }

    #[test]
    fn http_mp3_stream_is_decoded_without_ffmpeg() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let bytes = include_bytes!("../tests/fixtures/audio_stream.mp3").to_vec();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut request = BufReader::new(client.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                request.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            write!(
                client,
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            client.write_all(&bytes).unwrap();
        });

        let stream = AudioStream {
            url: format!("http://{address}/fixture.mp3"),
            short_name: "synthetic".into(),
            talkgroup: 12,
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let result = stream_once(&stream, 1, &tx, &Arc::new(AtomicBool::new(false)));
        server.join().unwrap();

        assert_eq!(result, Err("stream ended".into()));
        let messages: Vec<_> = rx.try_iter().collect();
        assert!(messages.iter().any(|message| matches!(
            message,
            EngineCmd::AudioStreamStatus {
                connected: true,
                ..
            }
        )));
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, EngineCmd::AudioCall(_)))
        );
    }

    #[test]
    fn channel_selection_uses_the_configured_channel() {
        assert_eq!(
            select_channel(&[0.2, 0.8, 0.4, 0.6], 2, AudioStreamChannel::Left),
            [0.2, 0.4]
        );
        assert_eq!(
            select_channel(&[0.2, 0.8, 0.4, 0.6], 2, AudioStreamChannel::Right),
            [0.8, 0.6]
        );
        assert_eq!(
            select_channel(&[0.2, 0.8, 0.4, 0.6], 2, AudioStreamChannel::Mix),
            [0.5, 0.5]
        );
        assert_eq!(
            select_channel(&[0.4, 0.6], 1, AudioStreamChannel::Right),
            [0.4, 0.6]
        );
    }

    #[test]
    fn icy_metadata_is_removed_across_chunk_boundaries() {
        let mut stripper = IcyMetadataStripper::new(3);
        assert_eq!(stripper.strip(b"abc\x01"), b"abc");
        assert_eq!(stripper.strip(b"abcdefghijklmnopxyz"), b"xyz");
    }
}
