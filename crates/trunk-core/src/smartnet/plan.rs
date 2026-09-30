//! Finding a SmartNet system's band plan from the air.
//!
//! OSWs name channels by number, and on VHF / UHF (OBT) systems nothing on
//! the air says what frequency a number is. So watch it happen: while the
//! control channel keeps granting channel `n`, one narrowband carrier in the
//! spectrum is up that isn't when `n` is idle. For every channel number the
//! grants mention, compare each spectrum cell's mean power while the channel
//! is granted with its mean while it isn't; the cell that rises is where the
//! channel is. A handful of such (number, frequency) points — and the control
//! channel's own number, which it broadcasts, at the frequency it is heard —
//! fix the line `f = base + spacing · (n − offset)`.
//!
//! ```text
//! grants / updates (channel numbers, times) ─┐
//! spectrum cells every few ms ───────────────┴→ per channel: granted vs idle mean per cell
//!   → strongest rise ≥ 6 dB → point (n, Hz) → spacing (snapped to a standard
//!     step) + intercept → a known plan (800 / 900) if it fits, else OBT
//! ```
//!
//! Frequencies are in the radio's frame (its ppm error included); the
//! caller snaps the result to the channel raster.

use std::collections::BTreeMap;

use super::parser::Bandplan;

/// A channel counts as granted this long after its last grant / update, s.
const ACTIVE_S: f64 = 1.2;
/// Samples needed on each side before a channel is judged.
const MIN_SAMPLES: u32 = 40;
/// A channel's carrier must rise this much while granted, dB.
const MIN_RISE_DB: f64 = 6.0;
/// Standard channel steps, Hz.
const SPACINGS: [f64; 9] = [2500.0, 5000.0, 6250.0, 7500.0, 10_000.0, 12_500.0, 15_000.0, 25_000.0, 50_000.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub chan: u16,
    /// Where its carrier was seen (radio frame), Hz.
    pub hz: f64,
    /// Rise while granted, dB.
    pub rise_db: f64,
}

#[derive(Default)]
struct Acc {
    on: Vec<f64>,
    n_on: u32,
    last_s: f64,
}

/// Accumulates grants and spectra; see the module docs.
pub struct PlanFinder {
    center_hz: f64,
    fs: f64,
    cells: usize,
    /// Cells within this of the centre are judged (the edges roll off).
    max_offset_hz: f64,
    total: Vec<f64>,
    n_total: u32,
    chans: BTreeMap<u16, Acc>,
}

impl PlanFinder {
    pub fn new(center_hz: f64, fs: f64, cells: usize, max_offset_hz: f64) -> Self {
        PlanFinder { center_hz, fs, cells, max_offset_hz, total: vec![0.0; cells], n_total: 0, chans: BTreeMap::new() }
    }

    fn cell_hz(&self, k: f64) -> f64 {
        self.center_hz - self.fs / 2.0 + (k + 0.5) * self.fs / self.cells as f64
    }

    /// The control channel granted or updated channel `chan` at `t` (s).
    pub fn heard(&mut self, chan: u16, t: f64) {
        let cells = self.cells;
        let a = self.chans.entry(chan).or_insert_with(|| Acc { on: vec![0.0; cells], n_on: 0, last_s: f64::NEG_INFINITY });
        a.last_s = a.last_s.max(t);
    }

    /// Channel numbers heard so far.
    pub fn channels(&self) -> Vec<u16> {
        self.chans.keys().copied().collect()
    }

    /// One spectrum ([`crate::dsp::Channelizer::cell_powers`]) at `t`.
    pub fn spectrum(&mut self, row: &[f32], t: f64) {
        if row.len() != self.cells {
            return;
        }
        for (s, &v) in self.total.iter_mut().zip(row) {
            *s += v as f64;
        }
        self.n_total += 1;
        for a in self.chans.values_mut() {
            if t - a.last_s <= ACTIVE_S && t >= a.last_s - 0.05 {
                for (s, &v) in a.on.iter_mut().zip(row) {
                    *s += v as f64;
                }
                a.n_on += 1;
            }
        }
    }

