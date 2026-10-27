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
use trunk_app::protocol::{body, Audience, ToClient};
use trunk_app::saving::Saved;
use trunk_app::{Feeder, Output, Session};

use crate::config::{AirspyGain, Config, SampleFormat, Source};
use crate::plugins::{self, FileRules, PluginHost};
use crate::radio::{airspy, bladerf, soapy, uhd};
use crate::sdr::{self, Control, Gain, RtlConfig, SourceMsg};

/// A message for the connected browsers, serialized once for all of them.
pub enum Out {
    /// For every connection.
    Text(String),
    /// Only for connections with control access ([`Audience::Control`]).
    Control(String),
    /// The config: as it is, and as a listen-only connection sees it.
    Config { full: String, listen: String },
    /// Only for browsers that subscribed to `topic` (see [`trunk_app::stats::Topics`]).
    Topic { topic: String, text: String },
    /// Live audio (binary frame) for talkgroup `tg` of the system with short
    /// name `short_name` (the frame carries the system's number this run).
    Audio { short_name: String, tg: u32, frame: Vec<u8> },
    /// The recorder is quitting: every connection is told, then closed.
    Quit,
}

impl Out {
    /// `m`, as each kind of connection may hear it.
    pub fn of(m: &ToClient) -> Out {
        match m {
            ToClient::Quit => Out::Quit,
            ToClient::Config { .. } => {
                let mut listen = m.to_value();
                listen["config"] = crate::access::redact_config(listen["config"].take());
                Out::Config { full: m.to_json(), listen: listen.to_string() }
            }
            _ if m.audience() == Audience::Control => Out::Control(m.to_json()),
            _ => Out::Text(m.to_json()),
        }
    }
}

pub type Hub = broadcast::Sender<Arc<Out>>;

/// Send `m` to the browsers that may have it. Every message to all of them
/// goes this way.
pub fn publish(hub: &Hub, m: ToClient) {
    let _ = hub.send(Arc::new(Out::of(&m)));
}

#[derive(Clone, Debug, Default)]
pub struct PhaseInfo {
    /// "idle" | "starting" | "running" | "stopping"
    pub phase: &'static str,
    pub error: Option<String>,
    pub ended: bool,
}

impl PhaseInfo {
    pub fn message(&self) -> ToClient {
        ToClient::State { phase: self.phase, error: self.error.clone(), ended: self.ended }
    }
}

/// Shared between the web server and the runtime.
pub struct Ctx {
    pub config_path: PathBuf,
    pub config: Mutex<Config>,
    /// The config's version: one more each time it changes ([`Ctx::config_changed`]).
    pub config_version: AtomicU64,
    /// Each system's talkgroup file, as kept beside the config ([`talkgroups_path`]).
    pub talkgroups: Mutex<trunk_app::config::TalkgroupFiles>,
    pub hub: Hub,
    /// What the radios are doing ([`Lifecycle`]): changed only through its
    /// methods on `Ctx`, each holding the lock throughout, so two never
    /// interleave (two Starts at once would leave a recorder nothing stops).
    pub lifecycle: Mutex<Lifecycle>,
    /// The phase the browsers are told (the engine thread reports it as it goes).
    pub phase: Mutex<PhaseInfo>,
    /// Recently concluded calls (newest first), as sent to the browser.
    pub history: Mutex<VecDeque<Value>>,
    /// Signalled by a browser's `quit`.
    pub quit: tokio::sync::Notify,
    /// The survey's latest snapshot, for browsers that connect meanwhile.
    pub survey_last: Mutex<Option<ToClient>>,
    pub plugins: plugins::manage::Plugins,
    /// The address every interface listens on (`--bind`, else `server.bind`).
    pub bind: std::net::IpAddr,
    /// The interfaces besides the built-in one, and their listeners.
    pub interfaces: crate::interfaces::Interfaces,
    /// The radio registry and the event monitor: outlive recordings.
    pub shared: Arc<trunk_app::stats::Shared>,
    /// The dashboard's minute history (a week), and where new minutes go.
    pub series: Arc<Mutex<trunk_app::stats::History>>,
    pub store: crate::statstore::Store,
    /// Writes the files kept beside the config, in the background.
    pub saver: crate::saver::Saver,
    /// The codes conventional frequencies carried, as last told: for browsers
    /// that connect (the file is written only every so often).
    pub heard_last: Mutex<Option<ToClient>>,
    /// How many connected browsers watch each topic (the recording is told as it changes).
    pub topics: Mutex<std::collections::BTreeMap<String, usize>>,
    /// The recording's engine thread, while there is one: everything it's told comes this way ([`Ctx::tell_engine`]).
    pub engine: Mutex<Option<mpsc::Sender<EngineCmd>>>,
    /// The latest `host` message (the computer and plugins), for browsers that connect.
    pub host_last: Mutex<Option<ToClient>>,
    /// The RAM spool (`recording.ramSpool`), as last opened.
    pub spool: Mutex<Option<Arc<crate::spool::Spool>>>,
    /// The computer's clock, watched (#61): what calls' `clock_off` and the dashboard go by.
    pub clock: Arc<crate::clock::Clock>,
    /// Key containers unlocked from the interface ([`crate::unlock`]).
    pub unlocked: crate::unlock::Unlocked,
}

