//! The dashboard's measurements, kept apart from the session that feeds them.
//!
//! ```text
//! engine / DSP parts ── Instrumented::report ──┐
//! SampleMeter (headroom, clipping) ────────────┤
//! Tally (calls, airtime, reasons, voice errors)┴─► Aggregator ─► `stats` (1 s)
//!                                                       └──────► Rollup (1 min) ─► History / stats files
//! session events ─► Stats::observe ─► Registry (talkgroups, radios, frequencies)
//!                                  └─► Monitor (notable events ─► watchers, the feed)
//! ```
//!
//! The [`Session`](crate::Session) owns a [`Stats`] and calls three things:
//! [`Stats::observe`] for each event, the meters as samples arrive, and
//! [`Stats::tick`] when it polls. What outlives a session — the registry and
//! the monitor — is [`Shared`], held by the platform too (its queries).

pub mod aggregate;
pub mod events;
pub mod history;
pub mod meter;
pub mod radio;
pub mod ring;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use trunk_core::metrics::{key_part, Scoped, Sink};
use trunk_core::dsp::inband::signal::{Heard, Signal};
use trunk_core::trunk::{CallId, Engine, Event, MessageType};

use crate::config::Config;
use crate::protocol::ToClient;
use crate::session::Output;
pub use aggregate::{Aggregator, Rollup};
pub use events::{Emitted, Monitor, MonitorEvent, Watcher, ACTIVITY};
pub use history::{Bucket, Buckets, History, HourRow, Span};
pub use meter::SampleMeter;
pub use radio::{Names, Registry, TgInfo};

/// The `stats` message and the rollups come this often, ms of wall clock.
const TICK_MS: f64 = 1000.0;
/// A source clipping more than this share of samples, %, is an event.
const CLIP_EVENT_PCT: f64 = 0.5;

/// What outlives a session: the radio registry, the names it answers with,
/// and the event monitor (shared with the platform, which answers queries
/// and adds its own events).
#[derive(Default)]
pub struct Shared {
    pub radio: Mutex<Registry>,
    pub names: Mutex<NameBook>,
    pub monitor: Mutex<Monitor>,
}

impl Shared {
    pub fn new() -> Arc<Shared> {
        Arc::new(Shared::default())
    }

    /// Hand an event to the watchers; its `monitorEvent` message and where
    /// it goes (the feed, or the `activity` topic), if anywhere.
    pub fn emit(&self, t: f64, e: MonitorEvent) -> Option<Emitted> {
        self.monitor.lock().unwrap().emit(t, e)
    }

    pub fn recent_events(&self) -> Vec<Value> {
        self.monitor.lock().unwrap().recent()
    }

    /// A `radioQuery`: the registry, with talkgroups and radios named from
    /// the [`NameBook`] (`now`: Unix s). The desktop app's server and the
    /// browser build's worker both answer this way.
    pub fn radio_query(&self, q: &Value, now: i64) -> ToClient {
        let book = self.names.lock().unwrap();
        let tg = |sys: &str, t: u32| book.tg(sys, t);
        let unit = |sys: &str, u: u32| book.units.get(sys).and_then(|a| a.get(u)).map(str::to_string);
        let signals = |sys: &str, h: &Heard, tg: u32| book.signals(sys, h, tg);
        let mut out = self.radio.lock().unwrap().query(q, now, &Names { tg: &tg, unit: &unit, signals: &signals });
        out["id"] = q["id"].clone();
        ToClient::RadioResult(crate::protocol::body(out))
    }
}

/// The names the radio registry's answers use ([`Shared::radio_query`]):
/// each system's talkgroups (its talkgroup file's, and what a conventional
/// system's rows name) and talker aliases, by short name. A running session keeps
/// them as its engine has them; the desktop app fills them from its files
/// when it starts and when they change.
#[derive(Default)]
pub struct NameBook {
    talkgroups: HashMap<String, trunk_core::trunk::Talkgroups>,
    /// Conventional rows' names for the numbers they file under ([`Config::row_talkgroups`]).
    rows: HashMap<String, trunk_core::trunk::Talkgroups>,
    units: HashMap<String, trunk_core::trunk::UnitAliases>,
    signals: HashMap<String, Vec<Signal>>,
}

