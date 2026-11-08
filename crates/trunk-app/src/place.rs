//! Where to tune the radios: one placement for Start's auto centres
//! ([`Config::resolved_centers`](crate::config::Config::resolved_centers)),
//! the first-run guide's plan and the survey's suggestion. Each radio in
//! turn takes the window that reaches the most of what's still out of reach
//! — a site's control channels before anything else, then the busiest
//! channels — and, with everything reached, goes alongside the others.

use serde::Deserialize;

pub use trunk_core::trunk::{usable_half_width, Pins, DEFAULT_GUARD_HZ};

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

/// What the radios are placed to reach: groups of channels, each received
/// on the sources it may be.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Targets {
    pub groups: Vec<Group>,
}

/// Channels received together: a site, or a conventional system's channels
/// on the same sources.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    /// Every channel worth reaching, Hz, and how much it counts (calls seen
    /// on it; 1 when nothing is known).
    pub channels: Vec<Weighted>,
    /// What it needs reached before any other channel counts: one of these
    /// (a P25 or SmartNet site's control channels), or with `all` every one
    /// (a DMR, NXDN or EDACS site's control and watched frequencies, a
    /// conventional system's channels). While it isn't, each outweighs every
    /// channel. Empty: nothing.
    #[serde(default)]
    pub need: Vec<f64>,
    /// Every one of `need`, not one of them.
    #[serde(default)]
    pub all: bool,
    /// The sources it may be received on.
    #[serde(default)]
    pub pins: Pins,
}

impl Group {
    /// Its frequencies: its channels and what it needs.
    fn hz(&self) -> impl Iterator<Item = f64> + '_ {
        self.channels.iter().map(|c| c.hz).chain(self.need.iter().copied())
    }

    /// Of what it needs, what `reached` doesn't reach while its need isn't
    /// met (none reached; `all`: any not reached). Empty: met.
    fn wanting(&self, reached: impl Fn(f64) -> bool) -> Vec<f64> {
        let missed: Vec<f64> = self.need.iter().copied().filter(|&f| !reached(f)).collect();
        let met = if self.all { missed.is_empty() } else { self.need.is_empty() || missed.len() < self.need.len() };
        if met { Vec::new() } else { missed }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Weighted {
    pub hz: f64,
    pub weight: f64,
}

/// The source a frequency of a group with these `pins` is received on, the
/// sources tuned to `centers` (None: not placed): the first it allows that
/// reaches it ([`Pins::first`], the engine's own choice).
pub fn receiver(sources: &[Placing], centers: &[Option<f64>], pins: &Pins, hz: f64) -> Option<usize> {
    pins.first(sources.len(), |i| centers[i].is_some_and(|c| sources[i].reaches(c, hz)))
}

/// Each source's centre: a fixed one stays; each other, in order, over what
/// of the groups it may receive the ones before it don't reach yet
/// ([`busiest_window`], a group's need first), or with everything reached,
/// alongside them on the side with more of those channels. None when it may
/// receive nothing, or nothing is placed to be beside.
pub fn place_sources(sources: &[Placing], t: &Targets) -> Vec<Option<f64>> {
    let mut out: Vec<Option<f64>> = sources.iter().map(|s| s.fixed_hz).collect();
    let need = 1.0 + t.groups.iter().flat_map(|g| &g.channels).map(|c| c.weight).sum::<f64>();
    for k in 0..sources.len() {
        if out[k].is_some() {
            continue;
        }
        let mine: Vec<&Group> = t.groups.iter().filter(|g| g.pins.allows(k)).collect();
        let reached = |g: &Group, hz: f64| receiver(sources, &out, &g.pins, hz).is_some();
        // Each group's need not met yet: what of it is still wanted.
        let open: Vec<Vec<f64>> = mine.iter().map(|g| g.wanting(|f| reached(g, f))).filter(|w| !w.is_empty()).collect();
        let left: Vec<Weighted> = mine
            .iter()
            .flat_map(|g| g.channels.iter().filter(|c| !reached(g, c.hz)).copied())
            .chain(open.iter().flatten().map(|&hz| Weighted { hz, weight: need }))
            .collect();
        let all: Vec<f64> = mine.iter().flat_map(|g| g.hz()).collect();
        let s = &sources[k];
        let center = match busiest_window(&left, s, open.first().map(Vec::as_slice)) {
            Some(c) => Some(c),
            None => alongside(&out, sources, s, &all),
        };
        out[k] = center;
    }
    out
}

/// What sources tuned to some centres receive of the targets ([`coverage`]):
/// the one account of it Start's checks, the dashboard and advice read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Coverage {
    /// Each group's, in the targets' order.
    pub groups: Vec<GroupCover>,
    /// Per source: how many of the frequencies it is the first to receive
    /// (where the engine opens them); 0 for one nothing listed needs.
    pub listed: Vec<usize>,
}

