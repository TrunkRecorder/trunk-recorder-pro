//! FSK symbol levels, as every FSK receiver estimates them (C4FM / 4FSK for
//! P25, DMR and NXDN; 2FSK for SmartNet, EDACS and ProVoice): from a window
//! of recent symbol values (discriminator Hz at the sampling instants),
//! seeded by the window's 2 % / 98 % quantiles and refined by a few rounds of
//! assigning each value to its level and taking each level's mean.
//!
//! Every fit starts again from the quantiles, which don't depend on any
//! decision, so a transient (a channel's first moments, a burst of
//! interference) leaves with the window. The C4FM receiver slices on the
//! cluster means (the better estimate in noise: the quantiles widen with it);
//! the 2FSK receiver keeps its own running levels, which follow a signal
//! faster, and watches them against the quantiles' fit
//! ([`crate::smartnet::Fsk2`]).

/// Levels fitted to a window of symbol values (Hz).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// Midway between the outermost levels.
    pub center: f32,
    /// The outermost levels' distance from the center.
    pub outer: f32,
    /// 4FSK: the inner/outer decision threshold's distance from the center.
    pub thr: f32,
    /// Level step / spread of the values about their levels (~10 and up:
    /// clean decisions; ~1: noise), when the clusters could be fitted.
    pub separation: Option<f32>,
    /// The levels were far enough apart to believe ([`FitOptions::min_outer`]).
    pub believed: bool,
}

/// How a receiver has its levels fitted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitOptions {
    /// Levels closer than this to the center (Hz) aren't believed: the
    /// center is the quantiles', the outer level `default_outer`, and there's
    /// no separation.
    pub min_outer: f32,
    pub default_outer: f32,
    /// Refine from the clusters (else the quantiles only, as the first C4FM
    /// receiver did).
    pub means: bool,
}

/// Fit `N` (2 or 4) equally weighted levels to `vals` (reordered).
pub fn fit<const N: usize>(vals: &mut [f32], o: FitOptions) -> Fit {
    const { assert!(N == 2 || N == 4) };
    let n = vals.len();
    let (lo, hi) = (n * 2 / 100, (n * 98 / 100).min(n - 1));
    let q_lo = *vals.select_nth_unstable_by(lo, f32::total_cmp).1;
    let q_hi = *vals.select_nth_unstable_by(hi, f32::total_cmp).1;
    let mut center = (q_hi + q_lo) / 2.0;
    let mut outer = (q_hi - q_lo) / 2.0;
    let believed = outer > o.min_outer;
    if !believed {
        outer = o.default_outer;
    }
    let mut thr = outer * 2.0 / 3.0;
    let mut separation = None;
    if o.means && believed {
        let level = |x: f32, thr: f32| -> usize {
            if N == 2 {
                (x >= 0.0) as usize
            } else if x >= thr {
                3
            } else if x >= 0.0 {
                2
            } else if x >= -thr {
                1
            } else {
                0
            }
        };
        for _ in 0..3 {
            let (mut s, mut c) = ([0.0f64; N], [0u32; N]);
            for &v in vals.iter() {
                let k = level(v - center, thr);
                s[k] += v as f64;
                c[k] += 1;
            }
            if c.iter().any(|&c| c < 8) {
                break;
            }
            let m: [f32; N] = std::array::from_fn(|k| (s[k] / c[k] as f64) as f32);
            // Spread of the values about their levels, against the level step.
            let mut var = 0f64;
            for &v in vals.iter() {
                let k = (0..N).min_by(|&a, &b| (v - m[a]).abs().total_cmp(&(v - m[b]).abs())).unwrap();
                var += ((v - m[k]) as f64).powi(2);
            }
            let sd = (var / vals.len() as f64).sqrt() as f32;
            separation = Some((m[N - 1] - m[0]) / (N - 1) as f32 / sd.max(1e-3));
            center = (m[0] + m[N - 1]) / 2.0;
            outer = (m[N - 1] - m[0]) / 2.0;
            if N == 4 {
                // Thresholds halfway between the inner and outer levels.
                thr = ((m[2] + m[3]) / 2.0 - center + center - (m[0] + m[1]) / 2.0) / 2.0;
            }
        }
    }
    Fit { center, outer, thr, separation, believed }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: FitOptions = FitOptions { min_outer: 600.0, default_outer: 2400.0, means: true };

    /// `per` values at each of `levels`, spread ±`spread`, in a fixed jumble.
    fn values(levels: &[f32], per: usize, spread: f32) -> Vec<f32> {
        let mut s = 0x2545_f491u32;
        (0..per * levels.len())
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                levels[i % levels.len()] + spread * ((s % 2001) as f32 / 1000.0 - 1.0)
            })
            .collect()
    }

    #[test]
    fn four_and_two_levels_from_their_clusters() {
        let mut v = values(&[-1800.0, -600.0, 600.0, 1800.0], 200, 150.0);
        let f = fit::<4>(&mut v, FitOptions { min_outer: 300.0, default_outer: 1800.0, means: true });
        assert!(f.center.abs() < 20.0 && (f.outer - 1800.0).abs() < 20.0 && (f.thr - 1200.0).abs() < 20.0, "{f:?}");
        assert!(f.separation.unwrap() > 5.0);
        let mut v = values(&[-2300.0, 2700.0], 300, 400.0);
        let f = fit::<2>(&mut v, TWO);
        assert!((f.center - 200.0).abs() < 30.0 && (f.outer - 2500.0).abs() < 30.0, "{f:?}");
    }

    /// Most of one level's values pulled toward the other (a 2FSK's lone
    /// symbols at three samples a symbol), and more of one bit than the
    /// other: the levels are still the clusters'.
    #[test]
    fn two_levels_with_values_between() {
        let mut v = values(&[-2600.0, -2600.0, -2600.0, 2600.0, 2600.0, 900.0, -900.0], 100, 200.0);
        let f = fit::<2>(&mut v, TWO);
        assert!(f.center.abs() < 400.0 && f.outer > 1800.0, "{f:?}");
    }

    #[test]
    fn noise_alone_isnt_believed() {
        let mut v = values(&[0.0], 500, 200.0);
        let f = fit::<2>(&mut v, TWO);
        assert_eq!((f.outer, f.separation, f.believed), (2400.0, None, false));
    }
}
