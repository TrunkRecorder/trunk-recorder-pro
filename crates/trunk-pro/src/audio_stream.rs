//! A conventional stream row's audio ([`ChannelMode::Stream`]): fetched over
//! HTTP(S), its ICY metadata stripped, decoded (MP3 or WAV, natively), one
//! of a stereo stream's channels taken and resampled to 8 kHz — then pushed
//! into the engine as it comes, where it makes calls as a frequency's audio
//! does (trunk-core's `Conventional::on_audio`). Its clock is its samples:
//! at each (re)connect, and while it's away, silence fills in up to now.

use std::io::{self, Cursor, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::{Channel, Stereo};
use crate::runtime::EngineCmd;
use base64::Engine;
use percent_encoding::percent_decode_str;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use trunk_app::session::StreamFeeder;

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Stream row `stream` ([`crate::config::Config::streams`]) on a thread of
/// its own until `stop`: fetched, and fetched again after a failure (told
/// to the engine thread as [`EngineCmd::StreamError`]). `epoch`: the
/// session's clock (wall ms at an instant).
pub fn spawn(row: Channel, stream: usize, feeder: StreamFeeder, ctl: Sender<EngineCmd>, stop: Arc<AtomicBool>, epoch: (f64, Instant)) -> io::Result<JoinHandle<()>> {
    let now_ms = move || epoch.0 + epoch.1.elapsed().as_secs_f64() * 1000.0;
    std::thread::Builder::new().name(format!("stream-{stream}")).spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            // (Its clock catches up as its audio starts: the time it took to connect.)
            let mut fresh = true;
            let result = stream_once(&row, &stop, &mut |audio| {
                if std::mem::take(&mut fresh) {
                    feeder.catch_up(now_ms());
                }
                feeder.push(audio);
            });
            if let (Err(error), false) = (result, stop.load(Ordering::Relaxed)) {
                // (Not its address: it may hold the feed's password.)
                let name = if row.name.trim().is_empty() { format!("{}", stream + 1) } else { row.name.trim().to_string() };
                log::warn!("Stream {name}: {error}; reconnecting");
                let _ = ctl.send(EngineCmd::StreamError { stream, error });
            }
            // Away: its clock runs on (a call in progress times out), then it's tried again.
            let until = Instant::now() + RECONNECT_DELAY;
            while !stop.load(Ordering::Relaxed) && Instant::now() < until {
                feeder.catch_up(now_ms());
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    })
}