/// Something for the running session to do: the one way into the engine
/// thread, which waits on it ([`Ctx::engine`]).
pub enum EngineCmd {
    /// A source's error or end, by way of its decoding thread.
    Source(SourceMsg),
    /// What the connected browsers watch changed.
    Topics(trunk_app::stats::Topics),
    /// A system's talkgroup file changed (an Ignore flag set from the dashboard).
    Talkgroups { short_name: String, csv: String },
    /// A system's keys changed (a key container unlocked or locked): calls in progress take them too.
    Keys { short_name: String, ring: Option<Arc<trunk_core::p25::crypt::Keyring>> },
    /// A system's config changed: what sharing uses of it, its switch, directory link and licenses ([`trunk_app::share::sync`]).
    ShareSystem(trunk_app::config::System),
    /// How a system's share went.
    Shared { short_name: String, result: Result<Option<trunk_app::share::Linked>, String> },
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
        // (The snapshot is taken under the lock: the engine hears the changes in their order.)
        let topics = trunk_app::stats::Topics::new(t.keys().cloned());
        crate::logging::log_watched(t.contains_key("log"));
        drop(t);
        self.tell_engine(EngineCmd::Topics(topics));
    }

    /// Tell the recording's engine thread, if there is one.
    pub fn tell_engine(&self, c: EngineCmd) {
        if let Some(tx) = &*self.engine.lock().unwrap() {
            let _ = tx.send(c);
        }
    }

    /// Hand an event to the monitor, and to the browsers when it's one for the feed.
    pub fn event(&self, e: trunk_app::stats::MonitorEvent) {
        let t = crate::clock::unix_s();
        if let Some(v) = self.shared.emit(t, e) {
            publish(&self.hub, ToClient::MonitorEvent(body(v)));
        }
    }

    /// The config is now `c` (call with its lock held, so versions follow
    /// the changes in order): a new version, told to every browser. Returns it.
    pub fn config_changed(&self, c: &Config) -> u64 {
        let files = self.talkgroups.lock().unwrap().clone();
        self.shared.names.lock().unwrap().set_config(c, &files);
        let version = self.config_version.fetch_add(1, Ordering::SeqCst) + 1;
        publish(&self.hub, ToClient::Config { config: Box::new(c.clone()), version });
        version
    }

    pub fn set_phase(&self, phase: &'static str, error: Option<String>, ended: bool) {
        let p = PhaseInfo { phase, error, ended };
        publish(&self.hub, p.message());
        *self.phase.lock().unwrap() = p;
    }

    /// Start recording with `cfg`, in place of whatever the radios were
    /// doing (nothing once quitting). The engine thread reports the phase.
    /// A survey goes on only on a spare radio the new config doesn't use.
    pub fn begin_recording(self: &Arc<Self>, cfg: Config) -> Result<(), String> {
        let mut l = self.lifecycle.lock().unwrap();
        if l.quitting {
            return Ok(());
        }
        if let Some(r) = l.recording.take() {
            r.stop();
        }
        if l.survey.as_ref().is_some_and(|s| s.uses_any(&cfg.sources)) {
            self.end_survey(&mut l);
        }
        self.set_phase("starting", None, false);
        match start(self.clone(), cfg) {
            Ok(r) => {
                l.recording = Some(r);
                Ok(())
            }
            Err(e) => {
                self.set_phase("idle", Some(e.clone()), false);
                Err(e)
            }
        }
    }

    /// Start a survey, in place of one running: on a source once the radios
    /// are free ([`Ctx::free_radios`]), on a spare radio (one no source
    /// names) beside the recording.
    pub fn begin_survey(self: &Arc<Self>, cfg: Config, mut req: trunk_app::survey::Request) -> Result<(), String> {
        let mut l = self.lifecycle.lock().unwrap();
        if l.quitting {
            return Ok(());
        }
        self.end_survey(&mut l);
        match &req.radio {
            Some(r) => {
                if let Some(i) = cfg.sources.iter().position(|s| s.may_be(r)) {
                    return Err(format!("{} is source {} of the recorder: scan with that source, or pick a radio no source uses.", r.label(), i + 1));
                }
            }
            None => self.free_radios(&mut l, "the scan")?,
        }
        // Half the computer's cores (the recorder needs the rest), 1–8.
        req.threads = std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 8);
        l.survey = Some(crate::survey::start(self.clone(), cfg, req)?);
        Ok(())
    }

    /// `f` with the radios to itself (profiling a source): nothing starts
    /// meanwhile. None once quitting; Err while recording.
    pub fn with_radios<T>(&self, what: &str, f: impl FnOnce() -> T) -> Option<Result<T, String>> {
        let mut l = self.lifecycle.lock().unwrap();
        if l.quitting {
            return None;
        }
        Some(self.free_radios(&mut l, what).map(|()| {
            self.end_survey(&mut l);
            f()
        }))
    }

    /// The sources free for `what`: a survey on one ends, a recording whose
    /// capture files ended too, but one still recording isn't stopped for it.
    fn free_radios(&self, l: &mut Lifecycle, what: &str) -> Result<(), String> {
        if l.recording.as_ref().is_some_and(|r| !r.finished()) {
            return Err(format!("Stop recording first — {what} needs the radio to itself (or scan with a spare radio)."));
        }
        if let Some(r) = l.recording.take() {
            r.stop();
        }
        if l.survey.as_ref().is_some_and(|s| !s.spare()) {
            self.end_survey(l);
        }
        Ok(())
    }

    /// Stop recording, if it is (a survey on a spare radio goes on).
    pub fn stop_recording(&self) {
        if let Some(r) = self.lifecycle.lock().unwrap().recording.take() {
            r.stop();
        }
    }

    /// End a running survey (frees its radio) and tell the browsers.
    pub fn stop_survey(&self) {
        self.end_survey(&mut self.lifecycle.lock().unwrap());
    }

    /// Something for the running survey (a frequency to monitor, a rescan).
    pub fn survey_send(&self, c: crate::survey::UserCmd) {
        if let Some(r) = &self.lifecycle.lock().unwrap().survey {
            r.send(c);
        }
    }

    /// Stop everything, for good: nothing starts once quitting.
    pub fn quit(&self) {
        let mut l = self.lifecycle.lock().unwrap();
        if let Some(r) = l.recording.take() {
            r.stop();
        }
        self.end_survey(&mut l);
        l.quitting = true;
    }

    /// The survey stops, if one is running (the lock held).
    fn end_survey(&self, l: &mut Lifecycle) {
        if let Some(r) = l.survey.take() {
            r.stop();
            *self.survey_last.lock().unwrap() = None;
            publish(&self.hub, crate::survey::idle());
        }
    }
}

/// What the radios are doing: recording, a survey, or both (the survey on a
/// spare radio no source uses).
#[derive(Default)]
pub struct Lifecycle {
    pub recording: Option<Runner>,
    pub survey: Option<crate::survey::SurveyRunner>,
    /// The recorder is exiting: nothing starts any more.
    quitting: bool,
}

pub struct Runner {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    /// Each source's live controls (a gain changed in Setup goes through them).
    controls: Vec<Arc<Control>>,
}

/// The gain to set on a source running with `old` now that it is `new`:
/// when the gain is all that changed and the radio takes it while running.
/// The gain in dB always; AGC on or off only on an RTL-SDR or an Airspy (the
/// others take that at the next start).
pub fn live_gain(old: &Source, new: &Source) -> Option<Gain> {
    // `old` with `new`'s gain: equal to `new` when only the gain differs.
    let mut o = old.clone();
    let gain = match (&mut o, new) {
        (Source::Rtlsdr { gain_db, agc, .. }, Source::Rtlsdr { gain_db: g, agc: a, .. }) => {
            (*gain_db, *agc) = (*g, *a);
            if *a { Gain::Agc } else { Gain::Db(*g as f32) }
        }
        (
            Source::Airspy { gain_mode, gain_step, lna_step, mixer_step, vga_step, agc, .. },
            Source::Airspy { gain_mode: m, gain_step: g, lna_step: l, mixer_step: x, vga_step: v, agc: a, .. },
        ) => {
            (*gain_mode, *gain_step, *lna_step, *mixer_step, *vga_step, *agc) = (*m, *g, *l, *x, *v, *a);
            Gain::Airspy(airspy_gain(*m, *g, *l, *x, *v, *a))
        }
        (Source::Usrp { gain_db, agc: false, .. }, Source::Usrp { gain_db: g, agc: false, .. })
        | (Source::Bladerf { gain_db, agc: false, .. }, Source::Bladerf { gain_db: g, agc: false, .. }) => {
            *gain_db = *g;
            Gain::Db(*g as f32)
        }
        (Source::Soapy { gain_db, agc: false, .. }, Source::Soapy { gain_db: Some(g), agc: false, .. }) => {
            *gain_db = Some(*g);
            Gain::Db(*g as f32)
        }
        _ => return None,
    };
    (o == *new).then_some(gain)
}