impl NameBook {
    /// Each system's talkgroups, as an engine built from `cfg` and these talkgroup files has them.
    pub fn set_config(&mut self, cfg: &Config, files: &crate::config::TalkgroupLists) {
        self.set_systems(&cfg.engine_config(0.0, files).systems);
        self.set_rows(cfg);
    }

    /// The names `cfg`'s conventional rows give.
    pub fn set_rows(&mut self, cfg: &Config) {
        self.rows = cfg.row_talkgroups();
    }

    /// Talkgroup `tg` of `system`: named as its calls are — by a row, else
    /// by the talkgroup file (whose Ignore flag it keeps either way).
    pub fn tg(&self, system: &str, tg: u32) -> Option<TgInfo> {
        let file = tg_info(&self.talkgroups, system, tg);
        match self.rows.get(system).and_then(|r| r.get(&tg)) {
            Some(row) => Some(TgInfo { alpha_tag: row.alpha_tag.clone(), ignore: file.is_some_and(|f| f.ignore) }),
            None => file,
        }
    }

    /// One talkgroup of `system`, as its file now has it (none: not in it).
    pub fn set_talkgroup(&mut self, system: &str, number: u32, row: Option<trunk_core::trunk::Talkgroup>) {
        let table = self.talkgroups.entry(system.to_string()).or_default();
        match row {
            Some(t) => table.insert(number, t),
            None => table.remove(&number),
        };
    }

    /// Each system's talkgroups and signals, from an engine's systems.
    pub fn set_systems(&mut self, systems: &[trunk_core::trunk::SystemConfig]) {
        self.talkgroups = systems.iter().map(|s| (s.short_name.clone(), s.talkgroups.clone())).collect();
        self.signals = systems.iter().map(|s| (s.short_name.clone(), s.signals.clone())).collect();
    }

    /// The signals of `system` a pattern heard on talkgroup `tg` is (their ids).
    pub fn signals(&self, system: &str, h: &Heard, tg: u32) -> Vec<String> {
        self.signals.get(system).map_or_else(Vec::new, |v| v.iter().filter(|s| s.matches_heard(h, tg)).map(|s| s.id.clone()).collect())
    }

    /// System `short_name`'s talker aliases, from their CSV (as saved).
    pub fn set_units(&mut self, short_name: &str, csv: &str) {
        self.units.insert(short_name.to_string(), trunk_core::trunk::UnitAliases::parse_csv(csv));
    }
}

/// What someone is watching (the union of every connected dashboard's
/// `subscribe`). Costly outputs are only made for these:
/// `spectrum:<source>` (the waterfall), `log` (every control message as a
/// line), `rf:<source>` (`rfDetail`), `decode:<shortName>` (`decodeDetail`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Topics(BTreeSet<String>);

