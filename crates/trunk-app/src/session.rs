//! One recording session, independent of platform: the engine plus what the
//! interface needs — status, spectrum, log lines, concluded calls (with their
//! files) and live audio — as [`Output`]s. The platform supplies samples, the
//! wall clock (`now_ms`), the local date for folder names, and does the I/O.

use serde_json::{json, Value};
use trunk_core::trunk::{heard_code, Call, Engine, Event, IdField, Identity, MessageType, ProtocolStatus};

use crate::filename;
use crate::log::{Body, CallState, CallTag, Level, Record};
use crate::heard::HeardCodes;
use trunk_core::dsp::tones::Tone;
use trunk_core::Complex32;

use crate::config::{Config, Source};
use trunk_recorder_plugin::{CallInfo, HostMessage, SystemStatus as PluginSystemStatus, UnitEvent};

pub enum Output {
    /// A JSON message for the interface.
    Text(String),
    /// Live audio frame `[2][u16 system][u32 call id][u32 talkgroup][i16…]`
    /// for talkgroup `tg` of `system` — that system's number for this run
    /// (`status.systems[].index`, or 65535 and down for conventional
    /// systems) — whose short name, its identity, is `short_name`.
    Audio { system: u16, short_name: String, tg: u32, frame: Vec<u8> },
    /// A concluded call to store at `<rel>.wav` / `<rel>.json` (relative to
    /// the recordings folder); `entry` is its history entry, which the
    /// platform sends as a `concluded` message once the files are stored.
    /// `frames`: the frame capture, for `<rel>.frames.jsonl`.
    File { rel: String, system: u16, wav: Vec<u8>, json: String, frames: Option<String>, entry: Value },
    /// An event for plugins (only those [`Session::plugin_topics`] asks for).
    Plugin(HostMessage),
    /// A line for the log (the platform's logger formats and routes it).
    Log(Record),
}

/// Trunk Recorder's status summary comes this often (engine time), s.
const STATUS_EVERY_S: f64 = 200.0;
/// The control channel decode rate is checked over this long, s.
const RATE_EVERY_S: f64 = 10.0;
/// A running source that sends nothing for this long is fed silence in its
/// place ([`Engine::push_gap`]), so time goes on: calls on it end, and its
/// clock stays in step with the others'. ms of wall clock.
const QUIET_MS: f64 = 1000.0;
/// The most silence fed for one gap, s: a driver's count of samples lost.
const MAX_GAP_S: f64 = 10.0;
/// A source's clock is measured against the wall clock over this long:
/// the least lag in it (samples only ever arrive late) is the clock's
/// ([`ClockFit`]). ms.
const CLOCK_WINDOW_MS: f64 = 10_000.0;
/// Once set, a source's clock offset moves at most this much per second of
/// wall clock (1000 ppm: far more than a crystal's error, yet time never
/// runs backwards).
const CLOCK_SLEW: f64 = 0.001;

/// What plugins subscribe to, so nothing else is built.
#[derive(Clone, Copy, Debug, Default)]
pub struct PluginTopics {
    pub calls: bool,
    pub units: bool,
    pub status: bool,
}

/// Local time's offset from UTC at a Unix time, s (east positive) — Trunk
/// Recorder's folders are in local time, which only the platform knows.
pub type LocalOffset = fn(i64) -> i32;

#[derive(Default, Clone)]
struct SourceStats {
    samples: u64,
    dropped: u64,
    errors: u64,
    last_error: Option<String>,
    rate_measured: f64,
    ended: bool,
    /// Pushed to since the last poll.
    fed: bool,
    /// Wall clock (ms) when it was last fed or filled in.
    fed_ms: Option<f64>,
    /// Its sample clock against the wall clock (live sources only).
    clock: ClockFit,
}

/// A source's sample clock against the wall clock. Sample time runs slow or
/// fast by the radio's crystal error, and starts late by the time the radio
/// took to open; the offset that makes it wall time is measured here and
/// set on the engine ([`Engine::set_clock_offset`]): at once on the first
/// samples, then in small steps toward each window's measure.
#[derive(Default, Clone)]
struct ClockFit {
    /// Whether the wall clock means anything for it (not a capture played
    /// as fast as it can be).
    live: bool,
    /// Samples fed, silence filled in for gaps included.
    samples: u64,
    /// The least lag (wall time since the start, less sample time, s) seen
    /// in this window, and when the window began (ms).
    window_min: Option<f64>,
    window_ms: f64,
    /// The offset being steered to, and the one set.
    target: Option<f64>,
    applied: Option<f64>,
}

