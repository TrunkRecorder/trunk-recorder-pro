//! The SmartNet control channel receiver: 3600 baud NRZ 2FSK.
//!
//! ```text
//! channel IQ → discriminator (Hz) → one-symbol moving average (matched filter)
//!   → level tracker (the two tones' mean frequencies; their midpoint is the
//!     carrier offset, so no separate AFC)
//!   → symbol clock (zero-crossing DPLL: crossings belong halfway between
//!     sampling instants) → bits with a reliability
//! ```
//!
//! Trunk Recorder's chain (OP25 fsk4_demod in 2-level mode behind a clamped
//! PLL discriminator) is replaced by this: the unclamped discriminator keeps
//! the tones symmetric under a carrier offset, and the level tracker removes
//! the offset.

use std::f64::consts::PI;

use num_complex::Complex32;

pub const SYMBOL_RATE: f64 = 3600.0;
/// Nominal deviation of each tone, Hz (the level tracker's starting point).
const NOMINAL_DEV_HZ: f32 = 2400.0;
/// Smallest believable tone separation, Hz: below it the levels are pushed apart.
const MIN_SPREAD_HZ: f32 = 1200.0;
/// Level tracker step per decided symbol.
const LEVEL_ALPHA: f32 = 0.02;
/// Symbol clock correction per zero crossing, fraction of the error.
const CLOCK_GAIN: f64 = 0.06;

/// One decided bit: `soft` > 0 means 1; |soft| ≈ 1 at a nominal tone.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bit {
    pub soft: f32,
    /// Channel-sample instant it was sampled at.
    pub sample: f64,
}

pub struct Fsk2 {
    sps: f64,
    last: Complex32,
    to_hz: f32,
    ma: Vec<f32>,
    ma_pos: usize,
    ma_sum: f32,
    hi: f32,
    lo: f32,
    /// Symbol phase at the previous sample: a symbol is sampled as it passes 1.
    phase: f64,
    prev: f32,
    n: u64,
}

impl Fsk2 {
    pub fn new(rate: f64) -> Self {
        let sps = rate / SYMBOL_RATE;
        Fsk2 {
            sps,
            last: Complex32::new(1.0, 0.0),
            to_hz: (rate / (2.0 * PI)) as f32,
            ma: vec![0.0; sps.round().max(1.0) as usize],
            ma_pos: 0,
            ma_sum: 0.0,
            hi: NOMINAL_DEV_HZ,
            lo: -NOMINAL_DEV_HZ,
            phase: 0.0,
            prev: 0.0,
            n: 0,
        }
    }

    /// Carrier offset: the midpoint of the two tones, Hz (+ = above the channel).
    pub fn offset_hz(&self) -> f32 {
        (self.hi + self.lo) / 2.0
    }

    /// Tone separation / 2, Hz.
    pub fn deviation_hz(&self) -> f32 {
        (self.hi - self.lo) / 2.0
    }

    pub fn push(&mut self, iq: &[Complex32], out: &mut Vec<Bit>) {
        let step = 1.0 / self.sps;
        let len = self.ma.len() as f32;
        for &x in iq {
            let f = (x * self.last.conj()).arg() * self.to_hz;
            self.last = x;
            self.ma_sum += f - self.ma[self.ma_pos];
            self.ma[self.ma_pos] = f;
            self.ma_pos = (self.ma_pos + 1) % self.ma.len();
            let y = self.ma_sum / len;
            let center = self.offset_hz();
            let (a, b) = (self.prev - center, y - center);
            let mut p0 = self.phase;
            // Zero crossing between the previous sample and this one: it
            // should fall at phase 0.5 (midway between two sampling instants).
            if (a < 0.0) != (b < 0.0) {
                let frac = (a / (a - b)) as f64;
                let mut err = p0 + frac * step - 0.5;
                err -= err.round();
                p0 -= CLOCK_GAIN * err;
            }
            let p1 = p0 + step;
            if p1 >= 1.0 {
                // Sampling instant between the two samples.
                let t = ((1.0 - p0) / step).clamp(0.0, 1.0) as f32;
                let v = self.prev + (y - self.prev) * t;
                self.decide(v, self.n as f64 - 1.0 + t as f64, out);
                self.phase = p1 - 1.0;
            } else {
                self.phase = p1;
            }
            self.prev = y;
            self.n += 1;
        }
    }

    fn decide(&mut self, v: f32, sample: f64, out: &mut Vec<Bit>) {
        let center = self.offset_hz();
        let half = (self.hi - self.lo) / 2.0;
        if v > center {
            self.hi += LEVEL_ALPHA * (v - self.hi);
        } else {
            self.lo += LEVEL_ALPHA * (v - self.lo);
        }
        if self.hi - self.lo < MIN_SPREAD_HZ {
            let c = self.offset_hz();
            self.hi = c + MIN_SPREAD_HZ / 2.0;
            self.lo = c - MIN_SPREAD_HZ / 2.0;
        }
        out.push(Bit { soft: ((v - center) / half).clamp(-2.0, 2.0), sample });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// NRZ 2FSK at `rate` for `bits` (1 = +dev), with a carrier offset and a
    /// fractional starting phase.
    pub fn modulate(bits: &[u8], rate: f64, dev_hz: f64, offset_hz: f64, start: f64) -> Vec<Complex32> {
        let sps = rate / SYMBOL_RATE;
        let n = (bits.len() as f64 * sps) as usize;
        let mut ph = 0.0f64;
        (0..n)
            .map(|i| {
                let k = ((i as f64 + start) / sps) as usize;
                let b = bits[k.min(bits.len() - 1)];
                let f = if b == 1 { dev_hz } else { -dev_hz } + offset_hz;
                ph += 2.0 * PI * f / rate;
                Complex32::from_polar(1.0, ph as f32)
            })
            .collect()
    }

    pub fn prbs(n: usize, mut s: u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s & 1) as u8
            })
            .collect()
    }

    #[test]
    fn recovers_bits_with_offset_and_odd_rate() {
        for &(rate, offset) in &[(28_125.0, 900.0), (37_500.0, -1500.0), (18_000.0, 0.0)] {
            let bits = prbs(4000, 0x1234_5678);
            let iq = modulate(&bits, rate, 2400.0, offset, 0.37);
            let mut rx = Fsk2::new(rate);
            let mut out = Vec::new();
            rx.push(&iq, &mut out);
            let got: Vec<u8> = out.iter().map(|b| (b.soft > 0.0) as u8).collect();
            // Align (the matched filter delays by about one symbol).
            let best = (0..4)
                .map(|lag| {
                    let tail = 1000;
                    (lag, (tail..got.len().min(bits.len() + lag) - 1).filter(|&i| got[i] != bits[i - lag]).count())
                })
                .min_by_key(|&(_, e)| e)
                .unwrap();
            assert_eq!(best.1, 0, "rate {rate} offset {offset}: {} errors at lag {}", best.1, best.0);
            assert!((rx.offset_hz() - offset as f32).abs() < 100.0, "offset estimate {}", rx.offset_hz());
        }
    }
}
