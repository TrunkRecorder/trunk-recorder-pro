//! One recording session, independent of platform: the engine plus what the
//! interface needs — status, spectrum, log lines, concluded calls (with their
//! files) and live audio — as [`Output`]s. The platform supplies samples, the
//! wall clock (`now_ms`), the local date for folder names, and does the I/O.

use serde_json::{json, Value};
use trunk_core::trunk::{parse_csv, Call, Engine, Event};
use trunk_core::Complex32;

use crate::config::Config;

pub enum Output {
    /// A JSON message for the interface.
    Text(String),
    /// Live audio frame `[1][u32 call id][u32 talkgroup][i16…]` for talkgroup `tg`.
    Audio { tg: u32, frame: Vec<u8> },
    /// A concluded call to store at `<rel>.wav` / `<rel>.json` (relative to
    /// the recordings folder); `entry` is its history entry, also sent as a
    /// `concluded` message.
    File { rel: String, wav: Vec<u8>, json: String, entry: Value },
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
}

impl Session {
    pub fn new(cfg: Config, epoch_ms: f64, bandplan: Option<&str>, local_ymd: LocalYmd) -> Result<Session, String> {
        if let Some(p) = cfg.problem() {
            return Err(p);
        }
        let mut engine = Engine::new(cfg.engine_config(epoch_ms), parse_csv(&cfg.system.talkgroups_csv))?;
        if let Some(b) = bandplan {
            engine.load_bandplan(b);
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

    /// The band plan, to keep for the next run.
    pub fn bandplan(&self) -> String {
        self.engine.bandplan()
    }

    fn flush_log(&mut self, out: &mut Vec<Output>) {
        if !self.log.is_empty() {
            out.push(Output::Text(json!({ "type": "log", "lines": std::mem::take(&mut self.log) }).to_string()));
        }
    }

    fn handle(&mut self, ev: Event, out: &mut Vec<Output>) {
        match ev {
            Event::Message(m) => self.log.push(json!({ "timeS": m.time_s, "kind": m.kind.as_str(), "text": m.meta })),
            Event::ControlChannel { freq_hz } => {
                self.log.push(json!({ "timeS": 0, "kind": "control", "text": format!("Control channel {:.5} MHz", freq_hz as f64 / 1e6) }))
            }
            Event::Audio { call_id, talkgroup, samples } => {
                if !self.want_audio {
                    return;
                }
                let mut frame = Vec::with_capacity(9 + samples.len() * 2);
                frame.push(1u8);
                frame.extend_from_slice(&call_id.to_le_bytes());
                frame.extend_from_slice(&talkgroup.to_le_bytes());
                for s in samples {
                    frame.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                out.push(Output::Audio { tg: talkgroup, frame });
            }
            Event::Concluded(k) => {
                // Trunk Recorder's layout: <shortName>/<year>/<month>/<day>/, local time.
                let record: Value = serde_json::from_str(&k.json).unwrap_or(Value::Null);
                let (y, m, d) = (self.local_ymd)(record["start_time"].as_i64().unwrap_or(0));
                let rel = format!("{}/{y}/{m}/{d}/{}", self.cfg.system.short_name, k.base_name);
                let entry = json!({ "path": rel, "record": record });
                out.push(Output::File { rel, wav: trunk_core::wav::encode(&k.audio, 8000), json: k.json, entry: entry.clone() });
                out.push(Output::Text(json!({ "type": "concluded", "entry": entry }).to_string()));
            }
            Event::CallStart(_) | Event::CallUpdate(_) | Event::CallEnd(_) => {}
        }
    }

    fn status_json(&self, load: f64) -> Value {
        let st = self.engine.status();
        let id = &st.identity;
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
        json!({
            "type": "status",
            "status": {
                "nowS": st.now_s, "controlChannelHz": st.control_channel_hz,
                "identity": { "nac": id.nac, "wacn": id.wacn, "sysId": id.sys_id, "rfss": id.rfss, "site": id.site },
                "good": st.good, "bad": st.bad, "modulation": if st.modulation.is_empty() { None } else { Some(st.modulation) },
                "activeCalls": st.active_calls, "recording": st.recording, "channelsOpen": st.channels_open, "conventionalOpen": st.conventional_open,
                "callsConcluded": st.calls_concluded,
            },
            "sources": sources,
            "load": load,
            "calls": self.engine.active_calls().iter().map(call_view).collect::<Vec<_>>(),
        })
    }
}

fn call_view(c: &Call) -> Value {
    json!({
        "id": c.id,
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
    })
}