pub struct Session {
    cfg: Config,
    engine: Engine,
    stats: Vec<SourceStats>,
    log: Vec<Value>,
    /// Log records not yet handed out.
    records: Vec<Record>,
    local_offset: LocalOffset,
    busy_ms: f64,
    start_ms: Option<f64>,
    last_poll_ms: Option<f64>,
    last_status_ms: f64,
    last_spec_ms: f64,
    rate_mark: (f64, Vec<u64>),
    /// Build audio frames (skip when nobody is listening).
    pub want_audio: bool,
    pub plugin_topics: PluginTopics,
    /// Wall clock (Unix ms) at the engine's time 0.
    epoch_ms: f64,
    last_plugin_status_ms: f64,
    plugin_good: Vec<u64>,
    /// The codes conventional frequencies carried ([`crate::heard`]).
    heard: HeardCodes,
    /// Each system's control messages counted at (its time, the count), for the decode rate.
    rate_at: Vec<(f64, u64)>,
    next_status_s: f64,
}

impl Session {
    /// `bandplan`: the band plan saved for a system's short name ([`Session::bandplans`]).
    pub fn new(cfg: Config, epoch_ms: f64, bandplan: &dyn Fn(&str) -> Option<String>, local_offset: LocalOffset) -> Result<Session, String> {
        if let Some(p) = cfg.problem() {
            return Err(p);
        }
        let mut engine = Engine::new(cfg.engine_config(epoch_ms))?;
        let names: Vec<String> = engine.systems().iter().map(|s| s.short_name.clone()).collect();
        for (i, n) in names.iter().enumerate() {
            if let Some(b) = bandplan(n) {
                engine.load_bandplan(i, &b);
            }
        }
        let n = cfg.sources.len();
        // (A capture played as fast as it can be has no wall clock to keep to.)
        let stats = cfg
            .sources
            .iter()
            .map(|s| SourceStats { clock: ClockFit { live: !matches!(s, Source::File { realtime: false, .. }), ..Default::default() }, ..Default::default() })
            .collect();
        Ok(Session {
            cfg,
            engine,
            stats,
            log: Vec::new(),
            records: Vec::new(),
            local_offset,
            busy_ms: 0.0,
            start_ms: None,
            last_poll_ms: None,
            last_status_ms: 0.0,
            last_spec_ms: 0.0,
            rate_mark: (0.0, vec![0; n]),
            want_audio: true,
            plugin_topics: PluginTopics::default(),
            epoch_ms,
            last_plugin_status_ms: 0.0,
            plugin_good: Vec::new(),
            heard: HeardCodes::default(),
            rate_at: Vec::new(),
            next_status_s: STATUS_EVERY_S,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Raw u8 IQ from `source`; `dropped`: samples the driver knows it lost
    /// before these (fed as silence, so time keeps up).
    pub fn push(&mut self, source: usize, bytes: &[u8], dropped: u64) {
        self.gap(source, dropped);
        self.engine.push_u8(source, bytes);
        let s = &mut self.stats[source];
        s.samples += bytes.len() as u64 / 2;
        s.clock.samples += bytes.len() as u64 / 2;
        s.fed = true;
    }

    /// Float IQ from `source` (USRP, Airspy, float captures).
    pub fn push_iq(&mut self, source: usize, iq: &[Complex32], dropped: u64) {
        self.gap(source, dropped);
        self.engine.push_iq(source, iq);
        let s = &mut self.stats[source];
        s.samples += iq.len() as u64;
        s.clock.samples += iq.len() as u64;
        s.fed = true;
    }

    /// `dropped` samples of `source` lost: counted, and fed as silence.
    fn gap(&mut self, source: usize, dropped: u64) {
        if dropped == 0 {
            return;
        }
        self.stats[source].dropped += dropped;
        let most = (MAX_GAP_S * self.engine.sources()[source].rate_hz) as u64;
        self.engine.push_gap(source, dropped.min(most));
        self.stats[source].clock.samples += dropped.min(most);
    }

    /// Fill in for sources that went quiet while running (unplugged, a
    /// driver reopening): silence for the time they've sent nothing.
    fn fill_quiet(&mut self, now_ms: f64) {
        for i in 0..self.stats.len() {
            let s = &mut self.stats[i];
            if std::mem::take(&mut s.fed) || s.fed_ms.is_none() {
                // (Not before its first samples: a radio still opening isn't a gap.)
                if s.samples > 0 {
                    s.fed_ms = Some(now_ms);
                }
                continue;
            }
            let since = s.fed_ms.unwrap_or(now_ms);
            if s.ended || now_ms - since < QUIET_MS {
                continue;
            }
            s.fed_ms = Some(now_ms);
            let n = ((now_ms - since) / 1000.0).min(MAX_GAP_S) * self.engine.sources()[i].rate_hz;
            self.engine.push_gap(i, n as u64);
            self.stats[i].clock.samples += n as u64;
        }
    }

    /// Measure each live source's clock against the wall clock, and steer
    /// its offset ([`ClockFit`]). `dt`: wall seconds since the last poll.
    fn steer_clocks(&mut self, now_ms: f64, dt: f64) {
        let since_epoch = (now_ms - self.epoch_ms) / 1000.0;
        for i in 0..self.stats.len() {
            let rate = self.engine.sources()[i].rate_hz;
            let (fed, c) = (self.stats[i].fed, &mut self.stats[i].clock);
            if !c.live || c.samples == 0 {
                continue;
            }
            if fed {
                // Samples only ever arrive late (buffers, a busy engine): the
                // least lag over a window is the clock's own.
                let lag = since_epoch - c.samples as f64 / rate;
                c.window_min = Some(c.window_min.map_or(lag, |m| m.min(lag)));
                if c.applied.is_none() {
                    // The first samples: set at once (the radio's open time).
                    c.target = Some(lag);
                    c.window_ms = now_ms;
                } else if now_ms - c.window_ms >= CLOCK_WINDOW_MS {
                    c.target = c.window_min.take();
                    c.window_ms = now_ms;
                }
            }
            let Some(target) = c.target else { continue };
            let next = match c.applied {
                None => target,
                Some(a) => a + (target - a).clamp(-CLOCK_SLEW * dt, CLOCK_SLEW * dt),
            };
            if c.applied != Some(next) {
                c.applied = Some(next);
                self.engine.set_clock_offset(i, next);
            }
        }
    }

    /// Time spent decoding (for the load figure), measured by the caller.
    pub fn add_busy_ms(&mut self, ms: f64) {
        self.busy_ms += ms;
    }

    pub fn source_error(&mut self, source: usize, error: &str) {
        let s = &mut self.stats[source];
        s.errors += 1;
        s.last_error = Some(error.to_string());
        self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": "error", "text": format!("source {source}: {error}") }));
        self.records.push(Record::text(Level::Error, None, format!("Source {source}: {error}")));
    }

    /// A finite source ran out; true when all have.
    pub fn source_ended(&mut self, source: usize) -> bool {
        self.stats[source].ended = true;
        self.stats.iter().all(|s| s.ended)
    }

    /// Collect what happened since the last poll; status every 500 ms and
    /// spectra every 150 ms of wall clock. `now_ms`: the wall clock, Unix
    /// ms, on the same clock as [`Session::new`]'s `epoch_ms` and never
    /// jumping (the epoch plus a monotonic clock): sources' clocks are
    /// measured against it.
    pub fn poll(&mut self, now_ms: f64, out: &mut Vec<Output>) {
        let start = *self.start_ms.get_or_insert(now_ms);
        let dt = (now_ms - self.last_poll_ms.unwrap_or(now_ms)).max(0.0) / 1000.0;
        self.last_poll_ms = Some(now_ms);
        self.steer_clocks(now_ms, dt);
        self.fill_quiet(now_ms);
        for ev in self.engine.drain_events() {
            self.handle(ev, out);
        }
        self.log_rates_and_status(out);
        out.extend(self.records.drain(..).map(Output::Log));
        if now_ms - self.last_spec_ms >= 150.0 {
            self.last_spec_ms = now_ms;
            for (i, s) in self.engine.sources().iter().enumerate() {
                let bins: Vec<f32> = self.engine.spectrum(i, 512).iter().map(|v| (v * 10.0).round() / 10.0).collect();
                out.push(Output::Text(json!({ "type": "spectrum", "source": i, "centerHz": s.center_hz, "rateHz": s.rate_hz, "bins": bins }).to_string()));
            }
        }
        if now_ms - self.last_status_ms >= 500.0 {
            self.last_status_ms = now_ms;
            let dt = (now_ms - self.rate_mark.0) / 1000.0;
            if dt >= 1.0 {
                for (i, s) in self.stats.iter_mut().enumerate() {
                    s.rate_measured = (s.samples - self.rate_mark.1[i]) as f64 / dt;
                    self.rate_mark.1[i] = s.samples;
                }
                self.rate_mark.0 = now_ms;
            }
            self.flush_log(out);
            if self.heard.take_unshown() {
                out.push(Output::Text(json!({ "type": "heard", "heard": self.heard.json() }).to_string()));
            }
            if self.plugin_topics.status && now_ms - self.last_plugin_status_ms >= 5000.0 {
                let dt = if self.last_plugin_status_ms == 0.0 { 0.0 } else { (now_ms - self.last_plugin_status_ms) / 1000.0 };
                self.last_plugin_status_ms = now_ms;
                out.push(Output::Plugin(HostMessage::Status(self.plugin_status(dt))));
            }
            let load = self.busy_ms / (now_ms - start).max(1.0);
            out.push(Output::Text(self.status_json(load).to_string()));
        }
    }

    /// End of recording: release held frames, end every call.
    pub fn finish(&mut self, out: &mut Vec<Output>) {
        self.engine.finish();
        for ev in self.engine.drain_events() {
            self.handle(ev, out);
        }
        out.extend(self.records.drain(..).map(Output::Log));
        self.flush_log(out);
    }

    /// Preload each system's talker aliases: `units` gives the CSV saved
    /// for a short name ([`Session::units_changed`]).
    pub fn load_units(&mut self, units: &dyn Fn(&str) -> Option<String>) {
        for n in self.engine.unit_table_names() {
            if let Some(csv) = units(&n) {
                self.engine.load_units(&n, &csv);
            }
        }
    }

    /// (short name, talker alias CSV) of each system that learned an alias
    /// since the last call, to save.
    pub fn units_changed(&mut self) -> Vec<(String, String)> {
        self.engine.units_changed()
    }

    /// Preload the codes conventional frequencies carried before (what
    /// [`Session::heard_unsaved`] gave, saved as [`Session::heard_file`]).
    pub fn load_heard(&mut self, json: &str) {
        self.heard = HeardCodes::load(json);
    }

    /// The codes conventional frequencies carried, as JSON to save, when
    /// they changed since the last call.
    pub fn heard_unsaved(&mut self) -> Option<String> {
        self.heard.take_unsaved()
    }

    /// The file name (beside the config) the codes are kept in.
    pub fn heard_file(_cfg: &Config) -> String {
        "conventional.heard.json".into()
    }

    /// Each system's (short name, band plan), to keep for the next run.
    pub fn bandplans(&self) -> Vec<(String, String)> {
        self.engine.systems().iter().enumerate().map(|(i, s)| (s.short_name.clone(), self.engine.bandplan(i))).collect()
    }

    /// A system's short name (a call's `system`: a trunked or a conventional system's).
    fn system_name(&self, system: u16) -> &str {
        self.cfg.short_name_of(system).unwrap_or("conv")
    }

    /// Talkgroups as the interface shows them: number and alpha tag (from system `system`'s file; "" when not in it).
    fn tg_names(&self, system: u16, tgs: impl IntoIterator<Item = u32>) -> Vec<Value> {
        let file = self.engine.systems().get(system as usize).map(|s| &s.talkgroups);
        tgs.into_iter()
            .map(|tg| json!({ "talkgroup": tg, "alphaTag": file.and_then(|f| f.get(&tg)).map_or("", |t| t.alpha_tag.as_str()) }))
            .collect()
    }

    /// Unix seconds of engine time `s`.
    fn wall(&self, s: f64) -> f64 {
        self.epoch_ms / 1000.0 + s
    }

    /// The log header for a call.
    fn call_tag(c: &Call) -> CallTag {
        CallTag {
            num: c.id as u64,
            talkgroup: c.talkgroup,
            tag: c.talkgroup_info.as_ref().map_or(String::new(), |t| t.alpha_tag.clone()),
            encrypted: c.encrypted,
            freq_hz: c.freq_hz as f64,
        }
    }

    fn call_record(&self, level: Level, c: &Call, body: Body) -> Record {
        Record { level, system: Some(self.system_name(c.system).to_string()), call: Some(Self::call_tag(c)), body }
    }

    /// Trunk Recorder's control channel decode rate check, and its status summary.
    fn log_rates_and_status(&mut self, _out: &mut [Output]) {
        let st = self.engine.status();
        let warn = self.cfg.log.control_warn_rate;
        self.rate_at.resize(self.engine.systems().len(), (f64::NAN, 0));
        for y in &st.systems {
            let i = y.system as usize;
            let (t0, n0) = self.rate_at[i];
            if t0.is_nan() || y.now_s < t0 {
                self.rate_at[i] = (y.now_s, y.good);
                continue;
            }
            if y.now_s - t0 < RATE_EVERY_S {
                continue;
            }
            let per_s = y.good.saturating_sub(n0) as f64 / (y.now_s - t0);
            self.rate_at[i] = (y.now_s, y.good);
            let level = if per_s < warn { Level::Error } else if warn == -1.0 { Level::Info } else { continue };
            let body = Body::DecodeRate { freq_hz: y.control_channel_hz.map(|f| f as f64), per_s, count: y.good - n0 };
            self.records.push(Record { level, system: Some(y.short_name.clone()), call: None, body });
        }
        if st.now_s < self.next_status_s {
            return;
        }
        self.next_status_s = st.now_s + STATUS_EVERY_S;
        let calls = self.engine.active_calls();
        self.records.push(Record::text(Level::Info, None, format!("Active Calls: {}", calls.len())));
        for c in &calls {
            let state = if c.recording { CallState::Recording } else { CallState::Monitoring(c.reason.map_or("", |r| r.as_str())) };
            let r = self.call_record(Level::Info, c, Body::State { elapsed_s: (st.now_s - c.start_s).max(0.0), state });
            self.records.push(r);
        }
        self.records.push(Record::text(Level::Info, None, format!("Recorders: {} recording, {} channels open", st.recording, st.channels_open)));
        self.records.push(Record::text(Level::Info, None, "Control Channel Decode Rates:"));
        for y in &st.systems {
            let freq = y.control_channel_hz.map_or("-".to_string(), |f| self.cfg.log.format().freq(f as f64));
            let rate = if y.now_s > 0.0 { y.good as f64 / y.now_s } else { 0.0 };
            self.records.push(Record::text(Level::Info, Some(&y.short_name), format!("{freq}\t{rate:.1} msg/sec")));
        }
    }

    fn call_info(&self, c: &Call) -> CallInfo {
        CallInfo {
            id: c.id,
            system: c.system,
            short_name: self.system_name(c.system).to_string(),
            talkgroup: c.talkgroup,
            talkgroup_tag: c.talkgroup_info.as_ref().map_or(String::new(), |t| t.alpha_tag.clone()),
            freq_hz: c.freq_hz,
            tdma_slot: c.phase2_tdma.then_some(c.tdma_slot),
            analog: c.analog,
            encrypted: c.encrypted,
            emergency: c.emergency,
            recording: c.recording,
            reason: c.reason.map(|r| r.as_str().to_string()),
            start_time: self.wall(c.start_s),
            units: c.sources.iter().map(|s| s.src).collect(),
            patched_talkgroups: c.patched_talkgroups.clone(),
        }
    }

    /// `dt`: seconds since the last one (0: the first).
    fn plugin_status(&mut self, dt: f64) -> trunk_recorder_plugin::Status {
        let st = self.engine.status();
        self.plugin_good.resize(self.engine.systems().len(), 0);
        let systems = st
            .systems
            .iter()
            .map(|y| {
                let i = y.system as usize;
                let rate = if dt > 0.0 { y.good.saturating_sub(self.plugin_good[i]) as f64 / dt } else { 0.0 };
                self.plugin_good[i] = y.good;
                PluginSystemStatus {
                    index: y.system,
                    short_name: y.short_name.clone(),
                    control_channel_hz: y.control_channel_hz,
                    decode_rate: (rate * 10.0).round() / 10.0,
                    active_calls: y.active_calls as u32,
                    recording: y.recording as u32,
                }
            })
            .collect();
        trunk_recorder_plugin::Status { time: self.wall(st.now_s), systems }
    }

    fn flush_log(&mut self, out: &mut Vec<Output>) {
        if !self.log.is_empty() {
            out.push(Output::Text(json!({ "type": "log", "lines": std::mem::take(&mut self.log) }).to_string()));
        }
    }

    fn handle(&mut self, ev: Event, out: &mut Vec<Output>) {
        match ev {
            Event::Message { system, msg: m } => {
                let name = self.system_name(system).to_string();
                if self.plugin_topics.units {
                    if let Some(kind) = unit_kind(m.kind) {
                        let talkgroup = matches!(kind, "affiliation" | "location" | "answer_request" | "call_alert").then_some(m.talkgroup);
                        if m.source >= 0 {
                            let e = UnitEvent { system, short_name: name.clone(), kind: kind.into(), unit: m.source as u32, talkgroup, time: self.wall(m.time_s) };
                            out.push(Output::Plugin(HostMessage::Unit(e)));
                        }
                    }
                }
                self.records.push(Record::text(Level::Trace, Some(&name), m.meta.clone()));
                self.log.push(json!({ "timeS": m.time_s, "kind": m.kind.as_str(), "text": m.meta, "system": name }))
            }
            Event::ControlChannel { system, freq_hz } => {
                let name = self.system_name(system).to_string();
                self.records.push(Record { level: Level::Info, system: Some(name.clone()), call: None, body: Body::ControlChannel { freq_hz: freq_hz as f64 } });
                self.log.push(json!({ "timeS": 0, "kind": "control", "text": format!("Control channel {:.5} MHz", freq_hz as f64 / 1e6), "system": name }))
            }
            Event::Note { system, text } => {
                let name = self.system_name(system).to_string();
                self.records.push(Record::text(Level::Warning, Some(&name), text.clone()));
                self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": "error", "text": text, "system": name }))
            }
            Event::Audio { call_id, system, talkgroup, samples } => {
                if !self.want_audio {
                    return;
                }
                let mut frame = Vec::with_capacity(11 + samples.len() * 2);
                frame.push(2u8);
                frame.extend_from_slice(&system.to_le_bytes());
                frame.extend_from_slice(&call_id.to_le_bytes());
                frame.extend_from_slice(&talkgroup.to_le_bytes());
                for s in samples {
                    frame.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                out.push(Output::Audio { system, short_name: self.system_name(system).to_string(), tg: talkgroup, frame });
            }
            Event::ConvSkipped { freq_hz, code } => {
                let ms = self.wall(self.engine.status().now_s) * 1000.0;
                self.heard.note(freq_hz, &code, false, ms);
            }
            Event::Concluded(k) => {
                if let Some(code) = heard_code(&k.call) {
                    self.heard.note(k.call.freq_hz, &code, true, self.wall(k.call.start_s) * 1000.0);
                }
                // Trunk Recorder's layout: <shortName>/<year>/<month>/<day>/, local time; or the system's filename format.
                let record: Value = serde_json::from_str(&k.json).unwrap_or(Value::Null);
                let start = record["start_time"].as_i64().unwrap_or(0);
                let offset = (self.local_offset)(start);
                let format = self.cfg.recording_of(k.call.system).filename_format;
                let rel = if format.trim().is_empty() {
                    filename::default_path(&k.short_name, &k.base_name, start, offset)
                } else {
                    filename::render(&format, &record, k.call.system, offset)
                };
                let entry = json!({ "path": rel, "record": record });
                out.push(Output::File { rel, system: k.call.system, wav: trunk_core::wav::encode(&k.audio, 8000), json: k.json, frames: k.frames, entry });
                let f = |key: &str| record[key].as_f64();
                let body = Body::Concluded { length_s: k.audio.len() as f64 / 8000.0, signal_db: f("signal"), noise_db: f("noise"), snr_db: f("snr"), clean_pct: f("clean_voice_pct") };
                let r = self.call_record(Level::Info, &k.call, body);
                self.records.push(r);
            }
            Event::UnitAlias { system, unit, alias, talkgroup } => {
                let name = self.system_name(system).to_string();
                self.records.push(Record::text(Level::Info, Some(&name), format!("Unit {unit} is \"{alias}\" (TG {talkgroup})")));
                self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": "alias", "text": format!("Unit {unit} is \"{alias}\" (TG {talkgroup})"), "system": name }));
                out.push(Output::Text(json!({ "type": "unitAlias", "system": name, "unit": unit, "alias": alias }).to_string()));
            }
            Event::Duplicate { call, kept } => {
                let tag = call.talkgroup_info.as_ref().map_or(String::new(), |t| format!(" {}", t.alpha_tag));
                let text = format!(
                    "TG {}{tag} was heard on {} and {}: saved {}'s copy",
                    call.talkgroup,
                    self.system_name(kept.system),
                    self.system_name(call.system),
                    self.system_name(kept.system)
                );
                self.records.push(Record::text(Level::Info, Some(self.system_name(call.system)), text.clone()));
                self.log.push(json!({ "timeS": call.start_s, "kind": "duplicate", "text": text, "system": self.system_name(call.system) }));
            }
            Event::CallStart(c) => {
                let r = match c.reason {
                    _ if c.recording => {
                        let kind = if c.analog { "Analog" } else if c.color_code.is_some() { "DMR" } else if c.phase2_tdma { "P25 Phase 2" } else { "P25" };
                        let slot = (c.phase2_tdma || c.color_code.is_some()).then_some(c.tdma_slot);
                        self.call_record(Level::Info, &c, Body::Recording { kind, slot })
                    }
                    Some(why) => {
                        let level = match why.as_str() {
                            "no_source" => Level::Error,
                            "no_recorder" => Level::Warning,
                            _ => Level::Info,
                        };
                        self.call_record(level, &c, Body::NotRecording(why.as_str()))
                    }
                    None => self.call_record(Level::Debug, &c, Body::Text("Following (not recorded)".into())),
                };
                self.records.push(r);
                if self.plugin_topics.calls {
                    out.push(Output::Plugin(HostMessage::CallStart(self.call_info(&c))));
                }
            }
            Event::NotSaved(c) => {
                let r = self.call_record(Level::Info, &c, Body::Dropped);
                self.records.push(r);
            }
            Event::CallEnd(c) => {
                if self.plugin_topics.calls {
                    out.push(Output::Plugin(HostMessage::CallEnd(self.call_info(&c))));
                }
            }
            Event::CallUpdate(_) => {}
        }
    }

