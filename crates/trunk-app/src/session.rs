//! One recording session, independent of platform: the engine plus what the
//! interface needs — status, spectrum, log lines, concluded calls (with their
//! files) and live audio — as [`Output`]s. The platform supplies samples, the
//! wall clock (`now_ms`), the local date for folder names, and does the I/O.

use serde_json::{json, Value};
use trunk_core::trunk::{Call, Engine, Event, Identity, MessageType};
use trunk_core::Complex32;

use crate::config::Config;
use trunk_recorder_plugin::{CallInfo, HostMessage, SystemStatus as PluginSystemStatus, UnitEvent};

pub enum Output {
    /// A JSON message for the interface.
    Text(String),
    /// Live audio frame `[2][u16 system][u32 call id][u32 talkgroup][i16…]`
    /// for talkgroup `tg` of `system` (an index into `status.systems`, or
    /// 65535 for a conventional channel).
    Audio { system: u16, tg: u32, frame: Vec<u8> },
    /// A concluded call to store at `<rel>.wav` / `<rel>.json` (relative to
    /// the recordings folder); `entry` is its history entry, also sent as a
    /// `concluded` message. `frames`: the frame capture, for
    /// `<rel>.frames.jsonl`.
    File { rel: String, system: u16, wav: Vec<u8>, json: String, frames: Option<String>, entry: Value },
    /// An event for plugins (only those [`Session::plugin_topics`] asks for).
    Plugin(HostMessage),
}

/// What plugins subscribe to, so nothing else is built.
#[derive(Clone, Copy, Debug, Default)]
pub struct PluginTopics {
    pub calls: bool,
    pub units: bool,
    pub status: bool,
}

/// Local calendar date (year, month, day) of a Unix time — Trunk Recorder's
/// folders are in local time, which only the platform knows.
pub type LocalYmd = fn(i64) -> (i32, u32, u32);

#[derive(Default, Clone)]
struct SourceStats {
    samples: u64,
    dropped: u64,
    errors: u64,
    last_error: Option<String>,
    rate_measured: f64,
    ended: bool,
}

pub struct Session {
    cfg: Config,
    engine: Engine,
    stats: Vec<SourceStats>,
    log: Vec<Value>,
    local_ymd: LocalYmd,
    busy_ms: f64,
    start_ms: Option<f64>,
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
}

