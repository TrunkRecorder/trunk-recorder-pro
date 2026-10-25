//! What a call has recorded so far, trunked or conventional alike: its
//! audio, the vocoder frames behind it, where each transmission starts, what
//! its voice carried besides speech, and how it came in. Voice goes in one
//! way ([`Recorded::push`]), whatever the call is.

use std::ops::Range;

use super::calls::CallSource;
use super::frames::{CallFrames, VoiceFrame};
use super::inband::{InBand, InBandSpec, News};
use super::record::{talking_at, Reception, Transmissions};
use crate::dsp::inband::pattern::Pattern;
use crate::mbe::SAMPLE_RATE;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub audio: Vec<f32>,
    pub frames: CallFrames,
    /// Where each transmission starts in `audio`.
    pub tx: Transmissions,
    pub inband: InBand,
    /// How strong it came in.
    pub reception: Reception,
    /// How far off its channel the voice came in, Hz from the nominal
    /// frequency: the sum of the measurements and their number.
    freq_error: (f64, u32),
}

impl Recorded {
    pub fn new(capture_frames: bool, inband: InBandSpec) -> Self {
        Recorded { audio: Vec::new(), frames: CallFrames::new(capture_frames), tx: Transmissions::default(), inband: InBand::new(inband), reception: Reception::default(), freq_error: (0.0, 0) }
    }

    /// Voice heard by air time `now_s`: `samples`, and (digital) the frames
    /// they were vocoded from. What it carried besides speech, newly found.
    pub fn push(&mut self, samples: &[f32], frames: impl IntoIterator<Item = VoiceFrame>, now_s: f64) -> News {
        let at = self.audio.len();
        self.tx.note(at, now_s);
        self.audio.extend_from_slice(samples);
        for f in frames {
            self.inband.frame(&f, self.frames.count());
            self.frames.push(f);
        }
        self.inband.audio(samples, at)
    }

    /// Patterns with who sent each: the source talking where it starts (0: none known).
    pub fn talkers(&self, sources: &[CallSource], patterns: Vec<Pattern>) -> Vec<(Pattern, u32)> {
        if patterns.is_empty() {
            return Vec::new();
        }
        let mut placed = sources.to_vec();
        self.tx.place(&mut placed, self.audio.len());
        patterns.into_iter().map(|p| { let src = talking_at(&placed, p.start); (p, src) }).collect()
    }

    /// FM: the voice just pushed, before its high-pass (sub-audible tones).
    pub fn subaudible(&mut self, samples: &[f32]) -> News {
        let at = self.audio.len().saturating_sub(samples.len());
        self.inband.subaudible(samples, at)
    }

    /// The call is over: what its detectors were in the middle of.
    pub fn finish(&mut self) -> News {
        self.inband.finish()
    }

    /// A measurement of how far off its channel the voice is, Hz.
    pub fn freq_error(&mut self, hz: f64) {
        self.freq_error.0 += hz;
        self.freq_error.1 += 1;
    }

    /// The average of its measurements, if any.
    pub fn freq_error_hz(&self) -> Option<f64> {
        (self.freq_error.1 > 0).then(|| self.freq_error.0 / self.freq_error.1 as f64)
    }

    /// Leave out what the saved call won't have: transmissions shorter than
    /// `min_tx_s` and encrypted ones not decrypted ([`Transmissions::kept`]),
    /// their frames, tones and error summary with them. Each source is placed
    /// in the audio first, and moves with it.
    pub fn trim(&mut self, sources: &mut [CallSource], min_tx_s: f64) {
        self.finish();
        self.tx.place(sources, self.audio.len());
        if let Some(ranges) = self.tx.kept(self.audio.len(), min_tx_s, SAMPLE_RATE) {
            // Kept audio before it (a source in a part left out starts where the next kept part does).
            let moved = |p: usize| ranges.iter().map(|r| p.clamp(r.start, r.end) - r.start).sum::<usize>();
            for s in sources.iter_mut() {
                s.audio_pos = s.audio_pos.map(moved);
            }
            self.keep(&ranges);
            self.tx = Default::default();
        }
    }

    fn keep(&mut self, ranges: &[Range<usize>]) {
        self.frames.keep(ranges, self.audio.len());
        self.inband.keep(ranges);
        self.audio = ranges.iter().flat_map(|r| self.audio[r.clone()].iter().copied()).collect();
    }

    /// Seconds of audio decoded cleanly: the vocoder frames not repeated,
    /// muted or lost (analog: all of it). Encrypted: none.
    pub fn clean_s(&self) -> f64 {
        if self.audio.is_empty() {
            return 0.0;
        }
        let iv = &self.frames.errors.intervals;
        if iv.is_empty() {
            return self.audio.len() as f64 / SAMPLE_RATE as f64;
        }
        let good: u32 = iv.iter().map(|i| i.frames - i.bad_frames).sum();
        good as f64 * crate::mbe::FRAME_SAMPLES as f64 / SAMPLE_RATE as f64
    }
}
