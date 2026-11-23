//! Model of the FPGA filter bank (`issues/01-filter-bank-model.md`): channels
//! split from an 8 MS/s bladeRF stream by a 1024-bin polyphase bank, then
//! finished on the host. It decides the design's parameters before any HDL,
//! and [`FixedBank`] is the arithmetic the RTL has to reproduce bit for bit.
//!
//! ```text
//! wideband → prototype filter folded to M points → M-point FFT → (−j)^(k·q)
//!          → the bin nearest a channel, 31.25 kS/s
//! (host)   → residual NCO → channel FIR → a head's output
//! ```
//!
//! Each bin is that of a mixer, the prototype low-pass and a decimator, with
//! phase referred to absolute input time. So a bin can be switched on at any
//! moment and its phase carries on as if it had always been running.
//!
//! The FFT is radix-2² decimation in frequency, as a pipeline would do it:
//! butterflies with their trivial −j, a complex multiply every second stage.

use std::f64::consts::PI;

use num_complex::{Complex32, Complex64};

use super::filters::blackman_sinc;

/// Bins, and so points of the FFT.
pub const M: usize = 1024;
/// Input samples per output sample of every bin.
pub const HOP: usize = 256;
/// Output samples in a block: the 12288 input samples of the [`super::Channelizer`]'s block.
pub const BLOCK_HOPS: usize = 48;
/// The only input rate: the prototype's widths scale with it.
pub const FS: f64 = 8_000_000.0;
pub const OUTPUT_RATE: f64 = FS / HOP as f64;
pub const BIN_HZ: f64 = FS / M as f64;
/// One input code of a bladeRF's SC16 Q11 sample is 1/2048.
const INPUT_FRAC_BITS: i32 = 11;
/// Output samples are Q15.
const OUTPUT_BITS: i32 = 16;
/// Butterfly stages: two per radix-2² stage.
const FFT_BITS: usize = 10;

/// The output gain shift the radios run with (issue 06's status).
pub const RADIO_GAIN_SHIFT: u32 = 2;

/// What the hardware's word widths and filter are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Design {
    /// Prototype length: a multiple of [`HOP`], so the time reference stays a power of −j.
    pub taps: usize,
    /// Kaiser window β.
    pub beta: f64,
    pub coef_bits: u32,
    /// The fold's and the FFT's data width.
    pub data_bits: u32,
    pub twiddle_bits: u32,
    /// Bit `s` set: halve after butterfly stage `s`, rounding.
    pub fft_shifts: u16,
    /// Output left shift, saturating: for signals too weak for the fixed scale.
    pub gain_shift: u32,
}

impl Default for Design {
    fn default() -> Self {
        Design { taps: 4096, beta: 7.0, coef_bits: 18, data_bits: 18, twiddle_bits: 18, fft_shifts: 0b1110111011, gain_shift: 0 }
    }
}

impl Design {
    /// `taps=4096,beta=7,coef=18,data=24,tw=18,shifts=0,gain=0`, any of them, on top of the default.
    pub fn parse(spec: &str) -> Result<Design, String> {
        let mut d = Design::default();
        for part in spec.split(',').filter(|p| !p.is_empty() && *p != "default") {
            let (k, v) = part.split_once('=').ok_or_else(|| format!("filter bank: {part}: expected name=value"))?;
            let bad = |_| format!("filter bank: {part}: bad number");
            match k {
                "taps" => d.taps = v.parse().map_err(|e: std::num::ParseIntError| bad(e.to_string()))?,
                "beta" => d.beta = v.parse().map_err(|e: std::num::ParseFloatError| bad(e.to_string()))?,
                "coef" => d.coef_bits = v.parse().map_err(|e: std::num::ParseIntError| bad(e.to_string()))?,
                "data" => d.data_bits = v.parse().map_err(|e: std::num::ParseIntError| bad(e.to_string()))?,
                "tw" => d.twiddle_bits = v.parse().map_err(|e: std::num::ParseIntError| bad(e.to_string()))?,
                "shifts" => d.fft_shifts = v.parse().map_err(|e: std::num::ParseIntError| bad(e.to_string()))?,
                "gain" => d.gain_shift = v.parse().map_err(|e: std::num::ParseIntError| bad(e.to_string()))?,
                _ => return Err(format!("filter bank: {k}: unknown setting")),
            }
        }
        if d.taps == 0 || d.taps % HOP != 0 {
            return Err(format!("filter bank: taps {} is not a multiple of {HOP}", d.taps));
        }
        Ok(d)
    }
}

