//! Tuning a radio's gain by what its systems decode, while recording: the
//! measurements, the scores and the search. The platform runs it (it sets
//! each candidate on the radio as a trial, not saved, and hands each second's
//! `stats` values in); what it tries, and in what order, is the radio's own
//! [`GainPlan`](crate::gain::GainPlan).
//!
//! A setting is scored by what the systems whose control channels the radio
//! receives decoded while it was on the air:
//!
//! - the control channel first (P25's coverage, else the share of messages
//!   decoded, counted only while it was locked): without it nothing is recorded;
//! - then the voice frames lost, when there was enough voice to tell;
//! - a setting that clips (more than [`CLIP_PCT`] of samples at full scale)
//!   is refused, whatever it decoded.
//!
//! The weakest system counts twice, so a gain that helps the strong systems a
//! little at the cost of a weak one isn't chosen.
//!
//! A control channel's decoding drifts by more from minute to minute (traffic,
//! simulcast fading) than a step of gain usually moves it, so a setting is
//! never judged by one measurement: each candidate is compared with the best
//! so far by turns, a slice each, for a few rounds ([`Cmp`]), and moved to only
//! when it is better by more than the margin and by more than twice the
//! standard error of the rounds' differences.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trunk_core::metrics::{key_part, system_key};
use trunk_core::trunk::SiteStatus;

use crate::gain::{GainPlan, Point, Setting};
pub use crate::stats::CLIP_PCT;

/// A control channel that was decoding, silent this long under a setting (s): the setting is given up.
pub const ABANDON_S: f64 = 10.0;
/// Voice frames a system needs in a slice (P25: 50 a second) before its voice counts.
const MIN_VOICE_FRAMES: f64 = 250.0;
/// How much of a system's score is its control channel, when its voice counts (the rest is voice).
const CC_WEIGHT: f64 = 0.7;
/// Control channel symbols a second (P25): what its coverage is out of.
const P25_SYM_S: f64 = 4800.0;

// ───────────────────────────── what's asked ─────────────────────────────

/// `tuneGain`: tune source `source`'s gain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TuneRequest {
    pub source: usize,
    /// Score only these systems (short names; none: all on the radio). The
    /// radio's others are still measured, and guarded ([`Self::protect`]).
    pub systems: Vec<String>,
    /// With `systems`: refuse a candidate that costs one of the radio's other
    /// systems more than this many points (0–100); none: allow it.
    pub protect: Option<f64>,
    /// Seconds each side of a round is measured, after `settle_s`.
    pub slice_s: f64,
    pub settle_s: f64,
    pub rounds: usize,
    /// At most this many candidates compared.
    pub max_steps: usize,
    /// A move is kept only when it scores this much better (0–100).
    pub margin: f64,
    /// An Airspy's or HydraSDR's: search this gain mode instead of the one it's in.
    pub mode: Option<crate::config::AirspyGain>,
    /// Compare this one setting with the radio's, and nothing else.
    #[serde(rename = "try")]
    pub try_setting: Option<Setting>,
    /// Keep (and save) the best; else the radio goes back as it was.
    pub apply: bool,
    /// Only say what it would do (the plan, the systems, how long), to the
    /// asker: nothing is changed.
    pub check: bool,
}

impl Default for TuneRequest {
    fn default() -> Self {
        TuneRequest { source: 0, systems: vec![], protect: Some(5.0), slice_s: 15.0, settle_s: 4.0, rounds: 3, max_steps: 12, margin: 1.0, mode: None, try_setting: None, apply: false, check: false }
    }
}

impl TuneRequest {
    /// Its numbers within reason.
    pub fn clamped(mut self) -> TuneRequest {
        self.slice_s = self.slice_s.max(5.0);
        self.settle_s = self.settle_s.max(0.0);
        self.rounds = self.rounds.clamp(2, 20);
        self.max_steps = self.max_steps.max(1);
        self.margin = self.margin.max(0.0);
        self.protect = self.protect.filter(|p| *p > 0.0);
        self
    }