    fn status_json(&self, load: f64) -> Value {
        let st = self.engine.status();
        let sources: Vec<Value> = self
            .cfg
            .sources
            .iter()
            .zip(self.engine.sources())
            .zip(&self.stats)
            .enumerate()
            .map(|(i, ((s, sc), ss))| {
                let label = s.label();
                let tune = st.sources.get(i).copied().unwrap_or_default();
                json!({ "index": i, "label": label, "centerHz": sc.center_hz, "rateHz": sc.rate_hz, "rateMeasured": ss.rate_measured,
                        "dropped": ss.dropped, "errors": ss.errors, "lastError": ss.last_error, "ended": ss.ended,
                        "errorPpm": tune.error_ppm, "tunePpm": tune.applied_ppm })
            })
            .collect();
        let systems: Vec<Value> = st
            .systems
            .iter()
            .map(|y| {
                let sys = y.system;
                json!({
                    "index": sys, "shortName": y.short_name, "nowS": y.now_s, "controlChannelHz": y.control_channel_hz,
                    "identity": identity_json(&y.identity),
                    "good": y.good, "bad": y.bad, "modulation": if y.modulation.is_empty() { None } else { Some(y.modulation) },
                    "activeCalls": y.active_calls, "recording": y.recording, "callsConcluded": y.calls_concluded,
                    "mismatch": y.mismatch, "siteGroup": y.site_group,
                    "adjacent": y.adjacent.iter().map(|a| json!({ "sysId": a.sys_id, "rfss": a.rfss, "site": a.site, "freqHz": a.freq_hz })).collect::<Vec<_>>(),
                    "patches": y.patches.iter().map(|(sg, members)| {
                        json!({ "supergroup": self.tg_names(sys, [*sg])[0], "members": self.tg_names(sys, members.iter().copied()) })
                    }).collect::<Vec<_>>(),
                    "dmr": match &y.protocol { ProtocolStatus::Dmr(d) => Some(d), _ => None }.map(|d| {
                        json!({
                            "variant": d.variant.map(|v| v.name()),
                            "colorCode": d.color_code,
                            "rest": d.rest.map(|(lsn, hz)| json!({ "lsn": lsn, "freqHz": if hz > 0 { Some(hz) } else { None } })),
                            "keyed": d.keyed,
                            "channels": d.channels.iter().map(|c| json!({ "lcn": c.lcn, "freqHz": c.hz, "configured": c.configured })).collect::<Vec<_>>(),
                            "carriers": d.carriers.iter().map(|c| json!({
                                "freqHz": c.hz, "control": c.control, "colorCode": c.color_code,
                                "slots": c.slots.iter().map(|s| s.map(|(tg, src)| json!({ "talkgroup": self.tg_names(sys, [tg])[0], "source": src }))).collect::<Vec<_>>(),
                            })).collect::<Vec<_>>(),
                        })
                    }),
                })
            })
            .collect();
        json!({
            "type": "status",
            "status": {
                "nowS": st.now_s,
                "activeCalls": st.active_calls, "recording": st.recording, "channelsOpen": st.channels_open, "conventionalOpen": st.conventional_open,
                "callsConcluded": st.calls_concluded,
                "systems": systems,
            },
            "sources": sources,
            "load": load,
            "calls": self
                .engine
                .active_calls()
                .iter()
                .map(|c| {
                    let mut v = call_view(c, self.system_name(c.system), self.tg_names(c.system, c.patched_talkgroups.iter().copied().filter(|&t| t != c.talkgroup)));
                    let twins: Vec<&str> = self.engine.twins(c.id).iter().map(|&(_, sys)| self.system_name(sys)).collect();
                    if !twins.is_empty() {
                        v["alsoOn"] = json!(twins);
                    }
                    v
                })
                .collect::<Vec<_>>(),
        })
    }
}

