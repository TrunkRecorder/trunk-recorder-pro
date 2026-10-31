//! The radio system as heard: talkgroups (first and last heard, calls and
//! airtime by hour), radios (when they transmitted, what they're affiliated
//! with, which talkgroups they use, who they talk with), and each frequency:
//! how well its voice decodes, and the calls on it no source covered. Fed by the session's events ([`Registry`]'s
//! `call_*` / `message` / `concluded`); kept per system across runs (saved as
//! JSON); asked by the dashboard ([`Registry::query`]).
//!
//! Every table is bounded: past its cap the least recently heard go.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use trunk_core::p25::crypt::algid_name;
use trunk_core::dsp::inband::signal::Heard;
use trunk_core::trunk::calls::Reason;
use trunk_core::trunk::{Call, Message, MessageType};

/// Caps per system: the least talkgroups kept ([`Caps`] sets the rest), and frequencies and logs.
const MAX_TGS: usize = 5000;
const MAX_FREQS: usize = 512;
pub const MAX_AFFILIATIONS: usize = 2000;
pub const MAX_LENGTHS: usize = 5000;
pub const MAX_TONES: usize = 5000;
const MAX_RECENT_TX: usize = 30;
const TOP_TGS: usize = 8;
/// Ciphers (ALGID, key ID) kept per talkgroup.
const MAX_CIPHERS: usize = 16;

/// How much of what a system's radio registry heard it keeps
/// (`storage.registryRadios` and `storage.registryDays`: [`Caps::of`]):
/// past a cap, the least recently heard talkgroups, radios and pairs go,
/// and those not heard for `days` go too. Talkgroups are a quarter of the
/// radios (5,000 at least) and pairs two and a half times them, as the
/// defaults are. What goes is told to the store as it goes (gone rows in
/// [`RegistryChanges`]), as anything else that changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub tgs: usize,
    pub units: usize,
    pub pairs: usize,
    /// Rows not heard for this long go (none: kept however old).
    pub days: Option<u32>,
}

impl Default for Caps {
    fn default() -> Self {
        Caps::of(&crate::config::StorageSettings::default())
    }
}

impl Caps {
    /// The caps `storage` sets: `registryRadios` radios a system (0: no
    /// cap, the days alone bounding it), `registryDays` (0: none).
    pub fn of(storage: &crate::config::StorageSettings) -> Caps {
        let days = (storage.registry_days > 0).then_some(storage.registry_days);
        match storage.registry_radios as usize {
            0 => Caps { tgs: usize::MAX, units: usize::MAX, pairs: usize::MAX, days },
            units => Caps { tgs: (units / 4).max(MAX_TGS), units, pairs: units.saturating_mul(5) / 2, days },
        }
    }
}

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
    /// The alpha tag its last named call carried: what it's called when
    /// neither a row nor the talkgroup file names its number (a conventional
    /// row that files under the air's talkgroup names every one it hears).
    pub name: String,
    pub calls: u32,
    pub secs: f32,
    /// Calls that were encrypted.
    pub enc: u32,
    /// The ciphers its calls' encryption sync named: (ALGID, key ID, calls,
    /// last heard), most recent first.
    pub ciphers: Vec<(u8, u16, u32, i64)>,
    /// (hour = Unix s / 3600, calls, seconds), oldest first, last week only.
    pub hours: VecDeque<(i64, (u32, f32))>,
}

impl Tg {
    /// A call of it began or ended: keep the name it carried, if any.
    fn note_name(&mut self, c: &Call) {
        if let Some(t) = c.talkgroup_info.as_ref().filter(|t| !t.alpha_tag.is_empty()) {
            if t.alpha_tag != self.name {
                self.name = t.alpha_tag.clone();
            }
        }
    }
}

/// What talkgroup `tg` of system `name` is called: by a conventional row or
/// the talkgroup file ([`Names::tg`]), else as its last named call was.
fn tg_alpha(name: &str, s: &SystemRadio, tg: u32, names: &Names) -> String {
    (names.tg)(name, tg)
        .map(|i| i.alpha_tag)
        .filter(|a| !a.is_empty())
        .or_else(|| s.tgs.get(&tg).map(|g| g.name.clone()))
        .unwrap_or_default()
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
    /// Calls granted on it that no source covered (out of band): how many,
    /// the last (Unix s), and by hour, last week.
    pub out_of_band: u32,
    pub out_of_band_last: i64,
    pub out_of_band_hours: VecDeque<(i64, u32)>,
}

impl Freq {
    /// When it was last heard: a call decoded, or one out of band.
    fn heard(&self) -> i64 {
        self.last.max(self.out_of_band_last)
    }
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
    /// The tone patterns its calls carried (pages, alerts, digits), newest last.
    pub tones: VecDeque<PatternSeen>,
    /// What changed since [`Registry::take_changes`] last took it (not saved).
    #[serde(skip)]
    changed: Changed,
    /// The tables' caps (the registry's, [`Registry::set_caps`]; not saved).
    #[serde(skip)]
    pub caps: Caps,
}

/// What changed in a system's registry, by row: which talkgroups, radios,
/// pairs and frequencies changed or went, and how many of the latest
/// affiliations, call lengths and tone patterns are new: what a store keeps
/// (cheap: a key for each row that changed).
#[derive(Clone, Debug, Default)]
struct Changed {
    tgs: BTreeSet<u32>,
    units: BTreeSet<u32>,
    pairs: BTreeSet<u64>,
    freqs: BTreeSet<u64>,
    gone: Gone,
    affiliations: usize,
    lengths: usize,
    tones: usize,
}

impl Changed {
    fn tg(&mut self, k: u32) {
        self.tgs.insert(k);
    }
    fn unit(&mut self, k: u32) {
        self.units.insert(k);
    }
    fn pair(&mut self, k: u64) {
        self.pairs.insert(k);
    }
    fn freq(&mut self, k: u64) {
        self.freqs.insert(k);
    }
}

/// Rows that went (past a cap, the least recently heard).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gone {
    pub tgs: Vec<u32>,
    pub units: Vec<u32>,
    pub pairs: Vec<u64>,
    pub freqs: Vec<u64>,
}

/// A system's registry rows that changed since the last time they were
/// taken ([`Registry::take_changes`]), and those that went: what a store
/// writes to keep it a row at a time (rows of a database, lines appended to
/// a log), not all of it. [`SystemRadio::apply`] makes them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RegistryChanges {
    /// When the system was first heard (Unix s).
    pub created: i64,
    pub tgs: Vec<(u32, Tg)>,
    pub units: Vec<(u32, Unit)>,
    pub pairs: Vec<(u64, Pair)>,
    pub freqs: Vec<(u64, Freq)>,
    pub gone: Gone,
    /// The newest of each, appended since.
    pub affiliations: Vec<(i64, u32, u32)>,
    pub lengths: Vec<(i64, u32, f32)>,
    pub tones: Vec<PatternSeen>,
}

/// A tone pattern heard on a call: when, on which talkgroup, and the pattern
/// as the call JSON and plugins have it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PatternSeen {
    /// Unix s.
    pub t: i64,
    pub tg: u32,
    #[serde(flatten)]
    pub pattern: trunk_recorder_plugin::TonePattern,
}