impl Session {
    /// `bandplan`: the band plan saved for a system's short name ([`Session::bandplans`]).
    pub fn new(cfg: Config, epoch_ms: f64, bandplan: &dyn Fn(&str) -> Option<String>, local_ymd: LocalYmd) -> Result<Session, String> {
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
        Ok(Session {
            cfg,
            engine,
            stats: vec![SourceStats::default(); n],
            log: Vec::new(),
            local_ymd,
            busy_ms: 0.0,
            start_ms: None,
            last_status_ms: 0.0,
            last_spec_ms: 0.0,
            rate_mark: (0.0, vec![0; n]),
            want_audio: true,
            plugin_topics: PluginTopics::default(),
            epoch_ms,
            last_plugin_status_ms: 0.0,
            plugin_good: Vec::new(),
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Raw u8 IQ from `source`; `dropped`: samples the driver knows it lost.
    pub fn push(&mut self, source: usize, bytes: &[u8], dropped: u64) {
        self.engine.push_u8(source, bytes);
        let s = &mut self.stats[source];
        s.samples += bytes.len() as u64 / 2;
        s.dropped += dropped;
    }

    /// Float IQ from `source` (USRP, Airspy, float captures).
    pub fn push_iq(&mut self, source: usize, iq: &[Complex32], dropped: u64) {
        self.engine.push_iq(source, iq);
        let s = &mut self.stats[source];
        s.samples += iq.len() as u64;
        s.dropped += dropped;
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
    }

    /// A finite source ran out; true when all have.
    pub fn source_ended(&mut self, source: usize) -> bool {
        self.stats[source].ended = true;
        self.stats.iter().all(|s| s.ended)
    }

    /// Collect what happened since the last poll; status every 500 ms and
    /// spectra every 150 ms of wall clock.
    pub fn poll(&mut self, now_ms: f64, out: &mut Vec<Output>) {
        let start = *self.start_ms.get_or_insert(now_ms);
        for ev in self.engine.drain_events() {
            self.handle(ev, out);
        }
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

    /// Each system's (short name, band plan), to keep for the next run.
    pub fn bandplans(&self) -> Vec<(String, String)> {
        self.engine.systems().iter().enumerate().map(|(i, s)| (s.short_name.clone(), self.engine.bandplan(i))).collect()
    }

    /// A system's short name (a call's `system`; conventional channels' for [`CONVENTIONAL`]).
    fn system_name(&self, system: u16) -> &str {
        match self.engine.systems().get(system as usize) {
            Some(s) => &s.short_name,
            None => &self.cfg.conventional.short_name,
        }
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
        self.plugin_good.resize(st.systems.len(), 0);
        let systems = st
            .systems
            .iter()
            .enumerate()
            .map(|(i, y)| {
                let rate = if dt > 0.0 { y.good.saturating_sub(self.plugin_good[i]) as f64 / dt } else { 0.0 };
                self.plugin_good[i] = y.good;
                PluginSystemStatus {
                    index: i as u16,
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
                self.log.push(json!({ "timeS": m.time_s, "kind": m.kind.as_str(), "text": m.meta, "system": name }))
            }
            Event::ControlChannel { system, freq_hz } => {
                let name = self.system_name(system).to_string();
                self.log.push(json!({ "timeS": 0, "kind": "control", "text": format!("Control channel {:.5} MHz", freq_hz as f64 / 1e6), "system": name }))
            }
            Event::Note { system, text } => {
                let name = self.system_name(system).to_string();
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
                out.push(Output::Audio { system, tg: talkgroup, frame });
            }
            Event::Concluded(k) => {
                // Trunk Recorder's layout: <shortName>/<year>/<month>/<day>/, local time.
                let record: Value = serde_json::from_str(&k.json).unwrap_or(Value::Null);
                let (y, m, d) = (self.local_ymd)(record["start_time"].as_i64().unwrap_or(0));
                let rel = format!("{}/{y}/{m}/{d}/{}", k.short_name, k.base_name);
                let entry = json!({ "path": rel, "record": record });
                out.push(Output::File { rel, system: k.call.system, wav: trunk_core::wav::encode(&k.audio, 8000), json: k.json, frames: k.frames, entry: entry.clone() });
                out.push(Output::Text(json!({ "type": "concluded", "entry": entry }).to_string()));
            }
            Event::UnitAlias { system, unit, alias, talkgroup } => {
                let name = self.system_name(system).to_string();
                self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": "alias", "text": format!("Unit {unit} is \"{alias}\" (TG {talkgroup})"), "system": name }));
                out.push(Output::Text(json!({ "type": "unitAlias", "system": name, "unit": unit, "alias": alias }).to_string()));
            }
            Event::CallStart(c) if self.plugin_topics.calls => out.push(Output::Plugin(HostMessage::CallStart(self.call_info(&c)))),
            Event::CallEnd(c) if self.plugin_topics.calls => out.push(Output::Plugin(HostMessage::CallEnd(self.call_info(&c)))),
            Event::CallStart(_) | Event::CallUpdate(_) | Event::CallEnd(_) => {}
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
                json!({ "index": i, "label": label, "centerHz": sc.center_hz, "rateHz": sc.rate_hz, "rateMeasured": ss.rate_measured,
                        "dropped": ss.dropped, "errors": ss.errors, "lastError": ss.last_error, "ended": ss.ended })
            })
            .collect();
        let systems: Vec<Value> = st
            .systems
            .iter()
            .enumerate()
            .map(|(i, y)| {
                json!({
                    "index": i, "shortName": y.short_name, "nowS": y.now_s, "controlChannelHz": y.control_channel_hz,
                    "identity": identity_json(&y.identity),
                    "good": y.good, "bad": y.bad, "modulation": if y.modulation.is_empty() { None } else { Some(y.modulation) },
                    "activeCalls": y.active_calls, "recording": y.recording, "callsConcluded": y.calls_concluded,
                    "mismatch": y.mismatch,
                    "adjacent": y.adjacent.iter().map(|a| json!({ "sysId": a.sys_id, "rfss": a.rfss, "site": a.site, "freqHz": a.freq_hz })).collect::<Vec<_>>(),
                    "patches": y.patches.iter().map(|(sg, members)| {
                        let sys = self.engine.systems().iter().position(|x| x.short_name == y.short_name).unwrap_or(usize::MAX) as u16;
                        json!({ "supergroup": self.tg_names(sys, [*sg])[0], "members": self.tg_names(sys, members.iter().copied()) })
                    }).collect::<Vec<_>>(),
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
                .map(|c| call_view(c, self.system_name(c.system), self.tg_names(c.system, c.patched_talkgroups.iter().copied().filter(|&t| t != c.talkgroup))))
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

fn identity_json(id: &Identity) -> Value {
    json!({ "nac": id.nac, "wacn": id.wacn, "sysId": id.sys_id, "rfss": id.rfss, "site": id.site })
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
        "state": if c.recording { "recording" } else { "monitoring" },
        "reason": c.reason.map(|r| r.as_str()),
        "encrypted": c.encrypted,
        "emergency": c.emergency,
        "startS": c.start_s,
        "sources": c.sources.iter().map(|s| s.src).collect::<Vec<_>>(),
        "patched": patched,
    })
}
