//! How far in from each edge of a source's band its anti-alias filter rolls
//! the noise floor off — so a guard band can be suggested for it.
//!
//! A second or so of IQ → an averaged power spectrum (Hann, no overlap) and a
//! small waterfall for display → the noise floor (a low percentile over a
//! sliding window, so signals don't count) → its level across the middle of
//! the band → from each edge inwards, where the floor comes back to within
//! [`DROP_DB`] of that level and stays there.
//!
//! The half-power point, not where the floor first dips: the roll-off takes
//! signal and noise down alike, so a gentle sag costs nothing (an RTL-SDR at
//! 2.4 MSPS is down 1 dB some 300 kHz in, 3 dB about 145 kHz in and ~5.5 dB
//! at the edge, where the alias folds back in). Past it, out-of-band signals
//! folded over are the trouble.

use std::sync::Arc;

use num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// Display width of the spectrum, floor and waterfall, bins.
pub const DISPLAY_BINS: usize = 1024;
/// Waterfall rows over the capture.
pub const ROWS: usize = 64;
/// The floor is "back" once it's within this of its mid-band level, dB.
pub const DROP_DB: f32 = 3.0;
/// Samples dropped first (the tuner settling, the AGC finding its level), s.
const SETTLE_S: f64 = 0.2;
/// Width of the window the floor is a percentile over, Hz.
const FLOOR_WINDOW_HZ: f64 = 25_000.0;
/// Which percentile of that window is the floor.
const FLOOR_PERCENTILE: f64 = 0.2;
/// The floor must stay back this far inwards before the edge is said to end, Hz.
const HOLD_HZ: f64 = 10_000.0;
/// Added to the roll-off: half a channel, so a channel at the guard's edge is
/// whole inside the flat part, Hz.
const CHANNEL_HALF_HZ: f64 = 7_500.0;
/// Guard suggestions are rounded up to this, Hz.
const ROUND_HZ: f64 = 5_000.0;

/// What [`Profiler::result`] found. Spectra run from −fs/2 to +fs/2, dB
/// (relative: dBFS-ish, uncalibrated).
#[derive(Clone, Debug)]
pub struct Rolloff {
    pub rate_hz: f64,
    /// Averaged power spectrum, [`DISPLAY_BINS`] wide.
    pub spectrum_db: Vec<f32>,
    /// The noise floor under it.
    pub floor_db: Vec<f32>,
    /// Waterfall, oldest row first, each [`DISPLAY_BINS`] wide.
    pub rows: Vec<Vec<f32>>,
    /// The floor's level across the middle of the band.
    pub reference_db: f32,
    /// How far in from the low / high edge the floor is down by more than [`DROP_DB`], Hz.
    pub low_hz: f64,
    pub high_hz: f64,
    /// How far the floor is down at the very edge, dB.
    pub low_drop_db: f32,
    pub high_drop_db: f32,
    /// The guard band to use at each edge, Hz.
    pub suggested_guard_hz: f64,
}

pub struct Profiler {
    rate_hz: f64,
    n: usize,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    skip: usize,
    /// Power summed per bin (FFT order) over every frame.
    sum: Vec<f64>,
    frames: usize,
    want_frames: usize,
    frames_per_row: usize,
    row: Vec<f64>,
    row_frames: usize,
    rows: Vec<Vec<f32>>,
}

