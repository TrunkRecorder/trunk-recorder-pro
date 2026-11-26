//! A source of audio rather than air: an internet stream, someone else's
//! receiver, demodulated already. Its conventional channels are heard as a
//! radio's are ([`crate::trunk::conventional`]): a channel's level against
//! its quiet floor opens it, its pre-roll is the audio before that, and
//! [`AudioGate`] is the analog receiver's squelch with nothing to demodulate.
//!
//! ```text
//! 8 kHz audio, mono or stereo → 20 ms blocks; for each of mix, left, right:
//!   mean square                  → the block's power (a radio's band power)
//!   the last 5 s of block powers → its quiet floor: their 20th percentile
//!                                  (a radio's noise floor; held in a gap)
//!   the last history_s of audio  → pre-roll
//! ```
//!
//! The floor follows what the feed sounds like between calls, so a quiet feed
//! (−100 dBFS between calls) and one with a hum under it both open on voice;
//! it goes on learning during a call, so a feed whose background comes up
//! doesn't hold a call open on it.

use std::collections::VecDeque;

use super::filters::Biquad;
use super::fm::{AUDIO_RATE, HPF_SOS};
use super::squelch::{Gate, SquelchTiming};

/// Samples per block: 20 ms.
pub const BLOCK: usize = 160;
/// Block powers the floor is taken over: 5 s.
const FLOOR_BLOCKS: usize = 250;
/// The floor is this quantile of them.
const FLOOR_QUANTILE: f64 = 0.2;
/// The floor is kept between these, dBFS: digital silence isn't a floor a
/// whisper should open over, and a feed loud all through still opens on
/// louder (at the default squelch, between −100 and −30 dBFS).
const MIN_FLOOR_DBFS: f64 = -108.0;
const MAX_FLOOR_DBFS: f64 = -38.0;

/// Which of a stream's channels a row records: both mixed (the default; a
/// mono stream is all three alike), or one of a stereo stream's two, which
/// may carry a channel each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Stereo {
    #[default]
    Mix,
    Left,
    Right,
}

impl Stereo {
    pub const ALL: [Stereo; 3] = [Stereo::Mix, Stereo::Left, Stereo::Right];

    /// As the config and a channel file name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Stereo::Mix => "mix",
            Stereo::Left => "left",
            Stereo::Right => "right",
        }
    }

    /// The one [`Stereo::as_str`] names, in any case; empty is the default (mix).
    pub fn parse(text: &str) -> Option<Stereo> {
        let t = text.trim();
        if t.is_empty() {
            return Some(Stereo::Mix);
        }
        Self::ALL.into_iter().find(|s| s.as_str().eq_ignore_ascii_case(t))
    }

    pub fn is_mix(&self) -> bool {
        *self == Stereo::Mix
    }

    fn index(self) -> usize {
        self as usize
    }
}

serde_by_name!(Stereo, as_str, parse, "stereo channel: use mix, left or right");

/// One source's audio, in blocks, as its conventional channels hear it.
pub struct AudioIn {
    /// The block being filled, and the last one run: mix, left, right.
    fill: usize,
    next: [Vec<f32>; 3],
    out: [Vec<f32>; 3],
    blocks: u64,
    /// Recent audio of each, for pre-roll.
    history: [VecDeque<f32>; 3],
    keep: usize,
    /// Recent block powers of each (none from a gap), for the floor.
    powers: [VecDeque<f64>; 3],
    power: [f64; 3],
}

impl AudioIn {
    /// Keeping `history_s` of audio for pre-roll.
    pub fn new(history_s: f64) -> Self {
        let keep = (history_s * AUDIO_RATE).ceil() as usize;
        AudioIn { fill: 0, next: Default::default(), out: Default::default(), blocks: 0, history: Default::default(), keep, powers: Default::default(), power: [0.0; 3] }
    }

    /// Its rate (and its channels'), Hz.
    pub fn fs(&self) -> f64 {
        AUDIO_RATE
    }

    /// Seconds of audio one block is.
    pub fn block_seconds(&self) -> f64 {
        BLOCK as f64 / AUDIO_RATE
    }

    /// Samples (per channel) taken so far, the next block's start.
    pub fn sample_position(&self) -> u64 {
        self.blocks * BLOCK as u64 + self.fill as u64
    }