    /// One candidate's comparison at most, s.
    pub fn per_candidate_s(&self) -> f64 {
        self.rounds as f64 * 2.0 * (self.settle_s + self.slice_s)
    }
}

// ───────────────────────────── what a slice measured ─────────────────────────────

/// One system's control channel (its sites on this radio, together) and voice over a slice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SysTally {
    pub seconds: f64,
    pub locked_s: f64,
    pub good: f64,
    pub bad: f64,
    /// P25: control channel symbols decoded; `None` when the system doesn't report them.
    pub air_sym: Option<f64>,
    pub snr: Mean,
    pub phase_err: Mean,
    pub voice_frames: f64,
    pub voice_bad: f64,
}

/// The radio over a slice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SrcTally {
    pub seconds: f64,
    pub clip: Mean,
    pub dropped: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mean {
    sum: f64,
    n: u32,
}

impl Mean {
    pub fn add(&mut self, v: f64) {
        if v.is_finite() {
            self.sum += v;
            self.n += 1;
        }
    }
    pub fn get(&self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / f64::from(self.n))
    }
    fn merge(&mut self, o: &Mean) {
        self.sum += o.sum;
        self.n += o.n;
    }
}

impl SysTally {
    fn merge(&mut self, o: &SysTally) {
        self.seconds += o.seconds;
        self.locked_s += o.locked_s;
        self.good += o.good;
        self.bad += o.bad;
        if let Some(s) = o.air_sym {
            *self.air_sym.get_or_insert(0.0) += s;
        }
        self.snr.merge(&o.snr);
        self.phase_err.merge(&o.phase_err);
        self.voice_frames += o.voice_frames;
        self.voice_bad += o.voice_bad;
    }

    /// The control channel's share decoded, 0–1: P25's coverage (symbols
    /// decoded of those it carries), else messages decoded of those framed,
    /// for the share of the slice it was locked.
    pub fn control(&self) -> f64 {
        if self.seconds <= 0.0 {
            return 0.0;
        }
        match self.air_sym {
            Some(sym) => (sym / (P25_SYM_S * self.seconds)).min(1.0),
            None if self.good + self.bad > 0.0 => self.good / (self.good + self.bad) * (self.locked_s / self.seconds),
            None => 0.0,
        }
    }

    /// Voice frames lost, 0–1, when there were enough to tell.
    pub fn voice_lost(&self) -> Option<f64> {
        (self.voice_frames >= MIN_VOICE_FRAMES).then(|| (self.voice_bad / self.voice_frames).clamp(0.0, 1.0))
    }

    /// 0–100.
    pub fn score(&self) -> f64 {
        let cc = self.control();
        match self.voice_lost() {
            Some(lost) => 100.0 * (CC_WEIGHT * cc + (1.0 - CC_WEIGHT) * (1.0 - lost)),
            None => 100.0 * cc,
        }
    }
}

/// What one setting measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Step {
    pub systems: BTreeMap<String, SysTally>,
    pub src: SrcTally,
    /// Given up early, and why.
    pub abandoned: Option<String>,
}

impl Step {
    /// Adds `o`'s tallies to these (a setting's slices, together). Its being given up isn't carried.
    pub fn merge(&mut self, o: &Step) {
        for (k, t) in &o.systems {
            self.systems.entry(k.clone()).or_default().merge(t);
        }
        self.src.seconds += o.src.seconds;
        self.src.clip.merge(&o.src.clip);
        self.src.dropped += o.src.dropped;
    }

    /// 0–100, the weakest system counted twice; `None` when the setting is refused
    /// (given up, or clipping). `only`: these systems alone (the others measured are only watched).
    pub fn score_of(&self, only: Option<&[String]>) -> Option<f64> {
        if self.abandoned.is_some() || self.clipping() {
            return None;
        }
        let scores: Vec<f64> = self.systems.iter().filter(|(n, _)| only.is_none_or(|o| o.contains(n))).map(|(_, t)| t.score()).collect();
        if scores.is_empty() {
            return None;
        }
        let worst = scores.iter().copied().fold(f64::INFINITY, f64::min);
        Some((scores.iter().sum::<f64>() + worst) / (scores.len() as f64 + 1.0))
    }