impl Profiler {
    /// Look at `seconds` of a source at `rate_hz` (after a short settle).
    pub fn new(rate_hz: f64, seconds: f64) -> Self {
        // Bins of ~1 kHz or finer, whatever the rate.
        let n = ((rate_hz / 1000.0).max(1.0) as usize).next_power_of_two().clamp(2048, 32768);
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
        let window = (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect();
        let want_frames = ((seconds * rate_hz / n as f64).ceil() as usize).max(ROWS);
        Profiler {
            rate_hz,
            n,
            scratch: vec![Complex32::default(); fft.get_inplace_scratch_len()],
            fft,
            window,
            buf: Vec::with_capacity(n),
            skip: (SETTLE_S * rate_hz) as usize,
            sum: vec![0.0; n],
            frames: 0,
            want_frames,
            frames_per_row: (want_frames / ROWS).max(1),
            row: vec![0.0; n],
            row_frames: 0,
            rows: Vec::new(),
        }
    }

    /// Whether enough has been seen.
    pub fn done(&self) -> bool {
        self.frames >= self.want_frames
    }

    /// How far along, 0..1.
    pub fn progress(&self) -> f64 {
        (self.frames as f64 / self.want_frames as f64).min(1.0)
    }

    /// Unsigned 8-bit IQ (rtl_sdr).
    pub fn push_u8(&mut self, bytes: &[u8]) {
        for p in bytes.chunks_exact(2) {
            self.push_one(Complex32::new((p[0] as f32 - 127.5) / 127.5, (p[1] as f32 - 127.5) / 127.5));
        }
    }

    pub fn push_iq(&mut self, iq: &[Complex32]) {
        for &s in iq {
            self.push_one(s);
        }
    }

    fn push_one(&mut self, s: Complex32) {
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        if self.done() {
            return;
        }
        self.buf.push(s);
        if self.buf.len() == self.n {
            self.frame();
        }
    }

    fn frame(&mut self) {
        for (x, w) in self.buf.iter_mut().zip(&self.window) {
            *x *= *w;
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        for (k, x) in self.buf.iter().enumerate() {
            let p = x.norm_sqr() as f64;
            self.sum[k] += p;
            self.row[k] += p;
        }
        self.buf.clear();
        self.frames += 1;
        self.row_frames += 1;
        if self.row_frames == self.frames_per_row && self.rows.len() < ROWS {
            let row: Vec<f64> = shifted(&self.row).iter().map(|p| p / self.row_frames as f64).collect();
            self.rows.push(to_db(&display(&row)));
            self.row.iter_mut().for_each(|p| *p = 0.0);
            self.row_frames = 0;
        }
    }

    /// The analysis (None until a frame has been seen).
    pub fn result(&self) -> Option<Rolloff> {
        if self.frames == 0 {
            return None;
        }
        let n = self.n;
        let psd: Vec<f64> = shifted(&self.sum).iter().map(|p| p / self.frames as f64).collect();
        let psd_db = to_db(&psd);
        let bin_hz = self.rate_hz / n as f64;
        let floor = floor_curve(&psd_db, ((FLOOR_WINDOW_HZ / bin_hz) as usize).max(5));
        // Mid-band level: the middle half, less a little around DC (its spike, the IQ imbalance).
        let dc = ((0.02 * n as f64) as usize).max(2);
        let mut mid: Vec<f32> = (n / 4..3 * n / 4).filter(|&k| k.abs_diff(n / 2) > dc).map(|k| floor[k]).collect();
        mid.sort_by(f32::total_cmp);
        let reference = mid[mid.len() / 2];
        let hold = ((HOLD_HZ / bin_hz).ceil() as usize).max(1);
        let back = |k: usize| floor[k] >= reference - DROP_DB;
        // From an edge inwards (`order`): the first bin where the floor is back and stays back for `hold`.
        let edge = |order: &mut dyn Iterator<Item = usize>| -> usize {
            let idx: Vec<usize> = order.collect();
            for (i, w) in idx.windows(hold).enumerate() {
                if w.iter().all(|&k| back(k)) {
                    return i;
                }
            }
            idx.len()
        };
        let low_bins = edge(&mut (0..n / 2));
        let high_bins = edge(&mut (n / 2..n).rev());
        let low_hz = low_bins as f64 * bin_hz;
        let high_hz = high_bins as f64 * bin_hz;
        // The very edge: its outermost ½ %.
        let tip = ((n as f64 * 0.005) as usize).max(1);
        let avg = |r: std::ops::Range<usize>| r.clone().map(|k| floor[k]).sum::<f32>() / r.len() as f32;
        let low_drop_db = (reference - avg(0..tip)).max(0.0);
        let high_drop_db = (reference - avg(n - tip..n)).max(0.0);
        let suggested_guard_hz = (((low_hz.max(high_hz) + CHANNEL_HALF_HZ) / ROUND_HZ).ceil() * ROUND_HZ).min(self.rate_hz / 4.0);
        Some(Rolloff {
            rate_hz: self.rate_hz,
            spectrum_db: display_db(&psd_db),
            floor_db: display_db(&floor),
            rows: self.rows.clone(),
            reference_db: reference,
            low_hz,
            high_hz,
            low_drop_db,
            high_drop_db,
            suggested_guard_hz,
        })
    }
}

/// FFT order → −fs/2 … +fs/2.
fn shifted(v: &[f64]) -> Vec<f64> {
    let h = v.len() / 2;
    v[h..].iter().chain(&v[..h]).copied().collect()
}

fn to_db(p: &[f64]) -> Vec<f32> {
    p.iter().map(|&x| (10.0 * (x + 1e-20).log10()) as f32).collect()
}

/// Power, averaged down to [`DISPLAY_BINS`].
fn display(p: &[f64]) -> Vec<f64> {
    let g = (p.len() / DISPLAY_BINS).max(1);
    p.chunks(g).map(|c| c.iter().sum::<f64>() / c.len() as f64).collect()
}

/// dB, averaged (as power) down to [`DISPLAY_BINS`].
fn display_db(db: &[f32]) -> Vec<f32> {
    let p: Vec<f64> = db.iter().map(|&d| 10f64.powf(d as f64 / 10.0)).collect();
    to_db(&display(&p))
}

/// Each bin's floor: the [`FLOOR_PERCENTILE`] of the `w` bins around it
/// (clipped at the band's ends, so an edge is judged by what's at the edge).
fn floor_curve(db: &[f32], w: usize) -> Vec<f32> {
    let n = db.len();
    let h = w / 2;
    let mut win = Vec::with_capacity(w + 1);
    (0..n)
        .map(|k| {
            win.clear();
            win.extend_from_slice(&db[k.saturating_sub(h)..(k + h + 1).min(n)]);
            let i = ((win.len() - 1) as f64 * FLOOR_PERCENTILE) as usize;
            *win.select_nth_unstable_by(i, f32::total_cmp).1
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// White noise through a filter that's flat to `flat` × fs/2 then falls
    /// `slope_db` per % of the band, plus a few carriers.
    fn synth(rate: f64, flat: f64, slope_db: f64, seconds: f64) -> Vec<Complex32> {
        let n = 4096;
        let fft = FftPlanner::<f32>::new().plan_fft_inverse(n);
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut out = Vec::new();
        while (out.len() as f64) < seconds * rate {
            // Random-phase spectrum shaped by the filter → one block of noise.
            let mut x: Vec<Complex32> = (0..n)
                .map(|k| {
                    let f = if k < n / 2 { k as f64 } else { k as f64 - n as f64 } / n as f64 * 2.0;
                    let over = (f.abs() - flat).max(0.0) * 100.0;
                    let g = 10f64.powf(-over * slope_db / 20.0);
                    let ph = rnd() * std::f64::consts::TAU;
                    Complex32::from_polar(g as f32, ph as f32)
                })
                .collect();
            // Carriers at +200 kHz and −700 kHz.
            for off in [200_000.0, -700_000.0] {
                let k = ((off / rate * n as f64).round() as i64).rem_euclid(n as i64) as usize;
                x[k] += Complex32::new(300.0, 0.0);
            }
            fft.process(&mut x);
            out.extend(x.iter().map(|v| v * (1.0 / n as f32)));
        }
        out
    }

    #[test]
    fn finds_the_rolloff() {
        let rate = 2_400_000.0;
        // Flat to 90 % of fs/2 (120 kHz roll-off each side), falling 3 dB per %.
        let iq = synth(rate, 0.9, 3.0, 1.0);
        let mut p = Profiler::new(rate, 0.6);
        p.push_iq(&iq);
        assert!(p.done());
        let r = p.result().unwrap();
        // Floor down by 3 dB 1 % (12 kHz) past the knee: 108 kHz in (the
        // floor's low percentile reads a few kHz further in).
        for (side, hz) in [("low", r.low_hz), ("high", r.high_hz)] {
            assert!((104_000.0..=120_000.0).contains(&hz), "{side} roll-off {hz}");
        }
        assert!(r.low_drop_db > 15.0 && r.high_drop_db > 15.0, "{} {}", r.low_drop_db, r.high_drop_db);
        assert!((115_000.0..=130_000.0).contains(&r.suggested_guard_hz), "{}", r.suggested_guard_hz);
        assert_eq!(r.spectrum_db.len(), DISPLAY_BINS);
        assert_eq!(r.rows.len(), ROWS);
    }

    #[test]
    fn flat_band_needs_little() {
        let rate = 2_400_000.0;
        let iq = synth(rate, 1.0, 0.0, 1.0);
        let mut p = Profiler::new(rate, 0.6);
        p.push_iq(&iq);
        let r = p.result().unwrap();
        assert!(r.suggested_guard_hz <= 15_000.0, "{}", r.suggested_guard_hz);
    }
}