impl Runner {
    /// Source `i`'s settings went from `old` to `new` in Setup: its gain is
    /// applied at once when that's all that changed and the radio can take it
    /// live ([`live_gain`]): that gain, or None when the change waits for the next start.
    pub fn live_source(&self, i: usize, old: &Source, new: &Source) -> Option<Gain> {
        let (c, g) = (self.controls.get(i)?, live_gain(old, new)?);
        *c.gain.lock().unwrap() = Some(g);
        Some(g)
    }

    /// The sources went from `old` to `new` in Setup: each changed one, by its
    /// place, with the gain it took at once ([`Runner::live_source`]) or None.
    /// Sources added, removed or moved wait for the next start (all of them).
    pub fn live_sources(&self, old: &[Source], new: &[Source]) -> Vec<(usize, Option<Gain>)> {
        if old.len() != new.len() {
            return vec![];
        }
        old.iter().zip(new).enumerate().filter(|(_, (o, n))| o != n).map(|(i, (o, n))| (i, self.live_source(i, o, n))).collect()
    }

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
        ctx.config_changed(&shared);
        if let Some(e) = errors.into_iter().next() {
            return Err(e);
        }
    }
    if let Some(p) = cfg.problem() {
        return Err(p);
    }
    // Encryption keys: a file that can't be read, or a bad key in it, stops the start.
    let keys = cfg.load_keys(&ctx.config_path, &|name| crate::unlock::held(&ctx, &cfg, name))?;
    warn_readable_keys(&ctx, &cfg);
    open_spool(&ctx, &cfg);
    // The wall clock now, and a monotonic clock from now: the session's
    // `now_ms` is the one plus the other (wall time that never jumps).
    let epoch_ms = crate::clock::unix_ms() as f64;
    let epoch_at = Instant::now();
    let talkgroups = ctx.talkgroups.lock().unwrap().clone();
    let mut session = Session::new(cfg.clone(), &talkgroups, epoch_ms, &|name| fs::read_to_string(bandplan_path(name)).ok(), local_offset)?;
    session.attach(ctx.shared.clone());
    if cfg.systems.iter().any(|s| s.share) {
        session.set_recorder(&crate::directory::recorder_id());
    }
    // (Testing sharing on a short capture: share after this many seconds of decoding, not 5 minutes.)
    if let Some(s) = std::env::var("TRUNK_PRO_SHARE_AFTER_S").ok().and_then(|v| v.parse::<f64>().ok()) {
        session.share_after(s * 1000.0);
    }
    for k in keys {
        for s in &k.skipped {
            log::warn!("Key source {}: key left out: {s}", k.name);
        }
        let Some(ring) = k.ring else {
            log::warn!("Key source {} is locked: unlock it in Setup → Recording to decrypt {}", k.name, k.systems.join(", "));
            continue;
        };
        for w in ring.warnings() {
            log::warn!("Key source {}: {w}", k.name);
        }
        let n = ring.keys().len();
        log::info!("Key source {}: {n} encryption key{} loaded, for {}", k.name, if n == 1 { "" } else { "s" }, k.systems.join(", "));
        for s in &k.systems {
            session.set_keys(s, Some(ring.clone()));
        }
    }
    session.load_units(&|name| fs::read_to_string(units_path(name)).ok());
    session.load_heard(&fs::read_to_string(heard_path(&cfg)).unwrap_or_default());
    session.load_sharing(&fs::read_to_string(sharing_path()).unwrap_or_default());
    session.load_tune(&fs::read_to_string(tune_path()).unwrap_or_default());
    ctx.plugins.start(&cfg);
    if cfg.sources.iter().any(|s| matches!(s, Source::Rtlsdr { .. })) {
        if let Some(hint) = cfg.rtl_buflen_hint(sdr::rtl_most_on_a_bus()) {
            log::warn!("{hint}");
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    // Each source's samples queue for its own decoding thread ([`feed_source`]).
    let mut queues = Vec::new();
    let centers = cfg.resolved_centers();
    let controls: Vec<Arc<Control>> = cfg.sources.iter().map(|_| Arc::default()).collect();
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
        let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
        queues.push(rx);
        let (stop, center) = (stop.clone(), *center);
        let spawned = spawn_source(&src, i, center, tx, stop, Some(controls[i].clone()), &format!("source-{i}"));
        match spawned {
            Ok(t) => threads.push(t),
            Err(e) => return Err(undo(threads, e)),
        }
    }
    let (ctx2, stop2) = (ctx.clone(), stop.clone());
    // The engine's commands: open before its thread is, so none sent meanwhile is lost.
    let ctl = mpsc::channel::<EngineCmd>();
    *ctx.engine.lock().unwrap() = Some(ctl.0.clone());
    let spawned = std::thread::Builder::new().name("engine".into()).spawn(move || engine_thread(ctx2, cfg, session, queues, stop2, (epoch_ms, epoch_at), ctl));
    match spawned {
        Ok(t) => threads.push(t),
        Err(e) => {
            *ctx.engine.lock().unwrap() = None;
            return Err(undo(threads, e));
        }
    }
    Ok(Runner { stop, threads, controls })
}

/// Where a system's band plan is kept between runs.
/// Warn about a keys file others can read (the keys are as secret as the radios').
fn warn_readable_keys(ctx: &Ctx, cfg: &Config) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in cfg.key_files(&ctx.config_path) {
            if fs::metadata(&p).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                log::warn!("Keys file {} can be read by other users (chmod 600 it)", p.display());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (ctx, cfg);
}

fn bandplan_path(short_name: &str) -> PathBuf {
    crate::paths::data_dir().join(format!("{short_name}.bandplan"))
}

/// Where a system's talkgroup file is kept (`<short name>.talkgroups.csv`, beside the config).
pub(crate) fn talkgroups_path(short_name: &str) -> PathBuf {
    trunk_app::config::talkgroups_path(&crate::paths::data_dir(), short_name)
}

/// The talkgroup files of `cfg`'s systems (trunked and conventional), as kept.
pub fn load_talkgroups(cfg: &Config) -> trunk_app::config::TalkgroupFiles {
    let names = cfg.systems.iter().map(|s| &s.short_name).chain(cfg.conventional.iter().map(|c| &c.short_name));
    names.filter_map(|name| fs::read_to_string(talkgroups_path(name)).ok().map(|csv| (name.clone(), csv))).collect()
}