/// The modified Bessel function I₀, by its power series.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-14 * sum {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

/// The prototype: Kaiser-windowed sinc, −6 dB at half the output rate, DC gain 1.
pub fn prototype(taps: usize, beta: f64) -> Vec<f64> {
    let m = (taps - 1) as f64 / 2.0;
    let fc = OUTPUT_RATE / 2.0 / FS;
    let mut h: Vec<f64> = (0..taps)
        .map(|i| {
            let t = i as f64 - m;
            let sinc = if t == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * t).sin() / (PI * t) };
            sinc * bessel_i0(beta * (1.0 - (t / m).powi(2)).max(0.0).sqrt()) / bessel_i0(beta)
        })
        .collect();
    let dc: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= dc);
    h
}

/// `x / 2^s`, rounding half up (a left shift when `s` is negative).
fn round_shift(x: i64, s: i32) -> i64 {
    if s > 0 {
        (x + (1i64 << (s - 1))) >> s
    } else {
        x << -s
    }
}

fn bit_reverse(i: usize) -> usize {
    i.reverse_bits() >> (usize::BITS as usize - FFT_BITS)
}

/// The prototype in floating point: what [`FixedBank`] is compared with.
pub struct FloatBank {
    h: Vec<f64>,
    ring: Vec<Complex32>,
    at: usize,
    filled: usize,
    hop: u64,
    fft: std::sync::Arc<dyn rustfft::Fft<f64>>,
    buf: Vec<Complex64>,
    frame: Vec<Complex32>,
}

impl FloatBank {
    pub fn new(d: &Design) -> Self {
        FloatBank {
            h: prototype(d.taps, d.beta),
            ring: vec![Complex32::default(); d.taps],
            at: 0,
            filled: 0,
            hop: 0,
            fft: rustfft::FftPlanner::new().plan_fft_forward(M),
            buf: vec![Complex64::default(); M],
            frame: vec![Complex32::default(); M],
        }
    }

    /// Feed samples; `each` gets every frame, its sample for each bin (bin 0 at the centre, M−k at −k).
    pub fn push(&mut self, iq: &[Complex32], mut each: impl FnMut(&[Complex32])) {
        for &x in iq {
            self.ring[self.at] = x;
            self.at = (self.at + 1) % self.ring.len();
            self.filled += 1;
            if self.filled == HOP {
                self.filled = 0;
                self.buf.fill(Complex64::default());
                let t = self.ring.len();
                for i in 0..t {
                    let v = self.ring[(self.at + i) % t];
                    self.buf[i % M] += Complex64::new(v.re as f64, v.im as f64) * self.h[i];
                }
                self.fft.process(&mut self.buf);
                let q = time_reference(self.hop, t);
                for (k, f) in self.frame.iter_mut().enumerate() {
                    let v = self.buf[k] * Complex64::new(0.0, -1.0).powu((k as u32 * q) % 4);
                    *f = Complex32::new(v.re as f32, v.im as f32);
                }
                self.hop += 1;
                each(&self.frame);
            }
        }
    }
}

/// The oldest sample of frame `hop` is at absolute input index `(hop+1)·HOP − taps`:
/// that many quarter turns of the FFT's time origin, mod 4.
fn time_reference(hop: u64, taps: usize) -> u32 {
    ((hop as i64 + 1 - (taps / HOP) as i64).rem_euclid(4)) as u32
}

/// What saturated, in samples: non-zero in normal use means the design's scale is wrong.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Saturated {
    pub fold: u64,
    pub fft: u64,
    pub output: u64,
}

/// The bank in the hardware's arithmetic: the RTL's reference.
pub struct FixedBank {
    design: Design,
    coef: Vec<i32>,
    ring: Vec<[i16; 2]>,
    at: usize,
    filled: usize,
    hop: u64,
    fold_shift: i32,
    /// (real, imaginary), index `k` for `e^{−j2πk/M}`.
    twiddle: Vec<(i32, i32)>,
    re: Vec<i64>,
    im: Vec<i64>,
    frame: Vec<[i16; 2]>,
    pub saturated: Saturated,
}