    /// Audio of `channels` channels, interleaved (1: mono; 2: left, right;
    /// more: the first two). Takes until a block completes (and runs it) or the
    /// audio runs out; returns (samples taken, whether a block ran). `gap`:
    /// silence for audio that never came, which the floor isn't taken from.
    pub fn feed(&mut self, audio: &[f32], channels: usize, gap: bool) -> (usize, bool) {
        let channels = channels.max(1);
        let frames = (audio.len() / channels).min(BLOCK - self.fill);
        for f in audio[..frames * channels].chunks_exact(channels) {
            let (l, r) = (f[0], if channels > 1 { f[1] } else { f[0] });
            for (b, s) in self.next.iter_mut().zip([(l + r) / 2.0, l, r]) {
                b.push(s);
            }
        }
        self.fill += frames;
        let ran = self.fill == BLOCK;
        if ran {
            self.run_block(gap);
        }
        (frames * channels, ran)
    }

    fn run_block(&mut self, gap: bool) {
        for i in 0..3 {
            std::mem::swap(&mut self.out[i], &mut self.next[i]);
            self.next[i].clear();
            let block = &self.out[i];
            self.power[i] = block.iter().map(|&s| (s * s) as f64).sum::<f64>() / BLOCK as f64;
            let h = &mut self.history[i];
            h.extend(block.iter().copied());
            while h.len() > self.keep {
                h.pop_front();
            }
            if !gap {
                let p = &mut self.powers[i];
                p.push_back(self.power[i]);
                if p.len() > FLOOR_BLOCKS {
                    p.pop_front();
                }
            }
        }
        self.fill = 0;
        self.blocks += 1;
    }

    /// The last block of `pick`.
    pub fn output(&self, pick: Stereo) -> &[f32] {
        &self.out[pick.index()]
    }

    /// The last block's power (mean square) of `pick`.
    pub fn power(&self, pick: Stereo) -> f64 {
        self.power[pick.index()]
    }

    /// Each pick's quiet floor (in [`Stereo`] order): the power a channel's
    /// is measured against, as a radio's band power is against its noise
    /// ([`super::Channelizer::noise_profile`]). 0 before any audio.
    pub fn noise_profile(&self, out: &mut [f64]) {
        for (o, p) in out.iter_mut().zip(&self.powers) {
            *o = if p.is_empty() {
                0.0
            } else {
                let mut v: Vec<f64> = p.iter().copied().collect();
                let k = ((v.len() - 1) as f64 * FLOOR_QUANTILE) as usize;
                let (_, q, _) = v.select_nth_unstable_by(k, f64::total_cmp);
                q.clamp(dbfs(MIN_FLOOR_DBFS), dbfs(MAX_FLOOR_DBFS))
            };
        }
    }

    /// The audio of `pick` from up to `preroll_s` before the next block, and the
    /// sample its first is.
    pub fn preroll(&self, pick: Stereo, preroll_s: f64) -> (Vec<f32>, u64) {
        let h = &self.history[pick.index()];
        let n = h.len().min((preroll_s * AUDIO_RATE).round() as usize);
        (h.range(h.len() - n..).copied().collect(), self.sample_position() - n as u64)
    }
}