impl Topics {
    pub fn new(t: impl IntoIterator<Item = String>) -> Topics {
        Topics(t.into_iter().collect())
    }
    pub fn has(&self, t: &str) -> bool {
        self.0.contains(t)
    }
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// Running totals of what calls did, per system, by name.
#[derive(Default)]
struct Tally {
    counts: BTreeMap<String, u64>,
    /// Keys new since the last report: they count from 0.
    fresh: Vec<String>,
}

impl Tally {
    fn add(&mut self, key: String, n: u64) {
        match self.counts.get_mut(&key) {
            Some(v) => *v += n,
            None => {
                self.fresh.push(key.clone());
                self.counts.insert(key, n);
            }
        }
    }
    fn report(&mut self, agg: &mut Aggregator) {
        for k in self.fresh.drain(..) {
            agg.seed(&k);
        }
        for (k, v) in &self.counts {
            agg.counter(k, *v);
        }
    }
}

/// Each call in progress's air time (id, system key, ms so far) not yet
/// counted, by `air` (which it updates). A call gone from them whose end
/// hasn't come in a few ticks is forgotten.
fn air_since(air: &mut HashMap<CallId, (u64, u8)>, on: impl Iterator<Item = (CallId, String, u64)>) -> Vec<(String, u64)> {
    for (_, missed) in air.values_mut() {
        *missed += 1;
    }
    let mut out = Vec::new();
    for (id, sys, ms) in on {
        let (counted, _) = air.get(&id).copied().unwrap_or_default();
        out.push((sys, ms.saturating_sub(counted)));
        air.insert(id, (ms.max(counted), 0));
    }
    air.retain(|_, (_, missed)| *missed <= 5);
    out
}

/// Today's totals (local day), for the big numbers.
#[derive(Default, Clone, Copy)]
struct Day {
    index: i64,
    calls: u64,
    air_s: f64,
    audio_bytes: u64,
}

pub struct Stats {
    agg: Aggregator,
    /// (Each behind a lock: a source's feeder thread meters it.)
    meters: Vec<Arc<Mutex<SampleMeter>>>,
    /// Each source's series name (its label).
    names: Vec<String>,
    tally: Tally,
    day: Day,
    shared: Arc<Shared>,
    pub topics: Topics,
    /// Unix seconds now (set each poll).
    now: f64,
    last_tick_ms: Option<f64>,
    busy_mark: f64,
    /// Control channels decoding at the last tick, by system.
    locked: HashMap<String, bool>,
    /// Per source: dropping, clipping at the last tick.
    trouble: Vec<(bool, bool)>,
    /// Calls this minute, per system (CallVolume for watchers).
    minute_calls: BTreeMap<String, u32>,
    /// Air time counted so far of each call in progress, ms (counted as it
    /// goes, so a long call's minutes land in the minutes they were on the
    /// air, not all in the minute it ended), and the ticks since it was last
    /// seen in progress.
    air: HashMap<CallId, (u64, u8)>,
    local_offset: crate::session::LocalOffset,
}

impl Stats {
    pub fn new(cfg: &Config, shared: Arc<Shared>, local_offset: crate::session::LocalOffset) -> Stats {
        let names: Vec<String> = cfg.sources.iter().map(|s| key_part(&s.label())).collect();
        Stats {
            agg: Aggregator::default(),
            meters: names.iter().map(|_| Default::default()).collect(),
            trouble: vec![(false, false); names.len()],
            names,
            tally: Tally::default(),
            day: Day::default(),
            shared,
            topics: Topics::default(),
            now: 0.0,
            last_tick_ms: None,
            busy_mark: 0.0,
            locked: HashMap::new(),
            minute_calls: BTreeMap::new(),
            air: HashMap::new(),
            local_offset,
        }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Source `i`'s sample meter (the session feeds it).
    pub fn meter(&self, i: usize) -> std::sync::MutexGuard<'_, SampleMeter> {
        self.meters[i].lock().unwrap()
    }

    /// Source `i`'s sample meter, for a thread that feeds it ([`crate::session::Feeder`]).
    pub fn meter_handle(&self, i: usize) -> Arc<Mutex<SampleMeter>> {
        self.meters[i].clone()
    }

    /// Unix seconds, as last set.
    pub fn now(&self) -> f64 {
        self.now
    }

    /// The wall clock, Unix seconds (the session sets it as it polls).
    pub fn set_now(&mut self, unix_s: f64) {
        self.now = unix_s;
    }

    fn emit(&mut self, e: MonitorEvent, out: &mut Vec<Output>) {
        match self.shared.emit(self.now, e) {
            Some(Emitted::Feed(v)) => out.push(Output::Message(ToClient::MonitorEvent(crate::protocol::body(v)))),
            Some(Emitted::Activity(v)) => out.push(Output::Topic { topic: ACTIVITY.to_string(), msg: ToClient::MonitorEvent(crate::protocol::body(v)) }),
            None => {}
        }
    }

