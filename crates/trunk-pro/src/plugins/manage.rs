//! Plugins as the interface sees them: what's installed and what each is
//! (`plugins` messages), how each is doing while recording (`pluginRuntime`),
//! installing and removing them. Their settings are in the config (see
//! [`super`]); when a saved config changes them while recording, the server
//! calls [`Plugins::reload`], which swaps in a new [`PluginHost`].
//!
//! Browser → server: `plugins` (the list), `addPlugin {path}`, `removePlugin
//! {id}` (the server forgets it in the config), `pluginStore {refresh?}` (the registry's list),
//! `installPlugin {id}` (from the registry; also an update) or `installPlugin
//! {repository, tag?}` (a GitHub release that isn't in it). Server → browser:
//! `plugins`, `pluginRuntime {id, runtime}` (its state, counts and recent
//! log, as they change), `pluginStore`, and `pluginInstall {key, id, stage,
//! message?}` as an install goes ("finding", "downloading", "checking",
//! "installing", then "done" or "failed").

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use trunk_core::metrics::{key_part, Sink};
use trunk_recorder_plugin::{Level, Manifest, Metrics, Outcome, State, SystemInfo};

use super::host::Notes;
use super::store::{self, Catalog};
use super::{describe, executable, plugins_dir, Archive, Encoder, Note, PluginHost, Spec};
use crate::config::{Config, PluginSetup};
use crate::runtime::{publish, Hub};

/// How a plugin is doing, for the interface.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    /// "off" (not recording, or disabled) | "starting" | "ok" | "warning" | "error"
    pub state: &'static str,
    pub message: String,
    /// Calls handled since recording started.
    pub ok: u64,
    pub skipped: u64,
    pub failed: u64,
    pub last_failure: String,
    /// Its recent log lines.
    pub log: VecDeque<LogEntry>,
    /// What it last said of its work (queue, timing, services), if it says.
    pub metrics: Option<Metrics>,
    /// Unix s of its last result, last success, last failure.
    pub last_result: Option<f64>,
    pub last_ok: Option<f64>,
    pub last_fail: Option<f64>,
    /// Its process: restarts, events dropped (it fell behind), up for (s).
    pub restarts: u64,
    pub dropped: u64,
    pub uptime_s: Option<f64>,
    /// Results per minute, the last hour: (minute start, Unix s; [ok, skipped, failed]).
    pub minutes: VecDeque<(i64, [u32; 3])>,
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime {
            state: "off",
            message: String::new(),
            ok: 0,
            skipped: 0,
            failed: 0,
            last_failure: String::new(),
            log: VecDeque::new(),
            metrics: None,
            last_result: None,
            last_ok: None,
            last_fail: None,
            restarts: 0,
            dropped: 0,
            uptime_s: None,
            minutes: VecDeque::new(),
        }
    }
}

impl Runtime {
    fn count(&mut self, outcome: Outcome) {
        let t = now();
        self.last_result = Some(t);
        let m = (t as i64).div_euclid(60) * 60;
        if self.minutes.back().is_none_or(|b| b.0 != m) {
            self.minutes.push_back((m, [0; 3]));
            while self.minutes.front().is_some_and(|f| f.0 <= m - 3600) {
                self.minutes.pop_front();
            }
        }
        let b = &mut self.minutes.back_mut().unwrap().1;
        match outcome {
            Outcome::Ok => {
                b[0] += 1;
                self.last_ok = Some(t);
            }
            Outcome::Skipped => b[1] += 1,
            Outcome::Failed => {
                b[2] += 1;
                self.last_fail = Some(t);
            }
        }
    }

