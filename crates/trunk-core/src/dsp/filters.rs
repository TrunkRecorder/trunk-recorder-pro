//! FIR prototypes shared by the receivers: the raised-cosine and
//! root-raised-cosine pulses and the Blackman windowed-sinc low-pass. Taps
//! are made in f64, normalised to unit DC gain and only then rounded to f32.

use std::f64::consts::PI;

/// The root-raised-cosine pulse at `t` symbols from its centre, roll-off `alpha` (peak 1 − α + 4α/π).
pub fn rrc(t: f64, alpha: f64) -> f64 {
    if t.abs() < 1e-9 {
        1.0 - alpha + 4.0 * alpha / PI
    } else if (t.abs() - 1.0 / (4.0 * alpha)).abs() < 1e-9 {
        alpha / 2f64.sqrt() * ((1.0 + 2.0 / PI) * (PI / (4.0 * alpha)).sin() + (1.0 - 2.0 / PI) * (PI / (4.0 * alpha)).cos())
    } else {
        ((PI * t * (1.0 - alpha)).sin() + 4.0 * alpha * t * (PI * t * (1.0 + alpha)).cos()) / (PI * t * (1.0 - (4.0 * alpha * t).powi(2)))
    }
}

/// The raised-cosine pulse at `t` symbols from its centre, roll-off `a` (peak 1).
pub fn rc(t: f64, a: f64) -> f64 {
    let s = if t.abs() < 1e-9 { 1.0 } else { (PI * t).sin() / (PI * t) };
    let d = 1.0 - (2.0 * a * t).powi(2);
    if d.abs() < 1e-9 {
        PI / 4.0 * s
    } else {
        s * (PI * a * t).cos() / d
    }
}

/// `n` (odd) root-raised-cosine taps, `sps` samples per symbol, unit DC gain.
pub fn rrc_taps(alpha: f64, sps: f64, n: usize) -> Vec<f32> {
    let m = (n - 1) as f64 / 2.0;
    let mut h: Vec<f64> = (0..n).map(|i| rrc((i as f64 - m) / sps, alpha)).collect();
    let dc: f64 = h.iter().sum();
    for v in h.iter_mut() {
        *v /= dc;
    }
    h.into_iter().map(|v| v as f32).collect()
}

/// `taps` convolved with a boxcar `width` samples wide (fractional ends
/// weighted), DC gain kept at 1: the time-domain form of multiplying the
/// filter's response by sin(πfT)/(πfT), T = `width` samples.
pub fn with_boxcar(taps: &[f32], width: f64) -> Vec<f32> {
    let half = (width / 2.0).ceil() as usize;
    let k: Vec<f64> = (0..=2 * half)
        .map(|i| {
            let d = (i as f64 - half as f64).abs();
            (width / 2.0 - d + 0.5).clamp(0.0, 1.0)
        })
        .collect();
    let n = taps.len() + k.len() - 1;
    let mut out = vec![0f64; n];
    for (i, &t) in taps.iter().enumerate() {
        for (j, &w) in k.iter().enumerate() {
            out[i + j] += t as f64 * w;
        }
    }
    let dc: f64 = out.iter().sum();
    out.into_iter().map(|v| (v / dc) as f32).collect()
}

/// `n` Blackman windowed-sinc low-pass taps, cutoff `fc` in cycles per sample, not normalised (DC gain ≈ 1).
pub fn blackman_sinc(n: usize, fc: f64) -> Vec<f64> {
    let m = (n - 1) as f64;
    (0..n)
        .map(|i| {
            let k = i as f64 - m / 2.0;
            let sinc = if k == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * k).sin() / (PI * k) };
            let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / m).cos() + 0.08 * (4.0 * PI * i as f64 / m).cos();
            sinc * w
        })
        .collect()
}

/// Blackman windowed-sinc low-pass, DC gain 1. `fc` in cycles per sample.
pub fn lowpass(n: usize, fc: f64) -> Vec<f32> {
    let mut h = blackman_sinc(n, fc);
    let sum: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= sum);
    h.into_iter().map(|v| v as f32).collect()
}


/// One second-order IIR section (direct form II transposed).
#[derive(Clone, Copy)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    /// From [b0, b1, b2, a1, a2] (a0 = 1).
    pub fn new(c: [f64; 5]) -> Self {
        Biquad { b: [c[0] as f32, c[1] as f32, c[2] as f32], a: [c[3] as f32, c[4] as f32], z: [0.0; 2] }
    }
    #[inline]
    pub fn step(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// A 4th-order Butterworth high-pass at `fc` Hz for rate `fs`: two
/// sections (bilinear, prewarped), 3 dB down at `fc`, 24 dB per octave below.
pub fn butter_highpass4(fc: f64, fs: f64) -> [Biquad; 2] {
    let w0 = 2.0 * PI * fc / fs;
    // The two pole pairs' Q: 1 / (2 cos(π/8)), 1 / (2 cos(3π/8)).
    [0.541_196_100_146_197, 1.306_562_964_876_376_7].map(|q| {
        let alpha = w0.sin() / (2.0 * q);
        let (c, a0) = (w0.cos(), 1.0 + alpha);
        Biquad::new([(1.0 + c) / 2.0 / a0, -(1.0 + c) / a0, (1.0 + c) / 2.0 / a0, -2.0 * c / a0, (1.0 - alpha) / a0])
    })
}
