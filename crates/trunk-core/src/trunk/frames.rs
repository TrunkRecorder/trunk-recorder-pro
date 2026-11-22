//! Per-call vocoder frame records: the FEC error summary written to the call
//! JSON (in intervals, so bursts stand out), and the optional frame capture —
//! each frame's information bits and error counts as JSON lines, which
//! `trunk-pro tool revoice` decodes again offline.

use crate::mbe::{FRAME_SAMPLES, SAMPLE_RATE};
// (The plugin SDK's, which plugins get on the `voice` topic.)
pub use trunk_recorder_plugin::{Codec, VoiceFrame};

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
        self.add_stat(FrameStat::of(f));
    }

    fn add_stat(&mut self, f: FrameStat) {
        if self.intervals.last().is_none_or(|i| i.frames >= INTERVAL_FRAMES) {
            self.intervals.push(ErrorInterval::default());
        }
        let i = self.intervals.last_mut().unwrap();
        i.frames += 1;
        if let Some(errs) = f.errs {
            i.errors += errs as u64;
            i.max_frame_errors = i.max_frame_errors.max(errs);
        }
        i.bad_frames += f.bad as u32;
    }

    /// The share of voice frames decoded cleanly (not repeated, muted or
    /// lost), 0–1; None with no frames (analog).
    pub fn clean_share(&self) -> Option<f64> {
        let (n, bad) = self.intervals.iter().fold((0u64, 0u64), |(n, b), i| (n + i.frames as u64, b + i.bad_frames as u64));
        (n > 0).then(|| (n - bad) as f64 / n as f64)
    }

    pub fn total_errors(&self) -> u64 {
        self.intervals.iter().map(|i| i.errors).sum()
    }

    /// The call record's `errorList`: `pos` / `len` in seconds of the call's audio.
    pub fn intervals(&self) -> Vec<super::record::ErrorInterval> {
        let interval_s = INTERVAL_FRAMES as f64 * FRAME_SAMPLES as f64 / SAMPLE_RATE as f64;
        self.intervals
            .iter()
            .enumerate()
            .map(|(k, i)| super::record::ErrorInterval {
                pos: k as f64 * interval_s,
                len: (i.frames as f64 * FRAME_SAMPLES as f64 / SAMPLE_RATE as f64 * 100.0).round() / 100.0,
                frames: i.frames as u64,
                error_count: i.errors as u64,
                bad_frames: i.bad_frames as u64,
                max_frame_errors: i.max_frame_errors as u64,
            })
            .collect()
    }
}

/// A call's frame bookkeeping: always the error summary, the frames
/// themselves only when captured.
#[derive(Clone, Debug, Default)]
pub struct CallFrames {
    pub errors: FrameErrors,
    pub captured: Option<Vec<VoiceFrame>>,
    /// Each frame's errors, in order (so the summary can be redone for
    /// the frames a call keeps: [`CallFrames::keep`]).
    stats: Vec<FrameStat>,
}

/// What [`FrameErrors`] needs of a frame.
#[derive(Clone, Copy, Debug)]
struct FrameStat {
    /// Bit errors corrected; None when it was lost.
    errs: Option<u32>,
    bad: bool,
}

impl FrameStat {
    fn of(f: &VoiceFrame) -> FrameStat {
        FrameStat { errs: (!f.erased).then_some(f.errs), bad: f.bad() }
    }
}

impl CallFrames {
    pub fn new(capture: bool) -> Self {
        CallFrames { errors: FrameErrors::default(), captured: capture.then(Vec::new), stats: Vec::new() }
    }

    pub fn push(&mut self, f: &VoiceFrame) {
        let st = FrameStat::of(f);
        self.stats.push(st);
        self.errors.add_stat(st);
        if let Some(c) = self.captured.as_mut() {
            c.push(f.clone());
        }
    }

    /// Frames so far.
    pub fn count(&self) -> usize {
        self.stats.len()
    }

    /// Keep only the frames whose audio is in `ranges` (sample ranges of the
    /// call's `audio_len` samples), the error summary redone for them. A
    /// call's digital audio is its frames one after another; when it isn't
    /// (analog: no frames), nothing changes.
    pub fn keep(&mut self, ranges: &[std::ops::Range<usize>], audio_len: usize) {
        if self.stats.is_empty() || self.stats.len() * FRAME_SAMPLES != audio_len {
            return;
        }
        let kept = |k: usize| ranges.iter().any(|r| r.contains(&(k * FRAME_SAMPLES)));
        let mut k = 0;
        self.stats.retain(|_| (kept(k), k += 1).0);
        if let Some(c) = self.captured.as_mut() {
            let mut k = 0;
            c.retain(|_| (kept(k), k += 1).0);
        }
        self.errors = FrameErrors::default();
        for &st in &self.stats {
            self.errors.add_stat(st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mbe::Kind;

    fn frame(errs: u32, kind: Kind) -> VoiceFrame {
        VoiceFrame { codec: Codec::Imbe, bits: vec![1; 88], e0: 0, errs, erased: false, kind, tone: None }
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
        let list = e.intervals();
        assert_eq!((list[0].pos, list[0].len, list[0].frames), (0.0, 10.0, 500));
        assert_eq!((list[2].pos, list[2].len, list[2].frames, list[2].error_count, list[2].bad_frames, list[2].max_frame_errors), (20.0, 4.0, 200, 200, 1, 1));
    }
}
