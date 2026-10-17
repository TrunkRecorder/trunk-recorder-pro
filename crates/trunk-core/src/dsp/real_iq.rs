//! Real samples to IQ: an Airspy's ADC samples a low IF at twice the IQ
//! rate; this gives the complex baseband at half that rate, as libairspy's
//! own conversion would (same orientation, same scale) — but on whichever
//! thread calls it. libairspy converts on the thread that drains its USB
//! queue; when that thread runs late by more than its eight buffers
//! (~50 ms at 10 MS/s) it drops one. Taking real samples from it leaves that
//! thread a copy to do.
//!
//! The usual way: shift the IF at a quarter of the real rate to 0 Hz
//! (multiply by e^{+jπn/2}: 1, j, −1, −j), low-pass at half the band with a
//! half-band filter, keep every other sample. In a half-band filter every
//! tap at an even distance from the centre is zero, and after the shift the
//! even samples are real and the odd ones imaginary: so I is the even
//! samples through the filter's side taps and Q the odd ones through its
//! centre tap alone.

use num_complex::Complex32;

/// Side taps each side of the centre (79 taps in all): flat to about 0.43 of
/// the IQ rate either side, images 80 dB down.
const SIDE: usize = 20;
/// Kaiser window β (about 80 dB stopband).
const BETA: f64 = 8.0;
/// Full scale: libairspy's 12-bit samples, as its 16-bit ones (shifted up 4).
const SCALE: f32 = 1.0 / 32768.0;
/// The DC estimate is updated every DC_BLOCK samples (whatever the pushes'
/// lengths), by DC_ALPHA of the difference: a time constant of ~0.3 s at 20 MS/s.
const DC_BLOCK: usize = 65536;
const DC_ALPHA: f32 = 0.01;

pub struct RealToIq {
    /// Side tap i: the half-band filter's tap at distance 2i + 1 from the centre.
    g: [f32; SIDE],
    /// Even and odd samples, signed for the shift, from pair `base` on.
    e: Vec<f32>,
    o: Vec<f32>,
    /// Pairs taken in (the shift's sign alternates per pair).
    pairs: u64,
    /// A sample left over from an odd-length push.
    carry: Option<i16>,
    dc: f32,
    /// This DC block's sum and samples so far.
    dc_sum: f64,
    dc_n: usize,
}

impl Default for RealToIq {
    fn default() -> Self {
        Self::new()
    }
}

impl RealToIq {
    pub fn new() -> Self {
        // Half-band: sinc at a quarter of the rate, Kaiser-windowed, 4·SIDE − 1 taps.
        let half = (2 * SIDE - 1) as f64;
        let i0 = |x: f64| {
            let (mut s, mut t, mut k) = (1.0f64, 1.0f64, 1.0f64);
            while t > 1e-12 * s {
                t *= (x / (2.0 * k)).powi(2);
                s += t;
                k += 1.0;
            }
            s
        };
        let mut g = [0.0f64; SIDE];
        for (i, v) in g.iter_mut().enumerate() {
            let d = (2 * i + 1) as f64;
            let sinc = (std::f64::consts::PI * d / 2.0).sin() / (std::f64::consts::PI * d / 2.0) * 0.5;
            let w = i0(BETA * (1.0 - (d / half).powi(2)).max(0.0).sqrt()) / i0(BETA);
            *v = sinc * w;
        }
        // DC gain 1: the centre (½) and both sides' taps (½ between them).
        let sum: f64 = g.iter().sum();
        let mut gf = [0.0f32; SIDE];
        for (o, v) in gf.iter_mut().zip(g) {
            *o = (v * 0.25 / sum) as f32;
        }
        // (Zeros before the first pair: the output starts on time.)
        RealToIq { g: gf, e: vec![0.0; SIDE], o: vec![0.0; SIDE], pairs: 0, carry: None, dc: 0.0, dc_sum: 0.0, dc_n: 0 }
    }

    /// Real samples (libairspy's INT16_REAL) in; IQ at half their rate onto
    /// `out`, SIDE samples behind (the filter's look-ahead).
    pub fn push(&mut self, x: &[i16], out: &mut Vec<Complex32>) {
        // The ADC's offset (it would sit at the band's edge): a slowly
        // followed mean, updated at fixed sample counts.
        let mut x = x;
        while !x.is_empty() {
            let n = (DC_BLOCK - self.dc_n).min(x.len());
            let (seg, rest) = x.split_at(n);
            self.take(seg);
            self.dc_sum += seg.iter().map(|&v| v as f64).sum::<f64>();
            self.dc_n += n;
            if self.dc_n == DC_BLOCK {
                let mean = (self.dc_sum / DC_BLOCK as f64) as f32 * SCALE;
                self.dc += DC_ALPHA * (mean - self.dc);
                (self.dc_sum, self.dc_n) = (0.0, 0);
            }
            x = rest;
        }
        self.run(out);
    }