/// System `system`'s talkgroup file is now `csv` ("": none): kept, told to
/// every browser, and taken by the recording at once; written in the
/// background ([`crate::saver`]).
pub fn set_talkgroups(ctx: &Ctx, system: &str, csv: String) -> Result<(), String> {
    if system.trim().is_empty() || system.contains(['/', '\\']) || system.starts_with('.') {
        return Err(format!("bad short name {system:?}"));
    }
    // (Handed to the writer with the lock held: two browsers' edits are written in the order they're kept.)
    {
        let mut files = ctx.talkgroups.lock().unwrap();
        if csv.trim().is_empty() {
            files.remove(system);
            ctx.saver.remove(talkgroups_path(system));
        } else {
            files.insert(system.to_string(), csv.clone());
            ctx.saver.text(talkgroups_path(system), csv.clone());
        }
    }
    ctx.tell_engine(EngineCmd::Talkgroups { short_name: system.to_string(), csv: csv.clone() });
    // (The config first, then the names: as config_changed takes them.)
    let (cfg, files) = (ctx.config.lock().unwrap().clone(), ctx.talkgroups.lock().unwrap().clone());
    ctx.shared.names.lock().unwrap().set_config(&cfg, &files);
    publish(&ctx.hub, ToClient::Talkgroups { system: system.to_string(), csv });
    Ok(())
}

pub(crate) fn units_path(short_name: &str) -> PathBuf {
    crate::paths::data_dir().join(format!("{short_name}.units.csv"))
}

/// Hand what's due to be kept between runs ([`Session::due`]: it says when
/// each is) to the writer; `all`: everything that changed (the recording's end).
fn save_due(ctx: &Ctx, session: &mut Session, now_ms: f64, all: bool) {
    let heard = heard_path(session.config());
    for saved in session.due(now_ms, all) {
        match saved {
            Saved::Units { system, csv } => ctx.saver.text(units_path(&system), csv),
            Saved::Sharing(json) => ctx.saver.text(sharing_path(), json),
            Saved::Bandplan { key, plan } => ctx.saver.text(bandplan_path(&key), plan),
            Saved::Heard(json) => ctx.saver.text(heard.clone(), json),
            Saved::Tune(json) => ctx.saver.text(tune_path(), json),
            // (Turned into JSON by the writer: it can be megabytes.)
            Saved::Registry { system, radio } => ctx.saver.later(registry_path(&system), move || serde_json::to_string(&radio).ok()),
        }
    }
}

/// Where the radio registry (talkgroups, radios, frequencies heard) is kept, a file per system.
fn radio_dir() -> PathBuf {
    crate::paths::data_dir().join("radio")
}

/// The names the radio registry answers with, as saved: each system's
/// talkgroups (`cfg` with its talkgroup files) and talker aliases. A
/// recording keeps them current from then on.
pub fn load_names(shared: &trunk_app::stats::Shared, cfg: &Config, talkgroups: &trunk_app::config::TalkgroupFiles) {
    let mut book = shared.names.lock().unwrap();
    book.set_config(cfg, talkgroups);
    for n in cfg.systems.iter().map(|s| &s.short_name).chain(cfg.conventional.iter().map(|c| &c.short_name)) {
        if let Ok(csv) = fs::read_to_string(units_path(n)) {
            book.set_units(n, &csv);
        }
    }
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

/// A system's radio registry file.
fn registry_path(short_name: &str) -> PathBuf {
    radio_dir().join(format!("{}.json", trunk_core::metrics::key_part(short_name)))
}

/// Where the codes conventional frequencies carried are kept ([`trunk_app::heard`]).
pub(crate) fn heard_path(cfg: &trunk_app::Config) -> PathBuf {
    crate::paths::data_dir().join(Session::heard_file(cfg))
}

/// AutoTune's scores from earlier runs ([`Saved::Tune`]).
fn tune_path() -> PathBuf {
    crate::paths::data_dir().join("tune.json")
}

/// What sharing with the directory did (each record's version, the last
/// share): beside the config, so sharing never rewrites the config itself.
fn sharing_path() -> PathBuf {
    crate::paths::data_dir().join("sharing.json")
}

/// Replay a capture file as a source, paced to real time or as fast as possible.
/// A thread named `name` streaming source `src` (number `i`) into `tx` at
/// `center` until `stop`; `ctl`: retune and gain requests (the survey). A
/// capture file plays at its own centre. Recording and the survey both
/// start their sources this way.
pub(crate) fn spawn_source(src: &Source, i: usize, center: f64, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>, ctl: Option<Arc<sdr::Control>>, name: &str) -> std::io::Result<JoinHandle<()>> {
    let src = src.clone();
    std::thread::Builder::new().name(name.to_string()).spawn(move || match src {
        Source::Rtlsdr { serial, rate_hz, gain_db, agc, ppm, buf_len, buffers, .. } => {
            sdr::run_with(i, RtlConfig { serial, center_hz: center as u64, rate_hz: rate_hz as u32, gain_db: (!agc).then_some(gain_db as f32), ppm, buf_len, buffers }, tx, stop, ctl)
        }
        Source::Usrp { args, rate_hz, gain_db, agc, antenna, ppm, .. } => sdr::run_with(i, uhd::UsrpConfig { args, center_hz: center, rate_hz, gain_db, agc, antenna, ppm }, tx, stop, ctl),
        Source::Airspy { serial, rate_hz, gain_mode, gain_step, lna_step, mixer_step, vga_step, agc, bias_tee, packing, ppm, .. } => {
            let gain = airspy_gain(gain_mode, gain_step, lna_step, mixer_step, vga_step, agc);
            sdr::run_with(i, airspy::AirspyConfig { serial, center_hz: center, rate_hz, gain, bias_tee, packing, ppm }, tx, stop, ctl)
        }
        Source::Bladerf { serial, rate_hz, bandwidth_hz, gain_db, agc, bias_tee, ppm, .. } => {
            sdr::run_with(i, bladerf::BladerfConfig { serial, center_hz: center, rate_hz, bandwidth_hz, gain_db, agc, bias_tee, ppm }, tx, stop, ctl)
        }
        Source::Soapy { args, rate_hz, agc, gain_db, gains, antenna, settings, ppm, .. } => {
            let gains = gains.into_iter().collect();
            sdr::run_with(i, soapy::SoapyConfig { args, center_hz: center, rate_hz, agc, gain_db, gains, antenna, settings, ppm }, tx, stop, ctl)
        }
        Source::File { path, rate_hz, realtime, format, .. } => run_file(i, &path, rate_hz, realtime, SampleFormat::of(format, &path), tx, stop),
    })
}

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
    let pool = crate::sdr::Pool::default();
    let mut buf = vec![0u8; chunk];
    while !stop.load(Ordering::Relaxed) {
        buf.resize(chunk, 0);
        let n = read_full(&mut f, &mut buf);
        if n < bps {
            break;
        }
        buf.truncate(n - n % bps);
        sent += (buf.len() / bps) as u64;
        let msg = match format {
            SampleFormat::Cu8 => SourceMsg::Data { source, bytes: pool.copy(&buf), dropped: 0, at: Instant::now() },
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
                log::warn!("No RAM spool ({e}): calls go to the recordings folder");
                None
            }
        }
    } else {
        None
    };
    *ctx.spool.lock().unwrap() = spool;
}

