//! A running recorder: one thread per source (RTL-SDR or capture file) and an
//! engine thread that decodes, writes calls to disk in Trunk Recorder's layout
//! (`<captureDir>/<shortName>/<YYYY>/<M>/<D>/<tg>-<epoch>_<freq>.wav|json`) and
//! publishes what happens to the browser interface through the [`Hub`].

use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Local, TimeZone};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use trunk_core::trunk::{parse_csv, Call, Engine, Event};

use crate::config::{Config, Source};
use crate::sdr::{self, RtlConfig, SourceMsg};

/// A message for every connected browser.
pub enum Out {
    Text(String),
    /// Live audio (binary frame) for talkgroup `tg`.
    Audio { tg: u32, frame: Vec<u8> },
}

pub type Hub = broadcast::Sender<Arc<Out>>;

pub fn publish(hub: &Hub, v: Value) {
    let _ = hub.send(Arc::new(Out::Text(v.to_string())));
}

#[derive(Clone, Debug, Default)]
pub struct PhaseInfo {
    /// "idle" | "starting" | "running" | "stopping"
    pub phase: &'static str,
    pub error: Option<String>,
    pub ended: bool,
}

impl PhaseInfo {
    pub fn to_json(&self) -> Value {
        json!({ "type": "state", "phase": self.phase, "error": self.error, "ended": self.ended })
    }
}

/// Shared between the web server and the runtime.
pub struct Ctx {
    pub config_path: PathBuf,
    pub config: Mutex<Config>,
    pub hub: Hub,
    pub runner: Mutex<Option<Runner>>,
    pub phase: Mutex<PhaseInfo>,
    /// Recently concluded calls (newest first), as sent to the browser.
    pub history: Mutex<VecDeque<Value>>,
}

impl Ctx {
    pub fn set_phase(&self, phase: &'static str, error: Option<String>, ended: bool) {
        let p = PhaseInfo { phase, error, ended };
        publish(&self.hub, p.to_json());
        *self.phase.lock().unwrap() = p;
    }
}

pub struct Runner {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Runner {
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

/// Start recording with `cfg`. The engine thread reports the phase.
pub fn start(ctx: Arc<Ctx>, cfg: Config) -> Result<Runner, String> {
    if let Some(p) = cfg.problem() {
        return Err(p);
    }
    let epoch_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64);
    let talkgroups = parse_csv(&cfg.system.talkgroups_csv);
    let mut engine = Engine::new(cfg.engine_config(epoch_ms), talkgroups)?;
    let bandplan_path = crate::config::config_dir().join(format!("{}.bandplan", cfg.system.short_name));
    if let Ok(s) = fs::read_to_string(&bandplan_path) {
        engine.load_bandplan(&s);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
    let centers = cfg.resolved_centers();
    let mut threads = Vec::new();
    for (i, (src, center)) in cfg.sources.iter().zip(&centers).enumerate() {
        let (tx, stop) = (tx.clone(), stop.clone());
        let src = src.clone();
        let center = *center;
        threads.push(std::thread::Builder::new().name(format!("source-{i}")).spawn(move || match src {
            Source::Rtlsdr { serial, rate_hz, gain_db, ppm, .. } => {
                sdr::run(i, RtlConfig { serial, center_hz: center as u64, rate_hz: rate_hz as u32, gain_db, ppm }, tx, stop)
            }
            Source::File { path, rate_hz, realtime, .. } => run_file(i, &path, rate_hz, realtime, tx, stop),
        }).map_err(|e| e.to_string())?);
    }
    drop(tx);
    let (ctx2, stop2) = (ctx.clone(), stop.clone());
    threads.push(
        std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || engine_thread(ctx2, cfg, engine, rx, stop2, bandplan_path))
            .map_err(|e| e.to_string())?,
    );
    Ok(Runner { stop, threads })
}

/// Replay a capture file as a source, paced to real time or as fast as possible.
fn run_file(source: usize, path: &str, rate_hz: f64, realtime: bool, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>) {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            let _ = tx.send(SourceMsg::Error { source, error: format!("{path}: {e}") });
            let _ = tx.send(SourceMsg::End { source });
            return;
        }
    };
    let chunk = 65536usize;
    let t0 = Instant::now();
    let mut sent = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let mut buf = vec![0u8; chunk];
        let n = f.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.truncate(n & !1);
        sent += buf.len() as u64 / 2;
        if tx.send(SourceMsg::Data { source, bytes: buf, dropped: 0 }).is_err() {
            return;
        }
        if realtime {
            let due = Duration::from_secs_f64(sent as f64 / rate_hz);
            if let Some(wait) = due.checked_sub(t0.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    }
    let _ = tx.send(SourceMsg::End { source });
}