impl FixedBank {
    pub fn new(d: &Design) -> Self {
        let h = prototype(d.taps, d.beta);
        let peak = h.iter().fold(0.0f64, |a, v| a.max(v.abs()));
        // A power-of-two scale, so the only gain in the chain is exact.
        let coef_frac = (((1i64 << (d.coef_bits - 1)) - 1) as f64 / peak).log2().floor() as i32;
        let coef = h.iter().map(|v| (v * 2f64.powi(coef_frac)).round() as i32).collect();
        // A full-scale tone ends at the data word's full scale.
        let fft_gain_shift = d.fft_shifts.count_ones() as i32;
        let fold_shift = INPUT_FRAC_BITS + coef_frac - fft_gain_shift - (d.data_bits as i32 - 1);
        let tw = ((1i64 << (d.twiddle_bits - 1)) - 1) as f64;
        let twiddle = (0..M)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / M as f64;
                ((a.cos() * tw).round() as i32, (a.sin() * tw).round() as i32)
            })
            .collect();
        FixedBank {
            design: *d,
            coef,
            ring: vec![[0; 2]; d.taps],
            at: 0,
            filled: 0,
            hop: 0,
            fold_shift,
            twiddle,
            re: vec![0; M],
            im: vec![0; M],
            frame: vec![[0; 2]; M],
            saturated: Saturated::default(),
        }
    }

    /// The prototype's coefficients, oldest sample first: what the RTL's ROM holds.
    pub fn coefficients(&self) -> &[i32] {
        &self.coef
    }

    /// The fold's rounding right shift (before the FFT).
    pub fn fold_shift(&self) -> i32 {
        self.fold_shift
    }

    /// The FFT's twiddles (cos, −sin of 2πk/M), the RTL's other ROM.
    pub fn twiddles(&self) -> &[(i32, i32)] {
        &self.twiddle
    }

    /// Feed SC16 Q11 samples (12-bit I, Q); `each` gets every frame as Q15 (I, Q) per bin.
    pub fn push(&mut self, iq: &[[i16; 2]], mut each: impl FnMut(&[[i16; 2]])) {
        for &x in iq {
            self.ring[self.at] = x;
            self.at = (self.at + 1) % self.ring.len();
            self.filled += 1;
            if self.filled == HOP {
                self.filled = 0;
                self.run_frame();
                each(&self.frame);
            }
        }
    }

    fn run_frame(&mut self) {
        let t = self.ring.len();
        self.re.fill(0);
        self.im.fill(0);
        for i in 0..t {
            let v = self.ring[(self.at + i) % t];
            let c = self.coef[i] as i64;
            self.re[i % M] += v[0] as i64 * c;
            self.im[i % M] += v[1] as i64 * c;
        }
        let top = (1i64 << (self.design.data_bits - 1)) - 1;
        let mut sat = 0u64;
        for i in 0..M {
            self.re[i] = saturate(round_shift(self.re[i], self.fold_shift), top, &mut sat);
            self.im[i] = saturate(round_shift(self.im[i], self.fold_shift), top, &mut sat);
        }
        self.saturated.fold += sat;
        self.fft();
        self.output();
    }

    /// Radix-2² decimation in frequency, in place, its output in bit-reversed order.
    fn fft(&mut self) {
        let top = (1i64 << (self.design.data_bits - 1)) - 1;
        let tw_shift = self.design.twiddle_bits as i32 - 1;
        let shifts = self.design.fft_shifts;
        let (re, im) = (&mut self.re, &mut self.im);
        let mut sat = 0u64;
        let mut stage_bit = 0;
        let mut n = M;
        while n > 1 {
            for g in (0..M).step_by(n) {
                let s1 = i32::from(shifts >> stage_bit & 1);
                for i in 0..n / 2 {
                    butterfly(re, im, g + i, g + i + n / 2, false, s1, top, &mut sat);
                }
                let s2 = i32::from(shifts >> (stage_bit + 1) & 1);
                for i in 0..n / 4 {
                    butterfly(re, im, g + i, g + i + n / 4, false, s2, top, &mut sat);
                    butterfly(re, im, g + n / 2 + i, g + n / 2 + i + n / 4, true, s2, top, &mut sat);
                }
                // Blocks in order hold the output bins ≡ 0, 2, 1, 3 mod 4.
                for (block, q) in [0usize, 2, 1, 3].into_iter().enumerate() {
                    for i in 1..n / 4 {
                        let (wr, wi) = self.twiddle[i * q * (M / n) % M];
                        let (wr, wi) = (wr as i64, wi as i64);
                        let at = g + block * n / 4 + i;
                        let (a, b) = (re[at], im[at]);
                        let x = round_shift(a * wr - b * wi, tw_shift);
                        let y = round_shift(a * wi + b * wr, tw_shift);
                        re[at] = saturate(x, top, &mut sat);
                        im[at] = saturate(y, top, &mut sat);
                    }
                }
            }
            n /= 4;
            stage_bit += 2;
        }
        self.saturated.fft += sat;
    }

    /// Back in bin order, referred to absolute time, cut to Q15.
    fn output(&mut self) {
        let q = time_reference(self.hop, self.ring.len());
        self.hop += 1;
        let cut = self.design.data_bits as i32 - OUTPUT_BITS - self.design.gain_shift as i32;
        let mut sat = 0;
        for i in 0..M {
            let k = bit_reverse(i);
            let (a, b) = (self.re[i], self.im[i]);
            // (−j)^n needs no multiplier.
            let (a, b) = match (k as u32 * q) % 4 {
                0 => (a, b),
                1 => (b, -a),
                2 => (-a, -b),
                _ => (-b, a),
            };
            self.frame[k] = [
                saturate(round_shift(a, cut), i16::MAX as i64, &mut sat) as i16,
                saturate(round_shift(b, cut), i16::MAX as i64, &mut sat) as i16,
            ];
        }
        self.saturated.output += sat;
    }
}