/// The unit events plugins hear about, by their name there.
fn unit_kind(k: MessageType) -> Option<&'static str> {
    Some(match k {
        MessageType::Registration => "registration",
        MessageType::Deregistration => "deregistration",
        MessageType::Affiliation => "affiliation",
        MessageType::Acknowledge => "acknowledge",
        MessageType::Location => "location",
        MessageType::DataGrant => "data_grant",
        MessageType::UuAnsReq => "answer_request",
        MessageType::CallAlert => "call_alert",
        _ => return None,
    })
}

/// `{nac, wacn, sysId, rfss, site}`: every field, null when not stated.
pub(crate) fn identity_json(id: &Identity) -> Value {
    Value::Object(IdField::ALL.into_iter().map(|f| (f.key().to_string(), json!(id.get(f)))).collect())
}

/// `patched`: the talkgroups patched with it ([`Session::tg_names`]).
fn call_view(c: &Call, system_name: &str, patched: Vec<Value>) -> Value {
    json!({
        "id": c.id,
        // (65535: a conventional channel)
        "system": c.system,
        "systemName": system_name,
        "talkgroup": c.talkgroup,
        "alphaTag": c.talkgroup_info.as_ref().map_or("", |t| t.alpha_tag.as_str()),
        "freqHz": c.freq_hz,
        "slot": if c.phase2_tdma { Some(c.tdma_slot) } else { None },
        "analog": c.analog,
        // "151.4 Hz" or "D023N".
        "tone": c.tone.map(|h| match h.tone {
            Tone::Ctcss(_) => format!("{} Hz", h.tone),
            Tone::Dcs(..) => h.tone.to_string(),
        }),
        "state": if c.recording { "recording" } else { "monitoring" },
        "reason": c.reason.map(|r| r.as_str()),
        "encrypted": c.encrypted,
        "emergency": c.emergency,
        "startS": c.start_s,
        "sources": c.sources.iter().map(|s| s.src).collect::<Vec<_>>(),
        "patched": patched,
    })
}
