//! A running recorder: one thread per source (RTL-SDR, USRP, Airspy, SoapySDR or capture file) and an
//! engine thread that decodes, writes calls to disk in Trunk Recorder's layout
//! (`<captureDir>/<shortName>/<YYYY>/<M>/<D>/<tg>-<epoch>_<freq>.wav|json`, a folder per system) and
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

use chrono::{Local, Offset, TimeZone};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use trunk_app::{Output, Session};

use crate::config::{AirspyGain, Config, SampleFormat, Source};
use crate::plugins::{self, FileRules, PluginHost};
use crate::radio::{airspy, soapy, uhd};
use crate::sdr::{self, RtlConfig, SourceMsg};

/// A message for every connected browser.
pub enum Out {
    Text(String),
    /// Live audio (binary frame) for talkgroup `tg` of `system` (65535: conventional).
    Audio { system: u16, tg: u32, frame: Vec<u8> },
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
    /// Signalled by a browser's `quit`.
    pub quit: tokio::sync::Notify,
    /// The first-run survey, when one is running (never with recording).
    pub survey: Mutex<Option<crate::survey::SurveyRunner>>,
    /// Its latest snapshot, for browsers that connect meanwhile.
    pub survey_last: Mutex<Option<Value>>,
    pub plugins: plugins::manage::Plugins,
    /// `--ui <folder>`: the interface / shows, over `server.home`.
    pub home_dir: Option<PathBuf>,
    pub accounts: crate::auth::Accounts,
    pub stats: crate::stats::Stats,
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
pub fn start(ctx: Arc<Ctx>, mut cfg: Config) -> Result<Runner, String> {
    // Linked channel files are read afresh, so a spreadsheet's edits apply.
    if cfg.conventional.iter().any(|v| !v.channel_file.is_empty()) {
        let errors: Vec<String> = (0..cfg.conventional.len()).filter_map(|k| cfg.load_channel_file(k, &ctx.config_path).err()).collect();
        let mut shared = ctx.config.lock().unwrap();
        for (mine, read) in shared.conventional.iter_mut().zip(&cfg.conventional) {
            if !read.channel_file.is_empty() && mine.channel_file == read.channel_file {
                mine.channels = read.channels.clone();
                mine.channel_file_status = read.channel_file_status.clone();
            }
        }
        publish(&ctx.hub, serde_json::json!({ "type": "config", "config": &*shared }));
        if let Some(e) = errors.into_iter().next() {
            return Err(e);
        }
    }
    if let Some(p) = cfg.problem() {
        return Err(p);
    }
    let epoch_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64);
    let mut session = Session::new(cfg.clone(), epoch_ms, &|name| fs::read_to_string(bandplan_path(name)).ok(), local_offset)?;
    session.load_units(&|name| fs::read_to_string(units_path(name)).ok());
    session.load_heard(&fs::read_to_string(heard_path(&cfg)).unwrap_or_default());
    ctx.plugins.start(&cfg);
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
    let centers = cfg.resolved_centers();
    let mut threads = Vec::new();
    for (i, (src, center)) in cfg.sources.iter().zip(&centers).enumerate() {
        let (tx, stop) = (tx.clone(), stop.clone());
        let src = src.clone();
        let center = *center;
        threads.push(std::thread::Builder::new().name(format!("source-{i}")).spawn(move || match src {
            Source::Rtlsdr { serial, rate_hz, gain_db, agc, ppm, .. } => {
                sdr::run(i, RtlConfig { serial, center_hz: center as u64, rate_hz: rate_hz as u32, gain_db: (!agc).then_some(gain_db), ppm }, tx, stop)
            }
            Source::Usrp { args, rate_hz, gain_db, agc, antenna, ppm, .. } => {
                uhd::run(i, uhd::UsrpConfig { args, center_hz: center, rate_hz, gain_db, agc, antenna, ppm }, tx, stop)
            }
            Source::Airspy { serial, rate_hz, gain_mode, gain, lna_gain, mixer_gain, vga_gain, agc, bias_tee, ppm, .. } => {
                let gain = airspy_gain(gain_mode, gain, lna_gain, mixer_gain, vga_gain, agc);
                airspy::run(i, airspy::AirspyConfig { serial, center_hz: center, rate_hz, gain, bias_tee, ppm }, tx, stop)
            }
            Source::Soapy { args, rate_hz, agc, gain_db, gains, antenna, settings, ppm, .. } => {
                let gains = gains.into_iter().collect();
                soapy::run(i, soapy::SoapyConfig { args, center_hz: center, rate_hz, agc, gain_db, gains, antenna, settings, ppm }, tx, stop)
            }
            Source::File { path, rate_hz, realtime, format, .. } => run_file(i, &path, rate_hz, realtime, format, tx, stop),
        }).map_err(|e| e.to_string())?);
    }
    drop(tx);
    let (ctx2, stop2) = (ctx.clone(), stop.clone());
    threads.push(
        std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || engine_thread(ctx2, cfg, session, rx, stop2))
            .map_err(|e| e.to_string())?,
    );
    Ok(Runner { stop, threads })
}

