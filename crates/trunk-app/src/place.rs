//! Where to tune the radios: one placement for Start's auto centres
//! ([`Config::resolved_centers`](crate::config::Config::resolved_centers)),
//! the first-run guide's plan and the survey's suggestion. Each radio in
//! turn takes the window that reaches the most of what's still out of reach
//! — a site's control channels before anything else, then the busiest
//! channels — and, with everything reached, goes alongside the others.

use serde::Deserialize;

pub use trunk_core::trunk::{usable_half_width, DEFAULT_GUARD_HZ};

/// How far a channel is kept from a source's centre (its DC spike).
pub const DC_CLEAR_HZ: f64 = 25_000.0;
/// A window is this much narrower than a source's usable band each side, so
/// the centre can step off the DC spike and keep what it reaches.
const WINDOW_SLACK_HZ: f64 = 30_000.0;

/// A source to place: its rate and guard, and its centre if it can't be
/// moved (set by hand, a capture file, the radio the guide found it on).
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placing {
    pub rate_hz: f64,
    #[serde(default = "guard_default")]
    pub guard_hz: f64,
    #[serde(default)]
    pub fixed_hz: Option<f64>,
}

fn guard_default() -> f64 {
    DEFAULT_GUARD_HZ
}

impl Placing {
    /// How far from its centre a channel may be, Hz.
    pub fn half(&self) -> f64 {
        usable_half_width(self.rate_hz, self.guard_hz)
    }
    /// Whether tuned to `center` it reaches `hz`.
    pub fn reaches(&self, center: f64, hz: f64) -> bool {
        (hz - center).abs() <= self.half()
    }
}

/// What the radios are placed to reach.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Targets {
    /// Every channel worth reaching, Hz, and how much it counts (calls seen
    /// on it; 1 when nothing is known).
    pub channels: Vec<Weighted>,
    /// Groups a radio must reach one of before any other channel counts: a
    /// site's control channels (and the DMR / NXDN frequencies it watches),
    /// a conventional system's channels. Every channel of one that's out of
    /// reach outweighs all of `channels`.
    pub needs: Vec<Vec<f64>>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Weighted {
    pub hz: f64,
    pub weight: f64,
}

/// Each source's centre: a fixed one stays; each other, in order, over what
/// the ones before it don't reach yet ([`busiest_window`], a group of
/// `needs` not reached first), or with everything reached, alongside them
/// on the side with more of the channels. None only when there's nothing to
/// place it by.
pub fn place_sources(sources: &[Placing], t: &Targets) -> Vec<Option<f64>> {
    let mut out: Vec<Option<f64>> = sources.iter().map(|s| s.fixed_hz).collect();
    let all: Vec<f64> = t.channels.iter().map(|c| c.hz).chain(t.needs.iter().flatten().copied()).collect();
    for k in 0..sources.len() {
        if out[k].is_some() {
            continue;
        }
        let reached = |hz: f64| out.iter().zip(sources).any(|(c, s)| c.is_some_and(|c| s.reaches(c, hz)));
        let open: Vec<&Vec<f64>> = t.needs.iter().filter(|g| !g.iter().any(|&f| reached(f))).collect();
        let need = 1.0 + t.channels.iter().map(|c| c.weight).sum::<f64>();
        let left: Vec<Weighted> = t
            .channels
            .iter()
            .filter(|c| !reached(c.hz))
            .copied()
            .chain(open.iter().flat_map(|g| g.iter()).map(|&hz| Weighted { hz, weight: need }))
            .collect();
        let s = &sources[k];
        let center = match busiest_window(&left, s, open.first().map(|g| g.as_slice())) {
            Some(c) => Some(c),
            None => alongside(&out, sources, s, &all),
        };
        out[k] = center;
    }
    out
}