fn saturate(v: i64, top: i64, sat: &mut u64) -> i64 {
    if v > top || v < -top - 1 {
        *sat += 1;
        v.clamp(-top - 1, top)
    } else {
        v
    }
}

/// `(a, b) → (a + b, a − b)`, with `b` first turned by −j, halved when `shift` is 1.
#[allow(clippy::too_many_arguments)]
fn butterfly(re: &mut [i64], im: &mut [i64], a: usize, b: usize, minus_j: bool, shift: i32, top: i64, sat: &mut u64) {
    let (br, bi) = if minus_j { (im[b], -re[b]) } else { (re[b], im[b]) };
    let (ar, ai) = (re[a], im[a]);
    re[a] = saturate(round_shift(ar + br, shift), top, sat);
    im[a] = saturate(round_shift(ai + bi, shift), top, sat);
    re[b] = saturate(round_shift(ar - br, shift), top, sat);
    im[b] = saturate(round_shift(ai - bi, shift), top, sat);
}

/// The bin nearest `offset_hz` from the centre, and the rest of the way to it, Hz.
pub fn bin_for(offset_hz: f64) -> (usize, f64) {
    let bin = (offset_hz / BIN_HZ).round() as i64;
    (bin.rem_euclid(M as i64) as usize, offset_hz - bin as f64 * BIN_HZ)
}

/// The host's end of a channel: the residual NCO and the head's own filter.
pub struct BankHead {
    pub bin: usize,
    residual: f64,
    taps: Vec<f32>,
    line: Vec<Complex32>,
    at: usize,
    n: u64,
}

impl BankHead {
    /// The channel filter matches the [`super::Channelizer`]'s 4097 taps at
    /// 8 MS/s: the same Blackman window, 16 output samples long.
    pub fn new(offset_hz: f64, cutoff_hz: f64) -> Self {
        let (bin, _) = bin_for(offset_hz);
        Self::on_bin(bin, offset_hz, cutoff_hz, 0)
    }

    /// On a given bin (a neighbour of the nearest, when that one is already
    /// running), its first sample being output sample `first` of every bin:
    /// the NCO's phase follows absolute time, as the bins' does.
    pub fn on_bin(bin: usize, offset_hz: f64, cutoff_hz: f64, first: u64) -> Self {
        let signed = if bin >= M / 2 { bin as f64 - M as f64 } else { bin as f64 };
        let residual_hz = offset_hz - signed * BIN_HZ;
        let h = blackman_sinc(17, cutoff_hz / OUTPUT_RATE);
        let dc: f64 = h.iter().sum();
        BankHead {
            bin,
            residual: -2.0 * PI * residual_hz / OUTPUT_RATE,
            taps: h.iter().map(|v| (v / dc) as f32).collect(),
            line: vec![Complex32::default(); h.len()],
            at: 0,
            n: first,
        }
    }

    /// One sample of the bin; one sample of the channel.
    pub fn push(&mut self, x: Complex32) -> Complex32 {
        let ph = (self.residual * self.n as f64) % (2.0 * PI);
        self.n += 1;
        self.line[self.at] = x * Complex32::new(ph.cos() as f32, ph.sin() as f32);
        self.at = (self.at + 1) % self.line.len();
        let mut acc = Complex32::default();
        for (i, &t) in self.taps.iter().enumerate() {
            acc += self.line[(self.at + i) % self.taps.len()] * t;
        }
        acc
    }
}