impl PatternSeen {
    fn frame(&self) -> bool {
        self.pattern.origin == "frame"
    }
    fn tones(&self) -> Vec<f32> {
        self.pattern.tones.iter().map(|&f| f as f32).collect()
    }
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
    /// The system's signals a pattern heard on a talkgroup is (their ids).
    pub signals: &'a dyn Fn(&str, &Heard, u32) -> Vec<String>,
}

#[derive(Default)]
pub struct Registry {
    pub systems: BTreeMap<String, SystemRadio>,
    /// Every system's caps ([`Registry::set_caps`]).
    caps: Caps,
}

fn pair_key(a: u32, b: u32) -> u64 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    (lo as u64) << 32 | hi as u64
}

fn bump_hour<T>(hours: &mut VecDeque<(i64, T)>, h: i64, f: impl FnOnce(&mut T), new: impl FnOnce() -> T) {
    // (Usually the last hour; a long call's air reaches back into earlier ones.)
    let at = hours.iter().rposition(|(hh, _)| *hh <= h);
    match at {
        Some(i) if hours[i].0 == h => f(&mut hours[i].1),
        _ => {
            let mut v = new();
            f(&mut v);
            hours.insert(at.map_or(0, |i| i + 1), (h, v));
        }
    }
    let last = hours.back().map_or(h, |(hh, _)| *hh);
    while hours.front().is_some_and(|(hh, _)| *hh <= last - HOURS) {
        hours.pop_front();
    }
}

/// Keep the `cap` most recently heard of `m` (when it's grown 10% past it); those that went.
fn evict<K: Copy + Eq + std::hash::Hash, V>(m: &mut HashMap<K, V>, cap: usize, last: impl Fn(&V) -> i64) -> Vec<K> {
    if m.len() <= cap.saturating_add(cap / 10) {
        return Vec::new();
    }
    let mut ts: Vec<i64> = m.values().map(&last).collect();
    let cut = ts.len() - cap;
    let (_, t, _) = ts.select_nth_unstable(cut);
    let t = *t;
    let gone: Vec<K> = m.iter().filter(|(_, v)| last(v) < t).map(|(k, _)| *k).collect();
    m.retain(|_, v| last(v) >= t);
    gone
}

/// Keys that went: no longer changed, gone instead.
fn went<K: Ord + Copy>(gone: Vec<K>, changed: &mut BTreeSet<K>, list: &mut Vec<K>) {
    for k in gone {
        changed.remove(&k);
        list.push(k);
    }
}

/// `v` with `new` appended, the oldest dropped past `cap`.
fn append<T: Clone>(v: &mut VecDeque<T>, new: &[T], cap: usize) {
    v.extend(new.iter().cloned());
    while v.len() > cap {
        v.pop_front();
    }
}

/// The newest `n` of `v` (those appended since), oldest first.
fn newest<T: Clone>(v: &VecDeque<T>, n: usize) -> Vec<T> {
    v.iter().skip(v.len().saturating_sub(n)).cloned().collect()
}

impl SystemRadio {
    fn new(t: i64) -> Self {
        SystemRadio { created: t, ..Default::default() }
    }

    fn has_baseline(&self, t: i64) -> bool {
        t - self.created >= BASELINE_S
    }

    fn unit(&mut self, unit: u32, t: i64) -> (&mut Unit, bool) {
        self.changed.unit(unit);
        let new = !self.units.contains_key(&unit);
        let u = self.units.entry(unit).or_insert_with(|| Unit { first: t, ..Default::default() });
        u.last = u.last.max(t);
        (u, new)
    }

    fn pair(&mut self, a: u32, b: u32, t: i64) {
        if a == b || a == 0 || b == 0 {
            return;
        }
        self.changed.pair(pair_key(a, b));
        let p = self.pairs.entry(pair_key(a, b)).or_default();
        p.n += 1;
        p.last = t;
    }

    fn evict(&mut self) {
        let c = &mut self.changed;
        let (tgs, units, pairs, freqs) = (evict(&mut self.tgs, self.caps.tgs, |v| v.last), evict(&mut self.units, self.caps.units, |v| v.last), evict(&mut self.pairs, self.caps.pairs, |v| v.last), evict(&mut self.freqs, MAX_FREQS, Freq::heard));
        went(tgs, &mut c.tgs, &mut c.gone.tgs);
        went(units, &mut c.units, &mut c.gone.units);
        went(pairs, &mut c.pairs, &mut c.gone.pairs);
        went(freqs, &mut c.freqs, &mut c.gone.freqs);
    }

    /// Let go of the rows not heard since `caps.days` before `now` (Unix
    /// s), as gone: what a store is told, as when a cap evicts them.
    fn expire(&mut self, now: i64) {
        let Some(days) = self.caps.days else { return };
        let before = now - i64::from(days) * 86_400;
        fn old<K: Copy, V>(m: &mut HashMap<K, V>, before: i64, last: impl Fn(&V) -> i64) -> Vec<K> {
            let gone: Vec<K> = m.iter().filter(|(_, v)| last(v) < before).map(|(k, _)| *k).collect();
            m.retain(|_, v| last(v) >= before);
            gone
        }
        let c = &mut self.changed;
        went(old(&mut self.tgs, before, |v| v.last), &mut c.tgs, &mut c.gone.tgs);
        went(old(&mut self.units, before, |v| v.last), &mut c.units, &mut c.gone.units);
        went(old(&mut self.pairs, before, |v| v.last), &mut c.pairs, &mut c.gone.pairs);
        went(old(&mut self.freqs, before, Freq::heard), &mut c.freqs, &mut c.gone.freqs);
    }

    /// Every row, as changes: the whole system, as it's stored (the one
    /// shape a registry is kept in: whole, or what changed).
    pub fn whole(&self) -> RegistryChanges {
        RegistryChanges {
            created: self.created,
            tgs: self.tgs.iter().map(|(k, v)| (*k, v.clone())).collect(),
            units: self.units.iter().map(|(k, v)| (*k, v.clone())).collect(),
            pairs: self.pairs.iter().map(|(k, v)| (*k, *v)).collect(),
            freqs: self.freqs.iter().map(|(k, v)| (*k, v.clone())).collect(),
            gone: Gone::default(),
            affiliations: self.affiliations.iter().copied().collect(),
            lengths: self.lengths.iter().copied().collect(),
            tones: self.tones.iter().cloned().collect(),
        }
    }

    /// The system the stored rows `c` make (what a store loads).
    pub fn from_changes(c: &RegistryChanges) -> SystemRadio {
        let mut s = SystemRadio::new(c.created);
        s.apply(c);
        s
    }