/// Where a system's band plan is kept between runs.
fn bandplan_path(short_name: &str) -> PathBuf {
    crate::config::config_dir().join(format!("{short_name}.bandplan"))
}

/// Where a system's radios' talker aliases are kept (Trunk Recorder's unitTagsOTA CSV).
pub(crate) fn units_path(short_name: &str) -> PathBuf {
    crate::config::config_dir().join(format!("{short_name}.units.csv"))
}

/// Save the band plans that changed since `saved` (what was last written).
fn save_bandplans(session: &Session, saved: &mut std::collections::HashMap<String, String>) {
    for (name, plan) in session.bandplans() {
        if !plan.is_empty() && saved.get(&name) != Some(&plan) {
            let _ = fs::create_dir_all(crate::config::config_dir());
            let _ = fs::write(bandplan_path(&name), &plan);
            saved.insert(name, plan);
        }
    }
}

/// Where the codes conventional frequencies carried are kept ([`trunk_app::heard`]).
pub(crate) fn heard_path(cfg: &trunk_app::Config) -> PathBuf {
    crate::config::config_dir().join(Session::heard_file(cfg))
}

/// Save the talker aliases systems learned, and the codes conventional
/// frequencies carried, since the last save.
fn save_units(session: &mut Session) {
    for (name, csv) in session.units_changed() {
        let _ = fs::create_dir_all(crate::config::config_dir());
        let _ = fs::write(units_path(&name), csv);
    }
    if let Some(json) = session.heard_unsaved() {
        let _ = fs::create_dir_all(crate::config::config_dir());
        let _ = fs::write(heard_path(session.config()), json);
    }
}