    pub fn clipping(&self) -> bool {
        self.src.clip.get().is_some_and(|c| c > CLIP_PCT)
    }
}

/// Where one system's numbers are in the `stats` values: its control
/// channels' ([`trunk_core::trunk::SiteKey::stats_key`], one per site the radio receives) and its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Watch {
    pub short_name: String,
    pub cc_keys: Vec<String>,
}

/// The systems whose control channels source `source` receives, by the
/// engine's sites (each says the source it's on).
pub fn watches(sites: &[SiteStatus], source: usize) -> Vec<Watch> {
    let mut out: Vec<Watch> = vec![];
    for y in sites.iter().filter(|y| y.source == Some(source)) {
        let key = y.key.clone();
        match out.iter_mut().find(|w| w.short_name == y.short_name) {
            Some(w) => w.cc_keys.push(key),
            None => out.push(Watch { short_name: y.short_name.clone(), cc_keys: vec![key] }),
        }
    }
    out
}

/// One second's `stats` values.
pub type Values = serde_json::Map<String, Value>;

fn num(values: &Values, k: &str) -> Option<f64> {
    values.get(k).and_then(Value::as_f64)
}

/// Whether one of `w`'s control channels is locked in `values`.
pub fn locked(values: &Values, w: &Watch) -> bool {
    w.cc_keys.iter().any(|k| num(values, &format!("{k}/cc/locked")).unwrap_or(0.0) >= 0.5)
}

/// Adds one second's `stats` values (covering `dt` seconds) to `step`; `src_label`: the radio's ([`crate::config::Source::label`]).
pub fn tally(step: &mut Step, watches: &[Watch], src_label: &str, values: &Values, dt: f64) {
    let v = |k: &str| num(values, k);
    for w in watches {
        let t = step.systems.entry(w.short_name.clone()).or_default();
        t.seconds += dt;
        if locked(values, w) {
            t.locked_s += dt;
        }
        for k in &w.cc_keys {
            t.good += v(&format!("{k}/cc/good")).unwrap_or(0.0) * dt;
            t.bad += v(&format!("{k}/cc/bad")).unwrap_or(0.0) * dt;
            if let Some(s) = v(&format!("{k}/cc/airSym")) {
                *t.air_sym.get_or_insert(0.0) += s * dt;
            }
            if v(&format!("{k}/cc/locked")).unwrap_or(0.0) >= 0.5 {
                if let Some(s) = v(&format!("{k}/cc/snr")) {
                    t.snr.add(s);
                }
                if let Some(p) = v(&format!("{k}/cc/phaseErr")) {
                    t.phase_err.add(p);
                }
            }
        }
        let sys = system_key(&w.short_name);
        t.voice_frames += v(&format!("{sys}/voice/frames")).unwrap_or(0.0) * dt;
        t.voice_bad += v(&format!("{sys}/voice/bad")).unwrap_or(0.0) * dt;
    }
    let src = format!("src/{}", key_part(src_label));
    step.src.seconds += dt;
    if let Some(c) = v(&format!("{src}/clipPct")) {
        step.src.clip.add(c);
    }
    step.src.dropped += v(&format!("{src}/dropped")).unwrap_or(0.0) * dt;
}

/// The systems whose control channels were decoding before a change but
/// haven't been for the last [`ABANDON_S`]: `quiet` is how long each has been.
pub fn went_quiet(quiet: &BTreeMap<String, f64>, decoding_before: &[String]) -> Option<String> {
    let lost: Vec<&str> = decoding_before.iter().filter(|s| quiet.get(*s).is_some_and(|q| *q >= ABANDON_S)).map(String::as_str).collect();
    (!lost.is_empty()).then(|| format!("control channel lost: {}", lost.join(", ")))
}