    /// What the connections watch; the monitor builds the frequent events
    /// while someone subscribes to the `activity` topic.
    pub fn set_topics(&mut self, topics: Topics) {
        self.shared.monitor.lock().unwrap().set_subscribed(topics.has(ACTIVITY));
        self.topics = topics;
    }

    fn roll_day(&mut self) {
        let t = self.now as i64;
        let d = (t + (self.local_offset)(t) as i64).div_euclid(86400);
        if d != self.day.index {
            self.day = Day { index: d, ..Default::default() };
        }
    }

    /// One of the session's events. `name`: the short name of the event's system.
    pub fn observe(&mut self, ev: &Event, name: &str, engine: &Engine, out: &mut Vec<Output>) {
        let t = self.now as i64;
        let sys = format!("sys/{}", key_part(name));
        match ev {
            Event::CallStart(c) => {
                let why = if c.recording { "recorded" } else { c.reason.map_or("monitored", |r| r.as_str()) };
                self.tally.add(format!("{sys}/why/{why}"), 1);
                let firsts = self.shared.radio.lock().unwrap().call_start(name, c, t);
                if firsts.tg {
                    let alpha_tag = c.talkgroup_info.as_ref().map_or(String::new(), |g| g.alpha_tag.clone());
                    self.emit(MonitorEvent::TgFirstSeen { system: name.to_string(), talkgroup: c.talkgroup, alpha_tag }, out);
                }
                for u in firsts.units {
                    let alias = engine.unit_alias(c.system, u).unwrap_or_default();
                    self.emit(MonitorEvent::UnitFirstSeen { system: name.to_string(), unit: u, alias }, out);
                }
                if self.shared.monitor.lock().unwrap().wants_frequent() {
                    let units = c.sources.iter().map(|s| s.src).collect();
                    self.emit(MonitorEvent::TgActive { system: name.to_string(), talkgroup: c.talkgroup, units }, out);
                }
            }
            Event::CallEnd(c) => {
                let secs = (c.last_update_s - c.start_s).max(0.0);
                self.tally.add(format!("{sys}/calls"), 1);
                self.tally.add("all/calls".into(), 1);
                let counted = self.air.remove(&c.id).map_or(0, |(ms, _)| ms);
                self.add_air(&sys, ((secs * 1000.0) as u64).saturating_sub(counted));
                *self.minute_calls.entry(name.to_string()).or_default() += 1;
                self.roll_day();
                self.day.calls += 1;
                self.day.air_s += secs;
                let firsts = self.shared.radio.lock().unwrap().call_end(name, c, t);
                for u in firsts {
                    let alias = engine.unit_alias(c.system, u).unwrap_or_default();
                    self.emit(MonitorEvent::UnitFirstSeen { system: name.to_string(), unit: u, alias }, out);
                }
                if self.shared.monitor.lock().unwrap().wants_frequent() {
                    for s in &c.sources {
                        self.emit(MonitorEvent::UnitActive { system: name.to_string(), unit: s.src, talkgroup: c.talkgroup }, out);
                    }
                }
            }
            Event::Message { msg, .. } => {
                if !matches!(
                    msg.kind,
                    MessageType::Affiliation | MessageType::Location | MessageType::Registration | MessageType::Deregistration | MessageType::UuVGrant | MessageType::UuAnsReq | MessageType::PhoneGrant | MessageType::Grant | MessageType::Update
                ) {
                    return;
                }
                let aff = self.shared.radio.lock().unwrap().message(name, msg, t);
                if let Some((unit, talkgroup)) = aff {
                    if self.shared.monitor.lock().unwrap().wants_frequent() {
                        self.emit(MonitorEvent::UnitAffiliated { system: name.to_string(), unit, talkgroup }, out);
                    }
                }
            }
            Event::TonePattern(h) => {
                let p = trunk_core::trunk::record::tone_pattern(&h.pattern, h.src, h.signals.clone());
                self.shared.radio.lock().unwrap().tone_pattern(name, h.talkgroup, &p, t);
                // One of the system's signals always; any other pattern only when a watcher wants it.
                if !h.signals.is_empty() || self.shared.monitor.lock().unwrap().wants_frequent() {
                    self.emit(MonitorEvent::TonePattern { system: name.to_string(), talkgroup: h.talkgroup, text: p.text, signals: h.signals.clone() }, out);
                }
            }
            Event::Concluded(k) => {
                let record = &k.record;
                let bytes = 44 + k.audio.len() as u64 * 2;
                self.tally.add(format!("{sys}/audioBytes"), bytes);
                self.tally.add("all/audioBytes".into(), bytes);
                self.roll_day();
                self.day.audio_bytes += bytes;
                for i in &record.error_list {
                    self.tally.add(format!("{sys}/voice/frames"), i.frames);
                    self.tally.add(format!("{sys}/voice/errors"), i.error_count);
                    self.tally.add(format!("{sys}/voice/bad"), i.bad_frames);
                }
                self.shared.radio.lock().unwrap().concluded(name, record, t);
            }
            Event::NotSaved(_) => self.tally.add(format!("{sys}/notSaved"), 1),
            Event::Duplicate { .. } => self.tally.add(format!("{sys}/dup"), 1),
            _ => {}
        }
    }

