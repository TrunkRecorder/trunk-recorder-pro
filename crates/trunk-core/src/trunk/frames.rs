//! Per-call vocoder frame records: the FEC error summary written to the call
//! JSON (in intervals, so bursts stand out), and the optional frame capture —
//! each frame's information bits and error counts as JSON lines, which
//! `trunk-pro tool revoice` decodes again offline.

use std::fmt::Write;

use crate::mbe::{Kind, FRAME_SAMPLES, SAMPLE_RATE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Phase 1 full rate: 88 information bits.
    Imbe,
    /// Phase 2 half rate: 49 information bits.
    Ambe,
}

/// One vocoder frame as the tracker decoded it.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceFrame {
    pub codec: Codec,
    /// Information bits (0 / 1), after FEC.
    pub bits: Vec<u8>,
    /// IMBE: bit errors corrected in u0 (the Golay word carrying pitch). AMBE: 0.
    pub e0: u32,
    /// Bit errors the FEC corrected in the whole frame.
    pub errs: u32,
    /// Known lost (a cut LDU); `errs` is then meaningless.
    pub erased: bool,
    /// What the vocoder made of it.
    pub kind: Kind,
}

impl VoiceFrame {
    /// Repeated, muted or otherwise not synthesized as sent.
    pub fn bad(&self) -> bool {
        self.erased || self.kind != Kind::Voice
    }
}

/// Seconds of audio per [`ErrorInterval`].
pub const ERROR_INTERVAL_S: f64 = 10.0;
const INTERVAL_FRAMES: u32 = (ERROR_INTERVAL_S * SAMPLE_RATE as f64) as u32 / FRAME_SAMPLES as u32;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ErrorInterval {
    pub frames: u32,
    /// FEC-corrected bit errors.
    pub errors: u64,
    /// Frames repeated, muted or erased.
    pub bad_frames: u32,
    /// The most errors in one frame.
    pub max_frame_errors: u32,
}

/// A call's FEC errors, per [`ERROR_INTERVAL_S`] of its audio.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameErrors {
    pub intervals: Vec<ErrorInterval>,
}

impl FrameErrors {
    pub fn add(&mut self, f: &VoiceFrame) {
        if self.intervals.last().is_none_or(|i| i.frames >= INTERVAL_FRAMES) {
            self.intervals.push(ErrorInterval::default());
        }
        let i = self.intervals.last_mut().unwrap();
        i.frames += 1;
        if !f.erased {
            i.errors += f.errs as u64;
            i.max_frame_errors = i.max_frame_errors.max(f.errs);
        }
        i.bad_frames += f.bad() as u32;
    }

    pub fn total_errors(&self) -> u64 {
        self.intervals.iter().map(|i| i.errors).sum()
    }

    /// `"errorList":[…]` — `pos` / `len` in seconds of the call's audio.
    pub fn json(&self) -> String {
        let interval_s = INTERVAL_FRAMES as f64 * FRAME_SAMPLES as f64 / SAMPLE_RATE as f64;
        let mut j = String::from("\"errorList\":[");
        for (k, i) in self.intervals.iter().enumerate() {
            let len = i.frames as f64 * FRAME_SAMPLES as f64 / SAMPLE_RATE as f64;
            let _ = write!(
                j,
                "{}{{\"pos\":{},\"len\":{},\"frames\":{},\"error_count\":{},\"bad_frames\":{},\"max_frame_errors\":{}}}",
                if k > 0 { "," } else { "" },
                k as f64 * interval_s,
                (len * 100.0).round() / 100.0,
                i.frames,
                i.errors,
                i.bad_frames,
                i.max_frame_errors
            );
        }
        j.push(']');
        j
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Voice => "voice",
        Kind::Repeat => "repeat",
        Kind::Muted => "muted",
        Kind::Erasure => "erasure",
        Kind::Tone => "tone",
    }
}