/// The centre for `s` whose usable band takes in the most weight of `chans`
/// (and one of `must`, when given), centred on what it takes in and off the
/// DC spike. Of windows taking in as much, the one that leaves the rest
/// fewest windows to cover. None when `chans` is empty.
pub fn busiest_window(chans: &[Weighted], s: &Placing, must: Option<&[f64]>) -> Option<f64> {
    let w = 2.0 * s.half() - 2.0 * WINDOW_SLACK_HZ;
    let inside = |lo: f64| chans.iter().filter(move |c| c.hz >= lo && c.hz <= lo + w);
    let holds = |lo: f64| must.is_none_or(|m| m.iter().any(|&f| f >= lo && f <= lo + w));
    let mut hz: Vec<f64> = chans.iter().map(|c| c.hz).collect();
    hz.sort_by(f64::total_cmp);
    // Windows of width w the channels outside [lo, lo + w] take, laid from the lowest up (the fewest there are).
    let rest = |lo: f64| {
        let (mut n, mut edge) = (0, f64::NEG_INFINITY);
        for &f in hz.iter().filter(|&&f| f < lo || f > lo + w) {
            if f > edge {
                (n, edge) = (n + 1, f + w);
            }
        }
        n
    };
    // Each channel as a window's low edge or its high one (lowest first, so a tie goes low).
    let mut edges: Vec<f64> = hz.iter().flat_map(|&f| [f, f - w]).collect();
    edges.sort_by(f64::total_cmp);
    let mut best: Option<(f64, f64, usize)> = None;
    for &lo in edges.iter().filter(|&&lo| holds(lo)) {
        let score: f64 = inside(lo).map(|c| c.weight).sum();
        let better = match best {
            None => true,
            Some((_, b, _)) if (score - b).abs() > 1e-9 => score > b,
            Some((_, _, r)) => rest(lo) < r,
        };
        if better {
            best = Some((lo, score, rest(lo)));
        }
    }
    let (lo, ..) = best?;
    let hz: Vec<f64> = inside(lo).map(|c| c.hz).collect();
    let all: Vec<f64> = chans.iter().map(|c| c.hz).collect();
    auto_center(&hz, s.rate_hz, s.guard_hz).or_else(|| off_dc(lo + w / 2.0, &all, |c| hz.iter().all(|&f| s.reaches(c, f))))
}

/// `s` beside the sources placed in `out`, on the side of them with more of
/// `all`; None when nothing is placed or there's nothing to be beside.
fn alongside(out: &[Option<f64>], sources: &[Placing], s: &Placing, all: &[f64]) -> Option<f64> {
    let spans: Vec<(f64, f64)> = out.iter().zip(sources).filter_map(|(c, p)| c.map(|c| (c - p.half(), c + p.half()))).collect();
    if spans.is_empty() || all.is_empty() {
        return None;
    }
    let lo = spans.iter().map(|x| x.0).fold(f64::INFINITY, f64::min);
    let hi = spans.iter().map(|x| x.1).fold(f64::NEG_INFINITY, f64::max);
    let mid = all.iter().sum::<f64>() / all.len() as f64;
    let c = if mid >= (lo + hi) / 2.0 { hi + s.half() } else { lo - s.half() };
    let c = (c / 1000.0).round() * 1000.0;
    Some(off_dc(c, all, |_| true).unwrap_or(c))
}

/// A centre that puts every one of `hz` inside one source (and off the DC
/// spike), or None if they span too much.
pub fn auto_center(hz: &[f64], rate_hz: f64, guard_hz: f64) -> Option<f64> {
    if hz.is_empty() {
        return None;
    }
    let lo = hz.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = hz.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let half = usable_half_width(rate_hz, guard_hz);
    if hi - lo > 2.0 * half - 50_000.0 {
        return None;
    }
    let mid = ((lo + hi) / 2.0 / 1000.0).round() * 1000.0;
    off_dc(mid, hz, |c| hz.iter().all(|f| (f - c).abs() <= half - 10_000.0))
}