/// The unscored systems (`protected`) a candidate hurts: each one's score under the
/// candidate less under the incumbent, averaged over the rounds (`(incumbent, candidate)`
/// slices), where that is a drop of more than `limit` points. `None`: it hurts none.
pub fn harmed(rounds: &[(Step, Step)], protected: &[String], limit: f64) -> Option<String> {
    let hurt: Vec<String> = protected
        .iter()
        .filter_map(|n| {
            let d: Vec<f64> = rounds.iter().filter_map(|(i, c)| Some(c.systems.get(n)?.score() - i.systems.get(n)?.score())).collect();
            let mean = (!d.is_empty()).then(|| d.iter().sum::<f64>() / d.len() as f64)?;
            (mean < -limit).then(|| format!("{n} {mean:+.1}"))
        })
        .collect();
    (!hurt.is_empty()).then(|| format!("hurts {}", hurt.join(", ")))
}

// ───────────────────────────── comparing two settings ─────────────────────────────

/// A candidate setting measured against the one it would replace, in rounds of a
/// slice each, alternately first: how much better it scored (`diff`, candidate less
/// incumbent, averaged over the rounds) and how sure that is (`se`, the standard
/// error of the rounds' differences).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cmp {
    pub diff: f64,
    pub se: f64,
    pub incumbent: f64,
    pub candidate: f64,
}

impl Cmp {
    /// From each round's (incumbent, candidate) scores; `None` where a slice was refused.
    /// A candidate refused in any round is refused (`None`). An incumbent refused in a
    /// round scores 0 there: whatever the candidate did, it did better.
    pub fn of(rounds: &[(Option<f64>, Option<f64>)]) -> Option<Cmp> {
        if rounds.is_empty() || rounds.iter().any(|(_, c)| c.is_none()) {
            return None;
        }
        let pairs: Vec<(f64, f64)> = rounds.iter().map(|(i, c)| (i.unwrap_or(0.0), c.unwrap_or(0.0))).collect();
        let n = pairs.len() as f64;
        let diffs: Vec<f64> = pairs.iter().map(|(i, c)| c - i).collect();
        let diff = diffs.iter().sum::<f64>() / n;
        let se = if pairs.len() < 2 { 0.0 } else { (diffs.iter().map(|d| (d - diff).powi(2)).sum::<f64>() / (n - 1.0)).sqrt() / n.sqrt() };
        Some(Cmp { diff, se, incumbent: pairs.iter().map(|p| p.0).sum::<f64>() / n, candidate: pairs.iter().map(|p| p.1).sum::<f64>() / n })
    }

    /// Better by more than `margin`, and by more than twice its standard error (so not
    /// by the luck of the minute).
    pub fn better(&self, margin: f64) -> bool {
        self.diff > margin.max(2.0 * self.se)
    }
}

/// One system's figures in a comparison: the incumbent's, then the candidate's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemFigures {
    pub system: String,
    /// Scored (not only watched).
    pub scored: bool,
    /// Control channel decoded, %.
    pub control_pct: [f64; 2],
    /// Voice frames lost, %, when both had enough voice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_lost_pct: Option<[f64; 2]>,
    /// Control channel phase error, degrees (P25), else its SNR, dB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_err: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snr_db: Option<[f64; 2]>,
}

/// Each system's figures, `inc` against `cand`, in `names`' order.
pub fn figures(inc: &Step, cand: &Step, names: &[String], scored: Option<&[String]>) -> Vec<SystemFigures> {
    let pair = |a: Option<f64>, b: Option<f64>| Some([a?, b?]);
    names
        .iter()
        .filter_map(|n| {
            let (a, b) = (inc.systems.get(n)?, cand.systems.get(n)?);
            let phase_err = pair(a.phase_err.get(), b.phase_err.get());
            Some(SystemFigures {
                system: n.clone(),
                scored: scored.is_none_or(|s| s.contains(n)),
                control_pct: [a.control() * 100.0, b.control() * 100.0],
                voice_lost_pct: pair(a.voice_lost(), b.voice_lost()).map(|[x, y]| [x * 100.0, y * 100.0]),
                snr_db: pair(a.snr.get(), b.snr.get()).filter(|_| phase_err.is_none()),
                phase_err,
            })
        })
        .collect()
}