/// One source's decoding thread: its samples into the engine as they come
/// (each source on a core of its own), anything else to the engine thread.
/// On stop, what the source had already sent is decoded first (up to the
/// queue's 256 blocks: about 2.5 s of 10 ms USRP / Soapy blocks, about 14 s
/// of RTL-SDR's at 2.4 MSPS): calls in progress keep it.
fn feed_source(feeder: Feeder, rx: mpsc::Receiver<SourceMsg>, ctl: mpsc::Sender<EngineCmd>, stop: Arc<AtomicBool>, epoch: (f64, Instant), air: Air) {
    let now_ms = || epoch.0 + epoch.1.elapsed().as_secs_f64() * 1000.0;
    // An instant on the same clock (when a driver handed samples over).
    let at_ms = |at: Instant| epoch.0 + at.saturating_duration_since(epoch.1).as_secs_f64() * 1000.0;
    // (An Airspy's real samples become IQ here, on this thread: see [`crate::sdr::IqFrom`].)
    let mut iqf = crate::sdr::IqFrom::default();
    let mut take = |m: SourceMsg| match iqf.iq(m) {
        SourceMsg::Data { source, bytes, dropped, at } => {
            debug_assert_eq!(source, feeder.source());
            air.keep_up(&stop);
            feeder.push(&bytes, dropped, at_ms(at));
            air.fed((bytes.len() / 2) as u64 + dropped);
        }
        SourceMsg::Iq { source, samples, dropped, at } => {
            debug_assert_eq!(source, feeder.source());
            air.keep_up(&stop);
            feeder.push_iq(&samples, dropped, at_ms(at));
            air.fed(samples.len() as u64 + dropped);
        }
        m => {
            let _ = ctl.send(EngineCmd::Source(m));
        }
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            rx.try_iter().for_each(&mut take);
            return;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => take(m),
            Err(RecvTimeoutError::Timeout) => feeder.idle(now_ms()),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    air.done();
}

/// How far into its air each capture played as fast as it goes is, ms
/// (`u64::MAX`: done, or a source that keeps time by itself). Such a capture
/// has no clock to keep it in step with the others: it waits while it's
/// more than [`Air::LEAD_MS`] ahead of the slowest of them, as sources on the
/// air never are by much (a grant on one source opens a channel on another
/// where that one is). Sources on the air never wait, nor are waited for.
struct Air {
    at_ms: Arc<Vec<AtomicU64>>,
    me: usize,
    rate_hz: f64,
    samples: std::cell::Cell<u64>,
    paced: bool,
}

impl Air {
    const LEAD_MS: u64 = 100;

    fn fed(&self, n: u64) {
        if self.paced {
            return;
        }
        self.samples.set(self.samples.get() + n);
        self.at_ms[self.me].store((self.samples.get() as f64 / self.rate_hz * 1000.0) as u64, Ordering::Relaxed);
    }

    fn keep_up(&self, stop: &AtomicBool) {
        if self.paced {
            return;
        }
        let mine = self.at_ms[self.me].load(Ordering::Relaxed);
        while !stop.load(Ordering::Relaxed) && self.at_ms.iter().enumerate().any(|(i, a)| i != self.me && a.load(Ordering::Relaxed).saturating_add(Self::LEAD_MS) < mine) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn done(&self) {
        self.at_ms[self.me].store(u64::MAX, Ordering::Relaxed);
    }
}

/// `queues`: each source's samples. `epoch`: the wall clock (Unix ms) at an
/// instant, for the session's clock. `ctl`: everything for this thread — the
/// sources' errors and ends (by way of their decoding threads), and the
/// browsers' and the server's changes ([`Ctx::engine`] holds its sender).
fn engine_thread(ctx: Arc<Ctx>, cfg: Config, mut session: Session, queues: Vec<mpsc::Receiver<SourceMsg>>, stop: Arc<AtomicBool>, epoch: (f64, Instant), ctl: (mpsc::Sender<EngineCmd>, mpsc::Receiver<EngineCmd>)) {
    ctx.set_phase("running", None, false);
    // Calls are stamped by this clock: the clock's health watches it against the wall clock.
    ctx.clock.recording(Some(epoch));
    let mut radio_at = Instant::now();
    let (ctl_tx, rx) = ctl;
    session.set_topics(ctx.topics());
    let at_ms: Arc<Vec<AtomicU64>> = Arc::new(cfg.sources.iter().map(|_| AtomicU64::new(0)).collect());
    let feeders: Vec<JoinHandle<()>> = queues
        .into_iter()
        .enumerate()
        .map(|(i, q)| {
            let (f, ctl, stop) = (session.feeder(i), ctl_tx.clone(), stop.clone());
            let src = &cfg.sources[i];
            let paced = !matches!(src, Source::File { realtime: false, .. });
            if paced {
                at_ms[i].store(u64::MAX, Ordering::Relaxed);
            }
            let air = Air { at_ms: at_ms.clone(), me: i, rate_hz: src.rate_hz(), samples: Default::default(), paced };
            std::thread::Builder::new().name(format!("dsp-{i}")).spawn(move || feed_source(f, q, ctl, stop, epoch, air)).expect("thread")
        })
        .collect();
    drop(ctl_tx);
    let dir = PathBuf::from(&cfg.recording.capture_dir);
    // Each system's file rules, by a call's `system` (trunked or conventional).
    let rules = {
        let (each, all) = (cfg.recordings().iter().map(FileRules::of).collect::<Vec<_>>(), FileRules::of(&cfg.recording));
        move |system: u16| each.get(system as usize).copied().unwrap_or(all)
    };
    let spool = ctx.spool.lock().unwrap().clone();
    let encoder = plugins::Encoder::find(cfg.recording.m4a.encoder).map(|e| (e, cfg.recording.m4a.bitrate_kbps.clamp(8, 320)));
    if encoder.is_none() && cfg.recordings().iter().any(|r| r.archive_m4a) {
        log::warn!("{}", if cfg.recording.m4a.encoder == crate::config::M4aEncoder::None { "M4A encoding is off: calls are kept as WAV only" } else { "No M4A encoder found (install ffmpeg): calls are kept as WAV only" });
    }
    // Calls are finished on threads of their own (each may be encoding an
    // .m4a), so a slow disk or encoder doesn't hold up decoding.
    let (fin_tx, fin_rx) = mpsc::sync_channel::<Finish>(1024);
    let fin_rx = Arc::new(Mutex::new(fin_rx));
    let finishers: Vec<JoinHandle<()>> = (0..finishers())
        .map(|i| {
            let (ctx, dir, spool, encoder, rx) = (ctx.clone(), dir.clone(), spool.clone(), encoder.clone(), fin_rx.clone());
            std::thread::Builder::new().name(format!("finish-{i}")).spawn(move || finish_calls(&ctx, &dir, spool.as_deref(), &rx, encoder.as_ref())).expect("thread")
        })
        .collect();
    let fin = |f: Finish| {
        // Behind on writing or encoding: decoding is on the sources' own
        // threads, so this waits a while; past that, it's finished here (by
        // the same plan) rather than dropped.
        let until = Instant::now() + FINISH_WAIT;
        let mut f = f;
        loop {
            match fin_tx.try_send(f) {
                Ok(()) => return,
                Err(mpsc::TrySendError::Full(back)) if Instant::now() < until => {
                    f = back;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(mpsc::TrySendError::Full(back)) => return finish_one(&ctx, &dir, spool.as_deref(), back, encoder.as_ref()),
                Err(mpsc::TrySendError::Disconnected(_)) => return,
            }
        }
    };
    let now_ms = || epoch.0 + epoch.1.elapsed().as_secs_f64() * 1000.0;
    let mut out = Vec::new();
    let mut ended_all = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // (What the sources decoded meanwhile is collected below, every 20 ms at most.)
        // (What the sources decoded meanwhile is collected below, every 20 ms at most.)
        let mut next = rx.recv_timeout(Duration::from_millis(20)).ok();
        while let Some(c) = next {
            match c {
                EngineCmd::Source(SourceMsg::Error { source, error }) => session.source_error(source, &error),
                EngineCmd::Source(SourceMsg::End { source }) => {
                    if session.source_ended(source) {
                        // (Not "ended" when the user stopped it: the file thread ends on stop too.)
                        ended_all = !stop.load(Ordering::Relaxed);
                    }
                }
                EngineCmd::Source(_) => {}
                EngineCmd::Topics(t) => session.set_topics(t),
                EngineCmd::Talkgroups { short_name, csv } => {
                    if session.set_talkgroups(&short_name, &csv) {
                        log::info!("[{short_name}] Talkgroups updated");
                    }
                }
                EngineCmd::Keys { short_name, ring } => {
                    session.set_keys(&short_name, ring);
                }
                EngineCmd::ShareSystem(sys) => {
                    if sys.share {
                        session.set_recorder(&crate::directory::recorder_id());
                    }
                    session.set_share(&sys);
                }
                EngineCmd::Shared { short_name, result } => session.share_done(&short_name, now_ms(), result),
            }
            next = rx.try_recv().ok();
        }
        if ended_all {
            break;
        }
        // (The plugins can be swapped meanwhile: their settings changed. This iteration's, held by no lock.)
        let plugins = ctx.plugins.host();
        session.plugin_topics = plugins.as_ref().map_or(Default::default(), |p| p.topics);
        session.want_audio = ctx.hub.receiver_count() > 0 || plugins.as_ref().is_some_and(|p| p.audio);
        session.poll(now_ms(), &mut out);
        deliver(&ctx, &mut out, plugins.as_deref(), &rules, &fin);
        drop(plugins);
        // (Learned band plans and DMR channel tables, talker aliases, …: a crash or kill loses little.)
        save_due(&ctx, &mut session, now_ms(), false);
        if radio_at.elapsed() >= Duration::from_secs(5) {
            radio_at = Instant::now();
            ctx.clock.set_radio(session.radio_clocks());
        }
    }
    *ctx.engine.lock().unwrap() = None;
    ctx.set_phase("stopping", None, false);
    // The decoding threads finish what their sources had already sent
    // ([`feed_source`]); once the sources are gone (stopped, or files at their end).
    for f in feeders {
        let _ = f.join();
    }
    session.poll(now_ms(), &mut out);
    session.finish(&mut out);
    deliver(&ctx, &mut out, ctx.plugins.host().as_deref(), &rules, &fin);
    drop(fin_tx);
    for f in finishers {
        let _ = f.join();
    }
    // Uploads in flight get a moment to finish.
    ctx.plugins.stop(Duration::from_secs(10));
    save_due(&ctx, &mut session, now_ms(), true);
    // (All of it on disk before recording is said to have stopped.)
    ctx.saver.flush();
    ctx.clock.recording(None);
    stop.store(true, Ordering::Relaxed);
    ctx.set_phase("idle", None, ended_all);
}

/// A concluded call, for [`finish_calls`] to store.
struct Finish {
    rel: String,
    system: u16,
    wav: Vec<u8>,
    record: trunk_recorder_plugin::CallRecord,
    frames: Option<trunk_core::trunk::Capture>,
    rules: FileRules,
    /// Some of it was decrypted with our keys: only plugins with `shareDecrypted` take it.
    decrypted: bool,
}

/// How many calls are finished at once (each may be encoding an .m4a).
fn finishers() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 3)
}