/// `c` off the DC spike: moved to either side in widening 12.5 kHz steps
/// until no channel of `hz` is within [`DC_CLEAR_HZ`] of it and `keep(c)`
/// (what it must still reach). None if 40 steps don't find one. Every
/// centre the recorder or Setup picks goes through this.
fn off_dc(mut c: f64, hz: &[f64], keep: impl Fn(f64) -> bool) -> Option<f64> {
    for step in 0..=40 {
        if hz.iter().all(|f| (f - c).abs() >= DC_CLEAR_HZ) && keep(c) {
            return Some(c);
        }
        c += if step % 2 == 1 { -1.0 } else { 1.0 } * 12_500.0 * (step + 1) as f64;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTL: Placing = Placing { rate_hz: 2_400_000.0, guard_hz: DEFAULT_GUARD_HZ, fixed_hz: None };
    fn fixed(hz: f64) -> Placing {
        Placing { fixed_hz: Some(hz), ..RTL }
    }
    fn chans(x: &[(f64, f64)]) -> Vec<Weighted> {
        x.iter().map(|&(hz, weight)| Weighted { hz, weight }).collect()
    }
    fn off_dc_of(c: f64, hz: &[f64]) -> bool {
        hz.iter().all(|f| (f - c).abs() >= DC_CLEAR_HZ)
    }

    #[test]
    fn a_second_radio_takes_the_busiest_channels() {
        // Busy voice channels 3–4 MHz up, a quiet one further: the second radio takes the busy ones.
        let voice = chans(&[(854_000_000.0, 50.0), (854_500_000.0, 40.0), (856_900_000.0, 1.0), (851_200_000.0, 9.0)]);
        let t = Targets { channels: voice.clone(), needs: vec![vec![851_000_000.0]] };
        let plan = place_sources(&[fixed(851_400_000.0), RTL], &t);
        let c = plan[1].unwrap();
        assert!(RTL.reaches(c, 854_000_000.0) && RTL.reaches(c, 854_500_000.0), "{c}");
        assert!(off_dc_of(c, &[854e6, 854.5e6, 856.9e6, 851.2e6, 851e6]), "off the DC spike: {c}");
        // A fixed source stays; what it reaches isn't placed for again.
        let t = Targets { channels: voice[..2].to_vec(), ..t };
        let plan = place_sources(&[fixed(851_400_000.0), fixed(853_000_000.0), RTL], &t);
        assert_eq!(plan[1], Some(853_000_000.0));
        assert!(RTL.reaches(plan[2].unwrap(), 854_500_000.0), "{plan:?}");
    }

    /// An 800 MHz site whose voice channels span four 2.4 MSPS radios: each
    /// takes a stretch the ones before don't reach, the first the control
    /// channel too.
    #[test]
    fn every_radio_is_placed_over_a_wide_site() {
        let voice: Vec<Weighted> = (0..=32).map(|k| Weighted { hz: 851_012_500.0 + k as f64 * 250_000.0, weight: 1.0 }).collect();
        let t = Targets { channels: voice.clone(), needs: vec![vec![853_987_500.0]] };
        let plan = place_sources(&[RTL; 4], &t);
        let centers: Vec<f64> = plan.iter().map(|c| c.expect("placed")).collect();
        assert!(RTL.reaches(centers[0], 853_987_500.0), "the control channel first: {centers:?}");
        for v in &voice {
            assert!(centers.iter().any(|&c| RTL.reaches(c, v.hz)), "{} not reached by {centers:?}", v.hz);
        }
        let all: Vec<f64> = voice.iter().map(|v| v.hz).collect();
        assert!(centers.iter().all(|&c| off_dc_of(c, &all)), "{centers:?}");
    }

    /// With everything reached, spare radios go alongside — each beside the
    /// last, not all onto one place.
    #[test]
    fn spare_radios_go_side_by_side() {
        let t = Targets { channels: chans(&[(851_200_000.0, 1.0), (851_600_000.0, 1.0)]), needs: vec![vec![851_000_000.0]] };
        let plan = place_sources(&[RTL; 3], &t);
        let c: Vec<f64> = plan.iter().map(|c| c.unwrap()).collect();
        assert!(RTL.reaches(c[0], 851_000_000.0) && RTL.reaches(c[0], 851_600_000.0), "{c:?}");
        let h = RTL.half();
        assert!((c[1] - c[0]).abs() > 1.5 * h && (c[2] - c[1]).abs() > 1.5 * h && (c[2] - c[0]).abs() > 1.5 * h, "{c:?}");
        // Nothing to place by: nothing placed.
        assert_eq!(place_sources(&[RTL], &Targets::default()), [None]);
    }

    /// A control channel is reached before busier voice channels elsewhere,
    /// and each group of needs gets a radio before any voice channel does.
    #[test]
    fn needs_come_first() {
        let t = Targets { channels: chans(&[(851_000_000.0, 1.0), (860_000_000.0, 100.0), (860_200_000.0, 100.0)]), needs: vec![vec![851_000_000.0], vec![771_106_250.0]] };
        let plan = place_sources(&[RTL; 3], &t);
        assert!(RTL.reaches(plan[0].unwrap(), 851_000_000.0), "{plan:?}");
        assert!(RTL.reaches(plan[1].unwrap(), 771_106_250.0), "{plan:?}");
        assert!(RTL.reaches(plan[2].unwrap(), 860_000_000.0) && RTL.reaches(plan[2].unwrap(), 860_200_000.0), "{plan:?}");
    }
}
