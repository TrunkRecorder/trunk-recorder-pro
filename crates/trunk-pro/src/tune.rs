//! `trunk-pro tune [--url http://127.0.0.1:8080] [--source N] [--systems a,b] [--slice 15]
//! [--rounds 3] [--settle 4] [--max-steps 12] [--margin 1] [--mode manual|linearity|sensitivity] [--apply]` —
//! finds the gain at which a radio decodes its systems best, on a recorder that is
//! recording.
//!
//! It is a client of the recorder's API (`/api/ws`), like the interface: it changes
//! the radio's gain with `setConfig` (which the radio takes at once, as a gain changed
//! in Setup does), listens to the `stats` it publishes each second, and scores each
//! setting by what the radio's systems decoded while it was set:
//!
//! - the control channel first (P25's coverage, else the share of messages decoded,
//!   counted only while it was locked): without it nothing is recorded;
//! - then the voice frames lost, when there was enough voice to tell;
//! - a setting that clips (more than [`CLIP_MAX_PCT`] of samples at full scale) is
//!   refused, whatever it decoded.
//!
//! The weakest system counts twice, so a gain that helps the strong systems a little
//! at the cost of a weak one isn't chosen. The search ([`search`]) moves one gain
//! stage at a time from where the radio is, coarse steps first.
//!
//! A control channel's decoding drifts by more from minute to minute (traffic,
//! simulcast fading) than a step of gain usually moves it, so a setting is never
//! judged by one measurement: each candidate is compared with the best so far by
//! turns, a slice each, for a few rounds ([`Cmp`]), and moved to only when it is better
//! by more than `--margin` and by more than twice the standard error of the rounds'
//! differences. At the end the best is compared with the starting setting once more.
//!
//! Recording goes on throughout, so a bad setting costs that radio's systems while
//! it is tried: a setting under which a control channel that was decoding stops for
//! [`ABANDON_S`] seconds is given up at once. However it ends — done, an error,
//! Ctrl-C — the radio is put back as it was, unless `--apply` (then it keeps the best).

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trunk_core::metrics::{key_part, system_key};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::{die, Args};

/// A setting with more of its samples than this at full scale (%) is refused.
pub const CLIP_MAX_PCT: f64 = 0.5;
/// A control channel that was decoding, silent this long under a setting (s): the setting is given up.
pub const ABANDON_S: f64 = 10.0;
/// Voice frames a system needs in a step (P25: 50 a second) before its voice counts.
const MIN_VOICE_FRAMES: f64 = 250.0;
/// How much of a system's score is its control channel, when its voice counts (the rest is voice).
const CC_WEIGHT: f64 = 0.7;
/// The R820T / R828D's gains (dB): what an RTL-SDR can be set to.
const RTL_GAINS: [f64; 29] = [
    0.0, 0.9, 1.4, 2.7, 3.7, 7.7, 8.7, 12.5, 14.4, 15.7, 16.6, 19.7, 20.7, 22.9, 25.4, 28.0, 29.7, 32.8, 33.8, 36.4, 37.2, 38.6, 40.2, 42.1, 43.4, 43.9, 44.5, 48.0, 49.6,
];

// ───────────────────────────── the radio's gain stages ─────────────────────────────

/// One gain stage the search moves: the values it can take, and the steps (in places
/// along `values`) it tries, coarse first.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub name: &'static str,
    /// The config key it is written to.
    pub key: &'static str,
    pub values: Vec<f64>,
    pub steps: Vec<usize>,
}

/// A radio's gain, as the search sees it: its stages, and how they are written into
/// its config entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Knobs {
    /// What else its config entry needs set (an Airspy's `gainMode`, AGC off).
    fixed: Vec<(&'static str, Value)>,
    pub axes: Vec<Axis>,
}

fn range(lo: i32, hi: i32, by: i32) -> Vec<f64> {
    (lo..=hi).step_by(by as usize).map(f64::from).collect()
}

impl Knobs {
    /// The stages of the radio `src` (a config `sources` entry). `mode` searches an
    /// Airspy's other gain mode (`manual`, `linearity`, `sensitivity`) than the one it is in.
    pub fn of(src: &Value, mode: Option<&str>) -> Result<Knobs, String> {
        let kind = src["type"].as_str().unwrap_or("").to_ascii_lowercase();
        match kind.as_str() {
            "airspy" | "hydrasdr" => {
                let now = src["gainMode"].as_str().unwrap_or("linearity").to_ascii_lowercase();
                let mode = mode.map(str::to_ascii_lowercase).unwrap_or(if now.is_empty() { "linearity".into() } else { now });
                match mode.as_str() {
                    "manual" => Ok(Knobs {
                        fixed: vec![("gainMode", json!("manual")), ("agc", json!(false))],
                        axes: vec![
                            Axis { name: "LNA", key: "lnaStep", values: range(0, 14, 1), steps: vec![3, 1] },
                            Axis { name: "mixer", key: "mixerStep", values: range(0, 15, 1), steps: vec![3, 1] },
                            Axis { name: "VGA", key: "vgaStep", values: range(0, 15, 1), steps: vec![3, 1] },
                        ],
                    }),
                    "linearity" | "sensitivity" => Ok(Knobs {
                        fixed: vec![("gainMode", json!(mode))],
                        axes: vec![Axis { name: if mode == "linearity" { "linearity" } else { "sensitivity" }, key: "gainStep", values: range(0, 21, 1), steps: vec![4, 1] }],
                    }),
                    m => Err(format!("--mode {m}: manual, linearity or sensitivity")),
                }
            }
            "hackrf" => Ok(Knobs {
                fixed: vec![],
                axes: vec![
                    Axis { name: "LNA dB", key: "lnaDb", values: range(0, 40, 8), steps: vec![1] },
                    Axis { name: "VGA dB", key: "vgaDb", values: range(0, 62, 2), steps: vec![3, 1] },
                    Axis { name: "amp", key: "amp", values: vec![0.0, 1.0], steps: vec![1] },
                ],
            }),
            "rtlsdr" => Ok(Knobs { fixed: vec![("agc", json!(false))], axes: vec![Axis { name: "gain dB", key: "gainDb", values: RTL_GAINS.to_vec(), steps: vec![4, 1] }] }),
            "" => Err("the source has no type".into()),
            k => Err(format!("tuning a {k} source's gain isn't supported yet (Airspy, HydraSDR, HackRF and RTL-SDR are)")),
        }
    }

    /// Where `src`'s gain is now, as a place on each axis (the nearest value). A stage
    /// it doesn't set starts at the radio's default.
    pub fn point(&self, src: &Value) -> Vec<usize> {
        self.axes
            .iter()
            .map(|a| {
                let v = match &src[a.key] {
                    Value::Bool(b) => f64::from(u8::from(*b)),
                    v => v.as_f64().unwrap_or_else(|| default_of(a.key)),
                };
                nearest(&a.values, v)
            })
            .collect()
    }

    /// `src` with its gain set to `p`.
    pub fn apply(&self, src: &Value, p: &[usize]) -> Value {
        let mut s = src.clone();
        for (k, v) in &self.fixed {
            s[*k] = v.clone();
        }
        for (a, &i) in self.axes.iter().zip(p) {
            let v = a.values[i];
            s[a.key] = match a.key {
                "amp" => json!(v != 0.0),
                "gainDb" => json!(v),
                _ => json!(v as i64),
            };
        }
        s
    }