// ───────────────────────────── the search ─────────────────────────────

/// Why a search stopped before it was done.
#[derive(Debug, PartialEq)]
pub enum Stop {
    /// Asked to, the recording stopped, or the radio went away: put it back and leave.
    Interrupted(String),
}

/// One candidate the search tried: the candidate, the best it was compared with, how
/// it compared (`None`: refused) and whether the search moved to it.
pub type Tried = (Point, Point, Option<Cmp>, bool);

/// How the search compares two settings: `compare(best, candidate)`, `None` when the
/// candidate was refused.
pub type Compare<'a> = dyn FnMut(&Point, &Point) -> Result<Option<Cmp>, Stop> + 'a;

/// What the search found: each candidate tried, against what, how it compared and
/// whether it was moved to.
#[derive(Debug, Default)]
pub struct Found {
    pub tried: Vec<Tried>,
    pub best: Point,
    pub out_of_steps: bool,
}

/// Follows the radio's plan ([`GainPlan::moves`]) from `start`: each move a
/// candidate compared with the best so far, kept only when it is
/// [`Cmp::better`] by `margin`. A setting is never tried twice. At most
/// `budget` comparisons.
pub fn search(plan: &GainPlan, start: Point, margin: f64, budget: usize, compare: &mut Compare) -> Result<Found, Stop> {
    let mut found = Found { best: start.clone(), ..Default::default() };
    let mut visited = std::collections::BTreeSet::from([start]);
    let mut walk = plan.walk();
    while let Some(p) = walk.next(&found.best) {
        if !visited.insert(p.clone()) {
            walk.refused();
            continue;
        }
        if found.tried.len() >= budget {
            found.out_of_steps = true;
            break;
        }
        let cmp = compare(&found.best, &p)?;
        let kept = cmp.is_some_and(|c| c.better(margin));
        found.tried.push((p.clone(), found.best.clone(), cmp, kept));
        if kept {
            found.best = p;
            walk.kept();
        } else {
            walk.refused();
        }
    }
    Ok(found)
}

// ───────────────────────────── what's told ─────────────────────────────

/// Where a tuning is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TunePhase {
    /// What it would do ([`TuneRequest::check`]): nothing has changed.
    Planned,
    #[default]
    Running,
    /// Done: `best` is what it found (and kept, with `apply`).
    Done,
    /// Stopped before it was done (asked to, recording stopped, a Setup edit of the radio).
    Stopped,
    Failed,
}

/// One comparison as it went.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuneTry {
    pub candidate: Setting,
    pub against: Setting,
    /// Its comparison; none: refused (`why`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmp: Option<Cmp>,
    pub kept: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    pub systems: Vec<SystemFigures>,
    /// The candidate's clipping, % of samples at full scale.
    pub clip_pct: f64,
    /// The final check: the best against where it started.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub final_check: bool,
}