/// Replay a capture file as a source, paced to real time or as fast as possible.
pub(crate) fn run_file(source: usize, path: &str, rate_hz: f64, realtime: bool, format: SampleFormat, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>) {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            let _ = tx.send(SourceMsg::Error { source, error: format!("{path}: {e}") });
            let _ = tx.send(SourceMsg::End { source });
            return;
        }
    };
    let bps = format.bytes_per_sample();
    let chunk = 32768 * bps;
    let t0 = Instant::now();
    let mut sent = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let mut buf = vec![0u8; chunk];
        let n = read_full(&mut f, &mut buf);
        if n < bps {
            break;
        }
        buf.truncate(n - n % bps);
        sent += (buf.len() / bps) as u64;
        let msg = match format {
            SampleFormat::Cu8 => SourceMsg::Data { source, bytes: buf, dropped: 0 },
            _ => SourceMsg::Iq { source, samples: trunk_app::samples::to_iq(format, &buf), dropped: 0 },
        };
        if tx.send(msg).is_err() {
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

/// Fill `buf` unless the file ends first; bytes read.
fn read_full(f: &mut fs::File, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    n
}

/// An Airspy's gain settings, as its driver takes them.
pub fn airspy_gain(mode: AirspyGain, gain: u8, lna: u8, mixer: u8, vga: u8, agc: bool) -> airspy::AirspyGain {
    match mode {
        AirspyGain::Manual if agc => airspy::AirspyGain::Agc { vga },
        AirspyGain::Manual => airspy::AirspyGain::Manual { lna, mixer, vga },
        AirspyGain::Sensitivity => airspy::AirspyGain::Sensitivity(gain),
        AirspyGain::Linearity => airspy::AirspyGain::Linearity(gain),
    }
}

fn local_offset(t: i64) -> i32 {
    Local.timestamp_opt(t, 0).single().unwrap_or_else(Local::now).offset().fix().local_minus_utc()
}

fn engine_thread(ctx: Arc<Ctx>, cfg: Config, mut session: Session, rx: mpsc::Receiver<SourceMsg>, stop: Arc<AtomicBool>) {
    ctx.set_phase("running", None, false);
    let dir = PathBuf::from(&cfg.recording.capture_dir);
    // Each system's file rules, by a call's `system` (trunked or conventional).
    let rules = {
        let cfg = cfg.clone();
        move |system: u16| FileRules::of(&cfg.recording_of(system))
    };
    let (fin_tx, fin_rx) = mpsc::sync_channel::<Finish>(1024);
    let finisher = {
        let (ctx, dir, m4a) = (ctx.clone(), dir.clone(), cfg.recording.m4a.clone());
        std::thread::Builder::new().name("finish".into()).spawn(move || finish_calls(&ctx, &dir, fin_rx, &m4a)).expect("thread")
    };
    let fin = |rel: String, system: u16, json: String| {
        if let Err(mpsc::TrySendError::Full(f)) = fin_tx.try_send(Finish { rel, system, json, rules: rules(system) }) {
            // (Behind on encoding: this one goes out without its M4A rather than holding the engine up.)
            finish_one(&ctx, &dir, Finish { rules: FileRules { compress_wav: false, ..f.rules }, ..f }, None);
        }
    };
    let t0 = Instant::now();
    let now_ms = || t0.elapsed().as_secs_f64() * 1000.0;
    let mut out = Vec::new();
    let mut ended_all = false;
    // Band plans (and DMR channel tables) as last saved: learned ones survive a crash or kill too.
    let mut saved_plans: std::collections::HashMap<String, String> = Default::default();
    let mut plans_at = Instant::now();
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
            Ok(SourceMsg::Iq { source, samples, dropped }) => {
                let t = Instant::now();
                session.push_iq(source, &samples, dropped);
                session.add_busy_ms(t.elapsed().as_secs_f64() * 1000.0);
            }
            Ok(SourceMsg::Error { source, error }) => session.source_error(source, &error),
            Ok(SourceMsg::Tuned { .. }) => {}
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
        // (The plugins can be swapped meanwhile: their settings changed.)
        let plugins = ctx.plugins.host.read().unwrap();
        let mut topics: trunk_app::PluginTopics = plugins.as_ref().map_or(Default::default(), |p| p.topics);
        if ctx.stats.wants() {
            topics.calls = true;
            topics.units = true;
            topics.status = true;
        }
        session.plugin_topics = topics;
        session.want_audio = ctx.hub.receiver_count() > 0 || plugins.as_ref().is_some_and(|p| p.audio);
        session.want_trunk = ctx.hub.receiver_count() > 0;
        session.poll(now_ms(), &mut out);
        deliver(&ctx, &dir, &mut out, plugins.as_ref(), &fin);
        drop(plugins);
        save_units(&mut session);
        if plans_at.elapsed() >= Duration::from_secs(10) {
            plans_at = Instant::now();
            save_bandplans(&session, &mut saved_plans);
        }
    }
    ctx.set_phase("stopping", None, false);
    session.finish(&mut out);
    deliver(&ctx, &dir, &mut out, ctx.plugins.host.read().unwrap().as_ref(), &fin);
    drop(fin_tx);
    let _ = finisher.join();
    // Uploads in flight get a moment to finish.
    ctx.plugins.stop(Duration::from_secs(10));
    let _ = fs::create_dir_all(crate::config::config_dir());
    for (name, plan) in session.bandplans() {
        let _ = fs::write(bandplan_path(&name), plan);
    }
    save_units(&mut session);
    stop.store(true, Ordering::Relaxed);
    ctx.set_phase("idle", None, ended_all);
}