    /// `--try lna=11,vga=9`: `start` with these stages set. A stage is named by its
    /// config key (`lnaStep`, `vgaDb`…) or its short name (`lna`, `mixer`, `vga`, `step`,
    /// `gain`, `amp`); a value is snapped to the nearest the stage takes.
    pub fn parse_try(&self, start: &[usize], spec: &str) -> Result<Vec<usize>, String> {
        let mut p = start.to_vec();
        for kv in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("--try {kv}: write stage=value"))?;
            let k = k.trim().to_ascii_lowercase();
            let ax = self
                .axes
                .iter()
                .position(|a| a.key.to_ascii_lowercase() == k || a.name.split(' ').next().is_some_and(|n| n.to_ascii_lowercase() == k) || (k == "step" && a.key == "gainStep") || (k == "gain" && a.key == "gainDb"))
                .ok_or_else(|| format!("--try {kv}: this radio's stages are {}", self.axes.iter().map(|a| a.key).collect::<Vec<_>>().join(", ")))?;
            let v = match v.trim().to_ascii_lowercase().as_str() {
                "on" | "true" => 1.0,
                "off" | "false" => 0.0,
                n => n.parse::<f64>().map_err(|_| format!("--try {kv}: not a number"))?,
            };
            p[ax] = nearest(&self.axes[ax].values, v);
        }
        Ok(p)
    }

    /// "LNA 10 · mixer 7 · VGA 7".
    pub fn describe(&self, p: &[usize]) -> String {
        self.axes
            .iter()
            .zip(p)
            .map(|(a, &i)| match a.key {
                "amp" => format!("amp {}", if a.values[i] != 0.0 { "on" } else { "off" }),
                _ => format!("{} {}", a.name, a.values[i]),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// A stage's value when the config leaves it out (the recorder's defaults).
fn default_of(key: &str) -> f64 {
    match key {
        "gainStep" => 14.0,
        "lnaStep" | "mixerStep" | "vgaStep" => 10.0,
        "lnaDb" => 16.0,
        "vgaDb" => 20.0,
        "gainDb" => 25.4,
        _ => 0.0,
    }
}

fn nearest(values: &[f64], v: f64) -> usize {
    (0..values.len()).min_by(|&a, &b| (values[a] - v).abs().total_cmp(&(values[b] - v).abs())).unwrap_or(0)
}

// ───────────────────────────── what a step measured ─────────────────────────────

/// One system's control channel (its sites on this radio, together) and voice over a step.
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

/// The radio over a step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SrcTally {
    pub seconds: f64,
    pub clip: Mean,
    pub clip_max: f64,
    pub dropped: f64,
    pub noise: Mean,
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
}

impl Step {
    /// Adds `o`'s tallies to these (a setting's slices, together). Its being given up isn't carried.
    pub fn merge(&mut self, o: &Step) {
        for (k, t) in &o.systems {
            self.systems.entry(k.clone()).or_default().merge(t);
        }
        self.src.seconds += o.src.seconds;
        self.src.clip.merge(&o.src.clip);
        self.src.clip_max = self.src.clip_max.max(o.src.clip_max);
        self.src.dropped += o.src.dropped;
        self.src.noise.merge(&o.src.noise);
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

impl SysTally {
    /// The control channel's share decoded, 0–1: P25's coverage (symbols decoded
    /// against the 4800 a second it carries), else messages decoded of those framed,
    /// for the share of the step it was locked.
    pub fn control(&self) -> f64 {
        if self.seconds <= 0.0 {
            return 0.0;
        }
        match self.air_sym {
            Some(sym) => (sym / (4800.0 * self.seconds)).min(1.0),
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

impl Step {
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
        self.src.clip.get().is_some_and(|c| c > CLIP_MAX_PCT)
    }
}

/// Where one system's numbers are in a `stats` message: its control channels'
/// (`sys/<short>` or `sys/<short>/site/<site>`, one per site on this radio) and its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Watch {
    pub short_name: String,
    pub cc_keys: Vec<String>,
}

impl Watch {
    pub fn new(short_name: &str, sites: &[&str]) -> Watch {
        let sys = system_key(short_name);
        let cc_keys = sites
            .iter()
            .map(|site| if site.is_empty() || *site == short_name { sys.clone() } else { format!("{sys}/site/{}", key_part(site)) })
            .collect();
        Watch { short_name: short_name.to_string(), cc_keys }
    }
}

/// Adds one `stats` message (`values`, covering `dt` seconds) to `step`.
pub fn tally(step: &mut Step, watches: &[Watch], src_label: &str, values: &Value, dt: f64) {
    let v = |k: &str| values.get(k).and_then(Value::as_f64);
    for w in watches {
        let t = step.systems.entry(w.short_name.clone()).or_default();
        t.seconds += dt;
        // Locked while any of its control channels on this radio is.
        if w.cc_keys.iter().any(|k| v(&format!("{k}/cc/locked")).unwrap_or(0.0) >= 0.5) {
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
        step.src.clip_max = step.src.clip_max.max(c);
    }
    step.src.dropped += v(&format!("{src}/dropped")).unwrap_or(0.0) * dt;
    if let Some(n) = v(&format!("{src}/noise")) {
        step.src.noise.add(n);
    }
}

/// The systems whose control channels were locked at the start of a step but
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

// ───────────────────────────── the search ─────────────────────────────

/// Why a search stopped before it was done.
#[derive(Debug, PartialEq)]
pub enum Stop {
    /// Ctrl-C, or the recorder went away: put things back and leave.
    Interrupted(String),
}

/// A candidate setting measured against the one it would replace, in rounds of a
/// slice each, alternately first: how much better it scored (`diff`, candidate less
/// incumbent, averaged over the rounds) and how sure that is (`se`, the standard
/// error of the rounds' differences).
#[derive(Clone, Copy, Debug, PartialEq)]
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

/// One candidate the search tried: the candidate, the best it was compared with, how
/// it compared (`None`: refused) and whether the search moved to it.
pub type Tried = (Vec<usize>, Vec<usize>, Option<Cmp>, bool);

/// How the search compares two settings: `compare(best, candidate)`, `None` when the
/// candidate was refused.
pub type Compare<'a> = dyn FnMut(&[usize], &[usize]) -> Result<Option<Cmp>, Stop> + 'a;

/// What the search found: each candidate tried, against what, how it compared and
/// whether it was moved to.
#[derive(Debug, Default)]
pub struct Found {
    pub tried: Vec<Tried>,
    pub best: Vec<usize>,
    pub out_of_steps: bool,
}

/// Moves one axis at a time from `start` — each axis's coarse step first, then finer —
/// comparing each candidate with the best so far (`compare(best, candidate)`, `None`:
/// the candidate was refused) and moving to it only when it is [`Cmp::better`] by
/// `margin`. A setting is never tried twice. At most `budget` comparisons.
pub fn search(axes: &[Axis], start: Vec<usize>, margin: f64, budget: usize, compare: &mut Compare) -> Result<Found, Stop> {
    let mut found = Found::default();
    let mut visited: std::collections::BTreeSet<Vec<usize>> = std::collections::BTreeSet::from([start.clone()]);
    let mut best = start;
    let levels = axes.iter().map(|a| a.steps.len()).max().unwrap_or(0);
    'levels: for level in 0..levels {
        // Passes over the axes at this step size, while one of them moves.
        for _pass in 0..3 {
            let mut moved = false;
            for (ax, a) in axes.iter().enumerate() {
                let step = a.steps[level.min(a.steps.len() - 1)];
                for dir in [1i64, -1] {
                    let mut went = false;
                    loop {
                        let to = best[ax] as i64 + dir * step as i64;
                        if to < 0 || to >= a.values.len() as i64 {
                            break;
                        }
                        let mut p = best.clone();
                        p[ax] = to as usize;
                        if !visited.insert(p.clone()) {
                            break;
                        }
                        if found.tried.len() >= budget {
                            found.out_of_steps = true;
                            break 'levels;
                        }
                        let cmp = compare(&best, &p)?;
                        let kept = cmp.is_some_and(|c| c.better(margin));
                        found.tried.push((p.clone(), best.clone(), cmp, kept));
                        if !kept {
                            break;
                        }
                        (best, went, moved) = (p, true, true);
                    }
                    // Up helped: no need to look down from where it got to.
                    if went {
                        break;
                    }
                }
            }
            if !moved {
                break;
            }
        }
    }
    found.best = best;
    Ok(found)
}

// ───────────────────────────── talking to the recorder ─────────────────────────────

struct Conn {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    config: Value,
    version: Value,
    status: Option<Value>,
    /// The last `stats` message's values.
    last_stats: Value,
    phase: String,
    stop: Arc<AtomicBool>,
}

enum Got {
    Msg(Value),
    Nothing,
}

impl Conn {
    fn open(url: &str, stop: Arc<AtomicBool>) -> Result<Conn, String> {
        let ws_url = ws_url(url)?;
        let (ws, _) = tungstenite::connect(&ws_url).map_err(|e| format!("couldn't connect to {ws_url}: {e}"))?;
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_millis(500))).map_err(|e| e.to_string())?;
        }
        let mut c = Conn { ws, config: Value::Null, version: Value::Null, status: None, last_stats: Value::Null, phase: String::new(), stop };
        // The greeting, up to hello.
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            match c.next()? {
                Got::Msg(m) if m["type"] == "hello" => break,
                _ if Instant::now() > until => return Err("the recorder didn't finish greeting".into()),
                _ => {}
            }
        }
        if c.config.is_null() {
            return Err("the recorder didn't send its config".into());
        }
        Ok(c)
    }

    /// The next message (keeping the config, its version, the status and the state up to date).
    fn next(&mut self) -> Result<Got, String> {
        if self.stop.load(Ordering::SeqCst) {
            return Err("interrupted".into());
        }
        let m = match self.ws.read() {
            Ok(Message::Text(t)) => serde_json::from_str::<Value>(t.as_str()).map_err(|e| e.to_string())?,
            Ok(Message::Close(_)) => return Err("the recorder closed the connection".into()),
            Ok(_) => return Ok(Got::Nothing),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(Got::Nothing),
            Err(e) => return Err(format!("connection to the recorder: {e}")),
        };
        match m["type"].as_str() {
            Some("config") => {
                self.config = m["config"].clone();
                self.version = m["version"].clone();
            }
            Some("status") => self.status = Some(m.clone()),
            Some("stats") => self.last_stats = m["values"].clone(),
            Some("state") => self.phase = m["phase"].as_str().unwrap_or("").to_string(),
            _ => {}
        }
        Ok(Got::Msg(m))
    }

    fn send(&mut self, v: Value) -> Result<(), String> {
        self.ws.send(Message::text(v.to_string())).map_err(|e| format!("sending to the recorder: {e}"))
    }

    /// Writes `src` as source `i` with `setConfig`, and waits for it to be taken.
    /// Tried again once if the config changed elsewhere meanwhile.
    fn set_source(&mut self, i: usize, src: &Value) -> Result<(), String> {
        for attempt in 0..2 {
            let mut cfg = self.config.clone();
            cfg["sources"][i] = src.clone();
            let version = self.version.clone();
            self.send(json!({ "type": "setConfig", "config": cfg, "version": version }))?;
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                match self.next()? {
                    Got::Msg(m) if m["type"] == "configSaved" => {
                        match m["error"].as_str() {
                            None => return Ok(()),
                            Some(e) if attempt == 0 && e.contains("changed elsewhere") => break,
                            Some(e) => return Err(format!("the recorder refused the gain: {e}")),
                        }
                    }
                    Got::Msg(m) if m["type"] == "error" => return Err(format!("the recorder refused the gain: {}", m["message"].as_str().or(m["error"].as_str()).unwrap_or("error"))),
                    _ if Instant::now() > until => return Err("the recorder didn't answer setConfig".into()),
                    _ => {}
                }
            }
            // The newer config arrives as a `config` message before configSaved; take one more look for it.
            let until = Instant::now() + Duration::from_millis(500);
            while Instant::now() < until {
                self.next()?;
            }
        }
        Err("the config kept changing elsewhere; try again".into())
    }
}