    /// What changed since the last call, as rows (none: nothing did).
    fn take_changes(&mut self) -> Option<RegistryChanges> {
        let c = std::mem::take(&mut self.changed);
        let nothing = c.tgs.is_empty() && c.units.is_empty() && c.pairs.is_empty() && c.freqs.is_empty() && c.gone == Gone::default() && c.affiliations + c.lengths + c.tones == 0;
        if nothing {
            return None;
        }
        Some(RegistryChanges {
            created: self.created,
            tgs: c.tgs.iter().filter_map(|k| self.tgs.get(k).map(|v| (*k, v.clone()))).collect(),
            units: c.units.iter().filter_map(|k| self.units.get(k).map(|v| (*k, v.clone()))).collect(),
            pairs: c.pairs.iter().filter_map(|k| self.pairs.get(k).map(|v| (*k, *v))).collect(),
            freqs: c.freqs.iter().filter_map(|k| self.freqs.get(k).map(|v| (*k, v.clone()))).collect(),
            gone: c.gone,
            affiliations: newest(&self.affiliations, c.affiliations),
            lengths: newest(&self.lengths, c.lengths),
            tones: newest(&self.tones, c.tones),
        })
    }

    /// Make `c` (what [`Registry::take_changes`] gave) here: this system as
    /// it was then, from as it was when the changes before were taken.
    pub fn apply(&mut self, c: &RegistryChanges) {
        // (What went first: a row that went and was heard again since is among the changed.)
        for k in &c.gone.tgs {
            self.tgs.remove(k);
        }
        for k in &c.gone.units {
            self.units.remove(k);
        }
        for k in &c.gone.pairs {
            self.pairs.remove(k);
        }
        for k in &c.gone.freqs {
            self.freqs.remove(k);
        }
        self.tgs.extend(c.tgs.iter().cloned());
        self.units.extend(c.units.iter().cloned());
        self.pairs.extend(c.pairs.iter().cloned());
        self.freqs.extend(c.freqs.iter().cloned());
        append(&mut self.affiliations, &c.affiliations, MAX_AFFILIATIONS);
        append(&mut self.lengths, &c.lengths, MAX_LENGTHS);
        append(&mut self.tones, &c.tones, MAX_TONES);
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
        let caps = self.caps;
        self.systems.entry(name.to_string()).or_insert_with(|| SystemRadio { caps, ..SystemRadio::new(t) })
    }

    /// Put system `name` as a store gave it.
    pub fn put(&mut self, name: &str, mut s: SystemRadio) {
        s.caps = self.caps;
        self.systems.insert(name.to_string(), s);
    }

    /// Size every system's tables (now and as systems come); past a cap
    /// the least recently heard go on the next change.
    pub fn set_caps(&mut self, caps: Caps) {
        self.caps = caps;
        for s in self.systems.values_mut() {
            s.caps = caps;
        }
    }

    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// How full it is: each system's talkgroups, radios and pairs against
    /// the caps (none: no cap), for the dashboard (Platform → Storage).
    pub fn fill(&self) -> Value {
        let cap = |n: usize| (n != usize::MAX).then_some(n);
        let systems: Vec<Value> = self.systems.iter().map(|(n, s)| json!({ "system": n, "talkgroups": s.tgs.len(), "radios": s.units.len(), "pairs": s.pairs.len() })).collect();
        json!({ "caps": { "talkgroups": cap(self.caps.tgs), "radios": cap(self.caps.units), "pairs": cap(self.caps.pairs), "days": self.caps.days }, "systems": systems })
    }