/// One connection: its audio to `push` (8 kHz) until it ends, fails or `stop`.
/// `Ok` only when stopped (a stream that ends is tried again).
fn stream_once(row: &Channel, stop: &Arc<AtomicBool>, push: &mut dyn FnMut(&[f32])) -> Result<(), String> {
    let (url, authorization) = request_url(row.url.trim())?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_per_read(Some(Duration::from_secs(15)))
        .user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION")))
        .http_status_as_error(false)
        .build()
        .into();
    let mut request = agent.get(&url);
    if let Some(authorization) = authorization {
        request = request.header("Authorization", &authorization);
    }
    let response = request
        .call()
        .map_err(|e| format!("couldn't connect: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("the server answered {}", response.status()));
    }
    let icy_metaint = response
        .headers()
        .get("icy-metaint")
        .map(|v| v.to_str().ok().and_then(|v| v.parse::<usize>().ok()).filter(|&v| v > 0).ok_or_else(|| "invalid ICY metadata interval".to_string()))
        .transpose()?;
    // The network on this thread, the decoding on another (a slow read doesn't stall the decoder, nor it the read).
    let (chunk_tx, chunk_rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) = std::sync::mpsc::sync_channel(8);
    let (audio_tx, audio_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(64);
    let (decode_row, decode_stop) = (row.clone(), stop.clone());
    let decoder = std::thread::Builder::new()
        .name("stream-decode".into())
        .spawn(move || decode(HttpChunkReader::new(chunk_rx), &decode_row, &decode_stop, &mut |a| drop(audio_tx.send(a.to_vec()))))
        .map_err(|e| format!("couldn't start its decoder: {e}"))?;
    let mut body = response.into_body().into_reader();
    let mut icy = icy_metaint.map(IcyMetadataStripper::new);
    let mut buffer = [0u8; 16 * 1024];
    let network = loop {
        for audio in audio_rx.try_iter() {
            push(&audio);
        }
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        match body.read(&mut buffer) {
            Ok(0) => break Err("the stream ended".to_string()),
            Ok(n) => {
                let bytes = icy.as_mut().map_or_else(|| buffer[..n].to_vec(), |s| s.strip(&buffer[..n]));
                if !bytes.is_empty() && chunk_tx.send(bytes).is_err() {
                    break Err("its decoder stopped".into());
                }
            }
            Err(_) if stop.load(Ordering::Relaxed) => break Ok(()),
            Err(e) => break Err(format!("reading it failed: {e}")),
        }
    };
    drop(chunk_tx);
    let decoded = decoder.join().map_err(|_| "its decoder failed".to_string())?;
    for audio in audio_rx.try_iter() {
        push(&audio);
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    decoded?;
    network
}

fn request_url(url: &str) -> Result<(String, Option<String>), String> {
    let uri = url
        .parse::<ureq::http::Uri>()
        .map_err(|_| "invalid HTTP stream URL".to_string())?;
    let Some(authority) = uri.authority() else {
        return Ok((url.to_string(), None));
    };
    let Some((userinfo, host_port)) = authority.as_str().rsplit_once('@') else {
        return Ok((url.to_string(), None));
    };
    let (username, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
    if username.is_empty() && password.is_empty() {
        return Ok((url.to_string(), None));
    }

    let username = percent_decode_str(username)
        .decode_utf8()
        .map_err(|_| "invalid credentials in HTTP stream URL".to_string())?;
    let password = percent_decode_str(password)
        .decode_utf8()
        .map_err(|_| "invalid credentials in HTTP stream URL".to_string())?;
    let credentials = format!("{username}:{password}");
    let authorization = base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes());
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| "invalid HTTP stream URL".to_string())?;
    let path_and_query = uri.path_and_query().map_or("/", |path| path.as_str());
    Ok((
        format!("{scheme}://{host_port}{path_and_query}"),
        Some(format!("Basic {authorization}")),
    ))
}

/// Decode `input` (MP3 or WAV) to 8 kHz mono, in the row's stereo channel, into `push`.
fn decode(input: impl Read + Send + Sync + 'static, row: &Channel, stop: &AtomicBool, push: &mut dyn FnMut(&[f32])) -> Result<(), String> {
    let source = MediaSourceStream::new(
        Box::new(ReadOnlySource::new(input)),
        MediaSourceStreamOptions::default(),
    );
    let mut hint = Hint::new();
    let extension = row.url.trim().parse::<ureq::http::Uri>()
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
    let mut pcm = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => return Ok(()),
            Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(SymphoniaError::ResetRequired) => return Err("its format changed".into()),
            Err(e) => return Err(format!("couldn't read it: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("couldn't decode it: {e}")),
        };
        let (rate, channels) = (decoded.spec().rate(), decoded.spec().channels().count());
        if rate == 0 || channels == 0 {
            return Err("it has an invalid sample format".into());
        }
        let mut interleaved = Vec::with_capacity(decoded.samples_interleaved());
        decoded.copy_to_vec_interleaved(&mut interleaved);
        let mono = select_channel(&interleaved, channels, row.stereo);
        let converter = resampler.get_or_insert_with(|| StreamResampler::new(rate));
        if converter.sample_rate != rate {
            *converter = StreamResampler::new(rate);
        }
        pcm.clear();
        converter.process(&mono, &mut pcm);
        if !pcm.is_empty() {
            push(&pcm);
        }
    }
}

fn select_channel(samples: &[f32], channels: usize, selection: Stereo) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| match selection {
            Stereo::Left => frame[0],
            Stereo::Right => *frame.get(1).unwrap_or(&frame[0]),
            Stereo::Mix => frame.iter().sum::<f32>() / channels as f32,
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

/// The network thread's chunks as a reader, for the decoder (in a mutex: the
/// decoder's source must be `Sync`, a receiver isn't).
struct HttpChunkReader {
    receiver: Mutex<Receiver<Vec<u8>>>,
    current: Cursor<Vec<u8>>,
}

impl HttpChunkReader {
    fn new(receiver: Receiver<Vec<u8>>) -> Self {
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
            match receiver.recv() {
                Ok(chunk) => self.current = Cursor::new(chunk),
                Err(_) => return Ok(0),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ChannelMode;

    const SAMPLE_RATE: usize = 8000;

    #[test]
    fn request_url_moves_embedded_credentials_to_basic_auth() {
        let (url, authorization) =
            request_url("https://user%40domain:p%3Ass@radio.example:8443/live.mp3?mount=1")
                .unwrap();
        assert_eq!(url, "https://radio.example:8443/live.mp3?mount=1");
        assert_eq!(
            authorization.as_deref(),
            Some("Basic dXNlckBkb21haW46cDpzcw==")
        );
    }

    #[test]
    fn request_url_preserves_urls_without_credentials() {
        let original = "https://radio.example/live.mp3?mount=1";
        assert_eq!(request_url(original).unwrap(), (original.to_string(), None));
    }

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

    fn row(url: &str) -> Channel {
        serde_json::from_value(serde_json::json!({ "freqHz": 0, "mode": "stream", "url": url })).unwrap()
    }

    #[test]
    fn a_wav_stream_is_decoded_to_8_khz() {
        let samples = std::iter::repeat_n(0.0_f32, 4000).chain(std::iter::repeat_n(0.2_f32, 4000));
        let mut got = Vec::new();
        decode(Cursor::new(wav(samples)), &row("https://example.test/live.wav"), &AtomicBool::new(false), &mut |a| got.extend_from_slice(a)).unwrap();
        // (Less the resampler's filter, 128 taps, still in it.)
        assert!((7800..=8000).contains(&got.len()), "{}", got.len());
        assert!(got[..3000].iter().all(|x| x.abs() < 1e-3) && got[5000..].iter().all(|x| (x - 0.2).abs() < 0.02));
        assert_eq!(row("x").mode, ChannelMode::Stream);
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
            let mut authorized = false;
            let mut request_target_is_clean = false;
            loop {
                line.clear();
                request.read_line(&mut line).unwrap();
                if line == "GET /fixture.mp3 HTTP/1.1\r\n" {
                    request_target_is_clean = true;
                }
                if line.eq_ignore_ascii_case("Authorization: Basic dXNlcjpwYXNz\r\n") {
                    authorized = true;
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            assert!(authorized);
            assert!(request_target_is_clean);
            write!(
                client,
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            client.write_all(&bytes).unwrap();
        });

        let mut got = 0;
        let result = stream_once(
            &row(&format!("http://user:pass@{address}/fixture.mp3")),
            &Arc::new(AtomicBool::new(false)),
            &mut |audio| got += audio.len(),
        );
        server.join().unwrap();
        assert_eq!(result, Err("the stream ended".into()));
        assert!(got > 0, "its audio came through");
    }

    #[test]
    fn channel_selection_uses_the_configured_channel() {
        assert_eq!(
            select_channel(&[0.2, 0.8, 0.4, 0.6], 2, Stereo::Left),
            [0.2, 0.4]
        );
        assert_eq!(
            select_channel(&[0.2, 0.8, 0.4, 0.6], 2, Stereo::Right),
            [0.8, 0.6]
        );
        assert_eq!(
            select_channel(&[0.2, 0.8, 0.4, 0.6], 2, Stereo::Mix),
            [0.5, 0.5]
        );
        assert_eq!(
            select_channel(&[0.4, 0.6], 1, Stereo::Right),
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