/// How long decoding waits for a place in the finish queue before finishing a call itself.
const FINISH_WAIT: Duration = Duration::from_secs(5);

/// A finish thread: each call it's given, until the recording ends.
fn finish_calls(ctx: &Ctx, dir: &Path, spool: Option<&crate::spool::Spool>, rx: &Mutex<mpsc::Receiver<Finish>>, encoder: Option<&(plugins::Encoder, u32)>) {
    loop {
        let Ok(f) = rx.lock().unwrap().recv() else { return };
        finish_one(ctx, dir, spool, f, encoder);
    }
}

/// A call's files, all of them, as its [`plugins::plan`] says: what's kept
/// goes to the recordings folder, what only the plugins need to the spool
/// when there is one with room. Then the `concluded` message and history,
/// the .m4a (to keep, or for a plugin; its WAV instead should that fail),
/// and the plugins, handed the files as they are; their results settle
/// what's kept ([`crate::plugins::Archive`]).
fn finish_one(ctx: &Ctx, dir: &Path, spool: Option<&crate::spool::Spool>, mut f: Finish, encoder: Option<&(plugins::Encoder, u32)>) {
    let host = ctx.plugins.host();
    let r = f.rules;
    let to_plugins = host.as_ref().map_or(Default::default(), |h| h.plugin_audio(f.decrypted));
    let p = plugins::plan(r, to_plugins, encoder.is_some());
    let (keep_wav, keep_m4a, keep_json, keep_audio) = (p.keep_wav, p.keep_m4a, p.keep_json, p.keep_audio);
    let base = dir.join(&f.rel);
    // Stamped while the clock was off: said with it, for whoever matches it with others' calls.
    f.record.clock_off = ctx.clock.off();
    // (The record written once, as the .json file has it.)
    let json = f.record.to_json();
    let need = json.len() + f.wav.len() / 4 + if p.write_wav && !keep_wav { f.wav.len() } else { 0 };
    let spooled = spool.filter(|s| p.spool && s.room_for(need as u64)).map(|s| s.dir.join(&f.rel));
    let at = |kept: bool, ext: &str| PathBuf::from(format!("{}.{ext}", if kept { &base } else { spooled.as_ref().unwrap_or(&base) }.display()));
    let (wav_at, json_at, frames_at, m4a_at) = (at(keep_wav, "wav"), at(keep_json, "json"), at(keep_audio, f.frames.as_ref().map_or("sdr", |c| c.ext)), at(keep_m4a, "m4a"));
    for d in [&wav_at, &json_at, &m4a_at].into_iter().filter_map(|p| p.parent()) {
        let _ = fs::create_dir_all(d);
    }
    let ok = (!p.write_wav || fs::write(&wav_at, &f.wav).is_ok())
        && (!p.write_json || fs::write(&json_at, &json).is_ok())
        && f.frames.as_ref().filter(|_| p.write_frames).is_none_or(|c| fs::write(&frames_at, &c.bytes).is_ok());
    if !ok {
        log::warn!("Couldn't write {}", base.display());
        return;
    }
    // Whether its audio and JSON stay once the plugins are done (a plugin's failure may keep them after all: archive::settle).
    let mut entry = trunk_app::session::call_entry(&f.rel, &f.record);
    entry["audio"] = json!(keep_audio);
    entry["json"] = json!(keep_json);
    publish(&ctx.hub, ToClient::Concluded { entry: entry.clone() });
    {
        let mut h = ctx.history.lock().unwrap();
        h.push_front(entry);
        h.truncate(HISTORY_KEPT);
    }
    let mut wav_written = p.write_wav;
    let mut m4a = None;
    if let Some((e, kbps)) = encoder.filter(|_| p.make_m4a) {
        match e.m4a(&f.wav, &m4a_at, *kbps) {
            Ok(()) => m4a = Some(m4a_at),
            Err(err) => {
                m4a_failed(&f.rel, &err);
                // The call's audio is then the WAV.
                if !wav_written {
                    wav_written = fs::write(&wav_at, &f.wav).is_ok();
                }
            }
        }
    }
    ctx.plugins.archive.expect(&f.rel, to_plugins.takers, r, &base, spooled.as_deref(), Some(wav_written), Some(f.record.stop_time_ms.max(0) as u64));
    if let Some(h) = host {
        h.concluded(f.system, &f.rel, f.record, trunk_recorder_plugin::CallFiles { json: json_at, wav: wav_at, m4a }, f.decrypted);
    }
}

