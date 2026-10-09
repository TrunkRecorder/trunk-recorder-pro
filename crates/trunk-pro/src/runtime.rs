//! A running recorder: one thread per source (RTL-SDR, USRP, Airspy, SoapySDR or capture file) and an
//! engine thread that decodes, writes calls to disk in Trunk Recorder's layout
//! (`<captureDir>/<shortName>/<YYYY>/<M>/<D>/<tg>-<epoch>_<freq>.wav|json`, a folder per system) and
//! publishes what happens to the browser interface through the [`Hub`].

use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Only for browsers that subscribed to `topic` (see [`trunk_app::stats::Topics`]).
    Topic { topic: String, text: String },
    /// Live audio (binary frame) for talkgroup `tg` of the system with short
    /// name `short_name` (the frame carries the system's number this run).
    Audio { short_name: String, tg: u32, frame: Vec<u8> },
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
    /// Held through a start, stop, survey start or quit, so two never
    /// interleave (two Starts at once would leave a recorder nothing stops);
    /// true once quitting, when nothing may start any more.
    pub lifecycle: Mutex<bool>,
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
    /// The radio registry and the event monitor: outlive recordings.
    pub shared: Arc<trunk_app::stats::Shared>,
    /// The dashboard's minute history (a week), and where new minutes go.
    pub series: Arc<Mutex<trunk_app::stats::History>>,
    pub store: crate::statstore::Store,
    /// How many connected browsers watch each topic; `topics_gen` changes with it.
    pub topics: Mutex<std::collections::BTreeMap<String, usize>>,
    pub topics_gen: AtomicU64,
    /// For the engine thread, while recording.
    pub engine_cmds: Mutex<Vec<EngineCmd>>,
    /// The latest `host` message (the computer and plugins), for browsers that connect.
    pub host_last: Mutex<Option<Value>>,
    /// The RAM spool (`recording.ramSpool`), as last opened.
    pub spool: Mutex<Option<Arc<crate::spool::Spool>>>,
}

/// Something for the running session to do.
pub enum EngineCmd {
    /// A system's talkgroup file changed (an Ignore flag set from the dashboard).
    Talkgroups { short_name: String, csv: String },
}

impl Ctx {
    /// What the connected browsers watch.
    pub fn topics(&self) -> trunk_app::stats::Topics {
        trunk_app::stats::Topics::new(self.topics.lock().unwrap().keys().cloned())
    }

    /// A browser now watches `add` and no longer `remove`.
    pub fn retopic(&self, add: &[String], remove: &[String]) {
        let mut t = self.topics.lock().unwrap();
        for r in remove {
            if let Some(n) = t.get_mut(r) {
                *n -= 1;
                if *n == 0 {
                    t.remove(r);
                }
            }
        }
        for a in add {
            *t.entry(a.clone()).or_default() += 1;
        }
        self.topics_gen.fetch_add(1, Ordering::Relaxed);
    }

