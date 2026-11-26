//! An internet audio stream as a source ([`SourceKind::Stream`](crate::config::SourceKind)): an
//! HTTP or HTTPS MP3 or WAV feed, its ICY metadata taken out, decoded (natively: Symphonia) and
//! resampled to 8 kHz, both channels of a stereo one kept (a row picks: trunk-core's
//! `dsp::audio_in`), out through [`Sink::audio`]. Connecting again after a drop, and its errors,
//! are every radio's ([`sdr::run_with`](crate::sdr::run_with)); its clock is its samples, as a
//! radio's is, measured against the wall clock as they arrive (trunk-app's `Session`).

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use percent_encoding::percent_decode_str;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use trunk_core::dsp::fm::{AudioResampler, AUDIO_RATE};

use crate::sdr::{Control, Gain, Radio, Sink};

/// Connecting, s; and how long a read may wait before the stop is looked at.
const CONNECT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_secs(1);
/// Nothing from the server this long: it has gone, connect again.
const SILENT: Duration = Duration::from_secs(10);
/// Channels kept: a stereo stream's two (a row records one or both mixed).
const CHANNELS: usize = 2;

pub struct StreamConfig {
    pub url: String,
}

impl Radio for StreamConfig {
    const BACKOFF: Duration = Duration::from_secs(2);

    fn rate_hz(&self) -> f64 {
        AUDIO_RATE
    }
    /// (Audio: nothing to tune, and no gain.)
    fn retune_to(&mut self, _hz: f64) {}
    fn gain_to(&mut self, _g: Gain) {}
    fn gain(&self) -> Option<Gain> {
        None
    }

    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, _ctl: Option<&Control>) -> Result<(), String> {
        let (url, authorization) = request_url(self.url.trim())?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT))
            .timeout_per_read(Some(POLL))
            .user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION")))
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent.get(&url).header("Icy-MetaData", "1");
        if let Some(a) = &authorization {
            request = request.header("Authorization", a);
        }
        let response = request.call().map_err(|e| format!("couldn't connect: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("the server answered {}", response.status()));
        }
        let interval = match response.headers().get("icy-metaint") {
            None => None,
            Some(v) => Some(v.to_str().ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|&v| v > 0).ok_or("it gave a bad ICY metadata interval")?),
        };
        let body = Body { inner: Mutex::new(response.into_body().into_reader()), stop: stop.clone(), heard: Instant::now() };
        let input = Icy { inner: body, interval, audio_left: interval.unwrap_or(0), meta_left: None };
        decode(input, extension(&url), stop, &mut |audio, channels| sink.audio(audio, channels, Instant::now()))
    }
}

/// The server's bytes as the decoder reads them: a read that times out waits again (the stop
/// looked at), up to [`SILENT`]. (In a mutex: the decoder's input must be `Sync`, a body isn't.)
struct Body<R> {
    inner: Mutex<R>,
    stop: Arc<AtomicBool>,
    heard: Instant,
}

impl<R: Read> Read for Body<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let inner = self.inner.get_mut().map_err(|_| io::Error::other("its reader failed"))?;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(0);
            }
            match inner.read(out) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed the stream")),
                Ok(n) => {
                    self.heard = Instant::now();
                    return Ok(n);
                }
                Err(e) if timed_out(&e) && self.heard.elapsed() < SILENT => continue,
                Err(e) if timed_out(&e) => return Err(io::Error::new(io::ErrorKind::TimedOut, format!("nothing from the server for {} s", SILENT.as_secs()))),
                Err(e) => return Err(e),
            }
        }
    }
}

fn timed_out(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
        || e.get_ref().and_then(|s| s.downcast_ref::<ureq::Error>()).is_some_and(|e| matches!(e, ureq::Error::Timeout(_)))
}

/// A Shoutcast / Icecast stream's audio without the metadata it puts in every
/// `interval` bytes (asked for with `Icy-MetaData: 1`, so the stream title is
/// there to take out): a length byte (×16), then that much text.
struct Icy<R> {
    inner: R,
    interval: Option<usize>,
    audio_left: usize,
    /// In metadata: how much is left of it (None: audio, or its length byte next once `audio_left` is 0).
    meta_left: Option<usize>,
}