    /// Where each channel judged so far was seen.
    pub fn points(&self) -> Vec<Point> {
        let mut out = Vec::new();
        let w = self.fs / self.cells as f64;
        for (&chan, a) in &self.chans {
            let n_off = self.n_total.saturating_sub(a.n_on);
            if a.n_on < MIN_SAMPLES || n_off < MIN_SAMPLES {
                continue;
            }
            let rise = |k: usize| {
                let off = (self.total[k] - a.on[k]) / n_off as f64;
                let on = a.on[k] / a.n_on as f64;
                (on, off)
            };
            let mut best: Option<(usize, f64)> = None;
            for k in 0..self.cells {
                if (self.cell_hz(k as f64) - self.center_hz).abs() > self.max_offset_hz {
                    continue;
                }
                let (on, off) = rise(k);
                let db = 10.0 * (on.max(1e-30) / off.max(1e-30)).log10();
                if best.is_none_or(|(_, b)| db > b) {
                    best = Some((k, db));
                }
            }
            let Some((k, db)) = best else { continue };
            if db < MIN_RISE_DB {
                continue;
            }
            // Centre: the excess power's centroid over the carrier (±6 kHz).
            let span = (6000.0 / w).ceil() as isize;
            let (mut sw, mut swk) = (0.0, 0.0);
            for d in -span..=span {
                let j = k as isize + d;
                if j < 0 || j >= self.cells as isize {
                    continue;
                }
                let (on, off) = rise(j as usize);
                let e = (on - off).max(0.0);
                sw += e;
                swk += e * j as f64;
            }
            let kk = if sw > 0.0 { swk / sw } else { k as f64 };
            out.push(Point { chan, hz: self.cell_hz(kk), rise_db: db });
        }
        out
    }
}

/// `f = hz_at_zero + spacing · n` through the points (radio frame).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub spacing_hz: f64,
    pub hz_at_zero: f64,
    /// Points on the line (the anchor included).
    pub inliers: usize,
    /// Worst inlier residual, Hz.
    pub max_err_hz: f64,
}

impl Fit {
    pub fn hz(&self, chan: u16) -> f64 {
        self.hz_at_zero + self.spacing_hz * chan as f64
    }
}

/// Fit the channel line: through `anchor` (the control channel: its number
/// and where it is heard) when known, else through the points alone. Needs
/// two distinct channel numbers that agree.
pub fn fit(points: &[Point], anchor: Option<(u16, f64)>) -> Option<Fit> {
    let mut pts: Vec<(u16, f64)> = points.iter().map(|p| (p.chan, p.hz)).collect();
    if let Some(a) = anchor {
        pts.retain(|p| p.0 != a.0);
    }
    let bases: Vec<(u16, f64)> = match anchor {
        Some(a) => vec![a],
        None => pts.clone(),
    };
    let mut best: Option<Fit> = None;
    for &s in &SPACINGS {
        let tol = (s / 4.0).min(3000.0);
        for &(c0, f0) in &bases {
            let res: Vec<f64> = pts.iter().filter(|p| p.0 != c0).map(|&(c, f)| f - (f0 + s * (c as f64 - c0 as f64))).collect();
            let inl: Vec<f64> = res.iter().copied().filter(|e| e.abs() <= tol).collect();
            if inl.is_empty() {
                continue;
            }
            // Through the base exactly when it is the control channel; else the mean.
            let shift = if anchor.is_some() { 0.0 } else { inl.iter().sum::<f64>() / (inl.len() + 1) as f64 };
            let f = Fit {
                spacing_hz: s,
                hz_at_zero: f0 + shift - s * c0 as f64,
                inliers: inl.len() + 1,
                max_err_hz: inl.iter().map(|e| (e - shift).abs()).fold(shift.abs(), f64::max),
            };
            // Most points on the line; then the wider step (a narrower one
            // fits whatever a wider one does); then the tighter fit.
            let better = match &best {
                None => true,
                Some(b) => (f.inliers, f.spacing_hz as i64, -(f.max_err_hz as i64)) > (b.inliers, b.spacing_hz as i64, -(b.max_err_hz as i64)),
            };
            if better {
                best = Some(f);
            }
        }
    }
    best.filter(|f| f.inliers >= 2)
}

/// A band plan as Trunk Recorder's config names it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanConfig {
    /// "800_standard" | "800_reband" | "800_splinter" | "900" | "400_custom".
    pub name: &'static str,
    /// 400_custom only (Hz, channel number, Hz, Hz).
    pub base_hz: f64,
    pub offset: u16,
    pub spacing_hz: f64,
    pub high_hz: f64,
    pub plan: Bandplan,
    /// Heard − true for the anchor channel (the radio's error), Hz.
    pub error_hz: f64,
}