/// What of one group is received.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupCover {
    /// Its need is met ([`Group::need`]).
    pub met: bool,
    /// What it needs that no source receives.
    pub need_missed: Vec<f64>,
    /// The weight of its channels received.
    pub reached: f64,
    /// Its channels no source receives.
    pub missed: Vec<f64>,
}

impl Coverage {
    /// Groups whose need isn't met.
    pub fn unmet(&self) -> usize {
        self.groups.iter().filter(|g| !g.met).count()
    }
    /// The weight of every group's channels received.
    pub fn reached(&self) -> f64 {
        self.groups.iter().map(|g| g.reached).sum()
    }
    /// Sources something listed needs.
    pub fn needed(&self) -> usize {
        self.listed.iter().filter(|&&n| n > 0).count()
    }
}

/// What `sources` tuned to `centers` (None: not placed) receive of `t`
/// ([`receiver`]: the engine's choice of source).
pub fn coverage(sources: &[Placing], centers: &[Option<f64>], t: &Targets) -> Coverage {
    let mut listed = vec![0; sources.len()];
    let groups = t
        .groups
        .iter()
        .map(|g| {
            let rx = |hz: f64| receiver(sources, centers, &g.pins, hz);
            let mut hz: Vec<f64> = g.hz().collect();
            hz.sort_by(f64::total_cmp);
            hz.dedup();
            for f in hz {
                if let Some(k) = rx(f) {
                    listed[k] += 1;
                }
            }
            let (got, missed): (Vec<&Weighted>, Vec<&Weighted>) = g.channels.iter().partition(|c| rx(c.hz).is_some());
            let need_missed: Vec<f64> = g.need.iter().copied().filter(|&f| rx(f).is_none()).collect();
            GroupCover { met: g.wanting(|f| rx(f).is_some()).is_empty(), need_missed, reached: got.iter().map(|c| c.weight).sum(), missed: missed.iter().map(|c| c.hz).collect() }
        })
        .collect();
    Coverage { groups, listed }
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

/// What advice may change about a source ([`advise`]), beside its [`Placing`].
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choices {
    /// Its set centre may be advised on (one typed in); a capture file's can't.
    #[serde(default)]
    pub movable: bool,
    /// The rates it may run at ([`crate::rates::offered`]); empty: its own only.
    #[serde(default)]
    pub rates: Vec<u32>,
    /// The highest it runs at dependably ([`crate::rates::steady_max`]): advice
    /// goes past it only when that reaches more.
    #[serde(default)]
    pub steady_max: Option<u32>,
}

/// How good a way of tuning the sources is, compared in this order: every
/// need met, the weight of channels reached, the radios needed, those run
/// past their dependable top rate, their total sample rate (less CPU and
/// USB), and how little changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Score {
    /// Groups whose need isn't met ([`Coverage::unmet`]).
    pub unmet: usize,
    /// The weight of channels received ([`Coverage::reached`]).
    pub reached: f64,
    /// Sources something listed needs ([`Coverage::needed`]).
    pub needed: usize,
    /// Of those, the ones past their dependable top rate.
    pub unsteady: usize,
    /// Their sample rates, together, Hz.
    pub rate_hz: f64,
    /// Sources whose centre or rate changes.
    pub changes: usize,
}

impl Score {
    /// Better than `o`, by the first thing they differ in.
    pub fn better(&self, o: &Score) -> bool {
        use std::cmp::Ordering::*;
        let order = [
            o.unmet.cmp(&self.unmet),
            if (self.reached - o.reached).abs() < 1e-9 { Equal } else { self.reached.total_cmp(&o.reached) },
            o.needed.cmp(&self.needed),
            o.unsteady.cmp(&self.unsteady),
            if (self.rate_hz - o.rate_hz).abs() < 1.0 { Equal } else { o.rate_hz.total_cmp(&self.rate_hz) },
            o.changes.cmp(&self.changes),
        ];
        order.into_iter().find(|&x| x != Equal) == Some(Greater)
    }
}