#[derive(Default, Clone)]
struct SourceStats {
    bytes: u64,
    dropped: u64,
    errors: u64,
    last_error: Option<String>,
    rate_measured: f64,
    ended: bool,
}

fn engine_thread(ctx: Arc<Ctx>, cfg: Config, mut engine: Engine, rx: mpsc::Receiver<SourceMsg>, stop: Arc<AtomicBool>, bandplan_path: PathBuf) {
    ctx.set_phase("running", None, false);
    let dir = PathBuf::from(&cfg.recording.capture_dir);
    let n = cfg.sources.len();
    let mut stats = vec![SourceStats::default(); n];
    let mut rate_mark = (Instant::now(), vec![0u64; n]);
    let (mut busy, t0) = (Duration::ZERO, Instant::now());
    let (mut last_status, mut last_spec) = (Instant::now(), Instant::now());
    let mut log: Vec<Value> = Vec::new();
    let mut ended_all = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(SourceMsg::Data { source, bytes, dropped }) => {
                let t = Instant::now();
                engine.push_u8(source, &bytes);
                busy += t.elapsed();
                stats[source].bytes += bytes.len() as u64;
                stats[source].dropped += dropped;
            }
            Ok(SourceMsg::Error { source, error }) => {
                stats[source].errors += 1;
                stats[source].last_error = Some(error.clone());
                log.push(json!({ "timeS": engine.status().now_s, "kind": "error", "text": format!("source {source}: {error}") }));
            }
            Ok(SourceMsg::End { source }) => {
                stats[source].ended = true;
                if stats.iter().all(|s| s.ended) {
                    // (Not "ended" when the user stopped it: the file thread ends on stop too.)
                    ended_all = !stop.load(Ordering::Relaxed);
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for ev in engine.drain_events() {
            handle_event(&ctx, &cfg, &dir, &engine, ev, &mut log);
        }
        if last_spec.elapsed() >= Duration::from_millis(150) {
            last_spec = Instant::now();
            for (i, s) in engine.sources().iter().enumerate() {
                let bins: Vec<f32> = engine.spectrum(i, 512).iter().map(|v| (v * 10.0).round() / 10.0).collect();
                publish(&ctx.hub, json!({ "type": "spectrum", "source": i, "centerHz": s.center_hz, "rateHz": s.rate_hz, "bins": bins }));
            }
        }
        if last_status.elapsed() >= Duration::from_millis(500) {
            last_status = Instant::now();
            let dt = rate_mark.0.elapsed().as_secs_f64();
            if dt >= 1.0 {
                for (i, s) in stats.iter_mut().enumerate() {
                    s.rate_measured = (s.bytes - rate_mark.1[i]) as f64 / 2.0 / dt;
                    rate_mark.1[i] = s.bytes;
                }
                rate_mark.0 = Instant::now();
            }
            if !log.is_empty() {
                publish(&ctx.hub, json!({ "type": "log", "lines": std::mem::take(&mut log) }));
            }
            publish(&ctx.hub, status_json(&cfg, &engine, &stats, busy.as_secs_f64() / t0.elapsed().as_secs_f64().max(1e-3)));
        }
    }
    ctx.set_phase("stopping", None, false);
    engine.finish();
    for ev in engine.drain_events() {
        handle_event(&ctx, &cfg, &dir, &engine, ev, &mut log);
    }
    if !log.is_empty() {
        publish(&ctx.hub, json!({ "type": "log", "lines": log }));
    }
    let _ = fs::create_dir_all(bandplan_path.parent().unwrap_or(Path::new(".")));
    let _ = fs::write(&bandplan_path, engine.bandplan());
    stop.store(true, Ordering::Relaxed);
    ctx.set_phase("idle", None, ended_all);
}

fn call_view(c: &Call) -> Value {
    json!({
        "id": c.id,
        "talkgroup": c.talkgroup,
        "alphaTag": c.talkgroup_info.as_ref().map_or("", |t| t.alpha_tag.as_str()),
        "freqHz": c.freq_hz,
        "slot": if c.phase2_tdma { Some(c.tdma_slot) } else { None },
        "state": if c.recording { "recording" } else { "monitoring" },
        "reason": c.reason.map(|r| r.as_str()),
        "encrypted": c.encrypted,
        "emergency": c.emergency,
        "startS": c.start_s,
        "sources": c.sources.iter().map(|s| s.src).collect::<Vec<_>>(),
    })
}

fn status_json(cfg: &Config, engine: &Engine, stats: &[SourceStats], load: f64) -> Value {
    let st = engine.status();
    let id = &st.identity;
    let sources: Vec<Value> = cfg
        .sources
        .iter()
        .zip(engine.sources())
        .zip(stats)
        .enumerate()
        .map(|(i, ((s, sc), ss))| {
            let label = match s {
                Source::Rtlsdr { serial, .. } => format!("RTL-SDR {}", if serial.is_empty() { "(first)".into() } else { format!("SN {serial}") }),
                Source::File { path, .. } => format!("file {}", Path::new(path).file_name().map_or(path.clone(), |f| f.to_string_lossy().into_owned())),
            };
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
            "activeCalls": st.active_calls, "recording": st.recording, "channelsOpen": st.channels_open, "callsConcluded": st.calls_concluded,
        },
        "sources": sources,
        "load": load,
        "calls": engine.active_calls().iter().map(call_view).collect::<Vec<_>>(),
    })
}