fn ws_url(url: &str) -> Result<String, String> {
    let u = url.trim().trim_end_matches('/');
    let rest = u.strip_prefix("http://").or_else(|| u.strip_prefix("ws://")).unwrap_or(u);
    if u.starts_with("https://") || u.starts_with("wss://") {
        return Err("--url: https isn't supported; use http:// (or an SSH tunnel)".into());
    }
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        return Err(format!("--url {url}: no host"));
    }
    let host = if host.contains(':') { host.to_string() } else { format!("{host}:8080") };
    Ok(format!("ws://{host}/api/ws"))
}

/// The systems with a control channel on the radio `src` (a `status.sources` entry),
/// from the `status` message: each with the names of its sites there.
fn systems_on(status: &Value, src: &Value) -> Vec<Watch> {
    let center = src["centerHz"].as_f64().unwrap_or(0.0);
    let rate = src["rateHz"].as_f64().unwrap_or(0.0);
    let guard = src["settings"]["guardHz"].as_f64().unwrap_or(75_000.0);
    let half = rate / 2.0 - guard;
    let mut out = vec![];
    for y in status["status"]["systems"].as_array().into_iter().flatten() {
        let name = y["shortName"].as_str().unwrap_or("");
        let sites: Vec<&str> = y["sites"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["controlChannelHz"].as_f64().is_some_and(|f| (f - center).abs() <= half))
            .map(|s| s["name"].as_str().unwrap_or(""))
            .collect();
        if !name.is_empty() && !sites.is_empty() {
            out.push(Watch::new(name, &sites));
        }
    }
    out
}

// ───────────────────────────── the command ─────────────────────────────

pub fn cli(a: &Args) {
    if a.flag("help") || a.positional.first().is_some_and(|p| p == "help") {
        print!("{HELP}");
        return;
    }
    // Ctrl-C: put the radio back, then leave. A second one leaves at once.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
            rt.block_on(async {
                let _ = tokio::signal::ctrl_c().await;
                eprintln!("\nStopping: putting the radio back (Ctrl-C again to leave at once)…");
                stop.store(true, Ordering::SeqCst);
                let _ = tokio::signal::ctrl_c().await;
                std::process::exit(130);
            });
        });
    }
    if let Err(e) = run(a, stop) {
        die(&format!("tune: {e}"));
    }
}

const HELP: &str = "\
trunk-pro tune [--url http://127.0.0.1:8080] [--source N] [--systems a,b] [--slice 15] [--rounds 3]
               [--settle 4] [--max-steps 12] [--margin 1] [--mode manual|linearity|sensitivity]
               [--protect 5] [--try stage=value,…] [--apply]

Finds the gain at which a radio decodes its systems best, on a recorder that is
recording. Each candidate gain is compared with the best so far by turns, a --slice
of seconds each (after --settle), for --rounds rounds, and kept only if it scores
better by more than --margin and by more than twice the rounds' spread. Systems are
scored on control channel decoding first, then voice frames lost; a gain that clips is
refused. Recording goes on throughout; the radio is put back as it was at the end (or on
Ctrl-C) unless --apply keeps the best.

  --url        the recorder (its built-in interface's address)
  --source     the radio, by its place in Setup's list from 0 (needed with several)
  --systems    score only these systems; the radio's others are still watched, and a gain
               that silences one of their control channels is given up
  --protect    with --systems: refuse a gain that costs one of the radio's other systems more
               than this many points (0–100 scale; default 5, `off` to allow it)
  --try        compare one setting with the one it is at, no search: lna=11,mixer=10,vga=9
               (Airspy manual), step=16 (Airspy linearity/sensitivity), lna=32,vga=24,amp=on
               (HackRF), gain=33.8 (RTL-SDR)
  --mode       Airspy: search this gain mode instead of the one it is in
  --max-steps  at most this many candidates compared
  --margin     a move is kept only when it scores this much better (0–100 scale)
  --yes        don't ask before starting
  --apply      keep the best setting, and set the frequency correction (ppm) the control
               channels measure when it is 0.3 ppm or more off (it takes effect at the next Start)
";

/// What's fixed for a run: the radio, its systems, the timings.
struct Plan<'a> {
    i: usize,
    original: &'a Value,
    knobs: &'a Knobs,
    /// Every system on the radio: measured, and a setting that silences one is given up.
    watches: &'a [Watch],
    /// The systems scored (`--systems`); `None`: all of them.
    scored: Option<&'a [String]>,
    /// `--systems`' others, and how many points one may lose before a candidate is refused (`--protect`).
    protected: &'a [String],
    protect: Option<f64>,
    label: &'a str,
    settle: f64,
    slice: f64,
    rounds: usize,
    /// The search's starting point, when it stands for the radio as it is but isn't
    /// quite it (another Airspy mode, AGC on, a gain off the table): that point is
    /// set as the radio was, so the first comparisons are against what it really had.
    as_it_is: Option<&'a [usize]>,
}