/// A better way to tune the sources ([`advise`]).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Advice {
    /// How the sources do now, and with the advice.
    pub before: Score,
    pub after: Score,
    /// Each source, in order.
    pub sources: Vec<Suggested>,
}

/// One source, as advised.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggested {
    /// Where it's tuned (None: nothing to place it by), and at what rate.
    pub center_hz: Option<f64>,
    pub rate_hz: f64,
    /// Its centre or rate is other than now.
    pub changed: bool,
    /// Its centre is where auto places it, the other sources as they are
    /// now: taking the advice for it alone, it can be left on auto (it is,
    /// or set back to it) rather than set to `center_hz`.
    pub auto: bool,
    /// On auto in the advice: taking all of it, it is set to auto (the
    /// advice is where auto places them all).
    pub released: bool,
    /// Something listed needs it, with the advice; false: it can go.
    pub needed: bool,
}

/// A better way to tune `sources` for `t`, if there is one: sources whose
/// set centre may move ([`Choices::movable`]) released to auto (none, each
/// alone, or all), and each source's rates tried in turn, [`place_sources`]
/// placing what's on auto every time. None when nothing does better than now
/// ([`Score::better`]).
pub fn advise(sources: &[Placing], choices: &[Choices], t: &Targets) -> Option<Advice> {
    let rated = |rates: &[f64], released: &[bool]| -> Vec<Placing> {
        sources.iter().zip(rates).zip(released).map(|((s, &r), &free)| Placing { rate_hz: r, fixed_hz: if free { None } else { s.fixed_hz }, ..*s }).collect()
    };
    let score = |p: &[Placing]| -> (Score, Vec<Option<f64>>, Coverage) {
        let centers = place_sources(p, t);
        let cov = coverage(p, &centers, t);
        let needed = |k: usize| cov.listed[k] > 0;
        let changes = (0..p.len())
            .filter(|&k| (p[k].rate_hz - sources[k].rate_hz).abs() >= 1.0 || (sources[k].fixed_hz.is_some() && p[k].fixed_hz.is_none() && centers[k].zip(sources[k].fixed_hz).is_none_or(|(a, b)| (a - b).abs() >= 1000.0)))
            .count();
        let unsteady = (0..p.len()).filter(|&k| needed(k) && choices.get(k).and_then(|c| c.steady_max).is_some_and(|m| p[k].rate_hz > m as f64 + 1.0)).count();
        let rate_hz = (0..p.len()).filter(|&k| needed(k)).map(|k| p[k].rate_hz).sum();
        (Score { unmet: cov.unmet(), reached: cov.reached(), needed: cov.needed(), unsteady, rate_hz, changes }, centers, cov)
    };
    let now: Vec<f64> = sources.iter().map(|s| s.rate_hz).collect();
    let (before, ..) = score(sources);
    let movable: Vec<usize> = (0..sources.len()).filter(|&k| sources[k].fixed_hz.is_some() && choices.get(k).is_some_and(|c| c.movable)).collect();
    // Release none, each alone, or all.
    let mut sets: Vec<Vec<bool>> = vec![vec![false; sources.len()]];
    for &k in &movable {
        let mut r = vec![false; sources.len()];
        r[k] = true;
        sets.push(r);
    }
    if movable.len() > 1 {
        sets.push((0..sources.len()).map(|k| movable.contains(&k)).collect());
    }
    let mut best: Option<(Score, Vec<f64>, Vec<bool>)> = None;
    for released in sets {
        // Each source's rates in turn, kept when better, until a pass changes nothing.
        let mut rates = now.clone();
        let mut here = score(&rated(&rates, &released)).0;
        for _ in 0..3 {
            let mut moved = false;
            for k in 0..sources.len() {
                for &r in choices.get(k).map_or(&[][..], |c| &c.rates) {
                    let r = r as f64;
                    if (r - rates[k]).abs() < 1.0 {
                        continue;
                    }
                    let mut tried = rates.clone();
                    tried[k] = r;
                    let s = score(&rated(&tried, &released)).0;
                    if s.better(&here) {
                        (here, rates, moved) = (s, tried, true);
                    }
                }
            }
            if !moved {
                break;
            }
        }
        if best.as_ref().is_none_or(|(b, ..)| here.better(b)) {
            best = Some((here, rates, released));
        }
    }
    let (after, rates, released) = best?;
    if !after.better(&before) {
        return None;
    }
    let advised = rated(&rates, &released);
    let (_, centers, cov) = score(&advised);
    let suggested = (0..sources.len())
        .map(|k| {
            let changed = (rates[k] - now[k]).abs() >= 1.0 || centers[k].zip(sources[k].fixed_hz).is_some_and(|(a, b)| (a - b).abs() >= 1000.0);
            // On auto alone (the others as they are now, its rate as advised): placed here too?
            let auto = sources[k].fixed_hz.is_none() || {
                let mut alone = sources.to_vec();
                alone[k] = Placing { rate_hz: rates[k], fixed_hz: None, ..sources[k] };
                place_sources(&alone, t)[k].zip(centers[k]).is_some_and(|(a, b)| (a - b).abs() < 1000.0)
            };
            Suggested { center_hz: centers[k], rate_hz: rates[k], changed, auto, released: advised[k].fixed_hz.is_none(), needed: cov.listed[k] > 0 }
        })
        .collect();
    Some(Advice { before, after, sources: suggested })
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
    /// One group: these channels, needing `need`, on any source.
    fn one(channels: Vec<Weighted>, need: &[f64]) -> Targets {
        Targets { groups: vec![Group { channels, need: need.to_vec(), pins: Pins::any(), ..Default::default() }] }
    }
    fn off_dc_of(c: f64, hz: &[f64]) -> bool {
        hz.iter().all(|f| (f - c).abs() >= DC_CLEAR_HZ)
    }

    #[test]
    fn a_second_radio_takes_the_busiest_channels() {
        // Busy voice channels 3–4 MHz up, a quiet one further: the second radio takes the busy ones.
        let voice = chans(&[(854_000_000.0, 50.0), (854_500_000.0, 40.0), (856_900_000.0, 1.0), (851_200_000.0, 9.0)]);
        let plan = place_sources(&[fixed(851_400_000.0), RTL], &one(voice.clone(), &[851_000_000.0]));
        let c = plan[1].unwrap();
        assert!(RTL.reaches(c, 854_000_000.0) && RTL.reaches(c, 854_500_000.0), "{c}");
        assert!(off_dc_of(c, &[854e6, 854.5e6, 856.9e6, 851.2e6, 851e6]), "off the DC spike: {c}");
        // A fixed source stays; what it reaches isn't placed for again.
        let plan = place_sources(&[fixed(851_400_000.0), fixed(853_000_000.0), RTL], &one(voice[..2].to_vec(), &[851_000_000.0]));
        assert_eq!(plan[1], Some(853_000_000.0));
        assert!(RTL.reaches(plan[2].unwrap(), 854_500_000.0), "{plan:?}");
    }

    /// An 800 MHz site whose voice channels span four 2.4 MSPS radios: each
    /// takes a stretch the ones before don't reach, the first the control
    /// channel too.
    #[test]
    fn every_radio_is_placed_over_a_wide_site() {
        let voice: Vec<Weighted> = (0..=32).map(|k| Weighted { hz: 851_012_500.0 + k as f64 * 250_000.0, weight: 1.0 }).collect();
        let plan = place_sources(&[RTL; 4], &one(voice.clone(), &[853_987_500.0]));
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
        let t = one(chans(&[(851_200_000.0, 1.0), (851_600_000.0, 1.0)]), &[851_000_000.0]);
        let plan = place_sources(&[RTL; 3], &t);
        let c: Vec<f64> = plan.iter().map(|c| c.unwrap()).collect();
        assert!(RTL.reaches(c[0], 851_000_000.0) && RTL.reaches(c[0], 851_600_000.0), "{c:?}");
        let h = RTL.half();
        assert!((c[1] - c[0]).abs() > 1.5 * h && (c[2] - c[1]).abs() > 1.5 * h && (c[2] - c[0]).abs() > 1.5 * h, "{c:?}");
        // The spares reach nothing the first doesn't.
        assert_eq!(coverage(&[RTL; 3], &plan, &t).listed, [3, 0, 0]);
        // Nothing to place by: nothing placed.
        assert_eq!(place_sources(&[RTL], &Targets::default()), [None]);
    }

    /// A control channel is reached before busier voice channels, and each
    /// group's need gets a radio before any voice channel does.
    #[test]
    fn needs_come_first() {
        let a = Group { channels: chans(&[(851_000_000.0, 1.0), (860_000_000.0, 100.0), (860_200_000.0, 100.0)]), need: vec![851_000_000.0], ..Default::default() };
        let b = Group { need: vec![771_106_250.0], ..Default::default() };
        let plan = place_sources(&[RTL; 3], &Targets { groups: vec![a, b] });
        assert!(RTL.reaches(plan[0].unwrap(), 851_000_000.0), "{plan:?}");
        assert!(RTL.reaches(plan[1].unwrap(), 771_106_250.0), "{plan:?}");
        assert!(RTL.reaches(plan[2].unwrap(), 860_000_000.0) && RTL.reaches(plan[2].unwrap(), 860_200_000.0), "{plan:?}");
    }

    /// A pinned group is placed for only by the sources it may be received
    /// on, and they are the ones that receive it.
    #[test]
    fn a_pinned_group_is_placed_on_its_own_sources() {
        // A site on any source, and one pinned to source 1 (a radio on another antenna).
        let any = Group { channels: chans(&[(851_012_500.0, 1.0)]), need: vec![851_012_500.0], ..Default::default() };
        let pinned = Group { channels: chans(&[(770_106_250.0, 1.0)]), need: vec![770_106_250.0], pins: Pins::only(vec![1]), ..Default::default() };
        let t = Targets { groups: vec![pinned, any] };
        let plan = place_sources(&[RTL; 2], &t);
        assert!(RTL.reaches(plan[0].unwrap(), 851_012_500.0), "source 0 may not take the pinned site: {plan:?}");
        assert!(RTL.reaches(plan[1].unwrap(), 770_106_250.0), "{plan:?}");
        assert_eq!(coverage(&[RTL; 2], &plan, &t).listed, [1, 1]);
        // Both tuned over the pinned site: source 1 receives it (and is needed), source 0 doesn't.
        let both = [Some(770_100_000.0), Some(770_100_000.0)];
        assert_eq!(receiver(&[RTL; 2], &both, &Pins::only(vec![1]), 770_106_250.0), Some(1));
        assert_eq!(coverage(&[RTL; 2], &both, &Targets { groups: vec![t.groups[0].clone()] }).listed, [0, 1]);
    }

    /// A group needing all of its frequencies (a conventional system's
    /// channels, a DMR site's) gets them before busier channels elsewhere, and
    /// its need is met only with every one reached.
    #[test]
    fn a_group_needing_all_its_frequencies_gets_them_first() {
        // Two conventional channels 4 MHz apart, and a site's busy voice channels by its control channel.
        let conv = Group { channels: chans(&[(154_100_000.0, 1.0), (158_100_000.0, 1.0)]), need: vec![154_100_000.0, 158_100_000.0], all: true, ..Default::default() };
        let site = Group { channels: chans(&[(851_000_000.0, 1.0), (852_000_000.0, 500.0)]), need: vec![851_000_000.0], ..Default::default() };
        let t = Targets { groups: vec![conv, site] };
        let plan = place_sources(&[RTL; 3], &t);
        let cov = coverage(&[RTL; 3], &plan, &t);
        assert!(cov.groups.iter().all(|g| g.met), "{plan:?} {cov:?}");
        assert_eq!(cov.unmet(), 0);
        // Two radios: the second takes the conventional channel still wanted, not the busy voice channel.
        let two = place_sources(&[RTL; 2], &t);
        let cov = coverage(&[RTL; 2], &two, &t);
        assert!(cov.groups[0].met, "both conventional channels: {two:?}");
        assert!(!cov.groups[1].met && cov.groups[1].need_missed == [851_000_000.0], "{cov:?}");
        assert_eq!((cov.unmet(), cov.needed()), (1, 2));
        // One of them only: not met, and what's missed is said.
        let one_src = coverage(&[RTL], &[Some(154_200_000.0)], &t);
        assert_eq!((one_src.groups[0].met, one_src.groups[0].need_missed.as_slice()), (false, &[158_100_000.0][..]));
        assert_eq!(one_src.groups[1].reached, 0.0);
    }

    /// An RTL-SDR's choices: a typed-in centre may move, and its kind's rates.
    fn rtl_choices() -> Choices {
        Choices { movable: true, rates: crate::rates::of_kind("rtlsdr").to_vec(), steady_max: crate::rates::steady_max("rtlsdr") }
    }
    fn at(rate_hz: f64, center: Option<f64>) -> Placing {
        Placing { rate_hz, fixed_hz: center, ..RTL }
    }

    /// A centre typed in that misses voice channels: advice moves it over
    /// them, and since auto would put it there, it can go back to auto.
    #[test]
    fn advice_moves_a_set_centre_that_misses_channels() {
        let voice = chans(&[(851_200_000.0, 1.0), (851_700_000.0, 1.0), (852_100_000.0, 1.0)]);
        let t = one(voice, &[851_012_500.0]);
        // Tuned too low: the control channel at the top edge, two voice channels out.
        let a = advise(&[at(2_400_000.0, Some(850_000_000.0))], &[rtl_choices()], &t).expect("advice");
        assert!(a.after.reached > a.before.reached, "{a:?}");
        let s = &a.sources[0];
        assert!(s.changed && s.auto && s.needed, "{s:?}");
        let c = s.center_hz.unwrap();
        assert!([851_012_500.0, 851_200_000.0, 851_700_000.0, 852_100_000.0].iter().all(|&f| at(s.rate_hz, None).reaches(c, f)), "{s:?}");
    }

    /// Covering as much with as many radios at a lower rate is better (less
    /// CPU and USB): the lowest that still reaches it all.
    #[test]
    fn advice_lowers_a_rate_that_covers_the_same() {
        // 1.5 MHz of channels: 1.92 MSPS is the lowest rate that reaches them (1.024's band is 0.87 MHz).
        let t = one(chans(&[(851_000_000.0, 1.0), (852_500_000.0, 1.0)]), &[851_400_000.0]);
        let a = advise(&[at(3_200_000.0, None)], &[rtl_choices()], &t).expect("advice");
        let s = &a.sources[0];
        assert_eq!(s.rate_hz, 1_920_000.0, "{s:?}");
        assert_eq!((a.after.unmet, a.after.reached, a.after.needed), (a.before.unmet, a.before.reached, a.before.needed));
        assert!(a.after.rate_hz < a.before.rate_hz);
        // Already the lowest that covers it: nothing to say.
        assert_eq!(advise(&[at(s.rate_hz, s.center_hz)], &[Choices { movable: false, ..rtl_choices() }], &t), None);
    }

    /// A wider rate that lets one radio do two radios' work is advised, past
    /// the dependable top rate if that's what it takes; past it for nothing
    /// isn't.
    #[test]
    fn advice_frees_a_radio_and_minds_the_dependable_rate() {
        // 2.7 MHz of channels: two RTL-SDRs at 2.4, or one at 3.2.
        let voice: Vec<Weighted> = (0..=9).map(|k| Weighted { hz: 851_000_000.0 + k as f64 * 300_000.0, weight: 1.0 }).collect();
        let t = one(voice, &[851_000_000.0]);
        let both = [at(2_400_000.0, None), at(2_400_000.0, None)];
        let a = advise(&both, &[rtl_choices(), rtl_choices()], &t).expect("advice");
        assert_eq!((a.before.needed, a.after.needed, a.after.unmet), (2, 1, 0), "{a:?}");
        let used: Vec<&Suggested> = a.sources.iter().filter(|s| s.needed).collect();
        assert_eq!(used.len(), 1);
        assert!(used[0].rate_hz > 2_560_000.0, "past the dependable rate, to free a radio: {a:?}");
        assert_eq!(a.after.unsteady, 1);
        // 2.0 MHz of channels at 3.2 MSPS: 2.4 covers it, so not past the dependable rate.
        let narrow = one(chans(&[(851_000_000.0, 1.0), (852_000_000.0, 1.0)]), &[851_500_000.0]);
        let b = advise(&[at(3_200_000.0, None)], &[rtl_choices()], &narrow).expect("advice");
        assert!(b.sources[0].rate_hz <= 2_560_000.0 && b.after.unsteady == 0, "{b:?}");
    }

    /// A capture file's centre and rate are the recording's: never advised.
    #[test]
    fn advice_leaves_a_capture_file_alone() {
        let t = one(chans(&[(851_200_000.0, 1.0), (853_900_000.0, 1.0)]), &[851_012_500.0]);
        let file = Choices { movable: false, rates: vec![], steady_max: None };
        assert_eq!(advise(&[at(2_400_000.0, Some(850_000_000.0))], &[file], &t), None);
    }
}