    /// Hand an event to the monitor, and to the browsers when it's one for the feed.
    pub fn event(&self, e: trunk_app::stats::MonitorEvent) {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        if let Some(v) = self.shared.emit(t, e) {
            publish(&self.hub, v);
        }
    }

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
    /// The engine has stopped by itself (every capture file ended).
    pub fn finished(&self) -> bool {
        self.threads.last().is_some_and(|t| t.is_finished())
    }

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
    open_spool(&ctx, &cfg);
    // The wall clock now, and a monotonic clock from now: the session's
    // `now_ms` is the one plus the other (wall time that never jumps).
    let epoch_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64);
    let epoch_at = Instant::now();
    let mut session = Session::new(cfg.clone(), epoch_ms, &|name| fs::read_to_string(bandplan_path(name)).ok(), local_offset)?;
    session.attach(ctx.shared.clone());
    session.load_units(&|name| fs::read_to_string(units_path(name)).ok());
    session.load_heard(&fs::read_to_string(heard_path(&cfg)).unwrap_or_default());
    ctx.plugins.start(&cfg);
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
    let centers = cfg.resolved_centers();
    let mut threads = Vec::new();
    // A thread that can't be made: what was started is stopped again.
    let undo = |threads: Vec<JoinHandle<()>>, e: std::io::Error| {
        stop.store(true, Ordering::Relaxed);
        for t in threads {
            let _ = t.join();
        }
        ctx.plugins.stop(Duration::from_secs(2));
        e.to_string()
    };
    for (i, (src, center)) in cfg.sources.iter().zip(&centers).enumerate() {
        let (tx, stop) = (tx.clone(), stop.clone());
        let src = src.clone();
        let center = *center;
        let spawned = std::thread::Builder::new().name(format!("source-{i}")).spawn(move || match src {
            Source::Rtlsdr { serial, rate_hz, gain_db, agc, ppm, .. } => {
                sdr::run(i, RtlConfig { serial, center_hz: center as u64, rate_hz: rate_hz as u32, gain_db: (!agc).then_some(gain_db), ppm }, tx, stop)
            }
            Source::Usrp { args, rate_hz, gain_db, agc, antenna, ppm, .. } => {
                uhd::run(i, uhd::UsrpConfig { args, center_hz: center, rate_hz, gain_db, agc, antenna, ppm }, tx, stop)
            }
            Source::Airspy { serial, rate_hz, gain_mode, gain_step, lna_step, mixer_step, vga_step, agc, bias_tee, ppm, .. } => {
                let gain = airspy_gain(gain_mode, gain_step, lna_step, mixer_step, vga_step, agc);
                airspy::run(i, airspy::AirspyConfig { serial, center_hz: center, rate_hz, gain, bias_tee, ppm }, tx, stop)
            }
            Source::Soapy { args, rate_hz, agc, gain_db, gains, antenna, settings, ppm, .. } => {
                let gains = gains.into_iter().collect();
                soapy::run(i, soapy::SoapyConfig { args, center_hz: center, rate_hz, agc, gain_db, gains, antenna, settings, ppm }, tx, stop)
            }
            Source::File { path, rate_hz, realtime, format, .. } => run_file(i, &path, rate_hz, realtime, SampleFormat::of(format, &path), tx, stop),
        });
        match spawned {
            Ok(t) => threads.push(t),
            Err(e) => return Err(undo(threads, e)),
        }
    }
    drop(tx);
    let (ctx2, stop2) = (ctx.clone(), stop.clone());
    let spawned = std::thread::Builder::new().name("engine".into()).spawn(move || engine_thread(ctx2, cfg, session, rx, stop2, (epoch_ms, epoch_at)));
    match spawned {
        Ok(t) => threads.push(t),
        Err(e) => return Err(undo(threads, e)),
    }
    Ok(Runner { stop, threads })
}

/// Where a system's band plan is kept between runs.
fn bandplan_path(short_name: &str) -> PathBuf {
    crate::paths::data_dir().join(format!("{short_name}.bandplan"))
}

/// Where a system's radios' talker aliases are kept (Trunk Recorder's unitTagsOTA CSV).
pub(crate) fn units_path(short_name: &str) -> PathBuf {
    crate::paths::data_dir().join(format!("{short_name}.units.csv"))
}

/// Save the band plans that changed since `saved` (what was last written).
fn save_bandplans(session: &Session, saved: &mut std::collections::HashMap<String, String>) {
    for (name, plan) in session.bandplans() {
        if !plan.is_empty() && saved.get(&name) != Some(&plan) {
            let _ = fs::create_dir_all(crate::paths::data_dir());
            let _ = trunk_app::config::write_atomic(&bandplan_path(&name), &plan);
            saved.insert(name, plan);
        }
    }
}

/// Where the radio registry (talkgroups, radios, frequencies heard) is kept, a file per system.
fn radio_dir() -> PathBuf {
    crate::paths::data_dir().join("radio")
}

/// Read the radio registry saved by earlier runs.
pub fn load_registry(shared: &trunk_app::stats::Shared) {
    let mut r = shared.radio.lock().unwrap();
    for e in fs::read_dir(radio_dir()).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            if let (Some(name), Ok(text)) = (p.file_stem().and_then(|n| n.to_str()), fs::read_to_string(&p)) {
                r.load(name, &text);
            }
        }
    }
}