/// A tuning as it is: the `tune` message, to control connections, each time it moves.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuneReport {
    pub phase: TunePhase,
    pub source: usize,
    /// The radio's title ("Source 2 (Roof)").
    pub radio: String,
    /// The plan followed, in words: its stages in the order they're moved, and why.
    pub plan: Vec<String>,
    pub scored: Vec<String>,
    pub watched: Vec<String>,
    /// The setting it had.
    pub start: Setting,
    pub tried: Vec<TuneTry>,
    /// Comparing now (none: between them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<Setting>,
    /// At most this long, s (each candidate's rounds, and the final check).
    pub estimate_s: f64,
    pub elapsed_s: f64,
    /// Done: the better setting found (none: none was clearly better).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best: Option<Setting>,
    /// It was kept and saved.
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(v: Value) -> Values {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn the_watches_are_the_sites_the_engine_has_on_the_radio() {
        let site = |sys: &str, name: &str, source| SiteStatus { short_name: sys.into(), site_name: name.into(), key: trunk_core::trunk::SiteKey { system: sys, site: name }.stats_key(), source, ..Default::default() };
        let sites = [site("metro", "metro", Some(0)), site("county", "North", Some(0)), site("county", "South", Some(1)), site("county", "East", Some(0)), site("far", "far", None)];
        let w = watches(&sites, 0);
        assert_eq!(w.iter().map(|w| w.short_name.as_str()).collect::<Vec<_>>(), ["metro", "county"]);
        assert_eq!(w[0].cc_keys, ["sys/metro"]);
        assert_eq!(w[1].cc_keys, ["sys/county/site/North", "sys/county/site/East"], "only the sites this radio receives");
    }

    #[test]
    fn a_second_of_stats_is_tallied() {
        let w = vec![Watch { short_name: "metro".into(), cc_keys: vec!["sys/metro".into()] }];
        let mut s = Step::default();
        let v = values(json!({ "sys/metro/cc/locked": 1, "sys/metro/cc/airSym": 2400, "sys/metro/cc/phaseErr": 12, "sys/metro/voice/frames": 100, "sys/metro/voice/bad": 5, "src/rtlsdr-1/clipPct": 0.1 }));
        for _ in 0..3 {
            tally(&mut s, &w, "rtlsdr:1", &v, 1.0);
        }
        let t = &s.systems["metro"];
        assert_eq!(t.control(), 0.5, "P25: coverage");
        assert_eq!(t.voice_lost(), Some(0.05));
        assert_eq!(t.phase_err.get(), Some(12.0));
        assert!(!s.clipping());
    }

    #[test]
    fn the_weakest_system_counts_twice_and_clipping_refuses() {
        let sys = |sym: f64| SysTally { seconds: 1.0, air_sym: Some(sym), ..Default::default() };
        let mut s = Step { systems: BTreeMap::from([("a".into(), sys(4800.0)), ("b".into(), sys(2400.0))]), ..Default::default() };
        assert!((s.score_of(None).unwrap() - (100.0 + 50.0 + 50.0) / 3.0).abs() < 1e-9);
        assert_eq!(s.score_of(Some(&["a".to_string()])), Some(100.0));
        s.src.clip.add(CLIP_PCT * 2.0);
        assert_eq!(s.score_of(None), None);
    }

    #[test]
    fn a_candidate_is_better_only_past_its_spread() {
        let steady = Cmp::of(&[(Some(80.0), Some(84.0)), (Some(81.0), Some(85.0)), (Some(79.0), Some(83.0))]).unwrap();
        assert!(steady.better(1.0));
        let noisy = Cmp::of(&[(Some(80.0), Some(95.0)), (Some(90.0), Some(80.0)), (Some(80.0), Some(87.0))]).unwrap();
        assert!(!noisy.better(1.0), "{noisy:?}");
        assert_eq!(Cmp::of(&[(Some(80.0), Some(90.0)), (Some(80.0), None)]), None, "refused once: refused");
    }

    #[test]
    fn a_protected_system_that_loses_too_much_refuses_the_candidate() {
        let step = |sym: f64| Step { systems: BTreeMap::from([("other".into(), SysTally { seconds: 1.0, air_sym: Some(sym), ..Default::default() })]), ..Default::default() };
        let rounds = vec![(step(4800.0), step(4000.0))];
        assert_eq!(harmed(&rounds, &["other".into()], 5.0), Some("hurts other -16.7".into()));
        assert_eq!(harmed(&rounds, &["other".into()], 20.0), None);
    }

    #[test]
    fn a_requests_numbers_are_kept_within_reason() {
        let r = TuneRequest { rounds: 1, slice_s: 1.0, protect: Some(0.0), ..Default::default() }.clamped();
        assert_eq!((r.rounds, r.slice_s, r.protect), (2, 5.0, None));
        let parsed: TuneRequest = serde_json::from_value(json!({ "source": 1, "try": [["lna", 11.0]] })).unwrap();
        assert_eq!((parsed.source, parsed.rounds), (1, 3));
    }
}