    /// Once a second of wall clock (`now_ms`, the session's clock): the
    /// `stats` message, a [`Rollup`] each new minute, the detail messages
    /// someone is watching, and the edge-triggered events. `busy_ms`: the
    /// session's decoding time so far (the load). `site_names`: a call's
    /// system's short name and (by its site's number) site name.
    /// `decode_sites`: a system's sites' and patches' detail (by short name).
    pub fn tick(&mut self, now_ms: f64, busy_ms: f64, engine: &Engine, site_names: &dyn Fn(u16, Option<u16>) -> (String, String), decode_sites: DecodeSites, out: &mut Vec<Output>) {
        if self.last_tick_ms.is_some_and(|t| now_ms - t < TICK_MS) {
            return;
        }
        let dt = self.last_tick_ms.map_or(0.0, |t| now_ms - t);
        self.last_tick_ms = Some(now_ms);
        if let Some(r) = self.agg.begin(self.now) {
            if self.shared.monitor.lock().unwrap().wants_frequent() {
                for (system, n) in std::mem::take(&mut self.minute_calls) {
                    self.emit(MonitorEvent::CallVolume { system, per_min: n }, out);
                }
            }
            self.minute_calls.clear();
            out.push(Output::Rollup(r));
        }
        engine.report(&self.names, &mut self.agg);
        for (i, m) in self.meters.iter().enumerate() {
            m.lock().unwrap().report(&mut Scoped::new(&mut self.agg, format!("src/{}", self.names[i])));
        }
        self.count_air(engine, site_names);
        self.tally.report(&mut self.agg);
        if dt > 0.0 {
            self.agg.gauge("eng/load", ((busy_ms - self.busy_mark) / dt).clamp(0.0, 64.0));
        }
        self.busy_mark = busy_ms;
        self.roll_day();
        self.agg.gauge("day/calls", self.day.calls as f64);
        self.agg.gauge("day/airS", self.day.air_s);
        self.agg.gauge("day/audioBytes", self.day.audio_bytes as f64);
        self.edges(engine, out);
        out.push(Output::Message(ToClient::Stats { t: (self.now * 10.0).round() / 10.0, values: self.agg.values_json() }));
        self.details(engine, site_names, decode_sites, out);
    }