impl<R: Read> Read for Icy<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let Some(interval) = self.interval else { return self.inner.read(out) };
        loop {
            match self.meta_left {
                Some(0) => {
                    (self.meta_left, self.audio_left) = (None, interval);
                }
                Some(n) => {
                    let mut skip = vec![0u8; n.min(4096)];
                    let k = self.inner.read(&mut skip)?;
                    if k == 0 {
                        return Ok(0);
                    }
                    self.meta_left = Some(n - k);
                }
                None if self.audio_left == 0 => {
                    let mut len = [0u8; 1];
                    if self.inner.read(&mut len)? == 0 {
                        return Ok(0);
                    }
                    self.meta_left = Some(len[0] as usize * 16);
                }
                None => {
                    let want = out.len().min(self.audio_left);
                    let k = self.inner.read(&mut out[..want])?;
                    self.audio_left -= k;
                    return Ok(k);
                }
            }
        }
    }
}

/// The address to ask, and its login (`user:password@`, which a request doesn't carry) as a Basic `Authorization`.
fn request_url(url: &str) -> Result<(String, Option<String>), String> {
    let uri = url.parse::<ureq::http::Uri>().map_err(|_| "its address isn't one".to_string())?;
    let Some((login, host)) = uri.authority().and_then(|a| a.as_str().rsplit_once('@')) else { return Ok((url.to_string(), None)) };
    let (user, password) = login.split_once(':').unwrap_or((login, ""));
    let decode = |s: &str| percent_decode_str(s).decode_utf8().map(|s| s.into_owned()).map_err(|_| "its address has a login that isn't UTF-8".to_string());
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", decode(user)?, decode(password)?));
    let scheme = uri.scheme_str().unwrap_or("http");
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    Ok((format!("{scheme}://{host}{path}"), Some(format!("Basic {basic}"))))
}

/// The format its address names (`….mp3`, `….wav`), a hint for the decoder.
fn extension(url: &str) -> Option<String> {
    let path = url.parse::<ureq::http::Uri>().ok()?.path().to_string();
    let ext = path.rsplit('/').next()?.rsplit_once('.')?.1.to_ascii_lowercase();
    ["mp3", "wav"].contains(&ext.as_str()).then_some(ext)
}