fn run(a: &Args, stop: Arc<AtomicBool>) -> Result<(), String> {
    let url = a.get("url").unwrap_or("http://127.0.0.1:8080");
    // (--dwell: the name of the slice before rounds.)
    let slice = a.get("slice").or(a.get("dwell")).and_then(|s| s.parse().ok()).unwrap_or(15.0_f64).max(5.0);
    let rounds = a.num("rounds", 3.0).clamp(2.0, 20.0) as usize;
    let settle = a.num("settle", 4.0).max(0.0);
    let budget = a.num("max-steps", 12.0).max(1.0) as usize;
    let margin = a.num("margin", 1.0).max(0.0);
    let apply = a.flag("apply");

    let mut c = Conn::open(url, stop.clone())?;
    if c.phase != "running" {
        return Err(format!("the recorder at {url} isn't recording: start it first (the tuner scores what it decodes)"));
    }
    // A status, for where the radios are and which systems they carry.
    let until = Instant::now() + Duration::from_secs(10);
    while c.status.is_none() {
        if Instant::now() > until {
            return Err("no status from the recorder".into());
        }
        c.next()?;
    }
    let status = c.status.clone().unwrap_or_default();
    let sources = status["sources"].as_array().cloned().unwrap_or_default();
    let i = match a.get("source") {
        Some(s) => s.parse::<usize>().map_err(|_| format!("--source {s}: a number from 0"))?,
        None if sources.len() == 1 => 0,
        None => {
            let list: Vec<String> = sources.iter().map(|s| format!("  {}  {}", s["index"], s["label"].as_str().unwrap_or(""))).collect();
            return Err(format!("this recorder has {} radios; say which with --source:\n{}", sources.len(), list.join("\n")));
        }
    };
    let run_src = sources.iter().find(|s| s["index"].as_u64() == Some(i as u64)).ok_or_else(|| format!("--source {i}: no such radio"))?.clone();
    let label = run_src["label"].as_str().unwrap_or("").to_string();
    let original = c.config["sources"].get(i).cloned().ok_or_else(|| format!("--source {i}: not in the config"))?;
    let knobs = Knobs::of(&original, a.get("mode"))?;
    let watches = systems_on(&status, &run_src);
    if watches.is_empty() {
        return Err(format!("{label} carries no trunked system's control channel: nothing to score"));
    }
    let names: Vec<&str> = watches.iter().map(|w| w.short_name.as_str()).collect();
    // --systems: score these; the rest are still watched (and guarded).
    let scored: Option<Vec<String>> = match a.get("systems") {
        Some(only) => {
            let only: Vec<String> = only.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
            if let Some(missing) = only.iter().find(|s| !names.contains(&s.as_str())) {
                return Err(format!("--systems: {missing} has no control channel on {label} (it has: {})", names.join(", ")));
            }
            (!only.is_empty()).then_some(only)
        }
        None => None,
    };
    let protected: Vec<String> = match &scored {
        Some(s) => names.iter().filter(|n| !s.iter().any(|x| x == *n)).map(|n| n.to_string()).collect(),
        None => vec![],
    };
    // --protect N (points; default 5), or off.
    let protect = match a.get("protect") {
        Some("off" | "0") => None,
        Some(v) => Some(v.parse::<f64>().map_err(|_| format!("--protect {v}: points (0–100), or off"))?.max(0.0)),
        None => Some(5.0),
    }
    .filter(|_| !protected.is_empty());
    let start = knobs.point(&original);
    // The search's start, written back, isn't what the radio has: the start stands for it as it is.
    let as_it_is = (knobs.apply(&original, &start) != original).then_some(start.as_slice());
    // What to call the start in what's printed.
    let now = if as_it_is.is_some() { "the setting it had".to_string() } else { knobs.describe(&start) };
    // --try: one setting against the one it is at, no search.
    let tried = a.get("try").map(|t| knobs.parse_try(&start, t)).transpose()?;
    if tried.as_ref() == Some(&start) && as_it_is.is_none() {
        return Err(format!("--try: that is the setting it is at ({now})"));
    }
    let per = rounds as f64 * 2.0 * (settle + slice);
    println!("Tuning {label} (source {i}) on {url}");
    match &scored {
        Some(s) => println!(
            "  scoring: {} (also watching {}{})",
            s.join(", "),
            protected.join(", "),
            protect.map_or(String::new(), |p| format!(": a candidate that costs one of them more than {p} points is refused"))
        ),
        None => println!("  scoring: {}", names.join(", ")),
    }
    match as_it_is {
        Some(_) => println!("  now: as it is (comparisons start against it; the search begins at {})", knobs.describe(&start)),
        None => println!("  now: {}", knobs.describe(&start)),
    }
    match &tried {
        Some(t) => println!("  trying: {} against it, {rounds} rounds of {slice:.0} s each way (after {settle:.0} s to settle)", knobs.describe(t)),
        None => println!("  each candidate: {rounds} rounds against the best so far, {slice:.0} s each (after {settle:.0} s to settle): {per:.0} s; at most {budget} and a final check"),
    }
    println!("  to put it back by hand if this is killed: {original}");
    println!();
    // What's going to happen, in plain words, before anything changes.
    let total_s = match &tried {
        Some(_) => per,
        None => (budget + 1) as f64 * per,
    };
    println!("{}", estimate(total_s, tried.is_none(), &names, apply));
    let ppm_now = sources.iter().find(|s| s["index"].as_u64() == Some(i as u64)).and_then(|r| ppm_change(original["ppm"].as_f64().unwrap_or(0.0), r["ppmApply"].as_f64()));
    if let Some(to) = ppm_now {
        println!(
            "Its frequency correction is off: {} ppm is set, the control channels measure {to}.{}",
            original["ppm"].as_f64().unwrap_or(0.0),
            if apply { " --apply will set it, and it takes effect only when recording is restarted (Stop, then Start)." } else { "" }
        );
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) && !a.flag("yes") {
        println!("Press Enter to start, or Ctrl-C to leave without changing anything.");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        if stop.load(Ordering::SeqCst) {
            return Err("left before starting; nothing was changed".into());
        }
    }
    let began = Instant::now();
    println!();

    let plan = Plan { i, original: &original, knobs: &knobs, watches: &watches, scored: scored.as_deref(), protected: &protected, protect, label: &label, settle, slice, rounds, as_it_is };
    // The last comparison's tallies: the final check's (or --try's), for the summary.
    let mut last: Option<(Step, Step)> = None;
    let result = {
        let mut compare = |inc: &[usize], cand: &[usize]| -> Result<Option<Cmp>, Stop> {
            let (cmp, inc_step, cand_step) = compare(&mut c, &plan, inc, cand).map_err(Stop::Interrupted)?;
            println!("{}", line(&knobs, inc, cand, cmp, margin, &inc_step, &cand_step, &names, scored.as_deref()));
            last = Some((inc_step, cand_step));
            Ok(cmp)
        };
        match &tried {
            Some(t) => compare(&start, t).map(|check| (Found { best: t.clone(), ..Default::default() }, Some(check).flatten())),
            None => search(&knobs.axes, start.clone(), margin, budget, &mut compare).and_then(|found| {
                // The search moved by steps; the best against the start, once more, decides.
                if found.best == start {
                    return Ok((found, None));
                }
                println!("  final check: the best against the starting setting");
                let check = compare(&start, &found.best)?;
                Ok((found, check))
            }),
        }
    };

    // Whatever happened, the radio goes back unless the best is to be kept.
    let finish = |c: &mut Conn, keep: Option<&[usize]>| -> Result<(), String> {
        let src = match keep {
            Some(p) => knobs.apply(&original, p),
            None => original.clone(),
        };
        c.set_source(i, &src)
    };
    let (found, check) = match result {
        Ok(r) => r,
        Err(Stop::Interrupted(why)) => {
            // A fresh connection: the old one may be what failed.
            stop.store(false, Ordering::SeqCst);
            let restored = Conn::open(url, stop).and_then(|mut c2| finish(&mut c2, None));
            return Err(match restored {
                Ok(()) => format!("{why}; the radio is back at {}", now),
                Err(e) => format!("{why}; and putting the radio back failed ({e}): set it by hand to {original}"),
            });
        }
    };

    let better = check.is_some_and(|c| c.better(margin));
    println!();
    let set_ppm = original["ppm"].as_f64().unwrap_or(0.0);
    let run_src = c.status.as_ref().and_then(|s| s["sources"].as_array()).and_then(|v| v.iter().find(|s| s["index"].as_u64() == Some(i as u64))).cloned();
    let ppm_to = run_src.as_ref().and_then(|r| ppm_change(set_ppm, r["ppmApply"].as_f64()));
    let center = run_src.as_ref().and_then(|r| r["centerHz"].as_f64()).unwrap_or(0.0);
    let mins = began.elapsed().as_secs_f64() / 60.0;
    println!("Summary ({mins:.0} min{})", if tried.is_none() { format!(", {} settings compared{}", found.tried.len(), if found.out_of_steps { ", stopped at --max-steps" } else { "" }) } else { String::new() });

    // What's recommended, and what it does.
    let gain_part = better.then(|| format!("gain {} (now {now})", knobs.describe(&found.best)));
    let ppm_part = ppm_to.map(|to| format!("frequency correction {to} ppm (now {set_ppm})"));
    match (&gain_part, &ppm_part) {
        (None, None) => println!("  No change recommended: the radio is set as well as this could find."),
        (g, p) => println!("  Recommended: {}.", [g.clone(), p.clone()].into_iter().flatten().collect::<Vec<_>>().join(", and ")),
    }
    match (better, check, &last) {
        (true, Some(ck), Some((was, will))) => {
            println!("  With the new gain, measured against the current one by turns ({rounds} rounds): score {:+.1} ± {:.1}", ck.diff, ck.se);
            for b in benefits(was, will, &names, scored.as_deref()) {
                println!("    {b}");
            }
        }
        _ => {
            let why = match check {
                Some(ck) if found.best != start => format!("{} came closest, {:+.1} ± {:.1}: not clearly better", knobs.describe(&found.best), ck.diff, ck.se),
                None if found.best != start => format!("{} was refused (it hurt another system, lost a control channel, or clipped)", knobs.describe(&found.best)),
                _ => format!("No setting tried was clearly better than {now}"),
            };
            println!("  Gain: keep it. {why}.");
        }
    }
    if let Some(to) = ppm_to {
        let off_hz = ((to - set_ppm) * center / 1e6).abs();
        println!(
            "  Frequency correction: {set_ppm} → {to} ppm removes a tuning error of about {off_hz:.0} Hz{}.",
            if original["autoTune"] == true { " that AutoTune is correcting for meanwhile" } else { ", which costs decoding until it's set" }
        );
    }

    // Apply, or put back.
    let keep = (apply && better).then_some(found.best.as_slice());
    finish(&mut c, keep)?;
    // The ppm after the gain, as a change of its own: a gain is taken while recording, a ppm
    // only at the next Start, and an edit with both would hold the gain back with it.
    if let (true, Some(to)) = (apply, ppm_to) {
        let mut src = match keep {
            Some(p) => knobs.apply(&original, p),
            None => original.clone(),
        };
        src["ppm"] = json!(to);
        c.set_source(i, &src)?;
    }
    println!();
    match (apply, better) {
        (true, true) => println!("Applied: the gain {} is set now and saved.", knobs.describe(&found.best)),
        (false, true) => println!("Nothing was changed: the radio is back at {now}. Run again with --apply, or set it in Setup, to make the change."),
        (_, false) => println!("The gain is back at {now}."),
    }
    match (apply, ppm_to) {
        (true, Some(to)) => println!("⚠ The frequency correction {to} ppm is saved, but takes effect only when recording restarts: Stop, then Start (or restart the service)."),
        (false, Some(_)) => println!("The frequency correction wasn't changed: --apply sets it (or Setup's \"Set correction to\"); it then needs a restart of recording."),
        _ => {}
    }
    Ok(())
}