    fn add_air(&mut self, sys: &str, ms: u64) {
        self.tally.add(format!("{sys}/airMs"), ms);
        self.tally.add("all/airMs".into(), ms);
    }

    /// The air time of the calls in progress since the last tick.
    fn count_air(&mut self, engine: &Engine, site_names: &dyn Fn(u16, Option<u16>) -> (String, String)) {
        let calls = engine.active_calls().into_iter().filter(|c| !c.twin);
        let on = calls.map(|c| (c.id, format!("sys/{}", key_part(&site_names(c.system, c.site).0)), ((c.last_update_s - c.start_s).max(0.0) * 1000.0) as u64));
        for (sys, ms) in air_since(&mut self.air, on) {
            self.add_air(&sys, ms);
        }
    }

    /// Events on a change of state: a control channel lost or regained, a
    /// source starting to drop samples or clip.
    fn edges(&mut self, engine: &Engine, out: &mut Vec<Output>) {
        let systems = engine.systems();
        let sites: Vec<_> = trunk_core::trunk::sites(&systems).collect();
        for y in engine.status().sites {
            let Some(&(system, cfg)) = sites.get(y.site as usize) else { continue };
            let key = systems[system as usize].site_key(cfg);
            let Some(l) = self.agg.get(&format!("{}/cc/locked", key.stats_key())) else { continue };
            let now = l > 0.5;
            let (system, site, freq_hz) = (key.system.to_string(), key.own_name().map(str::to_string), y.control_channel_hz);
            match (self.locked.insert(key.to_string(), now), now) {
                (Some(true), false) => self.emit(MonitorEvent::ControlLost { system, site, freq_hz }, out),
                (Some(false), true) => self.emit(MonitorEvent::ControlRegained { system, site, freq_hz }, out),
                _ => {}
            }
        }
        for i in 0..self.names.len() {
            let n = &self.names[i];
            let drops = self.agg.get(&format!("src/{n}/dropped")).unwrap_or(0.0) as f64;
            let clip = self.agg.get(&format!("src/{n}/clipPct")).unwrap_or(0.0) as f64;
            let (was_d, was_c) = self.trouble[i];
            let (d, c) = (drops > 0.0, clip > CLIP_EVENT_PCT);
            self.trouble[i] = (d, c);
            let source = n.clone();
            if d && !was_d {
                self.emit(MonitorEvent::SourceDrops { source, per_s: drops }, out);
            } else if c && !was_c {
                self.emit(MonitorEvent::SourceClipping { source, pct: (clip * 10.0).round() / 10.0 }, out);
            } else if (was_d || was_c) && !d && !c {
                self.emit(MonitorEvent::SourceRecovered { source }, out);
            }
        }
    }