    /// Samples into the even / odd lines, signed for the shift, less the DC.
    fn take(&mut self, x: &[i16]) {
        let dc = self.dc;
        let mut x = x;
        if let Some(c) = self.carry.take() {
            let Some(&b) = x.first() else {
                self.carry = Some(c);
                return;
            };
            let sign = if self.pairs % 2 == 0 { 1.0f32 } else { -1.0 };
            self.e.push(sign * (c as f32 * SCALE - dc));
            self.o.push(sign * (b as f32 * SCALE - dc));
            self.pairs += 1;
            x = &x[1..];
        }
        let pairs = x.chunks_exact(2);
        self.carry = pairs.remainder().first().copied();
        let mut sign = if self.pairs % 2 == 0 { 1.0f32 } else { -1.0 };
        self.e.reserve(pairs.len());
        self.o.reserve(pairs.len());
        for p in pairs {
            self.e.push(sign * (p[0] as f32 * SCALE - dc));
            self.o.push(sign * (p[1] as f32 * SCALE - dc));
            sign = -sign;
        }
        self.pairs += (x.len() / 2) as u64;
    }

    /// Every output whose taps are all in: y[k] = −(Σ gᵢ (e[k−i] + e[k+1+i]) + j·½·o[k]).
    /// (The minus: libairspy's sign, so phases match its output.)
    fn run(&mut self, out: &mut Vec<Complex32>) {
        let n = self.e.len();
        if n < 2 * SIDE + 1 {
            return;
        }
        // Outputs k = SIDE − 1 .. n − SIDE − 1 (need e[k − SIDE + 1 ..= k + SIDE]).
        let (k0, k1) = (SIDE - 1, n - SIDE);
        let count = k1 - k0;
        let mut acc = vec![0.0f32; count];
        for (i, &g) in self.g.iter().enumerate() {
            let (a, b) = (&self.e[k0 - i..k0 - i + count], &self.e[k0 + 1 + i..k0 + 1 + i + count]);
            for ((s, &x), &y) in acc.iter_mut().zip(a).zip(b) {
                *s += g * (x + y);
            }
        }
        out.reserve(count);
        out.extend(acc.iter().zip(&self.o[k0..k1]).map(|(&i, &q)| Complex32::new(-i, -0.5 * q)));
        // Keep the history the next outputs need.
        self.e.drain(..count);
        self.o.drain(..count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// A real tone at `hz` (the ADC's view) at `fs`, full scale ¼.
    fn tone(hz: f64, fs: f64, n: usize) -> Vec<i16> {
        (0..n).map(|i| (8192.0 * (2.0 * PI * hz * i as f64 / fs).cos()).round() as i16).collect()
    }

    /// The output's power at `hz` (by correlation) and in total.
    fn power_at(y: &[Complex32], hz: f64, fs: f64) -> (f64, f64) {
        let y = &y[200..];
        let mut acc = num_complex::Complex64::new(0.0, 0.0);
        for (k, v) in y.iter().enumerate() {
            let ph = -2.0 * PI * hz * k as f64 / fs;
            acc += num_complex::Complex64::new(v.re as f64, v.im as f64) * num_complex::Complex64::new(ph.cos(), ph.sin());
        }
        let tot: f64 = y.iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / y.len() as f64;
        ((acc / y.len() as f64).norm_sqr(), tot)
    }

    fn convert(x: &[i16], chunk: usize) -> Vec<Complex32> {
        let mut c = RealToIq::new();
        let mut y = Vec::new();
        for p in x.chunks(chunk) {
            c.push(p, &mut y);
        }
        y
    }

    /// The IF a quarter of the real rate up is 0 Hz; a tone f above it comes
    /// out at −f, as libairspy has it (its IQ, for the same real samples, is
    /// the conjugate of a shift down by a quarter of the rate); at the level
    /// its float IQ has (a real tone of amplitude A: A/2).
    #[test]
    fn orientation_and_level() {
        let (fs, f) = (20e6, 1.234e6);
        for (rf, want) in [(5e6 + f, -f), (5e6 - f, f)] {
            let y = convert(&tone(rf, fs, 200_000), 4096);
            let (p, tot) = power_at(&y, want, fs / 2.0);
            let (q, _) = power_at(&y, -want, fs / 2.0);
            assert!((p / tot - 1.0).abs() < 0.01, "{rf}: tone at {want} is {:.4} of the power", p / tot);
            assert!(q < p * 1e-8, "{rf}: the image at {} is {:.1} dB down", -want, 10.0 * (q / p).log10());
            let amp = p.sqrt();
            assert!((amp - 0.125).abs() < 0.002, "level {amp}: libairspy's would be 0.125");
        }
    }

    /// Flat across the band a source uses (0.1 dB), each tone's mirror image far down.
    #[test]
    fn passband_and_images() {
        let fs = 20e6;
        for (off, image_db) in [(-4.3e6, 70.0), (-2e6, 80.0), (-0.5e6, 80.0), (0.3e6, 80.0), (2.5e6, 80.0), (4.3e6, 70.0)] {
            let y = convert(&tone(5e6 - off, fs, 100_000), 8192);
            let (p, _) = power_at(&y, off, fs / 2.0);
            let (q, _) = power_at(&y, -off, fs / 2.0);
            assert!((p.sqrt() - 0.125).abs() < 0.125 * 0.012, "{off}: level {} (flat to 0.1 dB)", p.sqrt());
            assert!(10.0 * (p / q).log10() > image_db, "{off}: image only {:.1} dB down", 10.0 * (p / q).log10());
        }
    }

    /// An NXDN48 signal as an Airspy's ADC would see it (real, a quarter of
    /// the rate up, 12 bits), back to IQ: decodes as the IQ it came from.
    #[test]
    fn a_real_if_decodes_as_its_iq() {
        use crate::dsp::c4fm::{C4fm, C4fmOptions};
        use crate::dsp::Receiver;
        use crate::nxdn::frame::Framer;
        use crate::nxdn::layer3::{CallHead, CALL_CONFERENCE};
        use crate::nxdn::synth::{dibits, modulate, Tx};
        use crate::nxdn::Rate;
        let fs_real = 96_000.0;
        let head = CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source: 77, destination: 3001 };
        let d = dibits(&Tx::new(Rate::N48, 5, head, 2, 12).frames());
        // Baseband at the real rate, 9 kHz off centre (the orientation shows), then the IF:
        // x = −2·Re{u·e^{−jπn/2}} (the converter gives y = −LPF(x·e^{+jπn/2}) = u).
        let mut ph = 0.0;
        let u = modulate(&d, None, Rate::N48, fs_real, 9_000.0, 0.25, &mut ph);
        let x: Vec<i16> = u
            .iter()
            .enumerate()
            .map(|(n, v)| {
                let rot = match n % 4 {
                    0 => Complex32::new(1.0, 0.0),
                    1 => Complex32::new(0.0, -1.0),
                    2 => Complex32::new(-1.0, 0.0),
                    _ => Complex32::new(0.0, 1.0),
                };
                // 12-bit, shifted up 4, as libairspy's INT16_REAL.
                ((-2.0 * (v * rot).re * 2048.0).round().clamp(-2048.0, 2047.0) as i16) << 4
            })
            .collect();
        let y = convert(&x, 4096);
        let direct: Vec<Complex32> = u.iter().step_by(2).copied().collect();
        let decode = |iq: &[Complex32]| {
            let mut rx = C4fm::with_options(fs_real / 2.0, C4fmOptions::nxdn(2400.0));
            let (mut syms, mut frames) = (Vec::new(), Vec::new());
            let mut framer = Framer::new();
            for c in iq.chunks(1024) {
                syms.clear();
                rx.push(c, &mut syms);
                for s in &syms {
                    framer.push(s, &mut frames);
                }
            }
            (frames.iter().filter(|f| crate::nxdn::channel::sacch(f).is_some()).count(), rx.offset_hz().unwrap_or(f32::NAN))
        };
        let ((got, off), (want, off_iq)) = (decode(&y), decode(&direct));
        assert!(want > 30 && got == want, "SACCH decoded: {got} through the real IF, {want} from the IQ");
        // The carrier where it was: 9 kHz up (the framer takes an inverted
        // signal too, so only the frequency tells a mirrored spectrum).
        assert!((off - 9_000.0).abs() < 300.0 && (off_iq - 9_000.0).abs() < 300.0, "carrier at {off} Hz (from the IQ: {off_iq})");
        let mirrored: Vec<Complex32> = y.iter().map(|v| v.conj()).collect();
        assert!((decode(&mirrored).1 + 9_000.0).abs() < 300.0, "the check tells a mirrored spectrum");
    }

    /// The same output however the samples come (odd lengths included).
    #[test]
    fn chunking_doesnt_matter() {
        let x = tone(5.3e6, 20e6, 50_001);
        let a = convert(&x, 50_001);
        for chunk in [1, 3, 4096, 7777] {
            let b = convert(&x, chunk);
            let n = a.len().min(b.len());
            assert!(n + 2 >= a.len() && a[..n] == b[..n], "chunks of {chunk}");
        }
    }
}

#[cfg(test)]
mod speed {
    /// ns per IQ sample (`cargo test --release -p trunk-core real_iq::speed -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn cost() {
        let x: Vec<i16> = (0..131072).map(|i| ((i * 7919) % 4096 - 2048) as i16 * 16).collect();
        let mut c = super::RealToIq::new();
        let mut y = Vec::new();
        let t = std::time::Instant::now();
        let mut n = 0;
        for _ in 0..200 {
            y.clear();
            c.push(&x, &mut y);
            n += y.len();
        }
        println!("{:.2} ns per IQ sample ({:.1} % of a core at 10 MS/s)", t.elapsed().as_nanos() as f64 / n as f64, t.elapsed().as_nanos() as f64 / n as f64 * 1e7 / 1e9 * 100.0);
    }
}
