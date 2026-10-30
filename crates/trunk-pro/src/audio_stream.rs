//! HTTP audio streams decoded to 8 kHz mono and divided into calls by
//! voice activity, then handed to the recorder's normal call finalizer.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::AudioStream;
use crate::runtime::EngineCmd;

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
    let mut args = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostats",
        "-f",
        "concat",
        "-safe",
        "0",
        "-protocol_whitelist",
        "http,https,tcp,tls,crypto,pipe",
        "-i",
        "pipe:0",
        "-map",
        "0:a:0",
        "-vn",
    ];
    if let Some(filter) = stream.channel.filter() {
        args.extend(["-af", filter]);
    }
    args.extend([
        "-ac",
        "1",
        "-ar",
        "8000",
        "-f",
        "f32le",
        "-acodec",
        "pcm_f32le",
        "pipe:1",
    ]);
    let mut child = Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "ffmpeg is required to decode internet audio streams".to_string()
            } else {
                format!("couldn't start ffmpeg: {e}")
            }
        })?;
    let escaped_url = stream.url.replace('\\', "\\\\").replace('\'', "\\'");
    let manifest = format!("ffconcat version 1.0\nfile '{escaped_url}'\n");
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(manifest.as_bytes()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("couldn't pass the stream URL to ffmpeg".into());
        }
    }
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("ffmpeg has no audio output".into());
    };
    let stream = stream.clone();
    let tx = tx.clone();
    let stop_reader = stop.clone();
    let reader = match std::thread::Builder::new()
        .name("audio-stream-decode".into())
        .spawn(move || decode(stdout, system, &stream, &tx, &stop_reader))
    {
        Ok(reader) => reader,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("couldn't start audio decoder thread: {e}"));
        }
    };

    let status = loop {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            break match child.wait() {
                Ok(status) => status,
                Err(e) => {
                    let _ = reader.join();
                    return Err(format!("couldn't stop ffmpeg: {e}"));
                }
            };
        }
        match child.try_wait() {
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(format!("ffmpeg process error: {e}"));
            }
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let decoded = reader
        .join()
        .map_err(|_| "audio decoder thread panicked".to_string())?;
    decoded?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if status.success() {
        Err("stream ended".into())
    } else {
        Err(format!(
            "ffmpeg exited with status {}",
            status
                .code()
                .map_or_else(|| "unknown".into(), |c| c.to_string())
        ))
    }
}

fn decode(
    mut stdout: impl Read,
    system: u16,
    stream: &AudioStream,
    tx: &Sender<EngineCmd>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut detector = Detector::new(stream.silence_timeout_s, stream.max_call_s, stream.max_continuous_s);
    let mut bytes = Vec::with_capacity(FRAME_SAMPLES * 4 * 8);
    let mut input = [0u8; 8192];
    let mut stream_samples = 0usize;
    let stream_started = Instant::now();
    let mut connected = false;
    let read_result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        let n = match stdout.read(&mut input) {
            Ok(n) => n,
            Err(e) => break Err(format!("couldn't read ffmpeg audio: {e}")),
        };
        if n == 0 {
            break Ok(());
        }
        if !connected {
            tx.send(EngineCmd::AudioStreamStatus {
                short_name: stream.short_name.clone(),
                connected: true,
                error: None,
            })
            .map_err(|_| "recorder stopped".to_string())?;
            connected = true;
        }
        bytes.extend_from_slice(&input[..n]);
        let complete = bytes.len() / (FRAME_SAMPLES * 4) * (FRAME_SAMPLES * 4);
        for frame in bytes[..complete].chunks_exact(FRAME_SAMPLES * 4) {
            let samples: Vec<f32> = frame
                .chunks_exact(4)
                .map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]).clamp(-1.0, 1.0))
                .collect();
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
        bytes.drain(..complete);
    };
    if let Some((call_num, samples, started_at)) = detector.flush() {
        send_call(stream, system, call_num, tx, samples, started_at)?;
    }
    read_result
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
                let keep = self.max_call_samples.saturating_sub(frame.len()).min(self.pre_roll.len());
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
                if self.active.as_ref().is_some_and(|samples| samples.len() >= self.max_call_samples) {
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
                && self.active.as_ref().is_some_and(|samples| samples.len() < self.max_call_samples)
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

    #[test]
    fn silence_delimits_audio_and_keeps_preroll() {
        let mut detector = Detector::new(0.25, 120.0, 600.0);
        let now = Instant::now();
        let silence = vec![0.0; FRAME_SAMPLES];
        let voice = vec![0.2; FRAME_SAMPLES];
        for i in 0..30 {
            assert!(detector
                .push(&silence, now + Duration::from_millis(i * 20))
                .is_none());
        }
        for i in 0..10 {
            assert!(detector
                .push(&voice, now + Duration::from_millis(600 + i * 20))
                .is_none());
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
        assert!(completed.iter().all(|(_, samples, _)| samples.len() == 2 * FRAME_SAMPLES));
        assert!(detector.waiting_for_silence);
        assert!(detector.push(&voice, now + Duration::from_millis(120)).is_none());

        detector.push(&silence, now + Duration::from_millis(140));
        detector.push(&silence, now + Duration::from_millis(160));
        assert!(!detector.waiting_for_silence);
        let next = detector.push(&voice, now + Duration::from_millis(180)).expect("recording resumes after silence");
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
        let mut bytes = Vec::new();
        for sample in std::iter::repeat_n(0.0_f32, 30 * FRAME_SAMPLES)
            .chain(std::iter::repeat_n(0.2_f32, 10 * FRAME_SAMPLES))
            .chain(std::iter::repeat_n(0.0_f32, 13 * FRAME_SAMPLES))
        {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        decode(Cursor::new(bytes), 2, &stream, &tx, &AtomicBool::new(false)).unwrap();

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
        assert!(frames
            .iter()
            .all(|f| f.system == 2 && f.talkgroup == 123 && f.short_name == "dispatch"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].samples.len(), 4000 + 1600 + 13 * FRAME_SAMPLES);
    }

    #[test]
    fn channel_selection_uses_expected_mono_mix() {
        use crate::config::AudioStreamChannel;
        assert_eq!(AudioStreamChannel::Left.filter(), Some("pan=mono|c0=c0"));
        assert_eq!(AudioStreamChannel::Right.filter(), Some("pan=mono|c0=c1"));
        assert_eq!(AudioStreamChannel::Mix.filter(), None);
    }
}