/// A power, from dBFS (a full-scale sine is −3).
fn dbfs(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

/// An analog receiver's back end for audio demodulated already: the FM
/// receiver's voice high-pass (CTCSS and DCS below it, kept for the tone
/// detector) and its squelch gate ([`Gate::detected`]), the audio's own level
/// standing in for a carrier. Audio has no carrier through the pauses between
/// words: the gate holds for [`WORD_GAP_S`] at least.
pub struct AudioGate {
    level: f32,
    a: f32,
    hpf: [Biquad; HPF_SOS.len()],
    gate: Gate,
}

/// The pause between words a gate on audio holds through, s.
pub const WORD_GAP_S: f64 = 0.5;
/// The level's smoothing, s (as an FM receiver's carrier meter's).
const LEVEL_TAU_S: f64 = 0.010;

impl AudioGate {
    pub fn new(timing: SquelchTiming) -> Self {
        let timing = SquelchTiming { hang_s: timing.hang_s.max(WORD_GAP_S), ..timing };
        AudioGate { level: 0.0, a: (1.0 - (-1.0 / (AUDIO_RATE * LEVEL_TAU_S)).exp()) as f32, hpf: HPF_SOS.map(Biquad::new), gate: Gate::detected(AUDIO_RATE, timing, LEVEL_TAU_S) }
    }

    /// How far behind the audio given it its own comes out, s ([`Gate::delay_s`]).
    pub fn delay_s(&self) -> f64 {
        self.gate.delay_s()
    }

    /// 8 kHz audio; the gate open while its level is over `open_power` (a
    /// power, as [`AudioIn::power`]). What it lets through goes to `out` (and
    /// before the high-pass to `low`). Returns whether the level was up.
    pub fn push(&mut self, audio: &[f32], open_power: f32, out: &mut Vec<f32>, mut low: Option<&mut Vec<f32>>) -> bool {
        let mut up_any = false;
        for &raw in audio {
            self.level += self.a * (raw * raw - self.level);
            let up = self.level > open_power;
            up_any |= up;
            self.gate.decide(up, up);
            let s = self.hpf.iter_mut().fold(raw, |s, h| h.step(s));
            self.gate.audio(s, raw, out, low.as_deref_mut());
        }
        up_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(dbfs: f64, n: usize) -> Vec<f32> {
        let a = (2.0 * 10f64.powf(dbfs / 10.0)).sqrt() as f32;
        (0..n).map(|i| a * (i as f32 * 0.785).sin()).collect()
    }

    /// Stereo: mix is the two averaged, left and right each alone; mono is all three.
    #[test]
    fn stereo_picks() {
        let mut a = AudioIn::new(1.0);
        let lr: Vec<f32> = (0..BLOCK).flat_map(|_| [0.5, -0.25]).collect();
        assert_eq!(a.feed(&lr, 2, false), (2 * BLOCK, true));
        assert_eq!(a.output(Stereo::Left)[0], 0.5);
        assert_eq!(a.output(Stereo::Right)[0], -0.25);
        assert_eq!(a.output(Stereo::Mix)[0], 0.125);
        a.feed(&vec![0.3; BLOCK], 1, false);
        assert!(Stereo::ALL.iter().all(|&p| a.output(p)[0] == 0.3));
    }

    /// Blocks are whole whatever the pieces fed: a block runs when one fills.
    #[test]
    fn blocks_from_any_pieces() {
        let mut a = AudioIn::new(1.0);
        assert_eq!(a.feed(&[0.0; 100], 1, false), (100, false));
        assert_eq!(a.feed(&[0.0; 100], 1, false), (60, true));
        assert_eq!(a.sample_position(), BLOCK as u64);
    }

    /// The floor is the feed's quiet, not its loud: a call over a quiet feed
    /// leaves it where the quiet was. A gap leaves it as it was.
    #[test]
    fn floor_is_the_quiet() {
        let mut a = AudioIn::new(1.0);
        let feed = |a: &mut AudioIn, x: &[f32], gap: bool| {
            let mut off = 0;
            while off < x.len() {
                off += a.feed(&x[off..], 1, gap).0;
            }
        };
        feed(&mut a, &sine(-80.0, 4 * 8000), false);
        feed(&mut a, &sine(-20.0, 8000 / 2), false);
        let mut f = [0.0; 3];
        a.noise_profile(&mut f);
        assert!((10.0 * f[0].log10() + 80.0).abs() < 1.0, "{}", 10.0 * f[0].log10());
        feed(&mut a, &vec![0.0; 10 * 8000], true);
        a.noise_profile(&mut f);
        assert!((10.0 * f[0].log10() + 80.0).abs() < 1.0, "a gap: as it was");
        feed(&mut a, &vec![0.0; 10 * 8000], false);
        a.noise_profile(&mut f);
        assert!((10.0 * f[0].log10() - MIN_FLOOR_DBFS).abs() < 0.1, "digital silence: the lowest floor");
    }

    /// Pre-roll: the audio just before the next block, at most what's kept.
    #[test]
    fn preroll_is_the_latest() {
        let mut a = AudioIn::new(0.05);
        let x: Vec<f32> = (0..3 * BLOCK).map(|i| i as f32).collect();
        let mut off = 0;
        while off < x.len() {
            off += a.feed(&x[off..], 1, false).0;
        }
        let (pre, start) = a.preroll(Stereo::Mix, 0.01);
        assert_eq!((pre.len(), start), (80, 400));
        assert_eq!(pre[0], 400.0);
        assert_eq!(a.preroll(Stereo::Mix, 1.0).0.len(), 400, "what's kept");
    }

    /// The gate lets the loud through, keeps a pause between words, and cuts
    /// the quiet after the last of them.
    #[test]
    fn gate_on_audio() {
        let mut g = AudioGate::new(SquelchTiming::default());
        let thr = dbfs(-50.0) as f32;
        let mut out = Vec::new();
        let mut words = sine(-20.0, 4000);
        words.extend(vec![0.0; 2000]);
        words.extend(sine(-20.0, 4000));
        assert!(g.push(&words, thr, &mut out, None));
        g.push(&vec![0.0; 16000], thr, &mut out, None);
        // Both words and the pause between them; the quiet after cut but for the gate's edges.
        assert!(out.len() >= 9_900 && out.len() <= 10_600, "{}", out.len());
        out.clear();
        assert!(!g.push(&sine(-60.0, 8000), thr, &mut out, None));
        assert!(out.is_empty(), "under the threshold: shut");
    }
}