/// Information bits, MSB first, as hex (the last byte zero-padded).
pub fn bits_hex(bits: &[u8]) -> String {
    bits.chunks(8)
        .map(|c| {
            let b = c.iter().enumerate().fold(0u8, |a, (i, &v)| a | ((v & 1) << (7 - i)));
            format!("{b:02x}")
        })
        .collect()
}

/// [`bits_hex`] back to `n` bits.
pub fn hex_bits(hex: &str, n: usize) -> Option<Vec<u8>> {
    if hex.len() != n.div_ceil(8) * 2 {
        return None;
    }
    let mut bits = Vec::with_capacity(n);
    for k in 0..hex.len() / 2 {
        let b = u8::from_str_radix(hex.get(2 * k..2 * k + 2)?, 16).ok()?;
        bits.extend((0..8).map(|i| (b >> (7 - i)) & 1));
    }
    bits.truncate(n);
    Some(bits)
}

/// The frame capture: one JSON object per line, `pos` in seconds of the
/// call's audio (frame k covers [pos, pos + 0.02)).
pub fn frames_jsonl(frames: &[VoiceFrame]) -> String {
    let mut s = String::with_capacity(frames.len() * 110);
    for (k, f) in frames.iter().enumerate() {
        let _ = writeln!(
            s,
            "{{\"pos\":{:.2},\"codec\":\"{}\",\"bits\":\"{}\",\"e0\":{},\"errs\":{},\"erased\":{},\"out\":\"{}\"}}",
            k as f64 * FRAME_SAMPLES as f64 / SAMPLE_RATE as f64,
            if f.codec == Codec::Imbe { "imbe" } else { "ambe" },
            bits_hex(&f.bits),
            f.e0,
            f.errs,
            f.erased,
            kind_name(f.kind)
        );
    }
    s
}

/// A call's frame bookkeeping: always the error summary, the frames
/// themselves only when captured.
#[derive(Clone, Debug, Default)]
pub struct CallFrames {
    pub errors: FrameErrors,
    pub captured: Option<Vec<VoiceFrame>>,
}

impl CallFrames {
    pub fn new(capture: bool) -> Self {
        CallFrames { errors: FrameErrors::default(), captured: capture.then(Vec::new) }
    }

    pub fn push(&mut self, f: VoiceFrame) {
        self.errors.add(&f);
        if let Some(c) = self.captured.as_mut() {
            c.push(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(errs: u32, kind: Kind) -> VoiceFrame {
        VoiceFrame { codec: Codec::Imbe, bits: vec![1; 88], e0: 0, errs, erased: false, kind }
    }

    #[test]
    fn errors_bucket_by_ten_seconds() {
        let mut e = FrameErrors::default();
        for k in 0..1200 {
            e.add(&frame(if k == 700 { 9 } else { 1 }, if k == 1100 { Kind::Repeat } else { Kind::Voice }));
        }
        assert_eq!(e.intervals.len(), 3);
        assert_eq!(e.intervals[0], ErrorInterval { frames: 500, errors: 500, bad_frames: 0, max_frame_errors: 1 });
        assert_eq!(e.intervals[1], ErrorInterval { frames: 500, errors: 508, bad_frames: 0, max_frame_errors: 9 });
        assert_eq!(e.intervals[2], ErrorInterval { frames: 200, errors: 200, bad_frames: 1, max_frame_errors: 1 });
        assert_eq!(e.total_errors(), 1208);
        let j = e.json();
        assert!(j.starts_with("\"errorList\":[{\"pos\":0,\"len\":10,\"frames\":500,"), "{j}");
        assert!(j.contains("{\"pos\":20,\"len\":4,\"frames\":200,\"error_count\":200,\"bad_frames\":1,\"max_frame_errors\":1}"), "{j}");
    }

    #[test]
    fn bits_round_trip_through_hex() {
        let bits: Vec<u8> = (0..49).map(|i| ((i * 7 + 3) % 5 == 0) as u8).collect();
        let h = bits_hex(&bits);
        assert_eq!(h.len(), 14);
        assert_eq!(hex_bits(&h, 49), Some(bits));
        assert_eq!(hex_bits("zz", 8), None);
    }
}
