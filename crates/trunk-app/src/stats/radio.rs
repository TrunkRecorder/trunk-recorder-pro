//! The radio system as heard: talkgroups (first and last heard, calls and
//! airtime by hour), radios (when they transmitted, what they're affiliated
//! with, which talkgroups they use, who they talk with), and how well each
//! frequency's voice decodes. Fed by the session's events ([`Registry`]'s
//! `call_*` / `message` / `concluded`); kept per system across runs (saved as
//! JSON); asked by the dashboard ([`Registry::query`]).
//!
//! Every table is bounded: past its cap the least recently heard go.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use trunk_core::trunk::{Call, Message, MessageType};

/// Caps per system.
const MAX_TGS: usize = 5000;
const MAX_UNITS: usize = 20_000;
const MAX_PAIRS: usize = 50_000;
const MAX_FREQS: usize = 512;
const MAX_AFFILIATIONS: usize = 2000;
const MAX_LENGTHS: usize = 5000;
const MAX_RECENT_TX: usize = 30;
const TOP_TGS: usize = 8;
/// Hours of per-hour activity kept (a week).
const HOURS: i64 = 168;
/// A registry this young has no baseline: everything in it is "new".
const BASELINE_S: i64 = 3600;
/// "New": first heard within this long.
const NEW_S: i64 = 24 * 3600;
/// Smoothing for per-frequency averages (per call).
const EWMA: f32 = 0.1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tg {
    pub first: i64,
    pub last: i64,
    pub calls: u32,
    pub secs: f32,
    /// Calls that were encrypted.
    pub enc: u32,
    /// (hour = Unix s / 3600, calls, seconds), oldest first, last week only.
    pub hours: VecDeque<(i64, (u32, f32))>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Unit {
    pub first: i64,
    pub last: i64,
    /// When it last transmitted (0: never heard to).
    pub last_tx: i64,
    pub tx: u32,
    pub tx_secs: f32,
    /// The talkgroup it last affiliated with, and when.
    pub aff: Option<u32>,
    pub aff_t: i64,
    /// Registered (true) / deregistered (false), when heard.
    pub reg: Option<bool>,
    /// Talkgroups it transmitted on: (talkgroup, transmissions), most first.
    pub tgs: Vec<(u32, u32)>,
    /// Its latest transmissions: (Unix s, talkgroup, seconds).
    pub recent: VecDeque<(i64, u32, f32)>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Pair {
    pub n: u32,
    pub last: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Freq {
    pub calls: u32,
    pub frames: u64,
    pub errors: u64,
    pub bad: u64,
    /// Smoothed per call.
    pub snr: Option<f32>,
    pub ferr: Option<f32>,
    pub clean: Option<f32>,
    pub last: i64,
    /// (hour, (calls, frames, errors, bad frames)), last week.
    pub hours: VecDeque<(i64, (u32, u64, u64, u64))>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemRadio {
    /// When this system was first heard (Unix s).
    pub created: i64,
    pub tgs: HashMap<u32, Tg>,
    pub units: HashMap<u32, Unit>,
    /// Radios heard on one call, or in a unit-to-unit call: key `a << 32 | b`, a < b.
    pub pairs: HashMap<u64, Pair>,
    pub freqs: HashMap<u64, Freq>,
    /// (Unix s, unit, talkgroup), newest last.
    pub affiliations: VecDeque<(i64, u32, u32)>,
    /// (Unix s, talkgroup, seconds) of recent calls, newest last.
    pub lengths: VecDeque<(i64, u32, f32)>,
}

/// What the configuration says of a talkgroup.
#[derive(Clone, Debug, Default)]
pub struct TgInfo {
    pub alpha_tag: String,
    pub ignore: bool,
}

/// Lookups the queries need from outside: a talkgroup's entry in the
/// system's file, a radio's name.
pub struct Names<'a> {
    pub tg: &'a dyn Fn(&str, u32) -> Option<TgInfo>,
    pub unit: &'a dyn Fn(&str, u32) -> Option<String>,
}

#[derive(Default)]
pub struct Registry {
    pub systems: BTreeMap<String, SystemRadio>,
    dirty: BTreeSet<String>,
}

fn pair_key(a: u32, b: u32) -> u64 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    (lo as u64) << 32 | hi as u64
}

fn bump_hour<T>(hours: &mut VecDeque<(i64, T)>, h: i64, f: impl FnOnce(&mut T), new: impl FnOnce() -> T) {
    match hours.back_mut() {
        Some((hh, v)) if *hh == h => f(v),
        _ => {
            let mut v = new();
            f(&mut v);
            hours.push_back((h, v));
        }
    }
    while hours.front().is_some_and(|(hh, _)| *hh <= h - HOURS) {
        hours.pop_front();
    }
}

/// Keep the `cap` most recently heard of `m` (when it's grown 10% past it).
fn evict<V>(m: &mut HashMap<impl Copy + Eq + std::hash::Hash, V>, cap: usize, last: impl Fn(&V) -> i64) {
    if m.len() <= cap + cap / 10 {
        return;
    }
    let mut ts: Vec<i64> = m.values().map(&last).collect();
    let cut = ts.len() - cap;
    let (_, t, _) = ts.select_nth_unstable(cut);
    let t = *t;
    m.retain(|_, v| last(v) >= t);
}

impl SystemRadio {
    fn new(t: i64) -> Self {
        SystemRadio { created: t, ..Default::default() }
    }

    fn has_baseline(&self, t: i64) -> bool {
        t - self.created >= BASELINE_S
    }

    fn unit(&mut self, unit: u32, t: i64) -> (&mut Unit, bool) {
        let new = !self.units.contains_key(&unit);
        let u = self.units.entry(unit).or_insert_with(|| Unit { first: t, ..Default::default() });
        u.last = u.last.max(t);
        (u, new)
    }

    fn pair(&mut self, a: u32, b: u32, t: i64) {
        if a == b || a == 0 || b == 0 {
            return;
        }
        let p = self.pairs.entry(pair_key(a, b)).or_default();
        p.n += 1;
        p.last = t;
    }

    fn evict(&mut self) {
        evict(&mut self.tgs, MAX_TGS, |v| v.last);
        evict(&mut self.units, MAX_UNITS, |v| v.last);
        evict(&mut self.pairs, MAX_PAIRS, |v| v.last);
        evict(&mut self.freqs, MAX_FREQS, |v| v.last);
    }
}

/// What a call's start told the registry: firsts, for the event feed.
#[derive(Default, Debug, PartialEq)]
pub struct Firsts {
    pub tg: bool,
    pub units: Vec<u32>,
}

impl Registry {
    fn sys(&mut self, name: &str, t: i64) -> &mut SystemRadio {
        self.dirty.insert(name.to_string());
        self.systems.entry(name.to_string()).or_insert_with(|| SystemRadio::new(t))
    }

    /// A call began (`t`: Unix s). Returns what was heard for the first time
    /// (once the system has a baseline).
    pub fn call_start(&mut self, system: &str, c: &Call, t: i64) -> Firsts {
        let s = self.sys(system, t);
        let base = s.has_baseline(t);
        let mut firsts = Firsts::default();
        if !c.unit_to_unit {
            let new = !s.tgs.contains_key(&c.talkgroup);
            let g = s.tgs.entry(c.talkgroup).or_insert_with(|| Tg { first: t, ..Default::default() });
            g.last = t;
            firsts.tg = new && base;
        }
        for src in &c.sources {
            if src.src != 0 && s.unit(src.src, t).1 && base {
                firsts.units.push(src.src);
            }
        }
        firsts
    }

    /// A call ended at `t` (Unix s): its airtime, its radios' transmissions
    /// and who they talked with. Returns radios heard for the first time.
    pub fn call_end(&mut self, system: &str, c: &Call, t: i64) -> Vec<u32> {
        let s = self.sys(system, t);
        let base = s.has_baseline(t);
        let secs = (c.last_update_s - c.start_s).max(0.0) as f32;
        let start = t - secs.round() as i64;
        let h = t.div_euclid(3600);
        if !c.unit_to_unit {
            let g = s.tgs.entry(c.talkgroup).or_insert_with(|| Tg { first: start, ..Default::default() });
            g.last = t;
            g.calls += 1;
            g.secs += secs;
            g.enc += c.encrypted as u32;
            bump_hour(&mut g.hours, h, |v: &mut (u32, f32)| {
                v.0 += 1;
                v.1 += secs;
            }, || (0, 0.0));
            s.lengths.push_back((t, c.talkgroup, secs));
            while s.lengths.len() > MAX_LENGTHS {
                s.lengths.pop_front();
            }
        }
        let mut firsts = Vec::new();
        // Each talker's turn: from when it was first heard to the next's (or the end).
        let srcs: Vec<_> = c.sources.iter().filter(|x| x.src != 0).collect();
        for (i, x) in srcs.iter().enumerate() {
            let until = srcs.get(i + 1).map_or(c.last_update_s, |n| n.time_s);
            let turn = (until - x.time_s).max(0.0) as f32;
            let at = start + (x.time_s - c.start_s).max(0.0).round() as i64;
            let (u, new) = s.unit(x.src, at);
            if new && base {
                firsts.push(x.src);
            }
            u.last_tx = u.last_tx.max(at);
            u.tx += 1;
            u.tx_secs += turn;
            match u.tgs.iter_mut().find(|(g, _)| *g == c.talkgroup) {
                Some((_, n)) => *n += 1,
                None => u.tgs.push((c.talkgroup, 1)),
            }
            u.tgs.sort_by_key(|x| std::cmp::Reverse(x.1));
            u.tgs.truncate(TOP_TGS);
            u.recent.push_back((at, c.talkgroup, turn));
            while u.recent.len() > MAX_RECENT_TX {
                u.recent.pop_front();
            }
        }
        for w in srcs.windows(2) {
            s.pair(w[0].src, w[1].src, t);
        }
        if c.unit_to_unit && srcs.len() == 1 {
            // A private call: the talkgroup field is the radio called.
            s.pair(srcs[0].src, c.talkgroup, t);
        }
        s.evict();
        firsts
    }

    /// A control channel message about a radio: affiliation, registration,
    /// location, grants. Returns the talkgroup on an affiliation.
    pub fn message(&mut self, system: &str, m: &Message, t: i64) -> Option<(u32, u32)> {
        if m.source <= 0 {
            return None;
        }
        let unit = m.source as u32;
        match m.kind {
            MessageType::Affiliation | MessageType::Location => {
                let s = self.sys(system, t);
                let (u, _) = s.unit(unit, t);
                let changed = u.aff != Some(m.talkgroup);
                u.aff = Some(m.talkgroup);
                u.aff_t = t;
                if changed {
                    s.affiliations.push_back((t, unit, m.talkgroup));
                    while s.affiliations.len() > MAX_AFFILIATIONS {
                        s.affiliations.pop_front();
                    }
                }
                changed.then_some((unit, m.talkgroup))
            }
            MessageType::Registration | MessageType::Deregistration => {
                let s = self.sys(system, t);
                let (u, _) = s.unit(unit, t);
                u.reg = Some(m.kind == MessageType::Registration);
                if m.kind == MessageType::Deregistration {
                    u.aff = None;
                }
                None
            }
            MessageType::UuVGrant | MessageType::UuAnsReq => {
                let s = self.sys(system, t);
                s.unit(unit, t);
                if m.talkgroup != 0 {
                    s.unit(m.talkgroup, t);
                }
                None
            }
            MessageType::Grant | MessageType::Update | MessageType::Acknowledge | MessageType::DataGrant | MessageType::CallAlert => {
                // Only when it's already known: grants repeat many times a call.
                if let Some(s) = self.systems.get_mut(system) {
                    if let Some(u) = s.units.get_mut(&unit) {
                        u.last = u.last.max(t);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// A recorded call's file (Trunk Recorder's call JSON): how its frequency decoded.
    pub fn concluded(&mut self, system: &str, record: &Value, t: i64) {
        let Some(freq) = record["freq"].as_u64() else { return };
        let (mut frames, mut errors, mut bad) = (0u64, 0u64, 0u64);
        for i in record["errorList"].as_array().into_iter().flatten() {
            frames += i["frames"].as_u64().unwrap_or(0);
            errors += i["error_count"].as_u64().unwrap_or(0);
            bad += i["bad_frames"].as_u64().unwrap_or(0);
        }
        let s = self.sys(system, t);
        let f = s.freqs.entry(freq).or_default();
        let ewma = |old: Option<f32>, new: Option<f64>| match (old, new) {
            (o, None) => o,
            (None, Some(n)) => Some(n as f32),
            (Some(o), Some(n)) => Some(o + EWMA * (n as f32 - o)),
        };
        f.calls += 1;
        f.frames += frames;
        f.errors += errors;
        f.bad += bad;
        f.snr = ewma(f.snr, record["snr"].as_f64());
        f.ferr = ewma(f.ferr, record["freq_error"].as_f64());
        f.clean = ewma(f.clean, record["clean_voice_pct"].as_f64());
        f.last = t;
        bump_hour(&mut f.hours, t.div_euclid(3600), |v: &mut (u32, u64, u64, u64)| {
            v.0 += 1;
            v.1 += frames;
            v.2 += errors;
            v.3 += bad;
        }, || (0, 0, 0, 0));
        s.evict();
    }

    /// Systems changed since the last call, as (short name, JSON) to save.
    pub fn take_dirty(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.dirty)
            .into_iter()
            .filter_map(|n| self.systems.get(&n).and_then(|s| serde_json::to_string(s).ok()).map(|j| (n, j)))
            .collect()
    }

    pub fn has_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Load a system saved by [`Registry::take_dirty`] (a bad file is ignored).
    pub fn load(&mut self, system: &str, json: &str) {
        if let Ok(s) = serde_json::from_str::<SystemRadio>(json) {
            self.systems.insert(system.to_string(), s);
        }
    }

    /// `radioQuery {what, system, key?, hours?, limit?}` at `now` (Unix s):
    /// the reply's body (the caller adds `type` and `id`).
    pub fn query(&self, q: &Value, now: i64, names: &Names) -> Value {
        let what = q["what"].as_str().unwrap_or("summary");
        if what == "summary" {
            let systems: serde_json::Map<String, Value> = self.systems.iter().map(|(n, s)| (n.clone(), summary(n, s, now, names))).collect();
            return json!({ "what": what, "systems": systems });
        }
        let Some(name) = q["system"].as_str() else { return json!({ "what": what, "error": "which system?" }) };
        let Some(s) = self.systems.get(name) else { return json!({ "what": what, "system": name, "rows": [] }) };
        let hours = q["hours"].as_i64().unwrap_or(24).clamp(1, HOURS);
        let limit = q["limit"].as_u64().unwrap_or(500).min(5000) as usize;
        let key = q["key"].as_u64().map(|k| k as u32);
        let mut out = match what {
            "talkgroups" => talkgroups(name, s, now, hours, limit, names),
            "units" => units(name, s, now, hours, limit, names),
            "tg" => match key {
                Some(k) => tg_detail(name, s, k, now, hours, names),
                None => json!({ "error": "which talkgroup?" }),
            },
            "unit" => match key {
                Some(k) => unit_detail(name, s, k, now, names),
                None => json!({ "error": "which radio?" }),
            },
            "freqs" => freqs(s, now, hours),
            "lengths" => json!({ "histogram": histogram(s.lengths.iter().filter(|l| l.0 > now - hours * 3600).map(|l| l.2)) }),
            _ => json!({ "error": format!("unknown query {what}") }),
        };
        out["what"] = json!(what);
        out["system"] = json!(name);
        out
    }
}

/// Call-length histogram: bins from 1 s, doubling (the last open-ended).
pub const LENGTH_BINS: [f32; 9] = [0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0];

fn histogram(lengths: impl Iterator<Item = f32>) -> Value {
    let mut h = [0u32; LENGTH_BINS.len()];
    for l in lengths {
        let i = LENGTH_BINS.iter().rposition(|&b| l >= b).unwrap_or(0);
        h[i] += 1;
    }
    json!({ "edges": LENGTH_BINS, "counts": h })
}

fn is_new(s: &SystemRadio, first: i64, now: i64) -> bool {
    first > now - NEW_S && first - s.created >= BASELINE_S
}

/// A talkgroup's per-hour activity over the last `hours`, oldest first: (calls, seconds).
fn hourly(g: &Tg, now: i64, hours: i64) -> (Vec<u32>, Vec<f32>) {
    let h1 = now.div_euclid(3600);
    let h0 = h1 - hours + 1;
    let (mut c, mut sec) = (vec![0u32; hours as usize], vec![0f32; hours as usize]);
    for &(h, (n, x)) in &g.hours {
        if h >= h0 && h <= h1 {
            c[(h - h0) as usize] = n;
            sec[(h - h0) as usize] = (x * 10.0).round() / 10.0;
        }
    }
    (c, sec)
}

fn tg_row(name: &str, s: &SystemRadio, tg: u32, g: &Tg, now: i64, hours: i64, names: &Names) -> Value {
    let info = (names.tg)(name, tg);
    let (c, sec) = hourly(g, now, hours);
    json!({
        "tg": tg,
        "alphaTag": info.as_ref().map_or("", |i| i.alpha_tag.as_str()),
        "known": info.is_some(),
        "ignore": info.as_ref().is_some_and(|i| i.ignore),
        "first": g.first, "last": g.last,
        "calls": c.iter().sum::<u32>(), "secs": (sec.iter().sum::<f32>() * 10.0).round() / 10.0,
        "totalCalls": g.calls,
        "encPct": if g.calls > 0 { (g.enc as f64 * 1000.0 / g.calls as f64).round() / 10.0 } else { 0.0 },
        "new": is_new(s, g.first, now),
        "hourly": c,
    })
}

fn talkgroups(name: &str, s: &SystemRadio, now: i64, hours: i64, limit: usize, names: &Names) -> Value {
    let since = now - hours * 3600;
    let mut rows: Vec<(u32, &Tg)> = s.tgs.iter().filter(|(_, g)| g.last >= since).map(|(k, g)| (*k, g)).collect();
    // Busiest first (airtime in the range), then most recent.
    let air = |g: &Tg| g.hours.iter().filter(|h| h.0 >= since.div_euclid(3600)).map(|h| (h.1).1).sum::<f32>();
    rows.sort_by(|a, b| air(b.1).total_cmp(&air(a.1)).then(b.1.last.cmp(&a.1.last)));
    let total = rows.len();
    let rows: Vec<Value> = rows.into_iter().take(limit).map(|(k, g)| tg_row(name, s, k, g, now, hours, names)).collect();
    json!({ "hours": hours, "total": total, "rows": rows })
}

fn partners(s: &SystemRadio, unit: u32, n: usize) -> Vec<(u32, u32)> {
    let mut p: Vec<(u32, u32)> = s
        .pairs
        .iter()
        .filter_map(|(k, v)| {
            let (a, b) = ((k >> 32) as u32, *k as u32);
            if a == unit {
                Some((b, v.n))
            } else if b == unit {
                Some((a, v.n))
            } else {
                None
            }
        })
        .collect();
    p.sort_by_key(|x| std::cmp::Reverse(x.1));
    p.truncate(n);
    p
}

fn unit_row(name: &str, s: &SystemRadio, id: u32, u: &Unit, now: i64, names: &Names, partners_n: usize) -> Value {
    json!({
        "unit": id,
        "alias": (names.unit)(name, id).unwrap_or_default(),
        "first": u.first, "last": u.last, "lastTx": if u.last_tx > 0 { Some(u.last_tx) } else { None },
        "tx": u.tx, "txSecs": (u.tx_secs * 10.0).round() / 10.0,
        "aff": u.aff, "affT": if u.aff.is_some() { Some(u.aff_t) } else { None }, "reg": u.reg,
        "tgs": u.tgs,
        "partners": if partners_n > 0 { partners(s, id, partners_n) } else { Vec::new() },
        "new": is_new(s, u.first, now),
    })
}

fn units(name: &str, s: &SystemRadio, now: i64, hours: i64, limit: usize, names: &Names) -> Value {
    let since = now - hours * 3600;
    let mut rows: Vec<(u32, &Unit)> = s.units.iter().filter(|(_, u)| u.last >= since).map(|(k, u)| (*k, u)).collect();
    rows.sort_by(|a, b| b.1.last_tx.cmp(&a.1.last_tx).then(b.1.last.cmp(&a.1.last)));
    let total = rows.len();
    // Partners only for the first rows (a scan of the pairs each).
    let rows: Vec<Value> = rows.into_iter().take(limit).enumerate().map(|(i, (k, u))| unit_row(name, s, k, u, now, names, if i < 100 { 3 } else { 0 })).collect();
    json!({ "hours": hours, "total": total, "rows": rows })
}

fn tg_detail(name: &str, s: &SystemRadio, tg: u32, now: i64, hours: i64, names: &Names) -> Value {
    let Some(g) = s.tgs.get(&tg) else { return json!({ "tg": tg, "row": null }) };
    // Who talks on it, and who is affiliated with it now.
    let mut talkers: Vec<(u32, u32, i64)> = s.units.iter().filter_map(|(k, u)| u.tgs.iter().find(|x| x.0 == tg).map(|x| (*k, x.1, u.last_tx))).collect();
    talkers.sort_by_key(|x| std::cmp::Reverse(x.1));
    talkers.truncate(60);
    let affiliated: Vec<u32> = s.units.iter().filter(|(_, u)| u.aff == Some(tg)).map(|(k, _)| *k).take(200).collect();
    let alias = |u: u32| (names.unit)(name, u).unwrap_or_default();
    json!({
        "tg": tg,
        "row": tg_row(name, s, tg, g, now, hours, names),
        "week": hourly(g, now, HOURS).0,
        "talkers": talkers.iter().map(|&(u, n, t)| json!({ "unit": u, "alias": alias(u), "n": n, "lastTx": t })).collect::<Vec<_>>(),
        "affiliated": affiliated.iter().map(|&u| json!({ "unit": u, "alias": alias(u) })).collect::<Vec<_>>(),
        "lengths": histogram(s.lengths.iter().filter(|l| l.1 == tg && l.0 > now - hours * 3600).map(|l| l.2)),
    })
}

fn unit_detail(name: &str, s: &SystemRadio, id: u32, now: i64, names: &Names) -> Value {
    let Some(u) = s.units.get(&id) else { return json!({ "unit": id, "row": null }) };
    let tg_name = |tg: u32| (names.tg)(name, tg).map(|i| i.alpha_tag).unwrap_or_default();
    let partners = partners(s, id, 24);
    json!({
        "unit": id,
        "row": unit_row(name, s, id, u, now, names, 0),
        "recent": u.recent.iter().rev().map(|&(t, tg, secs)| json!({ "t": t, "tg": tg, "alphaTag": tg_name(tg), "secs": (secs * 10.0).round() / 10.0 })).collect::<Vec<_>>(),
        "affiliations": s.affiliations.iter().rev().filter(|a| a.1 == id).take(50).map(|&(t, _, tg)| json!({ "t": t, "tg": tg, "alphaTag": tg_name(tg) })).collect::<Vec<_>>(),
        "tgs": u.tgs.iter().map(|&(tg, n)| json!({ "tg": tg, "alphaTag": tg_name(tg), "n": n })).collect::<Vec<_>>(),
        "partners": partners.iter().map(|&(p, n)| json!({ "unit": p, "alias": (names.unit)(name, p).unwrap_or_default(), "n": n })).collect::<Vec<_>>(),
    })
}

fn freqs(s: &SystemRadio, now: i64, hours: i64) -> Value {
    let h1 = now.div_euclid(3600);
    let h0 = h1 - hours + 1;
    let mut rows: Vec<Value> = s
        .freqs
        .iter()
        .map(|(hz, f)| {
            let (mut c, mut fr, mut er, mut bd) = (0u32, 0u64, 0u64, 0u64);
            // Each hour's share of voice frames that couldn't be decoded, %.
            let mut spark = vec![Value::Null; hours as usize];
            for &(h, (n, frames, errors, bad)) in &f.hours {
                if h >= h0 && h <= h1 {
                    c += n;
                    fr += frames;
                    er += errors;
                    bd += bad;
                    if frames > 0 {
                        spark[(h - h0) as usize] = json!((bad as f64 * 1000.0 / frames as f64).round() / 10.0);
                    }
                }
            }
            json!({
                "freqHz": hz, "calls": c, "frames": fr, "errors": er,
                "errPerFrame": if fr > 0 { Some((er as f64 / fr as f64 * 1000.0).round() / 1000.0) } else { None },
                "totalCalls": f.calls,
                "allErrPerFrame": if f.frames > 0 { Some((f.errors as f64 / f.frames as f64 * 1000.0).round() / 1000.0) } else { None },
                "badPct": if fr > 0 { Some((bd as f64 * 1000.0 / fr as f64).round() / 10.0) } else { None },
                "allBadPct": if f.frames > 0 { Some((f.bad as f64 * 1000.0 / f.frames as f64).round() / 10.0) } else { None },
                "snr": f.snr.map(|v| (v * 10.0).round() / 10.0),
                "freqError": f.ferr.map(|v| v.round()),
                "clean": f.clean.map(|v| (v * 10.0).round() / 10.0),
                "last": f.last,
                "hourly": spark,
            })
        })
        .collect();
    rows.sort_by_key(|r| r["freqHz"].as_u64());
    json!({ "hours": hours, "rows": rows })
}

fn summary(name: &str, s: &SystemRadio, now: i64, names: &Names) -> Value {
    let day = now - NEW_S;
    let new_tgs: Vec<Value> = s
        .tgs
        .iter()
        .filter(|(_, g)| is_new(s, g.first, now))
        .map(|(k, g)| json!({ "tg": k, "first": g.first, "alphaTag": (names.tg)(name, *k).map(|i| i.alpha_tag).unwrap_or_default() }))
        .collect();
    let unknown = s.tgs.iter().filter(|(k, g)| g.last >= day && (names.tg)(name, **k).is_none()).count();
    json!({
        "since": s.created,
        "talkgroups": s.tgs.len(),
        "tgs24h": s.tgs.values().filter(|g| g.last >= day).count(),
        "units": s.units.len(),
        "units1h": s.units.values().filter(|u| u.last >= now - 3600).count(),
        "units24h": s.units.values().filter(|u| u.last >= day).count(),
        "newUnits24h": s.units.values().filter(|u| is_new(s, u.first, now)).count(),
        "newTgs": new_tgs,
        "unknownTgs24h": unknown,
        "baseline": s.has_baseline(now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_core::trunk::CallSource;

    fn call(tg: u32, units: &[(u32, f64)], start: f64, end: f64) -> Call {
        Call {
            talkgroup: tg,
            start_s: start,
            last_update_s: end,
            sources: units.iter().map(|&(src, t)| CallSource { src, time_s: t, emergency: false }).collect(),
            ..Default::default()
        }
    }

    #[allow(clippy::type_complexity)]
    fn names() -> (impl Fn(&str, u32) -> Option<TgInfo>, impl Fn(&str, u32) -> Option<String>) {
        (|_: &str, tg: u32| (tg == 100).then(|| TgInfo { alpha_tag: "Fire Dispatch".into(), ignore: false }), |_: &str, u: u32| (u == 7).then(|| "E7".to_string()))
    }

    #[test]
    fn calls_make_talkgroups_units_and_pairs() {
        let mut r = Registry::default();
        let t0 = 1_700_000_000;
        let c = call(100, &[(7, 0.0), (8, 4.0)], 0.0, 10.0);
        assert_eq!(r.call_start("dcfd", &c, t0), Firsts::default(), "no baseline yet: nothing is 'first'");
        r.call_end("dcfd", &c, t0 + 10);
        let s = &r.systems["dcfd"];
        assert_eq!(s.tgs[&100].calls, 1);
        assert_eq!(s.tgs[&100].secs, 10.0);
        assert_eq!(s.units[&7].tx, 1);
        assert_eq!(s.units[&7].tx_secs, 4.0);
        assert_eq!(s.units[&8].tx_secs, 6.0);
        assert_eq!(s.pairs[&pair_key(8, 7)].n, 1);
        // Two hours on, a new talkgroup is "first heard".
        let c2 = call(200, &[(9, 0.0)], 0.0, 3.0);
        let f = r.call_start("dcfd", &c2, t0 + 7200);
        assert!(f.tg);
        assert_eq!(f.units, vec![9]);
        r.call_end("dcfd", &c2, t0 + 7203);

        let (tg, unit) = names();
        let n = Names { tg: &tg, unit: &unit };
        let q = r.query(&json!({ "what": "talkgroups", "system": "dcfd", "hours": 24 }), t0 + 7300, &n);
        assert_eq!(q["total"], 2);
        assert_eq!(q["rows"][0]["tg"], 100, "busiest first");
        assert_eq!(q["rows"][0]["alphaTag"], "Fire Dispatch");
        assert_eq!(q["rows"][1]["known"], false);
        assert_eq!(q["rows"][1]["new"], true);
        let q = r.query(&json!({ "what": "unit", "system": "dcfd", "key": 7 }), t0 + 7300, &n);
        assert_eq!(q["row"]["alias"], "E7");
        assert_eq!(q["partners"][0]["unit"], 8);
        let q = r.query(&json!({ "what": "summary" }), t0 + 7300, &n);
        assert_eq!(q["systems"]["dcfd"]["unknownTgs24h"], 1);
        assert_eq!(q["systems"]["dcfd"]["newTgs"][0]["tg"], 200);
    }

    #[test]
    fn affiliations_and_saving() {
        let mut r = Registry::default();
        let m = Message { kind: MessageType::Affiliation, source: 42, talkgroup: 100, ..Default::default() };
        assert_eq!(r.message("x", &m, 10), Some((42, 100)));
        assert_eq!(r.message("x", &m, 11), None, "the same again isn't news");
        let rec = json!({ "freq": 851_000_000u64, "snr": 20.0, "freq_error": -30, "clean_voice_pct": 98.0, "errorList": [{ "frames": 100, "error_count": 5, "bad_frames": 1 }] });
        r.concluded("x", &rec, 20);
        let saved = r.take_dirty();
        assert_eq!(saved.len(), 1);
        assert!(r.take_dirty().is_empty());
        let mut back = Registry::default();
        back.load("x", &saved[0].1);
        assert_eq!(back.systems["x"].units[&42].aff, Some(100));
        assert_eq!(back.systems["x"].freqs[&851_000_000].errors, 5);
        let (tg, unit) = names();
        let q = back.query(&json!({ "what": "freqs", "system": "x" }), 30, &Names { tg: &tg, unit: &unit });
        assert_eq!(q["rows"][0]["errPerFrame"], 0.05);
    }

    #[test]
    fn tables_stay_bounded() {
        let mut m: HashMap<u32, i64> = (0..1200).map(|i| (i, i as i64)).collect();
        evict(&mut m, 1000, |v| *v);
        assert_eq!(m.len(), 1000);
        assert!(m.contains_key(&1199) && !m.contains_key(&10));
    }
}