    /// A call began (`t`: Unix s). Returns what was heard for the first time
    /// (once the system has a baseline).
    pub fn call_start(&mut self, system: &str, c: &Call, t: i64) -> Firsts {
        let s = self.sys(system, t);
        let base = s.has_baseline(t);
        let mut firsts = Firsts::default();
        if !c.unit_to_unit {
            s.changed.tg(c.talkgroup);
            let new = !s.tgs.contains_key(&c.talkgroup);
            let g = s.tgs.entry(c.talkgroup).or_insert_with(|| Tg { first: t, ..Default::default() });
            g.last = t;
            g.note_name(c);
            firsts.tg = new && base;
        }
        if c.reason == Some(Reason::NoSource) {
            s.changed.freq(c.freq_hz);
            let f = s.freqs.entry(c.freq_hz).or_default();
            f.out_of_band += 1;
            f.out_of_band_last = t;
            bump_hour(&mut f.out_of_band_hours, t.div_euclid(3600), |n: &mut u32| *n += 1, || 0);
            s.evict();
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
            s.changed.tg(c.talkgroup);
            let g = s.tgs.entry(c.talkgroup).or_insert_with(|| Tg { first: start, ..Default::default() });
            g.last = t;
            g.note_name(c);
            g.calls += 1;
            g.secs += secs;
            g.enc += c.encrypted as u32;
            if let Some(cp) = c.cipher {
                note_cipher(&mut g.ciphers, cp.algid, cp.keyid, t);
            }
            // Its air in the hours it was on (a channel held open past the hour isn't all the last one's); the call in the last.
            for hh in start.div_euclid(3600).max(h - HOURS + 1)..=h {
                let on = (t.min((hh + 1) * 3600) - start.max(hh * 3600)).max(0) as f32;
                let air = if t > start { secs * on / (t - start) as f32 } else { secs };
                bump_hour(&mut g.hours, hh, |v: &mut (u32, f32)| {
                    v.0 += (hh == h) as u32;
                    v.1 += air;
                }, || (0, 0.0));
            }
            append(&mut s.lengths, &[(t, c.talkgroup, secs)], MAX_LENGTHS);
            s.changed.lengths += 1;
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
                    append(&mut s.affiliations, &[(t, unit, m.talkgroup)], MAX_AFFILIATIONS);
                    s.changed.affiliations += 1;
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
            MessageType::UuVGrant | MessageType::UuAnsReq | MessageType::PhoneGrant => {
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
                        s.changed.unit(unit);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// A recorded call's file (Trunk Recorder's call JSON): how its frequency decoded.
    pub fn concluded(&mut self, system: &str, record: &trunk_recorder_plugin::CallRecord, t: i64) {
        let freq = record.freq;
        let (mut frames, mut errors, mut bad) = (0u64, 0u64, 0u64);
        for i in &record.error_list {
            frames += i.frames;
            errors += i.error_count;
            bad += i.bad_frames;
        }
        let s = self.sys(system, t);
        s.changed.freq(freq);
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
        f.snr = ewma(f.snr, record.snr);
        f.ferr = ewma(f.ferr, Some(record.freq_error as f64));
        f.clean = ewma(f.clean, record.clean_voice_pct);
        f.last = t;
        bump_hour(&mut f.hours, t.div_euclid(3600), |v: &mut (u32, u64, u64, u64)| {
            v.0 += 1;
            v.1 += frames;
            v.2 += errors;
            v.3 += bad;
        }, || (0, 0, 0, 0));
        s.evict();
    }

    /// A tone pattern heard on talkgroup `tg` of `system` at `t` (Unix s), as it was told.
    pub fn tone_pattern(&mut self, system: &str, tg: u32, pattern: &trunk_recorder_plugin::TonePattern, t: i64) {
        let s = self.sys(system, t);
        append(&mut s.tones, &[PatternSeen { t, tg, pattern: pattern.clone() }], MAX_TONES);
        s.changed.tones += 1;
    }

    /// Let go of what each system hasn't heard for its caps' days (`now`:
    /// Unix s): the next [`Registry::take_changes`] has them as gone.
    pub fn expire(&mut self, now: i64) {
        for s in self.systems.values_mut() {
            s.expire(now);
        }
    }

    /// Each system's rows that changed since the last call ([`RegistryChanges`]):
    /// what's kept (a store appends them, and writes a system whole now and
    /// then; the browser writes the systems they name whole).
    pub fn take_changes(&mut self) -> Vec<(String, RegistryChanges)> {
        self.systems.iter_mut().filter_map(|(n, s)| s.take_changes().map(|c| (n.clone(), c))).collect()
    }

    /// Load a system kept whole ([`SystemRadio::whole`], as JSON; a bad one is ignored).
    pub fn load(&mut self, system: &str, json: &str) {
        if let Ok(c) = serde_json::from_str::<RegistryChanges>(json) {
            self.put(system, SystemRadio::from_changes(&c));
        }
    }

    /// `radioQuery {what, system, key?, hours?, limit?}` at `now` (Unix s):
    /// the reply's body (the caller adds `type` and `id`).
    pub fn query(&self, q: &Value, now: i64, names: &Names) -> Value {
        let what = q["what"].as_str().unwrap_or("summary");
        if what == "summary" {
            let systems: serde_json::Map<String, Value> = self.systems.iter().map(|(n, s)| (n.clone(), summary(n, s, now, names))).collect();
            let cap = |n: usize| (n != usize::MAX).then_some(n);
            return json!({ "what": what, "systems": systems, "caps": { "talkgroups": cap(self.caps.tgs), "radios": cap(self.caps.units), "days": self.caps.days } });
        }
        let Some(name) = q["system"].as_str() else { return json!({ "what": what, "error": "which system?" }) };
        let Some(s) = self.systems.get(name) else { return json!({ "what": what, "system": name, "rows": [] }) };
        let hours = q["hours"].as_i64().unwrap_or(24).clamp(1, HOURS);
        // Up to the registry's own cap: a client may ask for every radio heard.
        let limit = q["limit"].as_u64().unwrap_or(500).min(self.caps.units as u64) as usize;
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
            "tones" => tones(name, s, now, hours, limit, names),
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

/// A call under (algid, keyid), at `t`: counted, moved to the front.
fn note_cipher(list: &mut Vec<(u8, u16, u32, i64)>, algid: u8, keyid: u16, t: i64) {
    let n = match list.iter().position(|x| x.0 == algid && x.1 == keyid) {
        Some(i) => list.remove(i).2,
        None => 0,
    };
    list.insert(0, (algid, keyid, n + 1, t));
    list.truncate(MAX_CIPHERS);
}

fn is_new(s: &SystemRadio, first: i64, now: i64) -> bool {
    first > now - NEW_S && first - s.created >= BASELINE_S
}

/// The hours of `per_hour` in the last `hours` to `now`, each with its place
/// among them (0: the oldest).
fn window<T>(per_hour: &VecDeque<(i64, T)>, now: i64, hours: i64) -> impl Iterator<Item = (usize, &T)> {
    let h1 = now.div_euclid(3600);
    let h0 = h1 - hours + 1;
    per_hour.iter().filter(move |(h, _)| *h >= h0 && *h <= h1).map(move |(h, v)| ((h - h0) as usize, v))
}

/// A talkgroup's per-hour activity over the last `hours`, oldest first: (calls, seconds).
fn hourly(g: &Tg, now: i64, hours: i64) -> (Vec<u32>, Vec<f32>) {
    let (mut c, mut sec) = (vec![0u32; hours as usize], vec![0f32; hours as usize]);
    for (i, &(n, x)) in window(&g.hours, now, hours) {
        c[i] = n;
        sec[i] = (x * 10.0).round() / 10.0;
    }
    (c, sec)
}

fn tg_row(name: &str, s: &SystemRadio, tg: u32, g: &Tg, now: i64, hours: i64, names: &Names) -> Value {
    let info = (names.tg)(name, tg);
    let (c, sec) = hourly(g, now, hours);
    json!({
        "tg": tg,
        "alphaTag": tg_alpha(name, s, tg, names),
        "known": info.is_some(),
        "ignore": info.as_ref().is_some_and(|i| i.ignore),
        "first": g.first, "last": g.last,
        "calls": c.iter().sum::<u32>(), "secs": (sec.iter().sum::<f32>() as f64 * 10.0).round() / 10.0,
        "totalCalls": g.calls,
        "encPct": if g.calls > 0 { (g.enc as f64 * 1000.0 / g.calls as f64).round() / 10.0 } else { 0.0 },
        "new": is_new(s, g.first, now),
        "ciphers": g.ciphers.iter().map(|&(a, k, n, t)| json!({ "algid": a, "keyId": k, "name": algid_name(a), "calls": n, "last": t })).collect::<Vec<_>>(),
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
        "tx": u.tx, "txSecs": (u.tx_secs as f64 * 10.0).round() / 10.0,
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
    let tg_name = |tg: u32| tg_alpha(name, s, tg, names);
    let partners = partners(s, id, 24);
    json!({
        "unit": id,
        "row": unit_row(name, s, id, u, now, names, 0),
        "recent": u.recent.iter().rev().map(|&(t, tg, secs)| json!({ "t": t, "tg": tg, "alphaTag": tg_name(tg), "secs": (secs as f64 * 10.0).round() / 10.0 })).collect::<Vec<_>>(),
        "affiliations": s.affiliations.iter().rev().filter(|a| a.1 == id).take(50).map(|&(t, _, tg)| json!({ "t": t, "tg": tg, "alphaTag": tg_name(tg) })).collect::<Vec<_>>(),
        "tgs": u.tgs.iter().map(|&(tg, n)| json!({ "tg": tg, "alphaTag": tg_name(tg), "n": n })).collect::<Vec<_>>(),
        "partners": partners.iter().map(|&(p, n)| json!({ "unit": p, "alias": (names.unit)(name, p).unwrap_or_default(), "n": n })).collect::<Vec<_>>(),
    })
}

/// Each frequency: how its voice decoded (`rows`: those with a call
/// recorded), and the calls on it out of band in the last `hours`
/// (`outOfBand`, the most first).
fn freqs(s: &SystemRadio, now: i64, hours: i64) -> Value {
    let mut rows: Vec<Value> = s
        .freqs
        .iter()
        .filter(|(_, f)| f.calls > 0)
        .map(|(hz, f)| {
            let (mut c, mut fr, mut er, mut bd) = (0u32, 0u64, 0u64, 0u64);
            // Each hour's share of voice frames that couldn't be decoded, %.
            let mut spark = vec![Value::Null; hours as usize];
            for (i, &(n, frames, errors, bad)) in window(&f.hours, now, hours) {
                c += n;
                fr += frames;
                er += errors;
                bd += bad;
                if frames > 0 {
                    spark[i] = json!((bad as f64 * 1000.0 / frames as f64).round() / 10.0);
                }
            }
            json!({
                "freqHz": hz, "calls": c, "frames": fr, "errors": er,
                "errPerFrame": if fr > 0 { Some((er as f64 / fr as f64 * 1000.0).round() / 1000.0) } else { None },
                "totalCalls": f.calls,
                "allErrPerFrame": if f.frames > 0 { Some((f.errors as f64 / f.frames as f64 * 1000.0).round() / 1000.0) } else { None },
                "badPct": if fr > 0 { Some((bd as f64 * 1000.0 / fr as f64).round() / 10.0) } else { None },
                "allBadPct": if f.frames > 0 { Some((f.bad as f64 * 1000.0 / f.frames as f64).round() / 10.0) } else { None },
                "snr": f.snr.map(|v| (v as f64 * 10.0).round() / 10.0),
                "freqError": f.ferr.map(|v| v.round()),
                "clean": f.clean.map(|v| (v as f64 * 10.0).round() / 10.0),
                "last": f.last,
                "hourly": spark,
            })
        })
        .collect();
    rows.sort_by_key(|r| r["freqHz"].as_u64());
    let mut out_of_band: Vec<(u32, Value)> = s
        .freqs
        .iter()
        .filter_map(|(hz, f)| {
            let mut spark = vec![0u32; hours as usize];
            for (i, &n) in window(&f.out_of_band_hours, now, hours) {
                spark[i] = n;
            }
            let n: u32 = spark.iter().sum();
            (n > 0).then(|| (n, json!({ "freqHz": hz, "calls": n, "totalCalls": f.out_of_band, "last": f.out_of_band_last, "hourly": spark })))
        })
        .collect();
    out_of_band.sort_by(|a, b| b.0.cmp(&a.0).then(a.1["freqHz"].as_u64().cmp(&b.1["freqHz"].as_u64())));
    json!({ "hours": hours, "rows": rows, "outOfBand": out_of_band.into_iter().map(|x| x.1).collect::<Vec<_>>() })
}

/// The tones log: the patterns heard over the last `hours`, alike ones
/// together (same talkgroup, pattern and digits, tones as close as a signal
/// matches them), most recently heard first, each named by the system's
/// signals as they are now.
fn tones(name: &str, s: &SystemRadio, now: i64, hours: i64, limit: usize, names: &Names) -> Value {
    use trunk_core::dsp::inband::pattern::{format_plan, PatternKind};
    use trunk_core::dsp::inband::tables::{identify_one, identify_pair, tolerance};
    let span = |t: &[f32]| (t.iter().copied().fold(f32::MAX, f32::min), t.iter().copied().fold(0.0, f32::max));
    let alike = |a: &PatternSeen, b: &PatternSeen| {
        let (pa, pb) = (&a.pattern, &b.pattern);
        let (ta, tb) = (a.tones(), b.tones());
        a.tg == b.tg
            && pa.pattern == pb.pattern
            && (pa.digits.as_str(), pa.knox) == (pb.digits.as_str(), pb.knox)
            && if pa.pattern == "sequence" {
                // A chirp or a selective call heard with a tone more or less at
                // either end (the vocoder drops some): the same sweep.
                let ((alo, ahi), (blo, bhi)) = (span(&ta), span(&tb));
                (alo - blo).abs() <= 32.0 && (ahi - bhi).abs() <= 32.0
            } else {
                ta.len() == tb.len() && ta.iter().zip(&tb).all(|(&x, &y)| (x - y).abs() <= tolerance(x, a.frame() && b.frame()))
            }
    };
    let mut groups: Vec<Vec<&PatternSeen>> = Vec::new();
    for h in s.tones.iter().rev().filter(|h| h.t > now - hours * 3600) {
        match groups.iter_mut().find(|g| alike(g[0], h)) {
            Some(g) => g.push(h),
            None => groups.push(vec![h]),
        }
    }
    let total = groups.len();
    let rows: Vec<Value> = groups
        .iter()
        .take(limit)
        .map(|g| {
            let (h, p) = (g[0], &g[0].pattern);
            let lengths: Vec<f64> = (0..p.lengths.len())
                .map(|k| {
                    let mut v: Vec<f64> = g.iter().filter_map(|x| x.pattern.lengths.get(k).copied()).collect();
                    v.sort_by(f64::total_cmp);
                    v[v.len() / 2]
                })
                .collect();
            let mut formats: BTreeMap<&str, u32> = BTreeMap::new();
            let mut radios: BTreeMap<u32, u32> = BTreeMap::new();
            for x in g {
                if !x.pattern.format.is_empty() {
                    *formats.entry(x.pattern.format.as_str()).or_default() += 1;
                }
                if x.pattern.src > 0 {
                    *radios.entry(x.pattern.src as u32).or_default() += 1;
                }
            }
            let mut radios: Vec<(u32, u32)> = radios.into_iter().collect();
            radios.sort_by_key(|r| std::cmp::Reverse(r.1));
            let format = formats.iter().max_by_key(|f| f.1).map_or("", |f| *f.0);
            // Which paging codes it may be, likeliest first; none: custom tones.
            // One tone is a code only held like a group call (8 s): a shorter
            // one is an alert tone, a beep speech misread or a tone in the background.
            let tones = h.tones();
            let frame = g.iter().all(|x| x.frame());
            let mut ids: Vec<String> = match (p.pattern.as_str(), tones.as_slice()) {
                ("two_tone", &[a, b]) => identify_pair(a, b, frame, format_plan(format)),
                ("long", &[f]) if lengths[0] >= 6.0 => identify_one(f, frame),
                _ => Vec::new(),
            };
            ids.truncate(4);
            let heard = PatternKind::from_name(&p.pattern).map(|kind| Heard { kind, hz: &tones, digits: &p.digits, knox: p.knox, frame });
            let info = (names.tg)(name, h.tg);
            json!({
                "tg": h.tg,
                "alphaTag": info.as_ref().map_or("", |i| i.alpha_tag.as_str()),
                "pattern": p.pattern,
                "tones": p.tones,
                "lengths": lengths,
                "digits": p.digits,
                "knox": p.knox,
                "cycles": p.cycles,
                "format": format,
                "text": p.text,
                "count": g.len(),
                "first": g.last().map_or(0, |x| x.t),
                "last": h.t,
                "radios": radios.iter().take(3).map(|&(u, n)| json!({ "unit": u, "name": (names.unit)(name, u), "n": n })).collect::<Vec<Value>>(),
                "frame": frame,
                "ids": ids,
                "signals": heard.map_or_else(Vec::new, |h| (names.signals)(name, &h, g[0].tg)),
            })
        })
        .collect();
    json!({ "hours": hours, "total": total, "rows": rows })
}

fn summary(name: &str, s: &SystemRadio, now: i64, names: &Names) -> Value {
    let day = now - NEW_S;
    let new_tgs: Vec<Value> = s
        .tgs
        .iter()
        .filter(|(_, g)| is_new(s, g.first, now))
        .map(|(k, g)| json!({ "tg": k, "first": g.first, "alphaTag": tg_alpha(name, s, *k, names) }))
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
    use trunk_core::trunk::{CallSource, CipherId};

    fn call(tg: u32, units: &[(u32, f64)], start: f64, end: f64) -> Call {
        Call {
            talkgroup: tg,
            start_s: start,
            last_update_s: end,
            sources: units.iter().map(|&(src, t)| CallSource { src, time_s: t, ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    #[allow(clippy::type_complexity)]
    fn no_signals(_: &str, _: &Heard, _: u32) -> Vec<String> {
        Vec::new()
    }

    fn names() -> (impl Fn(&str, u32) -> Option<TgInfo>, impl Fn(&str, u32) -> Option<String>) {
        (|_: &str, tg: u32| (tg == 100).then(|| TgInfo { alpha_tag: "Fire Dispatch".into(), ignore: false }), |_: &str, u: u32| (u == 7).then(|| "E7".to_string()))
    }

    #[test]
    fn a_long_calls_air_is_in_the_hours_it_was_on() {
        let mut r = Registry::default();
        let t0 = 1_700_000_000 / 3600 * 3600 + 1800;
        // 2½ hours held open, from half past: half an hour, an hour, an hour.
        r.call_end("x", &call(100, &[], 0.0, 9000.0), t0 + 9000);
        r.call_end("x", &call(100, &[], 0.0, 10.0), t0 + 9010);
        let (calls, secs) = hourly(&r.systems["x"].tgs[&100], t0 + 9010, 4);
        assert_eq!(calls, [0, 0, 0, 2], "a call counts in the hour it ended");
        assert_eq!(secs, [1800.0, 3600.0, 3600.0, 10.0]);
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
        let n = Names { tg: &tg, unit: &unit, signals: &no_signals };
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
    fn a_talkgroup_nothing_names_is_named_as_its_calls_were() {
        // A conventional catch-all that files under the air's talkgroup names every one it hears.
        let mut r = Registry::default();
        let t0 = 1_700_000_000;
        let mut c = call(7, &[], 0.0, 2.0);
        c.talkgroup_info = Some(trunk_core::trunk::Talkgroup { number: 7, alpha_tag: "Townville (other)".into(), ..Default::default() });
        r.call_start("conv", &c, t0);
        r.call_end("conv", &c, t0 + 2);
        // A later call that carried no name keeps it.
        c.talkgroup_info = None;
        r.call_end("conv", &c, t0 + 5);
        let (tg, unit) = names();
        let n = Names { tg: &tg, unit: &unit, signals: &no_signals };
        let q = r.query(&json!({ "what": "talkgroups", "system": "conv", "hours": 24 }), t0 + 10, &n);
        assert_eq!((q["rows"][0]["alphaTag"].as_str(), q["rows"][0]["known"].as_bool()), (Some("Townville (other)"), Some(false)));
    }

    #[test]
    fn affiliations_and_saving() {
        let mut r = Registry::default();
        let m = Message { kind: MessageType::Affiliation, source: 42, talkgroup: 100, ..Default::default() };
        assert_eq!(r.message("x", &m, 10), Some((42, 100)));
        assert_eq!(r.message("x", &m, 11), None, "the same again isn't news");
        let rec = trunk_recorder_plugin::CallRecord {
            freq: 851_000_000,
            snr: Some(20.0),
            freq_error: -30,
            clean_voice_pct: Some(98.0),
            error_list: vec![trunk_recorder_plugin::ErrorInterval { frames: 100, error_count: 5, bad_frames: 1, ..Default::default() }],
            ..Default::default()
        };
        r.concluded("x", &rec, 20);
        assert_eq!(r.take_changes().len(), 1);
        assert!(r.take_changes().is_empty());
        let mut back = Registry::default();
        back.load("x", &serde_json::to_string(&r.systems["x"].whole()).unwrap());
        assert_eq!(back.systems["x"].units[&42].aff, Some(100));
        assert_eq!(back.systems["x"].freqs[&851_000_000].errors, 5);
        let (tg, unit) = names();
        let q = back.query(&json!({ "what": "freqs", "system": "x" }), 30, &Names { tg: &tg, unit: &unit, signals: &no_signals });
        assert_eq!(q["rows"][0]["errPerFrame"], 0.05);
    }

    #[test]
    fn tones_log_groups_alike_pages() {
        use trunk_recorder_plugin::TonePattern;
        let page = |tones: [f64; 2], src| TonePattern { pattern: "two_tone".into(), tones: tones.to_vec(), lengths: vec![0.98, 2.98], format: "Quick Call II".into(), src, origin: "frame".into(), ..Default::default() };
        let mut r = Registry::default();
        r.tone_pattern("x", 100, &page([781.25, 875.0], 7), 1000);
        r.tone_pattern("x", 100, &page([781.25, 875.0], 7), 2000);
        r.tone_pattern("x", 100, &page([1593.75, 1468.75], 8), 3000);
        r.tone_pattern("x", 200, &page([781.25, 875.0], 9), 4000);
        let (tg, unit) = names();
        // Named by the system's signals as they are now.
        let signals = |_: &str, h: &Heard, tg: u32| if tg == 100 && h.hz.first().is_some_and(|&a| (a - 781.25).abs() < 1.0) { vec!["NORWALK_EMS".to_string()] } else { Vec::new() };
        let q = r.query(&json!({ "what": "tones", "system": "x", "hours": 24 }), 5000, &Names { tg: &tg, unit: &unit, signals: &signals });
        assert_eq!(q["total"], 3, "one page on two talkgroups is two rows");
        let rows = q["rows"].as_array().unwrap();
        assert_eq!((rows[0]["tg"].as_u64(), rows[0]["count"].as_u64()), (Some(200), Some(1)), "most recent first");
        let norwalk = &rows[2];
        assert_eq!((norwalk["count"].as_u64(), norwalk["first"].as_i64(), norwalk["last"].as_i64()), (Some(2), Some(1000), Some(2000)));
        assert_eq!(norwalk["alphaTag"], "Fire Dispatch");
        assert_eq!(norwalk["format"], "Quick Call II");
        assert_eq!(norwalk["radios"][0]["name"], "E7");
        assert_eq!(norwalk["ids"][0], "QC2 126/128 (268)");
        assert_eq!(norwalk["signals"][0], "NORWALK_EMS");
        assert_eq!(rows[1]["ids"][0], "QC2 173/170");
        assert_eq!(rows[0]["signals"].as_array().map(Vec::len), Some(0), "another talkgroup");
    }

    #[test]
    fn frequencies_no_source_covered_are_listed_apart_from_decoding() {
        let mut r = Registry::default();
        let t0 = 1_700_000_000;
        let oob = |hz| Call { freq_hz: hz, reason: Some(Reason::NoSource), ..call(100, &[], 0.0, 1.0) };
        for (hz, at) in [(860_000_000, t0), (860_000_000, t0 + 60), (861_000_000, t0 + 7200)] {
            r.call_start("x", &oob(hz), at);
        }
        // Another reason isn't out of band.
        r.call_start("x", &Call { freq_hz: 862_000_000, reason: Some(Reason::NoRecorder), ..call(100, &[], 0.0, 1.0) }, t0);
        r.concluded("x", &trunk_recorder_plugin::CallRecord { freq: 851_000_000, ..Default::default() }, t0);
        let (tg, unit) = names();
        let n = Names { tg: &tg, unit: &unit, signals: &no_signals };
        let q = r.query(&json!({ "what": "freqs", "system": "x", "hours": 24 }), t0 + 7300, &n);
        assert_eq!(q["rows"].as_array().unwrap().len(), 1, "decoding: only frequencies with a call recorded");
        assert_eq!(q["rows"][0]["freqHz"], 851_000_000u64);
        let o = &q["outOfBand"];
        assert_eq!((o[0]["freqHz"].as_u64(), o[0]["calls"].as_u64(), o[0]["last"].as_i64()), (Some(860_000_000), Some(2), Some(t0 + 60)), "the most first");
        assert_eq!(o[1]["freqHz"], 861_000_000u64);
        assert_eq!(o[0]["hourly"].as_array().unwrap().len(), 24);
        assert_eq!(o.as_array().unwrap().len(), 2);
        // Out of the range asked: not listed (all time still counted).
        let q = r.query(&json!({ "what": "freqs", "system": "x", "hours": 1 }), t0 + 7300, &n);
        assert_eq!(q["outOfBand"], json!([{ "freqHz": 861_000_000u64, "calls": 1, "totalCalls": 1, "last": t0 + 7200, "hourly": [1] }]));
        // Kept across runs.
        let mut back = Registry::default();
        back.load("x", &serde_json::to_string(&r.systems["x"].whole()).unwrap());
        assert_eq!(back.systems["x"].freqs[&860_000_000].out_of_band, 2);
    }

    /// The rows taken as they change, made over the registry as it was
    /// when last taken, are the registry: what went past a cap and what was
    /// heard again since, the newest affiliations, lengths and tones too.
    #[test]
    fn changes_taken_by_row_make_the_registry() {
        let mut r = Registry::default();
        r.call_end("x", &call(100, &[(1, 0.0), (2, 1.0)], 0.0, 3.0), 3);
        assert_eq!(r.take_changes().len(), 1);
        assert!(r.take_changes().is_empty(), "nothing since");
        let mut kept = r.systems["x"].clone();
        // Past the caps: radios and pairs go, the oldest first.
        let n = (Caps::default().pairs / 5 + Caps::default().pairs / 40) as u32;
        for i in 0..n {
            let talkers: Vec<(u32, f64)> = (0..6).map(|k| (10_000 + 6 * i + k, k as f64)).collect();
            r.call_end("x", &call(200 + i % 50, &talkers, 0.0, 6.0), 10 + i as i64);
        }
        // Radio 1 went, and is heard again.
        r.call_end("x", &call(100, &[(1, 0.0)], 0.0, 2.0), 100_000);
        r.message("x", &Message { kind: MessageType::Affiliation, source: 1, talkgroup: 300, ..Default::default() }, 100_001);
        r.tone_pattern("x", 100, &trunk_recorder_plugin::TonePattern { origin: "frame".into(), ..Default::default() }, 100_002);
        r.concluded("x", &trunk_recorder_plugin::CallRecord { freq: 851_000_000, ..Default::default() }, 100_003);
        let changes = r.take_changes();
        let (_, c) = &changes[0];
        assert!(!c.gone.units.is_empty() && !c.gone.pairs.is_empty() && c.gone.units.contains(&2), "{} radios, {} pairs went", c.gone.units.len(), c.gone.pairs.len());
        assert!(c.units.iter().any(|(u, _)| *u == 1), "radio 1 heard again");
        assert!(c.units.len() < r.systems["x"].units.len() + c.gone.units.len(), "only what changed");
        kept.apply(c);
        assert_eq!(serde_json::to_value(&kept).unwrap(), serde_json::to_value(&r.systems["x"]).unwrap());
        // The whole system as rows, made again: what a store imports and loads.
        let again = SystemRadio::from_changes(&r.systems["x"].whole());
        assert_eq!(serde_json::to_value(&again).unwrap(), serde_json::to_value(&r.systems["x"]).unwrap());
        // As a log's line, and back.
        let line = serde_json::to_string(c).unwrap();
        assert_eq!(serde_json::from_str::<RegistryChanges>(&line).unwrap().units.len(), c.units.len());
    }

    /// The caps the storage settings make: the defaults as ever, a number
    /// of radios with the other tables in proportion, 0 for none.
    #[test]
    fn caps_follow_the_storage_settings() {
        use crate::config::StorageSettings;
        let of = |registry_radios, registry_days| Caps::of(&StorageSettings { registry_radios, registry_days, ..Default::default() });
        assert_eq!(Caps::default(), Caps { tgs: 5000, units: 20_000, pairs: 50_000, days: Some(365) });
        assert_eq!(of(100_000, 30), Caps { tgs: 25_000, units: 100_000, pairs: 250_000, days: Some(30) });
        assert_eq!(of(1_000, 0), Caps { tgs: 5000, units: 1_000, pairs: 2_500, days: None }, "talkgroups 5,000 at least");
        assert_eq!(of(0, 90).units, usize::MAX, "no cap: the days alone");
    }

    /// Rows not heard for the caps' days go, as gone: a store is told.
    #[test]
    fn what_isnt_heard_for_its_days_goes() {
        let mut r = Registry::default();
        r.set_caps(Caps::of(&crate::config::StorageSettings { registry_days: 30, ..Default::default() }));
        let t0 = 1_700_000_000;
        r.call_end("x", &call(100, &[(1, 0.0), (2, 1.0)], 0.0, 3.0), t0);
        r.call_end("x", &call(200, &[(3, 0.0)], 0.0, 3.0), t0 + 29 * 86_400);
        r.take_changes();
        r.expire(t0 + 31 * 86_400);
        let x = &r.systems["x"];
        assert!(!x.units.contains_key(&1) && !x.units.contains_key(&2) && x.units.contains_key(&3));
        assert!(!x.tgs.contains_key(&100) && x.tgs.contains_key(&200) && x.pairs.is_empty());
        let (_, c) = r.take_changes().remove(0);
        assert_eq!((c.gone.units.clone(), c.gone.tgs.clone(), c.gone.pairs.len()), (vec![1, 2], vec![100], 1), "the store is told");
        r.set_caps(Caps::of(&crate::config::StorageSettings { registry_days: 0, ..Default::default() }));
        r.expire(t0 + 10_000 * 86_400);
        assert!(r.systems["x"].units.contains_key(&3), "0: kept however old");
    }

    /// A page is 500 rows by default, and a `limit` up to the cap is every
    /// radio heard (from PR #112: an interface listing a system's radios).
    #[test]
    fn a_query_may_ask_for_every_radio_heard() {
        let t = 1_790_000_000i64;
        let mut s = SystemRadio::new(t);
        for u in 0..6_000u32 {
            s.units.insert(1_000_000 + u, Unit { first: t, last: t, last_tx: t, tx: 1, tx_secs: 1.0, aff: None, aff_t: 0, reg: None, tgs: Default::default(), recent: Default::default() });
        }
        let mut r = Registry::default();
        r.put("x", s);
        let (tg, unit) = names();
        let n = Names { tg: &tg, unit: &unit, signals: &no_signals };
        let rows = |r: &Registry, limit: Option<u64>| {
            let mut q = json!({ "what": "units", "system": "x", "hours": 24 });
            if let Some(l) = limit {
                q["limit"] = json!(l);
            }
            let a = r.query(&q, t + 60, &n);
            assert_eq!(a["total"], 6_000);
            a["rows"].as_array().unwrap().len()
        };
        assert_eq!(rows(&r, None), 500, "the default page");
        assert_eq!(rows(&r, Some(Caps::default().units as u64)), 6_000, "every radio heard");
        assert_eq!(rows(&r, Some(1_000_000)), 6_000, "more than were heard is all of them");
        r.set_caps(Caps::of(&crate::config::StorageSettings { registry_radios: 0, ..Default::default() }));
        assert_eq!(rows(&r, Some(1_000_000)), 6_000, "no cap: as many as asked for");
    }

    #[test]
    fn caps_bound_the_tables_and_the_query() {
        let mut r = Registry::default();
        r.set_caps(Caps::of(&crate::config::StorageSettings { registry_radios: 1_000, ..Default::default() }));
        let t0 = 1_700_000_000;
        for u in 1..=1_200u32 {
            r.call_end("x", &call(100, &[(u, 0.0)], 0.0, 1.0), t0 + u as i64);
        }
        let n = r.systems["x"].units.len();
        assert!((1_000..=1_100).contains(&n), "past the cap by a tenth, the least recently heard go: {n}");
        assert!(!r.systems["x"].units.contains_key(&1) && r.systems["x"].units.contains_key(&1_200));
        let (tg, unit) = names();
        let n = Names { tg: &tg, unit: &unit, signals: &no_signals };
        let q = r.query(&json!({ "what": "units", "system": "x", "limit": 50_000 }), t0 + 1_300, &n);
        assert_eq!(q["rows"].as_array().unwrap().len(), 1_000, "a limit past the cap is the cap");
        let q = r.query(&json!({ "what": "summary" }), t0 + 1_300, &n);
        assert_eq!(q["caps"], json!({ "talkgroups": 5000, "radios": 1_000, "days": 365 }));
        // A system put from the store, and one made later, get the caps too.
        r.put("y", SystemRadio::new(t0));
        r.call_end("z", &call(100, &[(1, 0.0)], 0.0, 1.0), t0);
        assert_eq!((r.systems["y"].caps, r.systems["z"].caps), (r.caps(), r.caps()));
    }

    #[test]
    fn tables_stay_bounded() {
        let mut m: HashMap<u32, i64> = (0..1200).map(|i| (i, i as i64)).collect();
        assert_eq!(evict(&mut m, 1000, |v| *v).len(), 200, "those that went");
        assert_eq!(m.len(), 1000);
        assert!(m.contains_key(&1199) && !m.contains_key(&10));
    }

    #[test]
    fn ciphers_are_counted_per_talkgroup() {
        let mut r = Registry::default();
        let t0 = 1_700_000_000;
        let adp = CipherId { algid: 0xAA, keyid: 0x1B50 };
        for (i, cipher) in [Some(adp), None, Some(adp), Some(CipherId { algid: 0x84, keyid: 2 })].into_iter().enumerate() {
            let c = Call { encrypted: cipher.is_some(), cipher, ..call(100, &[], 0.0, 5.0) };
            r.call_end("dcfd", &c, t0 + 10 * i as i64);
        }
        assert_eq!(r.systems["dcfd"].tgs[&100].ciphers, vec![(0x84, 2, 1, t0 + 30), (0xAA, 0x1B50, 2, t0 + 20)]);
        let (tg, unit) = names();
        let n = Names { tg: &tg, unit: &unit, signals: &no_signals };
        let q = r.query(&json!({ "what": "tg", "system": "dcfd", "key": 100, "hours": 24 }), t0 + 40, &n);
        assert_eq!(q["row"]["ciphers"][1], json!({ "algid": 0xAA, "keyId": 0x1B50, "name": "ADP", "calls": 2, "last": t0 + 20 }));
    }

    /// How big a system's registry file gets, and how long turning it into
    /// JSON and copying it take: at every cap (a week of a very large
    /// system), and at about half. `cargo test --release -p trunk-app --lib
    /// registry_size -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn registry_size() {
        for (label, tgs, units, pairs) in [("at the caps", Caps::default().tgs, Caps::default().units, Caps::default().pairs), ("about half", Caps::default().tgs / 2, Caps::default().units / 2, Caps::default().pairs / 2)] {
            let t = 1_790_000_000i64;
            let mut s = SystemRadio::new(t);
            let hours = |n: usize| (0..n as i64).map(|h| (t / 3600 - h, (12u32, 95.5f32))).collect::<VecDeque<_>>();
            for g in 0..tgs as u32 {
                s.tgs.insert(10_000 + g, Tg { first: t, last: t + 600_000, name: format!("TG {g} Dispatch"), calls: 4321, secs: 98_765.5, enc: 12, ciphers: vec![(0x84, 0x1234, 40, t)], hours: hours(HOURS as usize) });
            }
            for u in 0..units as u32 {
                let recent = (0..MAX_RECENT_TX as i64).map(|k| (t + k * 60, 10_000 + u % 500, 4.25f32)).collect();
                s.units.insert(1_000_000 + u, Unit { first: t, last: t + 600_000, last_tx: t + 599_000, tx: 321, tx_secs: 1234.5, aff: Some(10_001), aff_t: t, reg: Some(true), tgs: (0..TOP_TGS as u32).map(|k| (10_000 + k, 40 - k)).collect(), recent });
            }
            for p in 0..pairs as u64 {
                s.pairs.insert(pair_key(1_000_000 + (p % 20_000) as u32, 1_020_000 + p as u32), Pair { n: 17, last: t });
            }
            for f in 0..MAX_FREQS as u64 {
                let hours = (0..HOURS).map(|h| (t / 3600 - h, (40u32, 100_000u64, 2_000u64, 30u64))).collect();
                s.freqs.insert(851_000_000 + f * 12_500, Freq { calls: 5000, frames: 9_000_000, errors: 12_345, bad: 678, snr: Some(18.5), ferr: Some(0.2), clean: Some(0.97), last: t, hours, ..Default::default() });
            }
            s.affiliations = (0..MAX_AFFILIATIONS as i64).map(|k| (t + k, 1_000_000 + k as u32, 10_001)).collect();
            s.lengths = (0..MAX_LENGTHS as i64).map(|k| (t + k, 10_001, 5.5f32)).collect();
            let at = std::time::Instant::now();
            let copy = s.clone();
            let cloned = at.elapsed();
            let at = std::time::Instant::now();
            let json = serde_json::to_string(&copy).unwrap();
            let made = at.elapsed();
            let at = std::time::Instant::now();
            let back: SystemRadio = serde_json::from_str(&json).unwrap();
            let read = at.elapsed();
            assert_eq!(back.units.len(), units);
            println!("{label}: {tgs} talkgroups, {units} radios, {pairs} pairs: {:.1} MB; copied in {cloned:.1?}, JSON in {made:.1?}, read back in {read:.1?}", json.len() as f64 / 1e6);
        }
    }
}
