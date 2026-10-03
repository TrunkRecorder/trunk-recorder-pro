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
use trunk_core::trunk::{Engine, Event, MessageType};

use crate::config::Config;
use crate::session::Output;
pub use aggregate::{Aggregator, Rollup};
pub use events::{Monitor, MonitorEvent, Watcher};
pub use history::History;
pub use meter::SampleMeter;
pub use radio::{Names, Registry, TgInfo};

/// The `stats` message and the rollups come this often, ms of wall clock.
const TICK_MS: f64 = 1000.0;
/// A source clipping more than this share of samples, %, is an event.
const CLIP_EVENT_PCT: f64 = 0.5;

/// What outlives a session: the radio registry and the event monitor
/// (shared with the platform, which answers queries and adds its own events).
#[derive(Default)]
pub struct Shared {
    pub radio: Mutex<Registry>,
    pub monitor: Mutex<Monitor>,
}

impl Shared {
    pub fn new() -> Arc<Shared> {
        Arc::new(Shared::default())
    }

    /// Hand an event to the watchers; its `monitorEvent` message when it's one for the feed.
    pub fn emit(&self, t: f64, e: MonitorEvent) -> Option<Value> {
        self.monitor.lock().unwrap().emit(t, e)
    }

    pub fn recent_events(&self) -> Vec<Value> {
        self.monitor.lock().unwrap().recent()
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
    meters: Vec<SampleMeter>,
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
    local_offset: crate::session::LocalOffset,
}

impl Stats {
    pub fn new(cfg: &Config, shared: Arc<Shared>, local_offset: crate::session::LocalOffset) -> Stats {
        let names: Vec<String> = cfg.sources.iter().map(|s| key_part(&s.label())).collect();
        Stats {
            agg: Aggregator::default(),
            meters: vec![SampleMeter::default(); names.len()],
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
            local_offset,
        }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Source `i`'s sample meter (the session feeds it).
    pub fn meter(&mut self, i: usize) -> &mut SampleMeter {
        &mut self.meters[i]
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
        if let Some(v) = self.shared.emit(self.now, e) {
            out.push(Output::Text(v.to_string()));
        }
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
                    let alias = engine.unit_alias(c.system, u).unwrap_or("").to_string();
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
                self.tally.add(format!("{sys}/airMs"), (secs * 1000.0) as u64);
                self.tally.add("all/calls".into(), 1);
                self.tally.add("all/airMs".into(), (secs * 1000.0) as u64);
                *self.minute_calls.entry(name.to_string()).or_default() += 1;
                self.roll_day();
                self.day.calls += 1;
                self.day.air_s += secs;
                let firsts = self.shared.radio.lock().unwrap().call_end(name, c, t);
                for u in firsts {
                    let alias = engine.unit_alias(c.system, u).unwrap_or("").to_string();
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
                    MessageType::Affiliation | MessageType::Location | MessageType::Registration | MessageType::Deregistration | MessageType::UuVGrant | MessageType::UuAnsReq | MessageType::Grant | MessageType::Update
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
            Event::Concluded(k) => {
                let record: Value = serde_json::from_str(&k.json).unwrap_or(Value::Null);
                let bytes = 44 + k.audio.len() as u64 * 2;
                self.tally.add(format!("{sys}/audioBytes"), bytes);
                self.tally.add("all/audioBytes".into(), bytes);
                self.roll_day();
                self.day.audio_bytes += bytes;
                for i in record["errorList"].as_array().into_iter().flatten() {
                    self.tally.add(format!("{sys}/voice/frames"), i["frames"].as_u64().unwrap_or(0));
                    self.tally.add(format!("{sys}/voice/errors"), i["error_count"].as_u64().unwrap_or(0));
                    self.tally.add(format!("{sys}/voice/bad"), i["bad_frames"].as_u64().unwrap_or(0));
                }
                self.shared.radio.lock().unwrap().concluded(name, &record, t);
            }
            Event::NotSaved(_) => self.tally.add(format!("{sys}/notSaved"), 1),
            Event::Duplicate { .. } => self.tally.add(format!("{sys}/dup"), 1),
            _ => {}
        }
    }

    /// Once a second of wall clock (`now_ms`, the session's clock): the
    /// `stats` message, a [`Rollup`] each new minute, the detail messages
    /// someone is watching, and the edge-triggered events. `busy_ms`: the
    /// session's decoding time so far (the load). `system_name`: a call's
    /// system number's short name.
    pub fn tick(&mut self, now_ms: f64, busy_ms: f64, engine: &Engine, system_name: &dyn Fn(u16) -> String, out: &mut Vec<Output>) {
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
        for (i, m) in self.meters.iter_mut().enumerate() {
            m.report(&mut Scoped::new(&mut self.agg, format!("src/{}", self.names[i])));
        }
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
        out.push(Output::Text(json!({ "type": "stats", "t": (self.now * 10.0).round() / 10.0, "values": self.agg.values_json() }).to_string()));
        self.details(engine, system_name, out);
    }

    /// Events on a change of state: a control channel lost or regained, a
    /// source starting to drop samples or clip.
    fn edges(&mut self, engine: &Engine, out: &mut Vec<Output>) {
        for y in engine.status().systems {
            let Some(l) = self.agg.get(&format!("sys/{}/cc/locked", key_part(&y.short_name))) else { continue };
            let now = l > 0.5;
            let (name, freq_hz) = (y.short_name, y.control_channel_hz);
            let was = self.locked.insert(name.clone(), now);
            match (was, now) {
                (Some(true), false) => self.emit(MonitorEvent::ControlLost { system: name, freq_hz }, out),
                (Some(false), true) => self.emit(MonitorEvent::ControlRegained { system: name, freq_hz }, out),
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
    fn details(&self, engine: &Engine, system_name: &dyn Fn(u16) -> String, out: &mut Vec<Output>) {
        let rf: Vec<usize> = (0..self.names.len()).filter(|i| self.topics.has(&format!("rf:{i}"))).collect();
        let decode: Vec<&str> = self.topics.iter().filter_map(|t| t.strip_prefix("decode:")).collect();
        if rf.is_empty() && decode.is_empty() {
            return;
        }
        let channels = engine.channels();
        let r1 = |v: f64| (v * 10.0).round() / 10.0;
        let ch_json = |c: &trunk_core::trunk::ChannelSnapshot| {
            json!({
                "system": system_name(c.system), "freqHz": c.freq_hz, "source": c.source, "kind": c.kind,
                "powerDb": r1(c.power_db), "noiseDb": r1(c.noise_db), "snrDb": r1(c.power_db - c.noise_db),
                "offsetHz": c.offset_hz.map(|v| v.round()), "quality": c.quality.map(|v| r1(v as f64)), "calls": c.calls,
            })
        };
        for i in rf {
            let profile: Vec<f64> = engine.noise_profile(i, 64).into_iter().map(r1).collect();
            let chans: Vec<Value> = channels.iter().filter(|c| c.source == i).map(ch_json).collect();
            let topic = format!("rf:{i}");
            out.push(Output::Topic { text: json!({ "type": "rfDetail", "source": i, "profile": profile, "channels": chans }).to_string(), topic });
        }
        for name in decode {
            let chans: Vec<Value> = channels.iter().filter(|c| system_name(c.system) == name).map(ch_json).collect();
            out.push(Output::Topic { topic: format!("decode:{name}"), text: json!({ "type": "decodeDetail", "system": name, "channels": chans }).to_string() });
        }
    }
}

/// The talkgroup files of `cfg`'s systems, parsed: (short name → talkgroups),
/// for [`Names`].
pub fn talkgroup_tables(cfg: &Config) -> HashMap<String, trunk_core::trunk::Talkgroups> {
    let mut m = HashMap::new();
    for s in &cfg.systems {
        m.insert(s.short_name.clone(), trunk_core::trunk::parse_csv(&s.talkgroups_csv));
    }
    for c in &cfg.conventional {
        m.entry(c.short_name.clone()).or_insert_with(trunk_core::trunk::Talkgroups::new);
    }
    m
}

/// A talkgroup's name and Ignore flag from parsed tables.
pub fn tg_info(tables: &HashMap<String, trunk_core::trunk::Talkgroups>, system: &str, tg: u32) -> Option<TgInfo> {
    tables.get(system)?.get(&tg).map(|t| TgInfo { alpha_tag: t.alpha_tag.clone(), ignore: t.ignore })
}
