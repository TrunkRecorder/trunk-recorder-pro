//! The SmartNet control channel receiver: 3600 baud NRZ 2FSK.
//!
//! ```text
//! channel IQ → discriminator (Hz) → one-symbol moving average
//!   → level tracker (the two tones' mean frequencies; their midpoint is the
//!     carrier offset, so no separate AFC)
//! channel IQ → each tone's energy over a symbol (noncoherent matched filters,
//!   at the tracked tone frequencies) → (E_hi − E_lo) / (E_hi + E_lo)
//!   → symbol clock (zero-crossing DPLL: crossings belong halfway between
//!     sampling instants) → bits with a reliability
//! ```
//!
//! Near threshold the discriminator's clicks cost bits the tone energies
//! don't: 1.5–2 dB on WMATA (`Fsk2Options::tones` off gives the old decisions).
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

/// Receiver settings, for weak-signal comparisons (`tool snr --variant`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fsk2Options {
    /// Moving average (the matched filter) width, symbols.
    pub ma_symbols: f64,
    /// Symbol clock correction per zero crossing.
    pub clock_gain: f64,
    /// Level tracker step per symbol.
    pub level_alpha: f32,
    /// Decide by the two tones' energies over a symbol (noncoherent matched
    /// filters) instead of the discriminator: 1.5–2 dB less signal for the
    /// same OSWs on WMATA (`tool snr`). The discriminator still tracks the
    /// tones (carrier offset) and runs the clock's level tracker.
    pub tones: bool,
}

impl Default for Fsk2Options {
    fn default() -> Self {
        Fsk2Options { ma_symbols: 1.0, clock_gain: CLOCK_GAIN, level_alpha: LEVEL_ALPHA, tones: true }
    }
}

impl Fsk2Options {
    /// Apply one `name=value` setting; false if unknown.
    pub fn set(&mut self, p: &str) -> bool {
        let Some((k, v)) = p.split_once('=') else { return false };
        let Ok(v) = v.parse::<f64>() else { return false };
        match k {
            "ma" => self.ma_symbols = v,
            "clock" => self.clock_gain = v,
            "level" => self.level_alpha = v as f32,
            "tones" => self.tones = v != 0.0,
            _ => return false,
        }
        true
    }
}

pub struct Fsk2 {
    opts: Fsk2Options,
    sps: f64,
    last: Complex32,
    to_hz: f32,
    ma: Vec<f32>,
    ma_pos: usize,
    /// (f64: a running sum kept for days on a control channel would drift in f32.)
    ma_sum: f64,
    hi: f32,
    lo: f32,
    /// Symbol phase at the previous sample: a symbol is sampled as it passes 1.
    phase: f64,
    prev: f32,
    n: u64,
    /// Tone detector: per tone, the last symbol's products x·e^(−jωn) and their sum.
    tone_hist: [Vec<Complex32>; 2],
    tone_sum: [num_complex::Complex64; 2],
    tone_pos: usize,
    tone_ph: [f64; 2],
    /// The discriminator's value at the last sample (levels are tracked on it).
    prev_disc: f32,
    rate: f64,
}

impl Fsk2 {
    pub fn new(rate: f64) -> Self {
        Self::with_options(rate, Fsk2Options::default())
    }

    pub fn with_options(rate: f64, opts: Fsk2Options) -> Self {
        let sps = rate / SYMBOL_RATE;
        Fsk2 {
            opts,
            sps,
            last: Complex32::new(1.0, 0.0),
            to_hz: (rate / (2.0 * PI)) as f32,
            ma: vec![0.0; (sps * opts.ma_symbols).round().max(1.0) as usize],
            ma_pos: 0,
            ma_sum: 0.0,
            hi: NOMINAL_DEV_HZ,
            lo: -NOMINAL_DEV_HZ,
            phase: 0.0,
            prev: 0.0,
            n: 0,
            tone_hist: [vec![Complex32::default(); sps.round().max(1.0) as usize], vec![Complex32::default(); sps.round().max(1.0) as usize]],
            tone_sum: [num_complex::Complex64::default(); 2],
            tone_pos: 0,
            tone_ph: [0.0; 2],
            prev_disc: 0.0,
            rate,
        }
    }

    /// The tone detector's statistic for sample `x`: (E_hi − E_lo) / (E_hi + E_lo),
    /// scaled to the discriminator's Hz so the clock and slicer take it as they are.
    fn tone_stat(&mut self, x: Complex32) -> f32 {
        let freqs = [self.lo as f64, self.hi as f64];
        let mut e = [0f32; 2];
        for k in 0..2 {
            self.tone_ph[k] = (self.tone_ph[k] - 2.0 * PI * freqs[k] / self.rate) % (2.0 * PI);
            let p = x * Complex32::from_polar(1.0, self.tone_ph[k] as f32);
            let h = &mut self.tone_hist[k];
            let d = p - h[self.tone_pos];
            self.tone_sum[k] += num_complex::Complex64::new(d.re as f64, d.im as f64);
            h[self.tone_pos] = p;
            e[k] = self.tone_sum[k].norm_sqr() as f32;
        }
        self.tone_pos = (self.tone_pos + 1) % self.tone_hist[0].len();
        let d = (e[1] - e[0]) / (e[1] + e[0]).max(1e-20);
        self.offset_hz() + d * (self.hi - self.lo) / 2.0
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
            self.ma_sum += (f - self.ma[self.ma_pos]) as f64;
            self.ma[self.ma_pos] = f;
            self.ma_pos = (self.ma_pos + 1) % self.ma.len();
            let disc = (self.ma_sum / len as f64) as f32;
            let y = if self.opts.tones { self.tone_stat(x) } else { disc };
            let center = self.offset_hz();
            let (a, b) = (self.prev - center, y - center);
            let mut p0 = self.phase;
            // Zero crossing between the previous sample and this one: it
            // should fall at phase 0.5 (midway between two sampling instants).
            if (a < 0.0) != (b < 0.0) {
                let frac = (a / (a - b)) as f64;
                let mut err = p0 + frac * step - 0.5;
                err -= err.round();
                p0 -= self.opts.clock_gain * err;
            }
            let p1 = p0 + step;
            if p1 >= 1.0 {
                // Sampling instant between the two samples.
                let t = ((1.0 - p0) / step).clamp(0.0, 1.0) as f32;
                let v = self.prev + (y - self.prev) * t;
                let lv = self.prev_disc + (disc - self.prev_disc) * t;
                self.decide(v, lv, self.n as f64 - 1.0 + t as f64, out);
                self.phase = p1 - 1.0;
            } else {
                self.phase = p1;
            }
            self.prev = y;
            self.prev_disc = disc;
            self.n += 1;
        }
    }

    /// `v` decides the bit; `level` (the discriminator's value) tracks the tones.
    fn decide(&mut self, v: f32, level: f32, sample: f64, out: &mut Vec<Bit>) {
        let center = self.offset_hz();
        let half = (self.hi - self.lo) / 2.0;
        let a = self.opts.level_alpha;
        if v > center {
            self.hi += a * (level - self.hi);
        } else {
            self.lo += a * (level - self.lo);
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