    /// `rfDetail` / `decodeDetail` for the topics someone watches.
    fn details(&self, engine: &Engine, site_names: &dyn Fn(u16, Option<u16>) -> (String, String), decode_sites: DecodeSites, out: &mut Vec<Output>) {
        let rf: Vec<usize> = (0..self.names.len()).filter(|i| self.topics.has(&format!("rf:{i}"))).collect();
        let decode: Vec<&str> = self.topics.iter().filter_map(|t| t.strip_prefix("decode:")).collect();
        if rf.is_empty() && decode.is_empty() {
            return;
        }
        let channels = engine.channels();
        let r1 = |v: f64| (v * 10.0).round() / 10.0;
        let ch_json = |c: &trunk_core::trunk::ChannelSnapshot| {
            let (system, site) = site_names(c.system, c.site);
            let mut v = json!({
                "system": system, "site": if site.is_empty() { None } else { Some(site) }, "freqHz": c.freq_hz, "source": c.source, "kind": c.kind.name(),
                "powerDb": r1(c.power_db), "noiseDb": r1(c.noise_db), "snrDb": r1(c.power_db - c.noise_db),
                "offsetHz": c.offset_hz.map(|v| v.round()), "quality": c.quality.sep.map(|v| r1(v as f64)),
                "phaseErrDeg": c.quality.phase_err_deg.map(|v| r1(v as f64)), "calls": c.calls,
            });
            // A conventional channel: its mode and its rows' names.
            if let trunk_core::trunk::ChannelKind::Conventional(m) = c.kind {
                v["mode"] = json!(m.as_str());
                v["names"] = json!(c.names);
            }
            v
        };
        for i in rf {
            let profile: Vec<f64> = engine.noise_profile(i, 64).into_iter().map(r1).collect();
            let chans: Vec<Value> = channels.iter().filter(|c| c.source == i).map(ch_json).collect();
            let topic = format!("rf:{i}");
            out.push(Output::Topic { msg: ToClient::RfDetail { source: i, profile, channels: chans }, topic });
        }
        for name in decode {
            let chans: Vec<Value> = channels.iter().filter(|c| site_names(c.system, c.site).0 == name).map(ch_json).collect();
            let (sites, patches) = decode_sites(engine, name);
            out.push(Output::Topic { topic: format!("decode:{name}"), msg: ToClient::DecodeDetail { system: name.to_string(), channels: chans, sites, patches } });
        }
    }
}

/// A system's (by short name) `decodeDetail` sites and patches.
pub type DecodeSites = fn(&Engine, &str) -> (Vec<Value>, Vec<Value>);

/// A talkgroup's name and Ignore flag from parsed tables.
pub fn tg_info(tables: &HashMap<String, trunk_core::trunk::Talkgroups>, system: &str, tg: u32) -> Option<TgInfo> {
    tables.get(system)?.get(&tg).map(|t| TgInfo { alpha_tag: t.alpha_tag.clone(), ignore: t.ignore })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Subscribing to `activity` makes the monitor build the frequent events,
    /// and the session sends them for the topic, not as a message for all.
    #[test]
    fn the_activity_topic_turns_the_frequent_events_on_and_routes_them() {
        let shared = Shared::new();
        let mut stats = Stats::new(&Config::default(), shared.clone(), |_| 0);
        let tg_active = || MonitorEvent::TgActive { system: "dcfd".into(), talkgroup: 1, units: vec![7] };
        let mut out = Vec::new();
        stats.emit(tg_active(), &mut out);
        assert!(out.is_empty(), "nobody asked");
        stats.set_topics(Topics::new(["activity".to_string()]));
        assert!(shared.monitor.lock().unwrap().wants_frequent());
        stats.emit(tg_active(), &mut out);
        match out.as_slice() {
            [Output::Topic { topic, msg: ToClient::MonitorEvent(body) }] => {
                assert_eq!(topic, ACTIVITY);
                assert_eq!(body["kind"], "tgActive");
            }
            other => panic!("not for the topic: {}", other.len()),
        }
        out.clear();
        stats.set_topics(Topics::default());
        assert!(!shared.monitor.lock().unwrap().wants_frequent());
        stats.emit(tg_active(), &mut out);
        assert!(out.is_empty(), "nobody asks any more");
    }

    #[test]
    fn a_long_calls_air_counts_in_the_minutes_it_was_on() {
        let mut air = HashMap::new();
        let on = |ms| std::iter::once((7, "sys/a".to_string(), ms));
        assert_eq!(air_since(&mut air, on(1000)), [("sys/a".to_string(), 1000)]);
        assert_eq!(air_since(&mut air, on(61_000)), [("sys/a".to_string(), 60_000)]);
        // Its end counts only what's left.
        assert_eq!(air.remove(&7).map(|(ms, _)| ms), Some(61_000));
        // Gone without its end: kept a few ticks (the end may come late), then forgotten.
        air_since(&mut air, on(5000));
        for _ in 0..5 {
            air_since(&mut air, std::iter::empty());
        }
        assert!(air.contains_key(&7));
        air_since(&mut air, std::iter::empty());
        assert!(air.is_empty());
    }
}
