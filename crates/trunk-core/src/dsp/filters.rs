//! FIR prototypes shared by the receivers: the root-raised-cosine pulse and
//! the Blackman windowed-sinc low-pass. Taps are made in f64, normalised to
//! unit DC gain and only then rounded to f32.

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

