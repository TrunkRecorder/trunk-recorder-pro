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
use trunk_app::{Output, Session};

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
    let bandplan_path = crate::config::config_dir().join(format!("{}.bandplan", cfg.system.short_name));
    let saved_plan = fs::read_to_string(&bandplan_path).ok();
    let session = Session::new(cfg.clone(), epoch_ms, saved_plan.as_deref(), local_ymd)?;
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
            .spawn(move || engine_thread(ctx2, cfg, session, rx, stop2, bandplan_path))
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

fn local_ymd(t: i64) -> (i32, u32, u32) {
    let d = Local.timestamp_opt(t, 0).single().unwrap_or_else(Local::now);
    (d.year(), d.month(), d.day())
}

fn engine_thread(ctx: Arc<Ctx>, cfg: Config, mut session: Session, rx: mpsc::Receiver<SourceMsg>, stop: Arc<AtomicBool>, bandplan_path: PathBuf) {
    ctx.set_phase("running", None, false);
    let dir = PathBuf::from(&cfg.recording.capture_dir);
    let t0 = Instant::now();
    let now_ms = || t0.elapsed().as_secs_f64() * 1000.0;
    let mut out = Vec::new();
    let mut ended_all = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(SourceMsg::Data { source, bytes, dropped }) => {
                let t = Instant::now();
                session.push(source, &bytes, dropped);
                session.add_busy_ms(t.elapsed().as_secs_f64() * 1000.0);
            }
            Ok(SourceMsg::Error { source, error }) => session.source_error(source, &error),
            Ok(SourceMsg::End { source }) => {
                if session.source_ended(source) {
                    // (Not "ended" when the user stopped it: the file thread ends on stop too.)
                    ended_all = !stop.load(Ordering::Relaxed);
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        session.want_audio = ctx.hub.receiver_count() > 0;
        session.poll(now_ms(), &mut out);
        deliver(&ctx, &dir, &mut out);
    }
    ctx.set_phase("stopping", None, false);
    session.finish(&mut out);
    deliver(&ctx, &dir, &mut out);
    let _ = fs::create_dir_all(bandplan_path.parent().unwrap_or(Path::new(".")));
    let _ = fs::write(&bandplan_path, session.bandplan());
    stop.store(true, Ordering::Relaxed);
    ctx.set_phase("idle", None, ended_all);
}

/// Write call files, keep history, forward everything to the browsers.
fn deliver(ctx: &Ctx, dir: &Path, out: &mut Vec<Output>) {
    for o in out.drain(..) {
        match o {
            Output::Text(t) => {
                let _ = ctx.hub.send(Arc::new(Out::Text(t)));
            }
            Output::Audio { tg, frame } => {
                let _ = ctx.hub.send(Arc::new(Out::Audio { tg, frame }));
            }
            Output::File { rel, wav, json, entry } => {
                let base = dir.join(&rel);
                if let Some(d) = base.parent() {
                    let _ = fs::create_dir_all(d);
                }
                let ok = fs::write(format!("{}.wav", base.display()), wav).is_ok() && fs::write(format!("{}.json", base.display()), json).is_ok();
                if !ok {
                    publish(&ctx.hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": "error", "text": format!("couldn't write {}", base.display()) }] }));
                }
                let mut h = ctx.history.lock().unwrap();
                h.push_front(entry);
                h.truncate(500);
            }
        }
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