/// An .m4a couldn't be made: said, at most once a minute.
fn m4a_failed(rel: &str, err: &str) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if last.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
        *last = Some(Instant::now());
        log::warn!("M4A of {rel}: {err} (its audio is the WAV)");
    }
}

/// Share a system with the directory; tell the engine how it went.
fn share(ctx: &Arc<Ctx>, short_name: String, observation: serde_json::Value, site_names: Vec<String>) {
    let base = ctx.config.lock().unwrap().server.trp_api_base().to_string();
    // The config gets the link only when it's new (a first share, a site new to
    // the directory): edit_config leaves it alone when nothing changed.
    let result = crate::directory::share(&base, &observation).and_then(|d| {
        if d.system.is_empty() {
            return Ok(None);
        }
        crate::server::edit_config(ctx, |c| {
            c.link_shared(&short_name, &d.system, &site_names, &d.sites);
        })
        .map_err(|e| format!("Shared, but the directory link couldn't be kept: {e}"))?;
        Ok(Some(trunk_app::share::Linked { id: d.system, rev: d.rev }))
    });
    match &result {
        Ok(_) => log::info!("{short_name}: shared with the directory"),
        Err(e) => log::warn!("{short_name}: not shared with the directory: {e}"),
    }
    ctx.tell_engine(EngineCmd::Shared { short_name, result });
}

