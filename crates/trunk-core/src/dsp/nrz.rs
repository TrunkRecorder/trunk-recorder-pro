//! Slow NRZ data sent under the voice of an FM channel: DCS codes (134.4
//! baud) and LTR's status words (300 baud), from the 8 kHz audio before the
//! voice high-pass.
//!
//! ```text
//! 8 kHz audio → Decimator (windowed-sinc low-pass, ÷N)
//!   → Slicers: DC (the carrier's offset) out, then NPHASE integrate-and-dump
//!     slicers on bit clocks staggered by 1/NPHASE of a bit: one of them is
//!     always within half a phase of the air's (no clock recovery to lose)
//! ```
//!
//! What a decoder makes of each slicer's bits — the frames, and which
//! slicer read them — is its own ([`super::inband::subaudible`], [`crate::ltr`]).

use super::filters::lowpass;

/// A windowed-sinc low-pass, every `factor`th output kept.
pub struct Decimator {
    taps: Vec<f32>,
    /// The last `taps.len()` inputs, twice over (a window without wrapping).
    ring: Vec<f32>,
    pos: usize,
    phase: usize,
    factor: usize,
}

impl Decimator {
    /// `n` taps, cutoff `cutoff` of the input rate, then ÷`factor`.
    pub fn new(n: usize, cutoff: f64, factor: usize) -> Self {
        Decimator { taps: lowpass(n, cutoff), ring: vec![0.0; 2 * n], pos: 0, phase: 0, factor }
    }

    /// Take `x`, handing each output to `emit`.
    pub fn push(&mut self, x: &[f32], mut emit: impl FnMut(f32)) {
        let n = self.taps.len();
        for &v in x {
            self.ring[self.pos] = v;
            self.ring[self.pos + n] = v;
            self.pos = (self.pos + 1) % n;
            self.phase += 1;
            if self.phase == self.factor {
                self.phase = 0;
                emit(self.taps.iter().zip(&self.ring[self.pos..self.pos + n]).map(|(t, v)| t * v).sum());
            }
        }
    }
}

/// Slicers in a bank (staggered bit clocks).
pub const NPHASE: usize = 8;

#[derive(Clone, Copy, Default)]
struct Slicer {
    clock: f32,
    acc: f32,
}

/// [`NPHASE`] integrate-and-dump slicers on one stream of samples, after
/// its DC is taken out.
pub struct Slicers {
    /// Bits a sample.
    step: f32,
    /// The DC follower's share of each sample.
    dc_alpha: f32,
    dc: f32,
    bank: [Slicer; NPHASE],
}

impl Slicers {
    /// `baud` bits a second at `rate` samples a second; DC followed by `dc_alpha` of each
    /// sample (slower than a run of like bits: DC-free codes can follow faster).
    pub fn new(baud: f32, rate: f32, dc_alpha: f32) -> Self {
        Slicers {
            step: baud / rate,
            dc_alpha,
            dc: 0.0,
            bank: std::array::from_fn(|k| Slicer { clock: k as f32 / NPHASE as f32, ..Default::default() }),
        }
    }

    /// One sample: `bit(k, b)` for each slicer `k` that finished a bit on it, in order.
    #[inline]
    pub fn push(&mut self, y: f32, mut bit: impl FnMut(usize, bool)) {
        self.dc += (y - self.dc) * self.dc_alpha;
        let v = y - self.dc;
        let step = self.step;
        for (k, s) in self.bank.iter_mut().enumerate() {
            s.clock += step;
            if s.clock < 1.0 {
                s.acc += v;
                continue;
            }
            s.clock -= 1.0;
            // (The part of this sample past the bit's end starts the next.)
            let over = s.clock / step;
            let b = s.acc + v * (1.0 - over) > 0.0;
            s.acc = v * over;
            bit(k, b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bit pattern at 300 baud, sampled at 2 kHz with a DC offset, comes
    /// out of every slicer (each a bit or two late at most) with the offset gone.
    #[test]
    fn every_slicer_reads_the_bits() {
        let pattern: Vec<bool> = (0..200u32).map(|i| (i.wrapping_mul(2654435761) >> 7) & 1 == 1).collect();
        let rate = 2000.0;
        let n = (pattern.len() as f32 / 300.0 * rate) as usize;
        let mut s = Slicers::new(300.0, rate, 0.001);
        let mut got: Vec<Vec<bool>> = vec![Vec::new(); NPHASE];
        for i in 0..n {
            let b = pattern[(i as f32 * 300.0 / rate) as usize];
            s.push(if b { 0.3 } else { -0.3 } + 0.2, |k, b| got[k].push(b));
        }
        let want: String = pattern[50..150].iter().map(|&b| if b { '1' } else { '0' }).collect();
        let found = got.iter().filter(|g| g.iter().map(|&b| if b { '1' } else { '0' }).collect::<String>().contains(&want)).count();
        assert!(found >= NPHASE / 2, "{found} of {NPHASE} slicers read it");
    }
}