/// Turn a fit into a plan: one of the fixed 800 / 900 plans when every
/// channel seen lands on it (allowing a common radio error), else an OBT plan
/// snapped to the channel raster. `cc` is the control channel's number (its
/// frequency is then exact on the raster); `chans` every number heard.
pub fn plan_for(fit: &Fit, cc: Option<u16>, chans: &[u16]) -> PlanConfig {
    let anchor = cc.or_else(|| chans.first().copied()).unwrap_or(0);
    let heard = fit.hz(anchor);
    let fixed: [(&'static str, Bandplan); 4] = [
        ("800_reband", Bandplan::B800 { rebanded: true, splinter: false }),
        ("800_standard", Bandplan::B800 { rebanded: false, splinter: false }),
        ("800_splinter", Bandplan::B800 { rebanded: false, splinter: true }),
        ("900", Bandplan::B900),
    ];
    for (name, plan) in fixed {
        let Some(f_cc) = plan.rx_hz(anchor) else { continue };
        let err = heard - f_cc as f64;
        // A radio error of up to ~25 ppm, and every channel on the same plan.
        if err.abs() > 20_000.0 {
            continue;
        }
        let fits = chans.iter().all(|&c| plan.rx_hz(c).is_some_and(|f| (fit.hz(c) - err - f as f64).abs() < 1500.0 + fit.spacing_hz / 4.0));
        if fits && (plan.rx_hz(anchor.saturating_add(1)).map_or(0.0, |f| f as f64 - f_cc as f64) - fit.spacing_hz).abs() < 1.0 {
            return PlanConfig { name, base_hz: 0.0, offset: 0, spacing_hz: fit.spacing_hz, high_hz: 0.0, plan, error_hz: err };
        }
    }
    // OBT: channels are on a 6.25 kHz raster (2.5 kHz when the step isn't a multiple of it).
    let raster = if fit.spacing_hz % 6250.0 == 0.0 { 6250.0 } else { 2500.0 };
    let true_cc = (heard / raster).round() * raster;
    let err = heard - true_cc;
    // Motorola OBT numbers outbound channels from 380 (inbound below); else from the lowest heard.
    let lowest = chans.iter().copied().chain(cc).min().unwrap_or(380);
    let offset = if lowest >= 380 { 380 } else { lowest };
    let at = |n: u16| true_cc + fit.spacing_hz * (n as f64 - anchor as f64);
    let (base_hz, high_hz) = (at(offset), at(0x2f7));
    let plan = Bandplan::Obt { base_hz, spacing_hz: fit.spacing_hz, offset, high_hz };
    PlanConfig { name: "400_custom", base_hz, offset, spacing_hz: fit.spacing_hz, high_hz, plan, error_hz: err }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wmata_hz(n: u16) -> f64 {
        489_087_500.0 + 25_000.0 * (n as f64 - 380.0)
    }

    #[test]
    fn fit_and_plan_obt_with_radio_error() {
        let err = 180.0;
        let pts: Vec<Point> = [663u16, 676, 675, 684].iter().map(|&c| Point { chan: c, hz: wmata_hz(c) + err + 90.0 * ((c % 3) as f64 - 1.0), rise_db: 12.0 }).collect();
        let f = fit(&pts, Some((674, wmata_hz(674) + err))).unwrap();
        assert_eq!(f.spacing_hz, 25_000.0);
        assert_eq!(f.inliers, 5);
        let p = plan_for(&f, Some(674), &[663, 674, 675, 676, 684, 448]);
        assert_eq!(p.name, "400_custom");
        assert_eq!((p.base_hz, p.offset, p.spacing_hz), (489_087_500.0, 380, 25_000.0));
        assert!((p.error_hz - err).abs() < 1.0);
        assert_eq!(p.plan.rx_hz(451), Some(490_862_500));
    }

    #[test]
    fn recognises_800_rebanded() {
        let plan = Bandplan::B800 { rebanded: true, splinter: false };
        let err = -2200.0;
        let hz = |c: u16| plan.rx_hz(c).unwrap() as f64 + err;
        let pts: Vec<Point> = [0x10u16, 0x40, 0x55].iter().map(|&c| Point { chan: c, hz: hz(c), rise_db: 10.0 }).collect();
        let f = fit(&pts, Some((0x20, hz(0x20)))).unwrap();
        let p = plan_for(&f, Some(0x20), &[0x10, 0x20, 0x40, 0x55]);
        assert_eq!(p.name, "800_reband");
        assert!((p.error_hz - err).abs() < 1.0);
    }

    #[test]
    fn finder_locates_a_granted_carrier() {
        // 64 cells over 64 kHz; channel 7 is a carrier at +10.5 kHz, up while granted.
        let (fs, cells) = (64_000.0, 64);
        let mut pf = PlanFinder::new(100e6, fs, cells, 30_000.0);
        for i in 0..2000 {
            let t = i as f64 * 0.005;
            let on = (t % 4.0) < 2.0;
            if on && i % 40 == 0 {
                pf.heard(7, t);
            }
            if i % 40 == 0 {
                pf.heard(9, t); // always granted, nothing visible: can't be judged
            }
            let mut row = vec![1.0f32; cells];
            if on {
                row[42] = 30.0; // cell 42 centre: −32 kHz + 42.5 kHz = +10.5 kHz
            }
            pf.spectrum(&row, t);
        }
        let pts = pf.points();
        assert_eq!(pts.len(), 1, "{pts:?}");
        assert_eq!(pts[0].chan, 7);
        assert!((pts[0].hz - 100_010_500.0).abs() < 600.0, "{}", pts[0].hz);
    }
}