/// Before anything changes: how long it will take, and what that means for the radio's systems.
pub fn estimate(total_s: f64, search: bool, systems: &[&str], apply: bool) -> String {
    let mins = (total_s / 60.0).ceil().max(1.0);
    let what = if search { format!("This will take up to about {mins:.0} minutes (often less: settings that do badly are dropped early).") } else { format!("This will take about {mins:.0} minutes.") };
    let after = if apply { "At the end it keeps the best gain (and sets the frequency correction if it's off)." } else { "At the end the radio goes back exactly as it was; nothing is saved (--apply keeps the best)." };
    format!(
        "{what}\nRecording and uploading carry on throughout, but {} may record worse while poorer settings are on the air, a minute or two at a time. {after}",
        join_and(systems)
    )
}

/// "a", "a and b", "a, b and c".
fn join_and(v: &[&str]) -> String {
    match v {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What a change does, system by system: `was` and `will` are the current and the new
/// setting's tallies from the same comparison. Starred: the systems scored, when only
/// some were.
pub fn benefits(was: &Step, will: &Step, names: &[&str], scored: Option<&[String]>) -> Vec<String> {
    names
        .iter()
        .filter_map(|n| {
            let (a, b) = (was.systems.get(*n)?, will.systems.get(*n)?);
            let star = if scored.is_some_and(|s| s.iter().any(|x| x == n)) { "*" } else { " " };
            let (c0, c1) = (a.control() * 100.0, b.control() * 100.0);
            let mut out = format!("{star}{n:<16} control channel {c0:.1}% → {c1:.1}% ({:+.1})", c1 - c0);
            if let (Some(v0), Some(v1)) = (a.voice_lost(), b.voice_lost()) {
                out.push_str(&format!(" · voice lost {:.1}% → {:.1}%", v0 * 100.0, v1 * 100.0));
            }
            match (a.phase_err.get(), b.phase_err.get(), a.snr.get(), b.snr.get()) {
                (Some(p0), Some(p1), _, _) => out.push_str(&format!(" · phase error {p0:.0}° → {p1:.0}°")),
                (None, None, Some(s0), Some(s1)) => out.push_str(&format!(" · SNR {s0:.0} → {s1:.0} dB")),
                _ => {}
            }
            Some(out)
        })
        .collect()
}

/// The correction to set: Setup's (`apply`, worked out from the control channels against
/// the ppm the radio runs with), when it is 0.3 ppm or more from `set`.
pub fn ppm_change(set: f64, apply: Option<f64>) -> Option<f64> {
    // (+ 0.0: no "-0".)
    apply.map(|a| a + 0.0).filter(|a| (a - set).abs() >= 0.3)
}

/// `cand` against `inc`, by turns (inc first in even rounds, cand first in odd ones,
/// so a trend over the minutes falls on both alike). A candidate refused in a slice
/// ends it at once. Returns the comparison and each side's tallies, added up.
fn compare(c: &mut Conn, plan: &Plan, inc: &[usize], cand: &[usize]) -> Result<(Option<Cmp>, Step, Step), String> {
    let (mut inc_all, mut cand_all) = (Step::default(), Step::default());
    let mut rounds = vec![];
    let mut slices: Vec<(Step, Step)> = vec![];
    for r in 0..plan.rounds {
        let (mut inc_slice, mut cand_slice) = (Step::default(), Step::default());
        // (Which side each slice is, by turns: the two can be the same point, see [`Plan::as_it_is`].)
        let order = if r % 2 == 0 { [false, true] } else { [true, false] };
        let (mut si, mut sc) = (None, None);
        for is_cand in order {
            let step = if is_cand { measure_slice(c, plan, cand, false)? } else { measure_slice(c, plan, inc, true)? };
            let s = step.score_of(plan.scored);
            if is_cand {
                cand_all.merge(&step);
                if s.is_none() {
                    cand_all.abandoned = step.abandoned.clone();
                    rounds.push((si.flatten(), None));
                    return Ok((Cmp::of(&rounds), inc_all, cand_all));
                }
                sc = Some(s);
                cand_slice = step;
            } else {
                inc_all.merge(&step);
                si = Some(s);
                inc_slice = step;
            }
        }
        rounds.push((si.flatten(), sc.flatten()));
        slices.push((inc_slice, cand_slice));
        // The other systems: refused when one loses more than --protect over the rounds, or
        // twice that in the first (so a setting that badly hurts one isn't kept on the air).
        if let Some(limit) = plan.protect {
            let limit = if r == 0 && plan.rounds > 1 { 2.0 * limit } else { limit };
            if let Some(why) = harmed(&slices, plan.protected, limit).filter(|_| r == 0 || r + 1 == plan.rounds) {
                cand_all.abandoned = Some(why);
                return Ok((None, inc_all, cand_all));
            }
        }
    }
    Ok((Cmp::of(&rounds), inc_all, cand_all))
}

/// Sets `p`, lets it settle, and measures it for a slice.
/// `incumbent`: the setting being compared against, which at the start point is the radio as it is.
fn measure_slice(c: &mut Conn, plan: &Plan, p: &[usize], incumbent: bool) -> Result<Step, String> {
    // The control channels decoding before the change: losing one of them gives the setting up.
    let locked = |values: &Value, w: &Watch| w.cc_keys.iter().any(|k| values.get(format!("{k}/cc/locked")).and_then(Value::as_f64).unwrap_or(0.0) >= 0.5);
    let decoding: Vec<String> = plan.watches.iter().filter(|w| locked(&c.last_stats, w)).map(|w| w.short_name.clone()).collect();
    let src = if incumbent && plan.as_it_is == Some(p) { plan.original.clone() } else { plan.knobs.apply(plan.original, p) };
    c.set_source(plan.i, &src)?;
    let settle_until = Instant::now() + Duration::from_secs_f64(plan.settle);
    let mut step = Step::default();
    let mut quiet: BTreeMap<String, f64> = BTreeMap::new();
    let mut last_t: Option<f64> = None;
    let mut heard = Instant::now();
    loop {
        if heard.elapsed() > Duration::from_secs(10) {
            return Err("the recorder stopped sending stats (has recording stopped?)".into());
        }
        let Got::Msg(m) = c.next()? else { continue };
        if m["type"] == "state" && m["phase"] != "running" {
            return Err("recording stopped".into());
        }
        if m["type"] != "stats" {
            continue;
        }
        heard = Instant::now();
        let t = m["t"].as_f64().unwrap_or(0.0);
        let dt = last_t.map_or(1.0, |l| (t - l).clamp(0.2, 5.0));
        last_t = Some(t);
        let values = &m["values"];
        // A control channel lost while settling counts toward giving the setting up.
        for w in plan.watches {
            let q = quiet.entry(w.short_name.clone()).or_default();
            *q = if locked(values, w) { 0.0 } else { *q + dt };
        }
        if let Some(why) = went_quiet(&quiet, &decoding) {
            step.abandoned = Some(why);
            return Ok(step);
        }
        if Instant::now() < settle_until {
            continue;
        }
        tally(&mut step, plan.watches, plan.label, values, dt);
        if step.src.clip.get().is_some_and(|c| c > 4.0 * CLIP_MAX_PCT) && step.src.seconds >= 5.0 {
            step.abandoned = Some(format!("clipping ({:.1}% of samples at full scale)", step.src.clip.get().unwrap_or(0.0)));
            return Ok(step);
        }
        if step.src.seconds >= plan.slice {
            return Ok(step);
        }
    }
}

/// A comparison's lines: the candidate against the best so far, then each system
/// (the candidate's figures, the incumbent's in brackets).
#[allow(clippy::too_many_arguments)]
fn line(knobs: &Knobs, inc: &[usize], cand: &[usize], cmp: Option<Cmp>, margin: f64, inc_step: &Step, cand_step: &Step, names: &[&str], scored: Option<&[String]>) -> String {
    let verdict = match (cmp, &cand_step.abandoned) {
        (_, Some(why)) => format!("given up: {why}"),
        (None, None) if cand_step.clipping() => format!("refused: clipping {:.2}%", cand_step.src.clip.get().unwrap_or(0.0)),
        (None, None) => "refused".into(),
        (Some(c), None) => format!("{:+5.1} ± {:.1}  {}", c.diff, c.se, if c.better(margin) { "better: moving to it" } else { "not clearly better" }),
    };
    let mut out = format!("  {}  vs {}:  {verdict}", knobs.describe(cand), knobs.describe(inc));
    for n in names {
        let (Some(t), Some(b)) = (cand_step.systems.get(*n), inc_step.systems.get(*n)) else { continue };
        let voice = match (t.voice_lost(), b.voice_lost()) {
            (Some(l), Some(m)) => format!(" · voice lost {:.1}% ({:.1}%)", l * 100.0, m * 100.0),
            (Some(l), None) => format!(" · voice lost {:.1}%", l * 100.0),
            _ => String::new(),
        };
        let q = match (t.phase_err.get(), b.phase_err.get(), t.snr.get(), b.snr.get()) {
            (Some(pe), Some(pb), _, _) => format!(" · {pe:.0}° ({pb:.0}°)"),
            (None, None, Some(s), Some(sb)) => format!(" · {s:.0} dB ({sb:.0} dB)"),
            _ => String::new(),
        };
        // Scored systems are starred when only some are.
        let star = if scored.is_some_and(|s| s.iter().any(|x| x == n)) { "*" } else { " " };
        out.push_str(&format!("\n    {star} {n:<16} control {:5.1}% ({:5.1}%){voice}{q}", t.control() * 100.0, b.control() * 100.0));
    }
    let clip = cand_step.src.clip.get().unwrap_or(0.0);
    if clip > 0.0 || cand_step.src.dropped > 0.0 {
        out.push_str(&format!("\n      radio: clipping {clip:.2}%{}", if cand_step.src.dropped > 0.0 { format!(" · {:.0} samples dropped", cand_step.src.dropped) } else { String::new() }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn airspy() -> Value {
        json!({ "type": "airspy", "serial": "644064DC042A24CD", "gainMode": "manual", "lnaStep": 14, "mixerStep": 7, "vgaStep": 7, "centerHz": 772000000, "rateHz": 10000000 })
    }

    #[test]
    fn an_airspys_manual_gain_is_read_and_written_stage_by_stage() {
        let k = Knobs::of(&airspy(), None).unwrap();
        let p = k.point(&airspy());
        assert_eq!(p, vec![14, 7, 7]);
        assert_eq!(k.describe(&p), "LNA 14 · mixer 7 · VGA 7");
        let s = k.apply(&airspy(), &[10, 8, 9]);
        assert_eq!((s["lnaStep"].clone(), s["mixerStep"].clone(), s["vgaStep"].clone(), s["gainMode"].clone()), (json!(10), json!(8), json!(9), json!("manual")));
        // Everything else stays as it was.
        assert_eq!((s["serial"].clone(), s["centerHz"].clone()), (airspy()["serial"].clone(), airspy()["centerHz"].clone()));
    }

    #[test]
    fn an_airspy_can_be_searched_in_another_mode() {
        let k = Knobs::of(&airspy(), Some("linearity")).unwrap();
        assert_eq!(k.axes.len(), 1);
        // Not in that mode now: it starts from the default step.
        assert_eq!(k.point(&airspy()), vec![14]);
        let s = k.apply(&airspy(), &[17]);
        assert_eq!((s["gainMode"].clone(), s["gainStep"].clone()), (json!("linearity"), json!(17)));
        assert!(Knobs::of(&airspy(), Some("loud")).is_err());
    }

    #[test]
    fn a_hackrfs_stages_and_amp() {
        let h = json!({ "type": "hackrf", "lnaDb": 40, "vgaDb": 18, "amp": false });
        let k = Knobs::of(&h, None).unwrap();
        let p = k.point(&h);
        assert_eq!(k.describe(&p), "LNA dB 40 · VGA dB 18 · amp off");
        let s = k.apply(&h, &[3, 10, 1]);
        assert_eq!((s["lnaDb"].clone(), s["vgaDb"].clone(), s["amp"].clone()), (json!(24), json!(20), json!(true)));
    }

    #[test]
    fn an_rtl_sdrs_gain_snaps_to_its_table_and_agc_goes_off() {
        let r = json!({ "type": "rtlsdr", "gainDb": 30, "agc": true });
        let k = Knobs::of(&r, None).unwrap();
        let p = k.point(&r);
        assert_eq!(k.axes[0].values[p[0]], 29.7);
        let s = k.apply(&r, &p);
        assert_eq!((s["gainDb"].clone(), s["agc"].clone()), (json!(29.7), json!(false)));
    }

    #[test]
    fn a_setting_to_try_is_written_by_stage() {
        let k = Knobs::of(&airspy(), None).unwrap();
        let start = k.point(&airspy());
        assert_eq!(k.parse_try(&start, "lna=11, vga=9").unwrap(), vec![11, 7, 9]);
        assert_eq!(k.parse_try(&start, "mixerStep=20").unwrap(), vec![14, 15, 7], "snapped to what the stage takes");
        assert!(k.parse_try(&start, "lnaDb=10").unwrap_err().contains("lnaStep, mixerStep, vgaStep"));
        assert!(k.parse_try(&start, "lna").is_err());
        let h = json!({ "type": "hackrf", "lnaDb": 40, "vgaDb": 18 });
        let kh = Knobs::of(&h, None).unwrap();
        assert_eq!(kh.describe(&kh.parse_try(&kh.point(&h), "lna=32,vga=24,amp=on").unwrap()), "LNA dB 32 · VGA dB 24 · amp on");
    }

    #[test]
    fn other_radios_are_refused_for_now() {
        assert!(Knobs::of(&json!({ "type": "usrp" }), None).unwrap_err().contains("isn't supported"));
    }

    #[test]
    fn a_systems_score_is_its_control_channel_then_its_voice() {
        // P25 coverage: 4320 of 4800 symbols a second = 90%.
        let t = SysTally { seconds: 10.0, locked_s: 10.0, air_sym: Some(43_200.0), ..Default::default() };
        assert!((t.control() - 0.9).abs() < 1e-9);
        assert!((t.score() - 90.0).abs() < 1e-9, "no voice to judge: the control channel alone");
        let v = SysTally { voice_frames: 1000.0, voice_bad: 100.0, ..t.clone() };
        assert!((v.voice_lost().unwrap() - 0.1).abs() < 1e-9);
        assert!((v.score() - (70.0 * 0.9 + 30.0 * 0.9)).abs() < 1e-9);
        // Too little voice to tell.
        assert_eq!(SysTally { voice_frames: 100.0, voice_bad: 50.0, ..t.clone() }.voice_lost(), None);
        // No coverage reported: messages decoded, for the time it was locked.
        let s = SysTally { seconds: 10.0, locked_s: 5.0, good: 90.0, bad: 10.0, ..Default::default() };
        assert!((s.control() - 0.45).abs() < 1e-9);
    }

    #[test]
    fn the_weakest_system_counts_twice_and_clipping_is_refused() {
        let sys = |cov: f64| SysTally { seconds: 1.0, locked_s: 1.0, air_sym: Some(4800.0 * cov), ..Default::default() };
        let mut step = Step::default();
        step.systems.insert("a".into(), sys(1.0));
        step.systems.insert("b".into(), sys(0.4));
        assert!((step.score_of(None).unwrap() - (100.0 + 40.0 + 40.0) / 3.0).abs() < 1e-9);
        // Scoring only some: the others are left out.
        assert_eq!(step.score_of(Some(&["a".to_string()])), Some(100.0));
        assert_eq!(step.score_of(Some(&["nobody".to_string()])), None);
        step.src.clip.add(0.8);
        assert_eq!(step.score_of(None), None);
        let given_up = Step { abandoned: Some("x".into()), ..Default::default() };
        assert_eq!(given_up.score_of(None), None);
    }

    #[test]
    fn stats_are_tallied_per_system_and_site() {
        let watches = vec![Watch::new("Dallas", &["Dallas"]), Watch::new("Metro", &["North", "South"])];
        assert_eq!(watches[1].cc_keys, vec!["sys/Metro/site/North".to_string(), "sys/Metro/site/South".to_string()]);
        let values = json!({
            "sys/Dallas/cc/locked": 1, "sys/Dallas/cc/good": 40, "sys/Dallas/cc/bad": 2, "sys/Dallas/cc/airSym": 4700, "sys/Dallas/cc/phaseErr": 9.0,
            "sys/Dallas/voice/frames": 100, "sys/Dallas/voice/bad": 4,
            "sys/Metro/site/North/cc/locked": 0, "sys/Metro/site/South/cc/locked": 1, "sys/Metro/site/South/cc/good": 10, "sys/Metro/site/South/cc/snr": 14.0,
            "src/Airspy SN 1/clipPct": 0.1, "src/Airspy SN 1/dropped": 0, "src/Airspy SN 1/noise": -90,
        });
        let mut step = Step::default();
        tally(&mut step, &watches, "Airspy SN 1", &values, 1.0);
        tally(&mut step, &watches, "Airspy SN 1", &values, 1.0);
        let d = &step.systems["Dallas"];
        assert_eq!((d.seconds, d.locked_s, d.good, d.bad, d.air_sym, d.voice_frames, d.voice_bad), (2.0, 2.0, 80.0, 4.0, Some(9400.0), 200.0, 8.0));
        assert_eq!(d.phase_err.get(), Some(9.0));
        let m = &step.systems["Metro"];
        assert_eq!((m.locked_s, m.good, m.air_sym), (2.0, 20.0, None), "locked while either site is");
        assert_eq!(m.snr.get(), Some(14.0));
        assert_eq!((step.src.seconds, step.src.clip.get()), (2.0, Some(0.1)));
    }

    #[test]
    fn a_source_label_with_a_slash_is_looked_up_as_the_stats_name_it() {
        let mut step = Step::default();
        tally(&mut step, &[], "USRP addr=1/2", &json!({ "src/USRP addr=1_2/clipPct": 3.0 }), 1.0);
        assert_eq!(step.src.clip.get(), Some(3.0));
    }

    #[test]
    fn a_control_channel_that_stops_decoding_gives_the_setting_up() {
        let decoding = vec!["a".to_string()];
        let mut quiet = BTreeMap::from([("a".to_string(), ABANDON_S - 1.0), ("b".to_string(), 100.0)]);
        assert_eq!(went_quiet(&quiet, &decoding), None, "b wasn't decoding to begin with");
        quiet.insert("a".into(), ABANDON_S);
        assert_eq!(went_quiet(&quiet, &decoding).as_deref(), Some("control channel lost: a"));
    }

    #[test]
    fn systems_are_on_the_radio_whose_band_has_their_control_channel() {
        let status = json!({ "status": { "systems": [
            { "shortName": "In", "sites": [{ "name": "In", "controlChannelHz": 856_000_000.0 }] },
            // 856.5 ± 5 MHz, less 75 kHz at each edge: up to 861.425.
            { "shortName": "Edge", "sites": [{ "name": "", "controlChannelHz": 861_450_000.0 }] },
            { "shortName": "Out", "sites": [{ "name": "Out", "controlChannelHz": 770_000_000.0 }] },
            { "shortName": "Two", "sites": [{ "name": "A", "controlChannelHz": 852_000_000.0 }, { "name": "B", "controlChannelHz": 771_000_000.0 }] },
        ] } });
        let src = json!({ "centerHz": 856_500_000.0, "rateHz": 10_000_000.0, "settings": { "guardHz": 75_000 } });
        let w = systems_on(&status, &src);
        assert_eq!(w.iter().map(|w| w.short_name.as_str()).collect::<Vec<_>>(), vec!["In", "Two"], "861.45 is in the guard band");
        assert_eq!(w[1].cc_keys, vec!["sys/Two/site/A".to_string()]);
    }

    #[test]
    fn urls() {
        assert_eq!(ws_url("http://10.1.100.43:8080").unwrap(), "ws://10.1.100.43:8080/api/ws");
        assert_eq!(ws_url("10.1.100.43").unwrap(), "ws://10.1.100.43:8080/api/ws");
        assert_eq!(ws_url("http://host:9000/setup/").unwrap(), "ws://host:9000/api/ws");
        assert!(ws_url("https://host").is_err());
    }

    /// A smooth hill: best at LNA 9, mixer 12, VGA 4.
    fn hill(p: &[usize]) -> f64 {
        let d = |a: usize, b: f64| (a as f64 - b).powi(2);
        100.0 - d(p[0], 9.0) - d(p[1], 12.0) - d(p[2], 4.0)
    }

    /// Compares on `f` exactly (three rounds with no spread).
    fn exact(f: impl Fn(&[usize]) -> Option<f64>) -> impl FnMut(&[usize], &[usize]) -> Result<Option<Cmp>, Stop> {
        move |inc, cand| Ok(Cmp::of(&[(f(inc), f(cand)); 3]))
    }

    #[test]
    fn a_comparison_is_the_rounds_average_difference_and_its_spread() {
        let c = Cmp::of(&[(Some(80.0), Some(84.0)), (Some(90.0), Some(92.0)), (Some(70.0), Some(76.0))]).unwrap();
        assert!((c.diff - 4.0).abs() < 1e-9);
        assert!((c.se - (4.0f64 / 3.0).sqrt()).abs() < 1e-9, "sd 2 over √3");
        assert!((c.incumbent - 80.0).abs() < 1e-9 && (c.candidate - 84.0).abs() < 1e-9);
        assert!(c.better(1.0));
        // The same average, all over the place: luck, not better.
        let noisy = Cmp::of(&[(Some(80.0), Some(100.0)), (Some(90.0), Some(78.0)), (Some(70.0), Some(74.0))]).unwrap();
        assert!((noisy.diff - 4.0).abs() < 1e-9);
        assert!(!noisy.better(1.0), "{noisy:?}");
        // Refused in a round: refused. An incumbent refused scores 0 in its round.
        assert_eq!(Cmp::of(&[(Some(80.0), Some(84.0)), (Some(80.0), None)]), None);
        assert_eq!(Cmp::of(&[(None, Some(60.0)), (None, Some(60.0))]).unwrap().diff, 60.0);
        assert_eq!(Cmp::of(&[]), None);
    }

    #[test]
    fn the_search_climbs_one_stage_at_a_time_to_the_best() {
        let k = Knobs::of(&airspy(), None).unwrap();
        let found = search(&k.axes, vec![14, 7, 7], 0.5, 60, &mut exact(|p| Some(hill(p)))).unwrap();
        assert_eq!(found.best, vec![9, 12, 4]);
        assert!(!found.out_of_steps);
        let tried: std::collections::BTreeSet<_> = found.tried.iter().map(|t| t.0.clone()).collect();
        assert_eq!(tried.len(), found.tried.len(), "each setting is tried once");
        assert!(found.tried.len() < 40, "{} candidates", found.tried.len());
        // Each compared with the best of its moment.
        assert_eq!(found.tried[0].1, vec![14, 7, 7]);
    }

    #[test]
    fn a_refused_setting_isnt_moved_to_and_the_budget_is_kept() {
        let k = Knobs::of(&airspy(), None).unwrap();
        // Above LNA 11 it clips: refused, though the hill would like more.
        let found = search(&k.axes, vec![8, 12, 4], 0.5, 60, &mut exact(|p| if p[0] > 11 { None } else { Some(100.0 - (p[0] as f64 - 14.0).powi(2)) })).unwrap();
        assert_eq!(found.best[0], 11);
        let few = search(&k.axes, vec![14, 7, 7], 0.5, 4, &mut exact(|p| Some(hill(p)))).unwrap();
        assert!(few.out_of_steps);
        assert_eq!(few.tried.len(), 4);
    }

    #[test]
    fn noise_doesnt_move_it() {
        let k = Knobs::of(&airspy(), None).unwrap();
        // Within the margin.
        let found = search(&k.axes, vec![10, 10, 10], 2.0, 60, &mut exact(|p| Some(80.0 + if p == [10, 13, 10] { 1.5 } else { 0.0 }))).unwrap();
        assert_eq!(found.best, vec![10, 10, 10]);
        // Ahead on average, by luck: one round far ahead, the others behind (what tr2's simulcast did).
        let found = search(&k.axes, vec![10, 10, 10], 1.0, 60, &mut |_, _| Ok(Cmp::of(&[(Some(80.0), Some(97.0)), (Some(85.0), Some(82.0)), (Some(90.0), Some(86.0))]))).unwrap();
        assert_eq!(found.best, vec![10, 10, 10]);
    }

    #[test]
    fn it_stops_when_interrupted() {
        let k = Knobs::of(&airspy(), None).unwrap();
        let mut n = 0;
        let r = search(&k.axes, vec![10, 10, 10], 1.0, 60, &mut |_, _| {
            n += 1;
            if n == 3 {
                Err(Stop::Interrupted("ctrl-c".into()))
            } else {
                Ok(Cmp::of(&[(Some(50.0), Some(40.0)); 3]))
            }
        });
        assert_eq!(r.unwrap_err(), Stop::Interrupted("ctrl-c".into()));
    }

    #[test]
    fn a_candidate_that_hurts_another_system_is_named() {
        let step = |a: f64, b: f64| {
            let mut s = Step::default();
            s.systems.insert("MetroC".into(), SysTally { seconds: 1.0, locked_s: 1.0, air_sym: Some(4800.0 * a), ..Default::default() });
            s.systems.insert("Balch".into(), SysTally { seconds: 1.0, locked_s: 1.0, air_sym: Some(4800.0 * b), ..Default::default() });
            s
        };
        let protected = ["Balch".to_string()];
        // tr2's LNA 13 for MetroC: Balch 74 % → 45 %.
        let rounds = [(step(0.756, 0.743), step(0.807, 0.446))];
        assert_eq!(harmed(&rounds, &protected, 5.0).as_deref(), Some("hurts Balch -29.7"));
        assert_eq!(harmed(&rounds, &protected, 30.0), None, "within what is allowed");
        // A round worse, a round better: on average within the limit.
        let mixed = [(step(0.8, 0.9), step(0.8, 0.8)), (step(0.8, 0.8), step(0.8, 0.86))];
        assert_eq!(harmed(&mixed, &protected, 5.0), None);
        // Only the protected are looked at.
        assert_eq!(harmed(&[(step(0.9, 0.9), step(0.1, 0.9))], &protected, 5.0), None);
    }

    #[test]
    fn the_ppm_is_changed_only_when_it_is_off_by_enough() {
        // tr1's HackRF: -0.29 set, -0.94 measured right; tr2's Airspy: -0.58 set, 0 right.
        assert_eq!(ppm_change(-0.29, Some(-0.94)), Some(-0.94));
        let to = ppm_change(-0.58, Some(-0.0)).unwrap();
        assert!(to == 0.0 && to.is_sign_positive(), "printed as 0, not -0");
        assert_eq!(ppm_change(1.0, Some(1.2)), None, "within 0.3");
        assert_eq!(ppm_change(1.0, None), None, "not measured");
    }

    #[test]
    fn the_estimate_says_how_long_and_who_is_affected() {
        let e = estimate(24.0 * 60.0 + 10.0, true, &["Lewisville", "Balch", "MetroC"], false);
        assert!(e.starts_with("This will take up to about 25 minutes"), "{e}");
        assert!(e.contains("Lewisville, Balch and MetroC may record worse"), "{e}");
        assert!(e.contains("nothing is saved"), "{e}");
        assert!(estimate(200.0, false, &["A"], true).starts_with("This will take about 4 minutes."));
        assert_eq!(join_and(&["a", "b"]), "a and b");
    }

    #[test]
    fn benefits_are_told_system_by_system() {
        let sys = |cov: f64, frames: f64, bad: f64, pe: f64| {
            let mut t = SysTally { seconds: 1.0, locked_s: 1.0, air_sym: Some(4800.0 * cov), voice_frames: frames, voice_bad: bad, ..Default::default() };
            t.phase_err.add(pe);
            t
        };
        let (mut was, mut will) = (Step::default(), Step::default());
        was.systems.insert("MetroC".into(), sys(0.756, 1000.0, 133.0, 19.0));
        will.systems.insert("MetroC".into(), sys(0.807, 1000.0, 65.0, 18.0));
        was.systems.insert("Balch".into(), sys(0.743, 0.0, 0.0, 21.0));
        will.systems.insert("Balch".into(), sys(0.760, 0.0, 0.0, 21.0));
        let b = benefits(&was, &will, &["Balch", "MetroC"], Some(&["MetroC".to_string()]));
        assert_eq!(b[0], " Balch            control channel 74.3% → 76.0% (+1.7) · phase error 21° → 21°");
        assert_eq!(b[1], "*MetroC           control channel 75.6% → 80.7% (+5.1) · voice lost 13.3% → 6.5% · phase error 19° → 18°");
    }

    #[test]
    fn slices_add_up() {
        let mut a = Step::default();
        a.systems.insert("s".into(), SysTally { seconds: 10.0, locked_s: 10.0, air_sym: Some(40_000.0), voice_frames: 200.0, voice_bad: 10.0, ..Default::default() });
        a.src.clip.add(0.1);
        let mut b = a.clone();
        b.systems.get_mut("s").unwrap().air_sym = Some(48_000.0);
        b.src.clip.add(0.3);
        b.abandoned = Some("x".into());
        let mut all = Step::default();
        all.merge(&a);
        all.merge(&b);
        let s = &all.systems["s"];
        assert_eq!((s.seconds, s.air_sym, s.voice_frames, s.voice_bad), (20.0, Some(88_000.0), 400.0, 20.0));
        assert_eq!(s.voice_lost(), Some(0.05), "enough voice together, though neither slice had");
        assert!((all.src.clip.get().unwrap() - 0.5 / 3.0).abs() < 1e-9);
        assert_eq!(all.abandoned, None);
    }
}