    /// For the dashboard's series: 0 ok, 1 warning, 2 error (off: nothing).
    fn state_code(&self) -> Option<f64> {
        match self.state {
            "ok" | "starting" => Some(0.0),
            "warning" => Some(1.0),
            "error" => Some(2.0),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    /// Unix seconds.
    pub time: f64,
    pub level: &'static str,
    pub text: String,
}

const LOG_LINES: usize = 50;
/// How long the registry's list is used before it's fetched again; sooner
/// when it couldn't be fetched.
const STORE_FRESH: Duration = Duration::from_secs(15 * 60);
const STORE_RETRY: Duration = Duration::from_secs(60);

/// An executable's modification time, and what `--describe` said then.
type Described = (Option<SystemTime>, Result<Manifest, String>);

/// The plugins' part of the app's shared state.
pub struct Plugins {
    /// The plugins running while recording.
    pub host: RwLock<Option<PluginHost>>,
    /// Calls whose files wait on the plugins' results.
    pub archive: Arc<Archive>,
    /// What the running recording's plugins were started with, to restart them.
    env: Mutex<Option<(Vec<SystemInfo>, PathBuf)>>,
    runtime: Arc<Mutex<BTreeMap<String, Runtime>>>,
    /// Manifests by executable (and its modification time): `--describe` runs once per build.
    manifests: Mutex<HashMap<PathBuf, Described>>,
    /// The registry's list, and when it was got.
    store: Mutex<Option<(Instant, Catalog)>>,
    /// Installs under way, by their key (the id, or the repository asked for).
    installing: Mutex<BTreeSet<String>>,
    hub: Hub,
    /// Where health changes go as events (the dashboard's feed, alert watchers).
    monitor: Arc<trunk_app::stats::Shared>,
}

impl Plugins {
    pub fn new(hub: Hub, monitor: Arc<trunk_app::stats::Shared>) -> Plugins {
        Plugins {
            monitor,
            host: RwLock::new(None),
            archive: Arc::default(),
            env: Mutex::new(None),
            runtime: Arc::new(Mutex::new(BTreeMap::new())),
            manifests: Mutex::new(HashMap::new()),
            store: Mutex::new(None),
            installing: Mutex::new(BTreeSet::new()),
            hub,
        }
    }

    /// The installed plugins (in the plugins folder) and those the config names.
    fn entries(cfg: &Config) -> BTreeMap<String, PluginSetup> {
        let mut all = cfg.plugins.clone();
        if let Ok(rd) = std::fs::read_dir(plugins_dir()) {
            for d in rd.flatten() {
                let id = d.file_name().to_string_lossy().to_string();
                if store::valid_id(&id) && executable(&id, &PluginSetup::default()).is_file() {
                    all.entry(id).or_default();
                }
            }
        }
        all
    }

    fn manifest(&self, exe: &Path) -> Result<Manifest, String> {
        let mtime = std::fs::metadata(exe).and_then(|m| m.modified()).ok();
        if let Some((t, m)) = self.manifests.lock().unwrap().get(exe) {
            if *t == mtime {
                return m.clone();
            }
        }
        let m = if exe.is_file() { describe(exe) } else { Err(format!("{} isn't there", exe.display())) };
        self.manifests.lock().unwrap().insert(exe.to_path_buf(), (mtime, m.clone()));
        m
    }

    /// The `plugins` message: what's installed and what each is. (Their
    /// settings are in the config.)
    pub fn list_json(&self, cfg: &Config) -> Value {
        let all = Self::entries(cfg);
        let runtime = self.runtime.lock().unwrap().clone();
        let plugins: Vec<Value> = all
            .iter()
            .map(|(id, e)| {
                let exe = executable(id, e);
                let (manifest, problem) = match self.manifest(&exe) {
                    Ok(m) => (Some(m), None),
                    Err(p) => (None, Some(p)),
                };
                json!({
                    "id": id,
                    "path": exe,
                    // A build of the user's own (not installed in the plugins folder).
                    "custom": !e.path.is_empty(),
                    // Installed from this GitHub repository, not the registry.
                    "unlistedFrom": if e.path.is_empty() { store::unlisted_from(id) } else { None },
                    "manifest": manifest,
                    "problem": problem,
                    "runtime": runtime.get(id).cloned().unwrap_or_default(),
                })
            })
            .collect();
        let found = Encoder::find(&cfg.recording.m4a.encoder).map(|e| e.name());
        json!({ "type": "plugins", "plugins": plugins, "encoderFound": found })
    }

    /// Start the config's enabled plugins for a recording.
    pub fn start(&self, cfg: &Config) {
        *self.env.lock().unwrap() = Some((super::systems_of(cfg), PathBuf::from(&cfg.recording.capture_dir)));
        let host = self.new_host(cfg, true, Arc::new(AtomicBool::new(true)));
        *self.host.write().unwrap() = host;
    }

    /// Stop the plugins (recording ended): they get `grace` to finish.
    pub fn stop(&self, grace: Duration) {
        *self.env.lock().unwrap() = None;
        let old = self.host.write().unwrap().take();
        if let Some(mut h) = old {
            h.shutdown(grace);
        }
        let mut rt = self.runtime.lock().unwrap();
        for r in rt.values_mut() {
            if r.state != "error" || r.message.is_empty() {
                r.state = "off";
                r.message.clear();
            }
        }
        drop(rt);
        self.publish_all_runtime();
    }

    /// While recording: restart the plugins with `cfg`'s settings. The old
    /// ones finish in the background; the new ones start once they have
    /// exited (each plugin's data folder, its saved queue, is theirs until
    /// then), and events wait for them meanwhile.
    pub fn reload(&self, cfg: &Config) {
        if self.env.lock().unwrap().is_none() {
            return;
        }
        let go = Arc::new(AtomicBool::new(false));
        let new = self.new_host(cfg, false, go.clone());
        let old = std::mem::replace(&mut *self.host.write().unwrap(), new);
        match old {
            Some(mut h) => {
                std::thread::spawn(move || {
                    h.shutdown(Duration::from_secs(10));
                    go.store(true, Ordering::Relaxed);
                });
            }
            None => go.store(true, Ordering::Relaxed),
        }
    }

    /// `fresh`: a new recording (counts start over); else a restart within one.
    /// `go`: when the processes may start (`PluginHost::start_when`).
    fn new_host(&self, cfg: &Config, fresh: bool, go: Arc<AtomicBool>) -> Option<PluginHost> {
        let (systems, capture_dir) = self.env.lock().unwrap().clone()?;
        let notes = self.notes();
        let specs = Spec::enabled(cfg);
        {
            let mut rt = self.runtime.lock().unwrap();
            for (id, e) in &cfg.plugins {
                let r = rt.entry(id.clone()).or_default();
                if fresh {
                    *r = Runtime { log: std::mem::take(&mut r.log), ..Default::default() };
                }
                r.state = if e.enabled { "starting" } else { "off" };
                r.message.clear();
            }
        }
        self.publish_all_runtime();
        if specs.is_empty() {
            return None;
        }
        let host = PluginHost::start_when(specs, &cfg.recording.m4a, &systems, &capture_dir, notes, go);
        (!host.is_empty()).then_some(host)
    }

    /// Each plugin's figures into the dashboard's series (`plg/<id>/…`), with
    /// its process's restarts, drops and uptime folded into its runtime.
    pub fn report(&self, sink: &mut dyn Sink) {
        let procs = self.host.read().unwrap().as_ref().map(|h| h.process_stats()).unwrap_or_default();
        let mut rt = self.runtime.lock().unwrap();
        for p in procs {
            let r = rt.entry(p.id.clone()).or_default();
            (r.restarts, r.dropped, r.uptime_s) = (p.restarts, p.dropped, p.uptime_s);
        }
        for (id, r) in rt.iter() {
            let k = format!("plg/{}", key_part(id));
            let Some(code) = r.state_code() else { continue };
            sink.gauge(&format!("{k}/state"), code);
            sink.counter(&format!("{k}/ok"), r.ok);
            sink.counter(&format!("{k}/skipped"), r.skipped);
            sink.counter(&format!("{k}/failed"), r.failed);
            sink.counter(&format!("{k}/dropped"), r.dropped);
            sink.counter(&format!("{k}/restarts"), r.restarts);
            if let Some(m) = &r.metrics {
                if let Some(q) = m.queued {
                    sink.gauge(&format!("{k}/queued"), q as f64);
                }
                if let Some(l) = m.latency_ms {
                    sink.gauge(&format!("{k}/latency"), l);
                }
                if let Some(b) = m.bytes_sent {
                    sink.counter(&format!("{k}/bytes"), b);
                }
            }
        }
    }

    /// Every plugin's runtime (for a dashboard that just connected).
    pub fn runtime_json(&self) -> Value {
        json!(*self.runtime.lock().unwrap())
    }

    fn publish_all_runtime(&self) {
        let rt = self.runtime.lock().unwrap().clone();
        for (id, r) in rt {
            publish(&self.hub, json!({ "type": "pluginRuntime", "id": id, "runtime": r }));
        }
    }

    /// What plugins say, as log lines, runtime updates and results for the interface.
    fn notes(&self) -> Notes {
        let (hub, runtime, archive, monitor) = (self.hub.clone(), self.runtime.clone(), self.archive.clone(), self.monitor.clone());
        let log = super::notes_to_hub(hub.clone());
        Arc::new(move |n: Note| {
            if let Note::Result { path, outcome, .. } = &n {
                archive.result(path, *outcome);
            }
            let id = match &n {
                Note::Log { plugin, .. } | Note::State { plugin, .. } | Note::Result { plugin, .. } | Note::Metrics { plugin, .. } => plugin.clone(),
            };
            if !id.is_empty() {
                let mut rt = runtime.lock().unwrap();
                let r = rt.entry(id.clone()).or_default();
                match &n {
                    Note::Log { level, text, .. } => {
                        if *level != Level::Debug {
                            r.log.push_back(LogEntry { time: now(), level: level_name(*level), text: text.clone() });
                            if r.log.len() > LOG_LINES {
                                r.log.pop_front();
                            }
                        }
                    }
                    Note::State { state, message, .. } => {
                        let was = r.state;
                        r.state = match state {
                            State::Ok => "ok",
                            State::Warning => "warning",
                            State::Error => "error",
                        };
                        r.message = message.clone();
                        // A change of health is an event (not every "ok" of a starting plugin).
                        if was != r.state && !(was == "starting" || was == "off") || r.state == "error" && was != "error" {
                            let e = trunk_app::stats::MonitorEvent::PluginHealth { plugin: id.clone(), state: r.state.to_string(), message: message.clone() };
                            if let Some(v) = monitor.emit(now(), e) {
                                publish(&hub, v);
                            }
                        }
                    }
                    Note::Result { outcome, message, path, .. } => {
                        r.count(*outcome);
                        match outcome {
                            Outcome::Ok => r.ok += 1,
                            Outcome::Skipped => r.skipped += 1,
                            Outcome::Failed => {
                                r.failed += 1;
                                r.last_failure = format!("{path}: {message}");
                            }
                        }
                    }
                    Note::Metrics { metrics, .. } => r.metrics = Some(metrics.clone()),
                }
                publish(&hub, json!({ "type": "pluginRuntime", "id": id, "runtime": r }));
            }
            log(n);
        })
    }

    /// `addPlugin {path}`: a plugin executable of the user's own (a build).
    /// Returns its id and path, for the server to put in the config, and a notice.
    pub fn add(&self, v: &Value, cfg: &Config) -> Result<(String, PathBuf, String), String> {
        let path = v["path"].as_str().map(str::trim).filter(|p| !p.is_empty()).ok_or("Which executable?")?;
        let path = PathBuf::from(shellexpand_home(path));
        let path = std::fs::canonicalize(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.manifests.lock().unwrap().remove(&path);
        let m = self.manifest(&path)?;
        let id = m.id.clone();
        let existing = executable(&id, cfg.plugins.get(&id).unwrap_or(&PluginSetup::default()));
        if existing.is_file() && existing != path {
            return Err(format!("There's already a plugin called {id} ({})", existing.display()));
        }
        Ok((id, path, format!("Added {} {}. Set it up, then turn it on.", m.name, m.version)))
    }

    /// `removePlugin {id}`: an installed copy is deleted (a build of the
    /// user's own isn't). Its data folder stays. The server then forgets it
    /// in the config; returns its id.
    pub fn remove(&self, v: &Value, cfg: &Config) -> Result<String, String> {
        let id = v["id"].as_str().ok_or("no plugin id")?.to_string();
        if !store::valid_id(&id) {
            return Err(format!("bad plugin id {id:?}"));
        }
        if cfg.plugins.get(&id).is_none_or(|p| p.path.is_empty()) {
            let dir = plugins_dir().join(&id);
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir).map_err(|err| format!("Couldn't delete {}: {err}", dir.display()))?;
            }
        }
        self.runtime.lock().unwrap().remove(&id);
        Ok(id)
    }

    /// The registry's list: fetched again when it's old, or when `refresh`.
    pub fn catalog(&self, refresh: bool) -> Catalog {
        let mut c = self.store.lock().unwrap();
        if let Some((t, cat)) = c.as_ref() {
            let fresh = if cat.source == "registry" { STORE_FRESH } else { STORE_RETRY };
            if !refresh && t.elapsed() < fresh {
                return cat.clone();
            }
        }
        let cat = store::catalog();
        *c = Some((Instant::now(), cat.clone()));
        cat
    }

    /// The `pluginStore` message.
    pub fn store_json(&self, refresh: bool) -> Value {
        store_message(&self.catalog(refresh))
    }

    /// `installPlugin {id}` or `installPlugin {repository, tag?}`. Says how
    /// it goes in `pluginInstall` messages, to everyone; blocks until done.
    /// A new plugin is off (nothing in the config changes); after an update
    /// the server reloads the plugins, so a running one restarts with it.
    pub fn install(&self, v: &Value, cfg: &Config) {
        let key = v["id"].as_str().or(v["repository"].as_str()).unwrap_or_default().trim().to_string();
        let say = |id: &str, stage: &str, message: Option<&str>| publish(&self.hub, json!({ "type": "pluginInstall", "key": key, "id": id, "stage": stage, "message": message }));
        if !self.installing.lock().unwrap().insert(key.clone()) {
            return say("", "failed", Some("That's being installed already."));
        }
        let mut id = String::new();
        let r = self.install_one(v, cfg, &mut id, &|id, stage| say(id, stage, None));
        self.installing.lock().unwrap().remove(&key);
        match r {
            Ok(notice) => say(&id, "done", Some(&notice)),
            Err(e) => say(&id, "failed", Some(&e)),
        }
    }

    fn install_one(&self, v: &Value, cfg: &Config, id_out: &mut String, stage: &dyn Fn(&str, &'static str)) -> Result<String, String> {
        stage("", "finding");
        let listing = match v["id"].as_str() {
            Some(id) => self.catalog(false).find(id).cloned().ok_or_else(|| format!("{id} isn't in the plugin registry"))?,
            None => {
                let repo = v["repository"].as_str().map(str::trim).filter(|r| !r.is_empty()).ok_or("Which plugin? Its GitHub repository, or its release's page.")?;
                let l = store::release_listing(repo, v["tag"].as_str())?;
                // A listed plugin's id is its own.
                if let Some(listed) = self.catalog(false).find(&l.id) {
                    if !listed.repository.eq_ignore_ascii_case(&l.repository) {
                        return Err(format!("{} is the id of {} in the plugin registry ({}). Install that one from the list.", l.id, listed.name, listed.repository));
                    }
                }
                l
            }
        };
        let id = listing.id.clone();
        *id_out = id.clone();
        if let Some(e) = cfg.plugins.get(&id) {
            if !e.path.is_empty() {
                return Err(format!("{} is your build at {}: remove it from the list first.", id, e.path));
            }
        }
        let exe = executable(&id, &PluginSetup::default());
        let before = self.manifest(&exe).ok().map(|m| m.version);
        store::install(&listing, &|s| stage(&id, s))?;
        self.manifests.lock().unwrap().remove(&exe);
        let (name, version) = (&listing.name, &listing.version);
        Ok(match before {
            Some(old) if store::newer(version, &old) => format!("Updated {name} from {old} to {version}."),
            Some(old) if &old != version => format!("Installed {name} {version} in place of {old}."),
            Some(_) => format!("Reinstalled {name} {version}."),
            None => format!("Installed {name} {version}. Set it up, then turn it on."),
        })
    }
}

fn shellexpand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(h)) => format!("{}/{rest}", Path::new(&h).display()),
        _ => p.to_string(),
    }
}

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Error => "error",
        Level::Warn => "warn",
        Level::Info => "info",
        Level::Debug => "debug",
    }
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The `pluginStore` message for a catalog.
pub(crate) fn store_message(cat: &store::Catalog) -> Value {
    let plugins: Vec<Value> = cat
        .index
        .plugins
        .iter()
        .map(|l| {
            let mut v = serde_json::to_value(l).unwrap_or_default();
            v["unavailable"] = json!(l.unavailable());
            v
        })
        .collect();
    json!({ "type": "pluginStore", "source": cat.source, "fetched": cat.fetched, "problem": cat.problem, "target": store::target(), "plugins": plugins })
}