fn handle_event(ctx: &Ctx, cfg: &Config, dir: &Path, engine: &Engine, ev: Event, log: &mut Vec<Value>) {
    let _ = engine;
    match ev {
        Event::Message(m) => log.push(json!({ "timeS": m.time_s, "kind": m.kind.as_str(), "text": m.meta })),
        Event::ControlChannel { freq_hz } => {
            log.push(json!({ "timeS": 0, "kind": "control", "text": format!("Control channel {:.5} MHz", freq_hz as f64 / 1e6) }))
        }
        Event::Audio { call_id, talkgroup, samples } => {
            if ctx.hub.receiver_count() == 0 {
                return;
            }
            let mut frame = Vec::with_capacity(9 + samples.len() * 2);
            frame.push(1u8);
            frame.extend_from_slice(&call_id.to_le_bytes());
            frame.extend_from_slice(&talkgroup.to_le_bytes());
            for s in samples {
                frame.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
            }
            let _ = ctx.hub.send(Arc::new(Out::Audio { tg: talkgroup, frame }));
        }
        Event::Concluded(k) => {
            // Trunk Recorder's layout: <shortName>/<year>/<month>/<day>/, local time.
            let start_s = serde_json::from_str::<Value>(&k.json).ok().and_then(|v| v["start_time"].as_i64()).unwrap_or(0);
            let t = Local.timestamp_opt(start_s, 0).single().unwrap_or_else(Local::now);
            let rel = format!("{}/{}/{}/{}/{}", cfg.system.short_name, t.year(), t.month(), t.day(), k.base_name);
            let base = dir.join(&rel);
            if let Some(d) = base.parent() {
                let _ = fs::create_dir_all(d);
            }
            let wav_ok = fs::write(format!("{}.wav", base.display()), trunk_core::wav::encode(&k.audio, 8000)).is_ok();
            let _ = fs::write(format!("{}.json", base.display()), &k.json);
            if !wav_ok {
                log.push(json!({ "timeS": 0, "kind": "error", "text": format!("couldn't write {}", base.display()) }));
            }
            let entry = json!({ "path": rel, "record": serde_json::from_str::<Value>(&k.json).unwrap_or(Value::Null) });
            {
                let mut h = ctx.history.lock().unwrap();
                h.push_front(entry.clone());
                h.truncate(500);
            }
            publish(&ctx.hub, json!({ "type": "concluded", "entry": entry }));
        }
        Event::CallStart(_) | Event::CallUpdate(_) | Event::CallEnd(_) => {}
    }
}

/// The newest `limit` calls already on disk (for the history list at startup).
pub fn scan_history(dir: &Path, limit: usize) -> VecDeque<Value> {
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "json") {
                let t = e.metadata().and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
                found.push((t, p));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found
        .into_iter()
        .take(limit)
        .filter_map(|(_, p)| {
            let record: Value = serde_json::from_str(&fs::read_to_string(&p).ok()?).ok()?;
            let rel = p.strip_prefix(dir).ok()?.with_extension("");
            Some(json!({ "path": rel.to_string_lossy().replace('\\', "/"), "record": record }))
        })
        .collect()
}