/// Save the systems whose registry changed since the last save.
pub fn save_registry(shared: &trunk_app::stats::Shared) {
    let changed = shared.radio.lock().unwrap().take_dirty();
    if changed.is_empty() {
        return;
    }
    let _ = fs::create_dir_all(radio_dir());
    for (name, json) in changed {
        let file = radio_dir().join(format!("{}.json", trunk_core::metrics::key_part(&name)));
        if let Err(e) = trunk_app::config::write_atomic(&file, &json) {
            log::warn!("Couldn't save {}: {e}", file.display());
        }
    }
}

/// How often the radio registry is saved while recording (it's rewritten whole).
const REGISTRY_EVERY: Duration = Duration::from_secs(15 * 60);

/// Where the codes conventional frequencies carried are kept ([`trunk_app::heard`]).
pub(crate) fn heard_path(cfg: &trunk_app::Config) -> PathBuf {
    crate::paths::data_dir().join(Session::heard_file(cfg))
}

/// Save the talker aliases systems learned, and the codes conventional
/// frequencies carried, since the last save.
fn save_units(session: &mut Session) {
    for (name, csv) in session.units_changed() {
        let _ = fs::create_dir_all(crate::paths::data_dir());
        let _ = trunk_app::config::write_atomic(&units_path(&name), csv);
    }
    if let Some(json) = session.heard_unsaved() {
        let _ = fs::create_dir_all(crate::paths::data_dir());
        let _ = trunk_app::config::write_atomic(&heard_path(session.config()), json);
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
            SampleFormat::Cu8 => SourceMsg::Data { source, bytes: buf, dropped: 0, at: Instant::now() },
            _ => SourceMsg::Iq { source, samples: trunk_app::samples::to_iq(format, &buf), dropped: 0, at: Instant::now() },
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

/// The RAM spool as the config has it: made (or found again), or, when it's
/// off, the one the app made emptied into the recordings folder and removed.
/// One that can't be made leaves calls going to the recordings folder.
fn open_spool(ctx: &Ctx, cfg: &Config) {
    let (want, capture) = (&cfg.recording.ram_spool, Path::new(&cfg.recording.capture_dir));
    if !want.enabled || !want.dir.is_empty() {
        crate::spool::retire(&crate::paths::data_dir(), capture);
    }
    let spool = if want.enabled {
        match crate::spool::Spool::open(want, &crate::paths::data_dir()) {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                let text = format!("No RAM spool ({e}): calls go to the recordings folder");
                log::warn!("{text}");
                publish(&ctx.hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": "error", "text": text }] }));
                None
            }
        }
    } else {
        None
    };
    *ctx.spool.lock().unwrap() = spool;
}

/// `epoch`: the wall clock (Unix ms) at an instant, for the session's clock.
fn engine_thread(ctx: Arc<Ctx>, cfg: Config, mut session: Session, rx: mpsc::Receiver<SourceMsg>, stop: Arc<AtomicBool>, epoch: (f64, Instant)) {
    ctx.set_phase("running", None, false);
    let dir = PathBuf::from(&cfg.recording.capture_dir);
    // Each system's file rules, by a call's `system` (trunked or conventional).
    let rules = {
        let cfg = cfg.clone();
        move |system: u16| FileRules::of(&cfg.recording_of(system))
    };
    let spool = ctx.spool.lock().unwrap().clone();
    let (fin_tx, fin_rx) = mpsc::sync_channel::<Finish>(1024);
    let finisher = {
        let (ctx, dir, spool, m4a) = (ctx.clone(), dir.clone(), spool.clone(), cfg.recording.m4a.clone());
        std::thread::Builder::new().name("finish".into()).spawn(move || finish_calls(&ctx, &dir, spool.as_deref(), fin_rx, &m4a)).expect("thread")
    };
    // Calls are written on the finish thread, so a slow disk doesn't hold up decoding.
    let fin = |f: Finish, plugins: Option<&PluginHost>| {
        if let Err(mpsc::TrySendError::Full(f)) = fin_tx.try_send(f) {
            // (Behind on writing or encoding: this one is written here, without its M4A, rather than waiting.)
            finish_one(&ctx, &dir, spool.as_deref(), Finish { rules: FileRules { compress_wav: false, ..f.rules }, ..f }, None, plugins);
        }
    };
    let now_ms = || epoch.0 + epoch.1.elapsed().as_secs_f64() * 1000.0;
    // An instant on the same clock (when a driver handed samples over).
    let at_ms = |at: Instant| epoch.0 + at.saturating_duration_since(epoch.1).as_secs_f64() * 1000.0;
    let mut out = Vec::new();
    let mut ended_all = false;
    // Band plans (and DMR channel tables) as last saved: learned ones survive a crash or kill too.
    let mut saved_plans: std::collections::HashMap<String, String> = Default::default();
    let mut plans_at = Instant::now();
    let mut registry_at = Instant::now();
    let mut topics_gen = u64::MAX;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(SourceMsg::Data { source, bytes, dropped, at }) => {
                let t = Instant::now();
                session.push(source, &bytes, dropped, at_ms(at));
                session.add_busy_ms(t.elapsed().as_secs_f64() * 1000.0);
            }
            Ok(SourceMsg::Iq { source, samples, dropped, at }) => {
                let t = Instant::now();
                session.push_iq(source, &samples, dropped, at_ms(at));
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
        session.plugin_topics = plugins.as_ref().map_or(Default::default(), |p| p.topics);
        session.want_audio = ctx.hub.receiver_count() > 0 || plugins.as_ref().is_some_and(|p| p.audio);
        let g = ctx.topics_gen.load(Ordering::Relaxed);
        if g != topics_gen {
            topics_gen = g;
            session.set_topics(ctx.topics());
        }
        for c in std::mem::take(&mut *ctx.engine_cmds.lock().unwrap()) {
            match c {
                EngineCmd::Talkgroups { short_name, csv } => {
                    if session.set_talkgroups(&short_name, &csv) {
                        log::info!("[{short_name}] Talkgroups updated");
                    }
                }
            }
        }
        session.poll(now_ms(), &mut out);
        deliver(&ctx, &mut out, plugins.as_ref(), &rules, &fin);
        drop(plugins);
        save_units(&mut session);
        if plans_at.elapsed() >= Duration::from_secs(10) {
            plans_at = Instant::now();
            save_bandplans(&session, &mut saved_plans);
        }
        if registry_at.elapsed() >= REGISTRY_EVERY {
            registry_at = Instant::now();
            save_registry(&ctx.shared);
        }
    }
    ctx.set_phase("stopping", None, false);
    // What the sources had already sent (up to the queue's ~3 s): calls in
    // progress keep it.
    for m in rx.try_iter() {
        match m {
            SourceMsg::Data { source, bytes, dropped, at } => session.push(source, &bytes, dropped, at_ms(at)),
            SourceMsg::Iq { source, samples, dropped, at } => session.push_iq(source, &samples, dropped, at_ms(at)),
            _ => {}
        }
    }
    session.poll(now_ms(), &mut out);
    session.finish(&mut out);
    deliver(&ctx, &mut out, ctx.plugins.host.read().unwrap().as_ref(), &rules, &fin);
    drop(fin_tx);
    let _ = finisher.join();
    // Uploads in flight get a moment to finish.
    ctx.plugins.stop(Duration::from_secs(10));
    let _ = fs::create_dir_all(crate::paths::data_dir());
    for (name, plan) in session.bandplans() {
        let _ = trunk_app::config::write_atomic(&bandplan_path(&name), plan);
    }
    save_units(&mut session);
    save_registry(&ctx.shared);
    stop.store(true, Ordering::Relaxed);
    ctx.set_phase("idle", None, ended_all);
}

/// A concluded call, for [`finish_calls`] to store.
struct Finish {
    rel: String,
    system: u16,
    wav: Vec<u8>,
    json: String,
    frames: Option<trunk_core::trunk::Capture>,
    /// Its history entry (`{path, record}`).
    entry: Value,
    rules: FileRules,
}

/// Each call: its files, then (once they're on disk) the `concluded`
/// message and history, its .m4a (compressWav), and the plugins, whose
/// results settle what's kept ([`crate::plugins::Archive`]).
fn finish_calls(ctx: &Ctx, dir: &Path, spool: Option<&crate::spool::Spool>, rx: mpsc::Receiver<Finish>, m4a: &crate::config::M4a) {
    let encoder = plugins::Encoder::find(&m4a.encoder).map(|e| (e, m4a.bitrate_kbps.clamp(8, 320)));
    let mut warned = false;
    for f in rx {
        if f.rules.compress_wav && encoder.is_none() && !std::mem::replace(&mut warned, true) {
            let text = if m4a.encoder == "none" { "M4A encoding is off: calls are kept as WAV only" } else { "No M4A encoder found (install ffmpeg): calls are kept as WAV only" };
            log::warn!("{text}");
            publish(&ctx.hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": "error", "text": text }] }));
        }
        let host = ctx.plugins.host.read().unwrap();
        finish_one(ctx, dir, spool, f, encoder.as_ref(), host.as_ref());
    }
}

/// `host`: the plugins, as the caller already holds them (taking the lock
/// again on the engine thread could deadlock with a plugin reload waiting).
///
/// What's kept once the plugins are done goes to the recordings folder, and
/// what only they need to the spool, when there is one with room. The WAV is
/// written only when it's kept or a plugin takes it: an .m4a is encoded
/// from memory.
fn finish_one(ctx: &Ctx, dir: &Path, spool: Option<&crate::spool::Spool>, f: Finish, encoder: Option<&(plugins::Encoder, u32)>, host: Option<&PluginHost>) {
    let r = f.rules;
    let takers = host.map_or(0, |h| h.call_takers());
    // (A call no plugin takes is kept whole.)
    let (keep_audio, keep_json) = (r.audio_archive || takers == 0, r.call_log || takers == 0);
    let keep_m4a = keep_audio && r.compress_wav;
    let write_wav = keep_audio || host.is_some_and(|h| h.needs_wav()) || (r.compress_wav && encoder.is_none());
    let base = dir.join(&f.rel);
    let spooling = takers > 0 && !(keep_audio && keep_json && keep_m4a);
    let need = f.json.len() + f.wav.len() / 4 + if write_wav && !keep_audio { f.wav.len() } else { 0 };
    let spooled = spool.filter(|s| spooling && s.room_for(need as u64)).map(|s| s.dir.join(&f.rel));
    let at = |kept: bool, ext: &str| PathBuf::from(format!("{}.{ext}", if kept { &base } else { spooled.as_ref().unwrap_or(&base) }.display()));
    let (wav_at, json_at, frames_at, m4a_at) = (at(keep_audio, "wav"), at(keep_json, "json"), at(keep_audio, f.frames.as_ref().map_or("sdr", |c| c.ext)), at(keep_m4a, "m4a"));
    for d in [&wav_at, &json_at, &m4a_at].into_iter().filter_map(|p| p.parent()) {
        let _ = fs::create_dir_all(d);
    }
    let ok = (!write_wav || fs::write(&wav_at, &f.wav).is_ok())
        && fs::write(&json_at, &f.json).is_ok()
        && f.frames.as_ref().is_none_or(|c| fs::write(&frames_at, &c.bytes).is_ok());
    if !ok {
        log::error!("Couldn't write {}", base.display());
        publish(&ctx.hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": "error", "text": format!("couldn't write {}", base.display()) }] }));
        return;
    }
    // Whether its audio and JSON stay once the plugins are done (an upload that fails may keep them after all: call_files).
    let mut entry = f.entry;
    entry["audio"] = json!(keep_audio);
    entry["json"] = json!(keep_json);
    publish(&ctx.hub, json!({ "type": "concluded", "entry": &entry }));
    {
        let mut h = ctx.history.lock().unwrap();
        h.push_front(entry);
        h.truncate(HISTORY_KEPT);
    }
    let mut audio = plugins::CallAudio {
        wav: Arc::new(f.wav),
        wav_written: write_wav,
        files: trunk_recorder_plugin::CallFiles { json: json_at, wav: wav_at, m4a: None },
        m4a_to: m4a_at,
    };
    if let Some((e, kbps)) = encoder.filter(|_| r.compress_wav) {
        match e.m4a(&audio.wav, &audio.m4a_to, *kbps) {
            Ok(()) => audio.files.m4a = Some(audio.m4a_to.clone()),
            Err(err) => {
                log::error!("M4A of {}: {err}", f.rel);
                publish(&ctx.hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": "error", "text": format!("M4A of {}: {err}", f.rel) }] }));
                // The call's audio is then the WAV.
                if !audio.wav_written {
                    audio.wav_written = fs::write(&audio.files.wav, &*audio.wav).is_ok();
                }
            }
        }
    }
    ctx.plugins.archive.expect(&f.rel, takers, r, &base, spooled.as_deref());
    if let Some(h) = host {
        h.concluded(f.system, &f.rel, &f.json, audio);
    }
}

/// Forward everything to the browsers and plugins; calls go to be stored.
fn deliver(ctx: &Ctx, out: &mut Vec<Output>, plugins: Option<&PluginHost>, rules: &dyn Fn(u16) -> FileRules, finish: &dyn Fn(Finish, Option<&PluginHost>)) {
    for o in out.drain(..) {
        match o {
            Output::Text(t) => {
                let _ = ctx.hub.send(Arc::new(Out::Text(t)));
            }
            Output::Audio { short_name, tg, frame, .. } => {
                if let Some(p) = plugins {
                    p.audio_frame(&frame);
                }
                let _ = ctx.hub.send(Arc::new(Out::Audio { short_name, tg, frame }));
            }
            Output::Plugin(m) => {
                if let Some(p) = plugins {
                    p.event(&m);
                }
            }
            Output::Log(r) => crate::logging::record(&r),
            Output::Topic { topic, text } => {
                let _ = ctx.hub.send(Arc::new(Out::Topic { topic, text }));
            }
            Output::Rollup(r) => ctx.store.add(r),
            Output::File { rel, system, wav, json, frames, entry } => finish(Finish { rel, system, wav, json, frames, entry, rules: rules(system) }, plugins),
        }
    }
}

/// A call's plugins are done: what of it was kept (`audio`, `json`). Its
/// history entry follows, and the interface is told when that's not what
/// the entry said (an upload failed and the files were kept after all).
pub fn call_files(ctx: &Ctx, rel: &str, audio: bool, json: bool) {
    let mut h = ctx.history.lock().unwrap();
    let Some(e) = h.iter_mut().find(|e| e["path"] == rel) else { return };
    if e["audio"] == json!(audio) && e["json"] == json!(json) {
        return;
    }
    e["audio"] = json!(audio);
    e["json"] = json!(json);
    drop(h);
    publish(&ctx.hub, json!({ "type": "callFiles", "path": rel, "audio": audio, "json": json }));
}

/// Fill the history list with the newest calls already on disk, in the
/// background: a capture folder with years of calls takes a while to walk.
/// Calls concluded meanwhile stay in front.
pub fn load_history(ctx: Arc<Ctx>) {
    let dir = PathBuf::from(&ctx.config.lock().unwrap().recording.capture_dir);
    let _ = std::thread::Builder::new().name("history".into()).spawn(move || {
        let found = scan_history(&dir, HISTORY);
        let mut h = ctx.history.lock().unwrap();
        let known: std::collections::HashSet<String> = h.iter().filter_map(|e| e["path"].as_str().map(String::from)).collect();
        h.extend(found.into_iter().filter(|e| e["path"].as_str().is_none_or(|p| !known.contains(p))));
        h.truncate(HISTORY_KEPT);
    });
}

/// Calls the interface's history list is given.
pub const HISTORY: usize = 300;
/// Calls kept in memory for it.
const HISTORY_KEPT: usize = 500;

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
            let audio = ["wav", "m4a"].iter().any(|x| p.with_extension(x).exists());
            Some(json!({ "path": rel.to_string_lossy().replace('\\', "/"), "record": record, "audio": audio, "json": true }))
        })
        .collect()
}