/// Forward everything to the browsers and plugins; calls go to be stored.
fn deliver(ctx: &Arc<Ctx>, out: &mut Vec<Output>, plugins: Option<&PluginHost>, rules: &dyn Fn(u16) -> FileRules, finish: &dyn Fn(Finish)) {
    for o in out.drain(..) {
        match o {
            Output::Message(m) => {
                if matches!(m, ToClient::Heard { .. }) {
                    *ctx.heard_last.lock().unwrap() = Some(m.clone());
                }
                publish(&ctx.hub, m)
            }
            Output::Audio { short_name, tg, frame, decrypted, .. } => {
                if let Some(p) = plugins {
                    p.audio_frame(&frame, decrypted);
                }
                let _ = ctx.hub.send(Arc::new(Out::Audio { short_name, tg, frame }));
            }
            Output::Plugin(m) => {
                if let Some(p) = plugins {
                    p.event(&m);
                }
            }
            Output::Log(r) => crate::logging::record(&r),
            Output::Topic { topic, msg } => {
                let _ = ctx.hub.send(Arc::new(Out::Topic { topic, text: msg.to_json() }));
            }
            Output::Rollup(r) => ctx.store.add(r),
            // Into the config where it has none, saved and sent to the browsers.
            Output::CallSign { short_name, freq_hz, call_sign } => {
                let mut learned = false;
                let saved = crate::server::edit_config(ctx, |c| learned = c.learn_call_sign(&short_name, freq_hz, &call_sign));
                let mhz = freq_hz / 1e6;
                match saved {
                    Err(e) => log::warn!("{short_name}: heard call sign {call_sign} on {mhz:.4} MHz, but {e}"),
                    Ok(()) if learned => log::info!("{short_name}: FCC call sign {call_sign} for {mhz:.4} MHz, from the air"),
                    Ok(()) => {}
                }
            }
            // To the directory, on a thread of its own; its answer links the system in the config.
            Output::Share { short_name, observation, site_names } => {
                let ctx = ctx.clone();
                let _ = std::thread::Builder::new().name("share".into()).spawn(move || share(&ctx, short_name, observation, site_names));
            }
            Output::File { rel, system, wav, record, frames, decrypted } => finish(Finish { rel, system, wav, record, frames, rules: rules(system), decrypted }),
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
    publish(&ctx.hub, ToClient::CallFiles { path: rel.to_string(), audio, json });
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

/// How far back `olderCalls` looks.
const OLDER_WINDOW_MS: i64 = 24 * 3600 * 1000;

/// `olderCalls` (the Listen page's "Load older"): up to `limit` calls that
/// started before `before_ms`, newest first, from the last 24 hours. Only the
/// day folders of Trunk Recorder's layout are read (a system with its own
/// filename format isn't searched), so this stays cheap without an index.
pub fn older_calls(ctx: &Ctx, before_ms: i64, limit: usize) -> ToClient {
    let (dir, names) = {
        let c = ctx.config.lock().unwrap();
        // (And a site's, as a system of its own: its calls from before are under its name.)
        let mut names: Vec<String> = c.systems.iter().flat_map(|s| std::iter::once(s.short_name.clone()).chain(trunk_app::migrate::old_names(s))).collect();
        names.extend(c.conventional.iter().map(|v| v.short_name.clone()));
        (PathBuf::from(&c.recording.capture_dir), names)
    };
    let now_ms = crate::clock::unix_ms() as i64;
    let floor_ms = now_ms - OLDER_WINDOW_MS;
    let before_ms = before_ms.min(now_ms + 60_000);
    // Each system's day folders from `before` back to the window's start.
    let mut folders = std::collections::BTreeSet::new();
    let mut t = before_ms.div_euclid(1000);
    while t * 1000 >= floor_ms - 86_400_000 {
        for n in &names {
            folders.insert(dir.join(trunk_app::filename::default_path(n, "", t, local_offset(t))));
        }
        t -= 86_400;
    }
    // A call's start from its name (<talkgroup>-<start>_<freq>), else when the file was written.
    let start_of = |p: &Path, e: &fs::DirEntry| -> i64 {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let from_name = stem.split_once('-').and_then(|(_, r)| r.split(['_', '.', '-']).next()).and_then(|s| s.parse::<i64>().ok());
        from_name.unwrap_or_else(|| e.metadata().and_then(|m| m.modified()).map_or(0, |t| crate::clock::unix_s_of(t) as i64))
    };
    let mut found: Vec<(i64, PathBuf)> = Vec::new();
    for f in &folders {
        let Ok(rd) = fs::read_dir(f) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                let s = start_of(&p, &e);
                if s * 1000 <= before_ms && (s + 1) * 1000 > floor_ms {
                    found.push((s, p));
                }
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    // (Names give the second: the last second taken is taken whole, so the
    // next page, before the oldest call's millisecond, misses none.)
    let mut entries: Vec<(i64, Value)> = Vec::new();
    let mut read = 0;
    for (s, p) in &found {
        if entries.len() >= limit && found[read - 1].0 != *s {
            break;
        }
        read += 1;
        let Some(record) = fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
        let start = record["start_time_ms"].as_i64().or_else(|| record["start_time"].as_i64().map(|s| s * 1000)).unwrap_or(0);
        if start >= before_ms || start < floor_ms {
            continue;
        }
        let Ok(rel) = p.strip_prefix(&dir) else { continue };
        let audio = ["wav", "m4a"].iter().any(|x| p.with_extension(x).exists());
        entries.push((start, json!({ "path": rel.with_extension("").to_string_lossy().replace('\\', "/"), "record": record, "audio": audio, "json": true })));
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let entries: Vec<Value> = entries.into_iter().map(|(_, e)| e).collect();
    ToClient::OlderCalls { before: before_ms, entries, more: read < found.len() }
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
            let audio = ["wav", "m4a"].iter().any(|x| p.with_extension(x).exists());
            Some(json!({ "path": rel.to_string_lossy().replace('\\', "/"), "record": record, "audio": audio, "json": true }))
        })
        .collect()
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use serde_json::json;

    /// A source of kind `kind` with the keys in `v`.
    fn src(kind: &str, mut v: serde_json::Value) -> Source {
        v["type"] = kind.into();
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_gain_change_goes_live_when_the_radio_can_take_it() {
        let rtl = |gain: f64, agc: bool| src("rtlsdr", json!({ "serial": "1000", "centerHz": 453e6, "rateHz": 2.4e6, "gainDb": gain, "agc": agc, "ppm": 0 }));
        assert_eq!(live_gain(&rtl(33.8, false), &rtl(22.9, false)), Some(Gain::Db(22.9)));
        assert_eq!(live_gain(&rtl(33.8, false), &rtl(33.8, true)), Some(Gain::Agc), "an RTL-SDR's AGC, live");
        let usrp = |gain: f64, agc: bool| src("usrp", json!({ "args": "", "antenna": "", "centerHz": 853e6, "rateHz": 8e6, "gainDb": gain, "agc": agc, "ppm": 0 }));
        assert_eq!(live_gain(&usrp(40.0, false), &usrp(30.0, false)), Some(Gain::Db(30.0)));
        assert_eq!(live_gain(&usrp(40.0, false), &usrp(40.0, true)), None, "USRP AGC waits for the start");
        let blade = |gain: f64, agc: bool| src("bladerf", json!({ "serial": "f12ce103", "centerHz": 857e6, "rateHz": 8e6, "gainDb": gain, "agc": agc }));
        assert_eq!(live_gain(&blade(30.0, false), &blade(42.0, false)), Some(Gain::Db(42.0)), "a bladeRF's gain, live");
        assert_eq!(live_gain(&blade(30.0, false), &blade(30.0, true)), None, "bladeRF AGC waits for the start");
        let air = |lna: u8| src("airspy", json!({ "serial": "", "centerHz": 852e6, "rateHz": 6e6, "gainMode": "manual", "lnaStep": lna, "ppm": 0 }));
        assert_eq!(live_gain(&air(10), &air(12)), Some(Gain::Airspy(airspy::AirspyGain::Manual { lna: 12, mixer: 10, vga: 10 })), "an Airspy's LNA, live");
    }

    #[test]
    fn anything_besides_the_gain_waits_for_the_start() {
        let rtl_at = |center: f64, gain: f64| src("rtlsdr", json!({ "serial": "1000", "centerHz": center, "rateHz": 2.4e6, "gainDb": gain, "ppm": 0 }));
        assert_eq!(live_gain(&rtl_at(453e6, 33.8), &rtl_at(460e6, 22.9)), None, "a retune with it");
        let air_at = |center: f64| src("airspy", json!({ "serial": "", "centerHz": center, "rateHz": 6e6, "gainMode": "manual", "lnaStep": 10, "ppm": 0 }));
        assert_eq!(live_gain(&air_at(852e6), &air_at(853e6)), None, "an Airspy retune waits");
        let rtl = src("rtlsdr", json!({ "serial": "", "centerHz": 453e6, "rateHz": 2.4e6, "gainDb": 30, "ppm": 0 }));
        assert_eq!(live_gain(&rtl, &air_at(852e6)), None, "another kind of radio");
    }

    #[test]
    fn several_airspys_each_take_their_own_gain() {
        let air = |serial: &str, lna: u8| src("airspy", json!({ "serial": serial, "centerHz": 852e6, "rateHz": 6e6, "gainMode": "manual", "lnaStep": lna, "ppm": 0 }));
        let lin = |serial: &str, step: u8| src("airspy", json!({ "serial": serial, "centerHz": 852e6, "rateHz": 6e6, "gainMode": "linearity", "gainStep": step, "ppm": 0 }));
        let r = Runner { stop: Arc::default(), threads: vec![], controls: (0..3).map(|_| Arc::default()).collect() };
        let pending = |i: usize| *r.controls[i].gain.lock().unwrap();
        let old = [air("A1", 10), air("B2", 10), air("C3", 10)];
        // The second's LNA: only it.
        let one = r.live_sources(&old, &[air("A1", 10), air("B2", 14), air("C3", 10)]);
        assert_eq!(one, vec![(1, Some(Gain::Airspy(airspy::AirspyGain::Manual { lna: 14, mixer: 10, vga: 10 })))]);
        assert_eq!((pending(0), pending(2)), (None, None), "the others are left alone");
        r.controls[1].take();
        // Two in one save, one of them to another mode: each its own.
        let two = r.live_sources(&old, &[lin("A1", 18), air("B2", 10), air("C3", 6)]);
        assert_eq!(two.len(), 2);
        assert_eq!(pending(0), Some(Gain::Airspy(airspy::AirspyGain::Linearity(18))));
        assert_eq!(pending(1), None);
        assert_eq!(pending(2), Some(Gain::Airspy(airspy::AirspyGain::Manual { lna: 6, mixer: 10, vga: 10 })));
        (r.controls[0].take(), r.controls[2].take());
        // Two swapped in Setup: another radio in each place, not a gain change.
        assert_eq!(r.live_sources(&old, &[air("B2", 10), air("A1", 10), air("C3", 10)]), vec![(0, None), (1, None)]);
        assert!((0..3).all(|i| pending(i).is_none()));
        // One added: everything waits for the start.
        assert!(r.live_sources(&old, &[air("A1", 12), air("B2", 10), air("C3", 10), air("D4", 10)]).is_empty());
        assert!(pending(0).is_none());
    }
}