/// A Q15 output sample as the receivers take it (unit gain, as the channelizer's).
pub fn q15(v: [i16; 2]) -> Complex32 {
    Complex32::new(v[0] as f32 / 32768.0, v[1] as f32 / 32768.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Channelizer;

    fn tone(hz: f64, amp: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let ph = 2.0 * PI * hz * i as f64 / FS;
                Complex32::new((amp * ph.cos()) as f32, (amp * ph.sin()) as f32)
            })
            .collect()
    }

    /// As SC16 Q11, rounded: what the radio gives.
    fn to_q11(iq: &[Complex32]) -> Vec<[i16; 2]> {
        iq.iter().map(|c| [(c.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16, (c.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16]).collect()
    }

    fn noise(n: usize, sigma: f32, seed: u64) -> Vec<Complex32> {
        let mut s = seed | 1;
        let mut u = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        (0..n).map(|_| Complex32::from_polar((-u().ln()).sqrt() as f32 * sigma, (2.0 * PI * u()) as f32)).collect()
    }

    fn fixed_channel(d: &Design, iq: &[Complex32], offset: f64, cutoff: f64) -> Vec<Complex32> {
        let mut bank = FixedBank::new(d);
        let mut head = BankHead::new(offset, cutoff);
        let mut out = Vec::new();
        bank.push(&to_q11(iq), |f| out.push(head.push(q15(f[head.bin]))));
        out
    }

    fn float_channel(d: &Design, iq: &[Complex32], offset: f64, cutoff: f64) -> Vec<Complex32> {
        let mut bank = FloatBank::new(d);
        let mut head = BankHead::new(offset, cutoff);
        let mut out = Vec::new();
        bank.push(iq, |f| out.push(head.push(f[head.bin])));
        out
    }

    fn power_db(x: &[Complex32]) -> f64 {
        10.0 * (x.iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / x.len() as f64 + 1e-30).log10()
    }

    /// Skip what the filters are still filling.
    const SETTLE: usize = 64;

    /// |H(f)| of a real FIR, in dB relative to DC.
    fn gain_db(h: &[f64], hz: f64, fs: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in h.iter().enumerate() {
            let ph = 2.0 * PI * hz / fs * i as f64;
            re += v * ph.cos();
            im += v * ph.sin();
        }
        10.0 * ((re * re + im * im) / h.iter().sum::<f64>().powi(2) + 1e-30).log10()
    }

    /// The worst thing that folds into a channel, by calculation over every
    /// frequency up to 200 kHz from the bin: the prototype at the tone, plus the
    /// channel filter where it lands. The channel is at the worst offset between
    /// bins, and only what lands within it (6.25 kHz, 1 kHz of tuning error) counts.
    fn alias_db(h: &[f64]) -> f64 {
        let fir = blackman_sinc(17, 7000.0 / OUTPUT_RATE);
        let residual = BIN_HZ / 2.0;
        let mut worst = f64::MIN;
        let mut f = 20_000.0;
        while f < 200_000.0 {
            for tone in [f, -f] {
                let folded = (tone - residual + OUTPUT_RATE / 2.0).rem_euclid(OUTPUT_RATE) - OUTPUT_RATE / 2.0;
                if folded.abs() <= 7250.0 {
                    worst = worst.max(gain_db(h, tone, FS) + gain_db(&fir, folded, OUTPUT_RATE));
                }
            }
            f += 50.0;
        }
        worst
    }

    #[test]
    fn fixed_fft_matches_a_real_fft() {
        let d = Design { data_bits: 30, twiddle_bits: 24, fft_shifts: 0, ..Design::default() };
        let mut bank = FixedBank::new(&d);
        let input = noise(M, 1.0, 7);
        for (i, v) in input.iter().enumerate() {
            bank.re[i] = (v.re * 1e5) as i64;
            bank.im[i] = (v.im * 1e5) as i64;
        }
        let mut want: Vec<Complex64> = (0..M).map(|i| Complex64::new(bank.re[i] as f64, bank.im[i] as f64)).collect();
        rustfft::FftPlanner::new().plan_fft_forward(M).process(&mut want);
        bank.fft();
        // Outputs reach millions; what is left is the multipliers' rounding noise.
        let err: f64 = (0..M).map(|i| (Complex64::new(bank.re[i] as f64, bank.im[i] as f64) - want[bit_reverse(i)]).norm_sqr()).sum();
        let rms = (err / M as f64).sqrt();
        assert!(rms < 10.0, "rms error {rms}");
        assert_eq!(bank.saturated, Saturated::default());
    }

    /// Bin centre, the worst point between two bins, and either sign.
    #[test]
    fn a_tone_comes_out_at_its_frequency_and_level_with_phase_unbroken() {
        let d = Design::default();
        for offset in [0.0, 40.5 * BIN_HZ, -40.5 * BIN_HZ, 300.25 * BIN_HZ, -3_000_000.0] {
            // 300 Hz from the channel's centre, so the output turns at a known rate.
            let out = fixed_channel(&d, &tone(offset + 300.0, 0.25, HOP * 800), offset, 7000.0);
            let out = &out[SETTLE..];
            assert!((power_db(out) - 20.0 * 0.25f64.log10()).abs() < 0.1, "at {offset}: {:.2} dB", power_db(out));
            let want = 2.0 * PI * 300.0 / OUTPUT_RATE;
            for w in out.windows(2) {
                let step = (w[1] * w[0].conj()).arg() as f64;
                assert!((step - want).abs() < 0.01, "at {offset}: step {step}, want {want}");
            }
        }
    }

    #[test]
    fn how_samples_are_grouped_changes_nothing() {
        let d = Design::default();
        let iq = to_q11(&noise(HOP * 200, 0.05, 3));
        let mut bank = FixedBank::new(&d);
        let mut whole = Vec::new();
        bank.push(&iq, |f| whole.push(f[77]));
        let mut again = FixedBank::new(&d);
        let mut pieces = Vec::new();
        for chunk in iq.chunks(100) {
            again.push(chunk, |f| pieces.push(f[77]));
        }
        assert_eq!(whole, pieces);
    }

    #[test]
    fn what_would_alias_into_a_channel_is_rejected() {
        // The ROM's rounded coefficients, not the ideal filter.
        let rom: Vec<f64> = FixedBank::new(&Design::default()).coefficients().iter().map(|&c| c as f64).collect();
        let level = alias_db(&rom);
        assert!(level < -70.0, "{level:.1} dB");
    }

    #[test]
    fn the_response_matches_the_channelizer_across_a_channel() {
        let d = Design::default();
        for offset in [0.0, 40.5 * BIN_HZ] {
            for from_centre in [0.0, 2000.0, 4000.0, 6250.0, 9000.0] {
                let x = tone(offset + from_centre, 0.25, HOP * 600);
                let bank = power_db(&float_channel(&d, &x, offset, 7000.0)[SETTLE..]);
                let mut chz = Channelizer::new(FS, 24_000.0, 0.0);
                assert_eq!(chz.output_rate(), OUTPUT_RATE);
                let (h, _, _) = chz.add_head(offset, 7000.0, 0.0);
                let mut out = Vec::new();
                let mut at = 0;
                while at < x.len() {
                    let (used, ran) = chz.feed(&x[at..]);
                    at += used;
                    if ran {
                        out.extend_from_slice(chz.output(h).unwrap());
                    }
                }
                let chan = power_db(&out[SETTLE..]);
                assert!((bank - chan).abs() < 0.5, "at {offset} + {from_centre}: bank {bank:.2} dB, channelizer {chan:.2} dB");
            }
        }
    }

    #[test]
    fn full_scale_input_saturates_cleanly() {
        let d = Design::default();
        let mut bank = FixedBank::new(&d);
        let square: Vec<[i16; 2]> = (0..HOP * 40).map(|i| if i / 512 % 2 == 0 { [2047, 2047] } else { [-2048, -2048] }).collect();
        let (mut hi, mut lo) = (0i16, 0i16);
        bank.push(&square, |f| {
            for v in f {
                hi = hi.max(v[0]);
                lo = lo.min(v[0]);
            }
        });
        assert!(hi > 0 && lo < 0, "no wraparound to one sign: {hi} {lo}");
    }

    #[test]
    fn a_design_is_read_from_its_spec() {
        let d = Design::parse("taps=3072,beta=6.5,data=22").unwrap();
        assert_eq!((d.taps, d.beta, d.data_bits, d.coef_bits), (3072, 6.5, 22, 18));
        assert_eq!(Design::parse("default").unwrap(), Design::default());
        assert!(Design::parse("taps=100").is_err());
        assert!(Design::parse("volume=11").is_err());
    }

    /// Worst alias rejection, passband ripple, and the arithmetic's own noise on
    /// a floor like the live radios' with a strong neighbour. Prints a table:
    /// `cargo test --release -p trunk-core dsp::filter_bank::tests::sweep -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn sweep() {
        let seconds = HOP * 4000;
        // The radios' floor: −95 dB in a 488 Hz bin is −53 dB over the 8 MHz (42 dB
        // more), about −80 dB in a channel. A strong carrier −15 dB well away, as
        // the radios' −20 dB peaks, and a wanted carrier −60 dB.
        let mut iq = noise(seconds, 10f32.powf(-53.0 / 20.0), 11);
        let wanted = 40.5 * BIN_HZ;
        for (hz, db) in [(wanted + 150.0, -60.0f64), (1_000_000.0, -15.0)] {
            for (s, t) in iq.iter_mut().zip(tone(hz, 10f64.powf(db / 20.0), seconds)) {
                *s += t;
            }
        }
        println!("{:>5} {:>5} {:>4} {:>4} {:>3} {:>6} | {:>9} {:>9} {:>9} {:>9} {:>11} {:>6} {:>10}", "taps", "beta", "coef", "data", "tw", "shifts", "alias dB", "rounded", "ripple dB", "err dBFS", "vs floor dB", "mults", "sat f/f/o");
        let today = blackman_sinc(4097, 7000.0 / FS);
        let today_alias = (0..3600).map(|i| 20_000.0 + i as f64 * 50.0).filter(|f| ((f - BIN_HZ / 2.0 + OUTPUT_RATE / 2.0) % OUTPUT_RATE - OUTPUT_RATE / 2.0).abs() <= 7250.0).map(|f| gain_db(&today, f, FS)).fold(f64::MIN, f64::max);
        println!("today's channelizer, same measure: {today_alias:.1} dB");
        let cases: Vec<Design> = [(2048, 4.0), (3072, 5.0), (3072, 6.0), (4096, 5.0), (4096, 6.0), (4096, 7.0), (4096, 8.0), (5120, 7.0), (5120, 9.0), (6144, 9.0)]
            .iter()
            .map(|&(taps, beta)| Design { taps, beta, ..Design::default() })
            .chain([16, 18, 20, 22, 24, 27].iter().map(|&data_bits| Design { data_bits, ..Design::default() }))
            .chain([12, 14, 16].iter().map(|&coef_bits| Design { coef_bits, ..Design::default() }))
            .chain([12, 14, 16].iter().map(|&twiddle_bits| Design { twiddle_bits, ..Design::default() }))
            .chain([0b1010101010u16, 0b0101010101, 0b1111111111, 0b1111100000, 0b0000011111, 0b0111111111, 0b1110111011].iter().flat_map(|&fft_shifts| [18, 20].map(|data_bits| Design { data_bits, fft_shifts, ..Design::default() })))
            .collect();
        for d in cases {
            let offset = wanted;
            let alias = alias_db(&prototype(d.taps, d.beta));
            // The same with the coefficients the ROM holds.
            let rounded = alias_db(&FixedBank::new(&d).coefficients().iter().map(|&c| c as f64).collect::<Vec<_>>());
            let flat: Vec<f64> = [0.0, 3000.0, 6250.0].iter().map(|&f| power_db(&float_channel(&d, &tone(offset + f, 1.0, HOP * 300), offset + f, 7000.0)[SETTLE..])).collect();
            let ripple = flat.iter().cloned().fold(f64::MIN, f64::max) - flat.iter().cloned().fold(f64::MAX, f64::min);
            // Against the same design in floating point, fed the same rounded input.
            let q11: Vec<Complex32> = to_q11(&iq).iter().map(|v| Complex32::new(v[0] as f32 / 2048.0, v[1] as f32 / 2048.0)).collect();
            let want = float_channel(&d, &q11, offset, 7000.0);
            let mut bank = FixedBank::new(&d);
            let mut head = BankHead::new(offset, 7000.0);
            let mut got = Vec::new();
            bank.push(&to_q11(&iq), |f| got.push(head.push(q15(f[head.bin]))));
            let err: Vec<Complex32> = want.iter().zip(&got).map(|(a, b)| a - b).collect();
            let err_db = power_db(&err[SETTLE..]);
            let floor = power_db(&float_channel(&d, &q11, -2_000_000.0, 7000.0)[SETTLE..]);
            let mut s = bank.saturated;
            // Loud: eight carriers summing near full scale. Must not saturate inside the chain.
            let mut loud = FixedBank::new(&d);
            let mut sum = vec![Complex32::default(); HOP * 200];
            for i in 0..8 {
                for (v, t) in sum.iter_mut().zip(tone(-3_500_000.0 + i as f64 * 1_000_000.0 + 40.5 * BIN_HZ, 0.12, HOP * 200)) {
                    *v += t;
                }
            }
            loud.push(&to_q11(&sum), |_| {});
            s.fold += loud.saturated.fold;
            s.fft += loud.saturated.fft;
            s.output += loud.saturated.output;
            println!("{:>5} {:>5.1} {:>4} {:>4} {:>3} {:>6} | {:>9.1} {:>9.1} {:>9.2} {:>9.1} {:>11.1} {:>6} {:>4}/{}/{}", d.taps, d.beta, d.coef_bits, d.data_bits, d.twiddle_bits, d.fft_shifts, alias, rounded, ripple, err_db, floor - err_db, multipliers(&d), s.fold, s.fft, s.output);
        }
    }

    /// A worst-case capture for `tool snr`, as SC16 Q11 (`FB_SYNTH_OUT=file.cs16`,
    /// `--ignored`): P25 voice midway between two bins at +316,406.25 Hz, louder P25
    /// on both neighbouring 12.5 kHz channels, and a floor like the live radios'.
    #[test]
    #[ignore]
    fn synth_capture() {
        use crate::p25::synth::{self, Tx};
        let Ok(path) = std::env::var("FB_SYNTH_OUT") else { return };
        let env = |k: &str, d: f64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let (level, neighbour) = (env("FB_SYNTH_DB", -60.0), env("FB_SYNTH_NEIGHBOUR_DB", -30.0));
        // FB_SYNTH_CALLS: that many short calls, starting at unrelated times, as
        // calls do; each is a fresh acquisition. 0: one long transmission.
        let calls = env("FB_SYNTH_CALLS", 0.0) as usize;
        let wanted = 40.5 * BIN_HZ;
        let tx = |seed: usize, frames: usize| Tx { nac: 0x293, talkgroup: 100 + seed as u16, source: 1234, cipher: None, voice: (0..frames).map(|n| synth::imbe(n + seed)).collect() };
        let on_air = |t: Tx, hz: f64, db: f64| synth::modulate(&synth::dibits(&t.frames()), FS, hz, 10f64.powf(db / 20.0) as f32, &mut 0.0);
        let mut iq = if calls == 0 {
            on_air(tx(0, 360), wanted, level)
        } else {
            let mut x = Vec::new();
            let mut r = 0x2545_f491_4f6c_dd1du64;
            for c in 0..calls {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                // 0.3 to 0.6 s of nothing, so symbol timing and phase differ every time.
                x.resize(x.len() + (0.3 * FS) as usize + (r % (0.3 * FS) as u64) as usize, Complex32::default());
                x.extend(on_air(tx(c * 100, 36), wanted, level));
            }
            x.resize(x.len() + (0.3 * FS) as usize, Complex32::default());
            x
        };
        // Neighbours on the air throughout.
        let frames = (iq.len() as f64 / FS * 50.0) as usize + 40;
        for (seed, hz) in [(1000, wanted - 12_500.0), (2000, wanted + 12_500.0)] {
            if neighbour > -150.0 {
                for (a, b) in iq.iter_mut().zip(on_air(tx(seed, frames), hz, neighbour)) {
                    *a += b;
                }
            }
        }
        let floor = noise(iq.len(), 10f32.powf(-53.0 / 20.0), 5);
        for (a, b) in iq.iter_mut().zip(floor) {
            *a += b;
        }
        let bytes: Vec<u8> = to_q11(&iq).iter().flat_map(|v| [v[0].to_le_bytes(), v[1].to_le_bytes()].concat()).collect();
        std::fs::write(path, bytes).unwrap();
    }

    /// 18×18 multipliers, roughly: filter at 80 MHz, four complex multiplies
    /// (a 27×27 takes two 18×18's room when the data word passes 18 bits).
    fn multipliers(d: &Design) -> usize {
        let mac = 2.0 * d.taps as f64 / HOP as f64 * FS;
        let filter = (mac / 80e6).ceil() as usize;
        let per_complex = if d.data_bits > 18 || d.twiddle_bits > 18 { 8 } else { 4 };
        filter + 4 * per_complex
    }
}