/// A call written, for [`finish_calls`].
struct Finish {
    rel: String,
    system: u16,
    json: String,
    rules: FileRules,
}

/// After a call's files are written: its .m4a (compressWav), then to the
/// plugins, whose results settle what's kept ([`crate::plugins::Archive`]).
fn finish_calls(ctx: &Ctx, dir: &Path, rx: mpsc::Receiver<Finish>, m4a: &crate::config::M4a) {
    let encoder = plugins::Encoder::find(&m4a.encoder).map(|e| (e, m4a.bitrate_kbps.clamp(8, 320)));
    let mut warned = false;
    for f in rx {
        if f.rules.compress_wav && encoder.is_none() && !std::mem::replace(&mut warned, true) {
            let text = if m4a.encoder == "none" { "M4A encoding is off: calls are kept as WAV only" } else { "No M4A encoder found (install ffmpeg): calls are kept as WAV only" };
            log::warn!("{text}");
            publish(&ctx.hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": "error", "text": text }] }));
        }
        finish_one(ctx, dir, f, encoder.as_ref());
    }
}

fn finish_one(ctx: &Ctx, dir: &Path, f: Finish, encoder: Option<&(plugins::Encoder, u32)>) {
    let base = dir.join(&f.rel);
    let m4a = match encoder.filter(|_| f.rules.compress_wav) {
        Some((e, kbps)) => {
            let (wav, out) = (PathBuf::from(format!("{}.wav", base.display())), PathBuf::from(format!("{}.m4a", base.display())));
            match e.m4a(&wav, &out, *kbps) {
                Ok(()) => Some(out),
                Err(err) => {
                    log::error!("M4A of {}: {err}", f.rel);
                    publish(&ctx.hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": "error", "text": format!("M4A of {}: {err}", f.rel) }] }));
                    None
                }
            }
        }
        None => None,
    };
    let host = ctx.plugins.host.read().unwrap();
    ctx.plugins.archive.expect(&f.rel, host.as_ref().map_or(0, |h| h.call_takers()), f.rules, &base);
    if let Some(h) = host.as_ref() {
        h.concluded(f.system, &f.rel, &f.json, m4a);
    }
}

/// Write call files, keep history, forward everything to the browsers and plugins.
fn deliver(ctx: &Ctx, dir: &Path, out: &mut Vec<Output>, plugins: Option<&PluginHost>, finish: &dyn Fn(String, u16, String)) {
    for o in out.drain(..) {
        match o {
            Output::Text(t) => {
                let _ = ctx.hub.send(Arc::new(Out::Text(t)));
            }
            Output::Audio { system, tg, frame } => {
                if let Some(p) = plugins {
                    p.audio_frame(&frame);
                }
                let _ = ctx.hub.send(Arc::new(Out::Audio { system, tg, frame }));
            }
            Output::Plugin(m) => {
                ctx.stats.event(&m);
                if let Some(p) = plugins {
                    p.event(&m);
                }
            }
            Output::Log(r) => crate::logging::record(&r),
            Output::File { rel, system, wav, json, frames, entry } => {
                let base = dir.join(&rel);
                if let Some(d) = base.parent() {
                    let _ = fs::create_dir_all(d);
                }
                let ok = fs::write(format!("{}.wav", base.display()), wav).is_ok()
                    && fs::write(format!("{}.json", base.display()), &json).is_ok()
                    && frames.is_none_or(|f| fs::write(format!("{}.frames.jsonl", base.display()), f).is_ok());
                ctx.stats.concluded(&json);
                if !ok {
                    log::error!("Couldn't write {}", base.display());
                    publish(&ctx.hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": "error", "text": format!("couldn't write {}", base.display()) }] }));
                } else {
                    finish(rel.clone(), system, json);
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