/// Decode `input` (MP3 or WAV) to 8 kHz, a stereo stream's two channels kept (more: the first
/// two), each piece to `push` (audio, channels). Ends when the input does, or the stop is set.
fn decode(input: impl Read + Send + Sync + 'static, ext: Option<String>, stop: &AtomicBool, push: &mut dyn FnMut(&[f32], usize) -> bool) -> Result<(), String> {
    let source = MediaSourceStream::new(Box::new(ReadOnlySource::new(input)), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(e) = &ext {
        hint.with_extension(e);
    }
    let mut format = symphonia::default::get_probe().probe(&hint, source, FormatOptions::default(), MetadataOptions::default()).map_err(|e| format!("it isn't MP3 or WAV audio ({e})"))?;
    let track = format.default_track(TrackType::Audio).ok_or("it has no audio in it")?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("its audio is of a kind not known")?;
    let mut decoder = symphonia::default::get_codecs().make_audio_decoder(params, &AudioDecoderOptions::default()).map_err(|e| format!("its audio can't be decoded ({e})"))?;
    let mut to_8k: Option<(u32, Vec<AudioResampler>)> = None;
    let (mut pcm, mut out) = (Vec::new(), Vec::new());
    while !stop.load(Ordering::Relaxed) {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(()),
            Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof && stop.load(Ordering::Relaxed) => return Ok(()),
            Err(SymphoniaError::IoError(e)) => return Err(e.to_string()),
            Err(SymphoniaError::ResetRequired) => return Err("its format changed".into()),
            Err(e) => return Err(format!("couldn't read it ({e})")),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // (A damaged frame: the next one decodes.)
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("couldn't decode it ({e})")),
        };
        let (rate, channels) = (decoded.spec().rate(), decoded.spec().channels().count());
        if rate == 0 || channels == 0 {
            return Err("its audio has no rate or channels".into());
        }
        pcm.clear();
        decoded.copy_to_vec_interleaved(&mut pcm);
        let keep = channels.min(CHANNELS);
        if to_8k.as_ref().is_none_or(|(r, c)| *r != rate || c.len() != keep) {
            let cutoff = 3400f64.min(rate as f64 * 0.45);
            to_8k = Some((rate, (0..keep).map(|_| AudioResampler::new(rate as f64, cutoff)).collect()));
        }
        let (_, resamplers) = to_8k.as_mut().unwrap();
        // (Each channel's resampler gives its samples at the same instants: they interleave as they come.)
        out.clear();
        let mut each: Vec<Vec<f32>> = vec![Vec::new(); keep];
        for frame in pcm.chunks_exact(channels) {
            for (k, r) in resamplers.iter_mut().enumerate() {
                r.push(frame[k], |s| each[k].push(s.clamp(-1.0, 1.0)));
            }
        }
        for i in 0..each[0].len() {
            out.extend(each.iter().map(|c| c[i]));
        }
        if !out.is_empty() && !push(&out, keep) {
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, frames: &[(i16, i16)]) -> Vec<u8> {
        let data: Vec<u8> = frames.iter().flat_map(|&(l, r)| if channels == 2 { [l.to_le_bytes(), r.to_le_bytes()].concat() } else { l.to_le_bytes().to_vec() }).collect();
        let mut w = Vec::new();
        w.extend(b"RIFF");
        w.extend((36 + data.len() as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(channels.to_le_bytes());
        w.extend(rate.to_le_bytes());
        w.extend((rate * channels as u32 * 2).to_le_bytes());
        w.extend((channels * 2).to_le_bytes());
        w.extend(16u16.to_le_bytes());
        w.extend(b"data");
        w.extend((data.len() as u32).to_le_bytes());
        w.extend(data);
        w
    }

    fn decoded(bytes: Vec<u8>, ext: Option<&str>) -> (Vec<f32>, usize) {
        let (mut all, mut ch) = (Vec::new(), 0);
        decode(io::Cursor::new(bytes), ext.map(str::to_string), &AtomicBool::new(false), &mut |a, c| {
            all.extend_from_slice(a);
            ch = c;
            true
        })
        .unwrap();
        (all, ch)
    }

    /// A stereo WAV at 16 kHz: 8 kHz out, both channels kept, each its own.
    #[test]
    fn stereo_wav_to_8k() {
        let frames: Vec<(i16, i16)> = (0..16_000).map(|i| ((((i as f32) * 0.05).sin() * 8000.0) as i16, 0)).collect();
        let (out, ch) = decoded(wav(16_000, 2, &frames), Some("wav"));
        assert_eq!(ch, 2);
        let n = out.len() / 2;
        assert!((7_800..=8_000).contains(&n), "{n}");
        let rms = |k: usize| (out.iter().skip(k).step_by(2).skip(200).map(|x| x * x).sum::<f32>() / (n - 200) as f32).sqrt();
        assert!(rms(0) > 0.1 && rms(1) < 1e-3, "left carries the tone, right is silent: {} {}", rms(0), rms(1));
    }

    /// The MP3 a stream sends decodes, mono.
    #[test]
    fn mp3_decodes() {
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/audio_stream.mp3")).unwrap();
        let (out, ch) = decoded(bytes, Some("mp3"));
        assert_eq!(ch, 1);
        assert!(out.len() > 4_000, "{}", out.len());
    }

    /// The metadata a Shoutcast server puts between the audio is taken out, wherever reads fall.
    #[test]
    fn icy_metadata_out() {
        let mut sent = Vec::new();
        sent.extend(b"abcd");
        sent.push(1);
        sent.extend(b"StreamTitle='x';");
        sent.extend(b"efgh");
        sent.push(0);
        sent.extend(b"ij");
        for size in [1, 3, 64] {
            let mut r = Icy { inner: io::Cursor::new(sent.clone()), interval: Some(4), audio_left: 4, meta_left: None };
            let mut got = Vec::new();
            let mut buf = vec![0u8; size];
            loop {
                match r.read(&mut buf).unwrap() {
                    0 => break,
                    n => got.extend_from_slice(&buf[..n]),
                }
            }
            assert_eq!(got, b"abcdefghij", "reads of {size}");
        }
    }

    /// A login in the address goes as Basic authorization, not in the address asked for.
    #[test]
    fn login_goes_as_authorization() {
        let (url, auth) = request_url("https://us%40er:p%3Ass@feeds.example.com:8443/123?x=1").unwrap();
        assert_eq!(url, "https://feeds.example.com:8443/123?x=1");
        assert_eq!(auth.unwrap(), format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("us@er:p:ss")));
        assert_eq!(request_url("http://feeds.example.com/123").unwrap(), ("http://feeds.example.com/123".into(), None));
        assert_eq!(extension("https://x.example.com/live/feed.MP3?a=b"), Some("mp3".into()));
    }
}
