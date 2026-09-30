//! Plugins as the interface sees them: what's installed and configured
//! (`plugins` messages), how each is doing while recording (`pluginRuntime`),
//! and changes from the Plugins page — saved to `plugins.json` and, while
//! recording, applied at once by swapping in a new [`PluginHost`].
//!
//! Browser → server: `plugins` (the list), `setPlugin {id, enabled?, config?,
//! systems?}`, `addPlugin {path}`, `removePlugin {id}`, `setPluginAudio
//! {encoder, bitrateKbps}`. Server → browser: `plugins`, `pluginRuntime {id,
//! runtime}`, and `pluginResult` for each call a plugin handled.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use trunk_recorder_plugin::{Level, Manifest, Outcome, State, SystemInfo};

use super::host::Notes;
use super::{describe, executable, plugins_dir, Encoder, Note, PluginEntry, PluginHost, PluginsFile, Spec};
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
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime { state: "off", message: String::new(), ok: 0, skipped: 0, failed: 0, last_failure: String::new(), log: VecDeque::new() }
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

/// An executable's modification time, and what `--describe` said then.
type Described = (Option<SystemTime>, Result<Manifest, String>);

/// The plugins' part of the app's shared state.
pub struct Plugins {
    /// plugins.json
    pub file_path: PathBuf,
    /// The plugins running while recording.
    pub host: RwLock<Option<PluginHost>>,
    /// What the running recording's plugins were started with, to restart them.
    env: Mutex<Option<(Vec<SystemInfo>, PathBuf)>>,
    runtime: Arc<Mutex<BTreeMap<String, Runtime>>>,
    /// Manifests by executable (and its modification time): `--describe` runs once per build.
    manifests: Mutex<HashMap<PathBuf, Described>>,
    /// One change to plugins.json at a time.
    edit: Mutex<()>,
    hub: Hub,
}

impl Plugins {
    pub fn new(config_path: &Path, hub: Hub) -> Plugins {
        Plugins {
            file_path: PluginsFile::path_for(config_path),
            host: RwLock::new(None),
            env: Mutex::new(None),
            runtime: Arc::new(Mutex::new(BTreeMap::new())),
            manifests: Mutex::new(HashMap::new()),
            edit: Mutex::new(()),
            hub,
        }
    }

    /// The installed plugins (in the plugins folder) and those plugins.json names.
    fn entries(&self) -> Result<(PluginsFile, BTreeMap<String, PluginEntry>), String> {
        let file = PluginsFile::load(&self.file_path)?;
        let mut all = file.plugins.clone();
        if let Ok(rd) = std::fs::read_dir(plugins_dir()) {
            for d in rd.flatten() {
                let id = d.file_name().to_string_lossy().to_string();
                if executable(&id, &PluginEntry::default()).is_file() {
                    all.entry(id).or_default();
                }
            }
        }
        Ok((file, all))
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

    /// The `plugins` message.
    pub fn list_json(&self, cfg: &crate::config::Config) -> Value {
        let (file, all) = match self.entries() {
            Ok(x) => x,
            Err(e) => return json!({ "type": "plugins", "file": self.file_path, "problem": e, "plugins": [], "systems": [], "audio": {} }),
        };
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
                    "enabled": e.enabled,
                    "path": exe,
                    // A build of the user's own (not installed in the plugins folder).
                    "custom": !e.path.is_empty(),
                    "manifest": manifest,
                    "problem": problem,
                    "config": e.config,
                    "systems": e.systems,
                    "runtime": runtime.get(id).cloned().unwrap_or_default(),
                })
            })
            .collect();
        let systems: Vec<String> = super::systems_of(cfg).into_iter().map(|s| s.short_name).collect();
        let found = Encoder::find(&file.audio.encoder).map(|e| e.name());
        json!({
            "type": "plugins",
            "file": self.file_path,
            "plugins": plugins,
            "systems": systems,
            "audio": { "encoder": file.audio.encoder, "bitrateKbps": file.audio.bitrate_kbps, "found": found },
        })
    }

    /// Start the enabled plugins for a recording.
    pub fn start(&self, systems: Vec<SystemInfo>, capture_dir: PathBuf) {
        *self.env.lock().unwrap() = Some((systems, capture_dir));
        let host = self.new_host(true);
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

    /// While recording: restart the plugins with plugins.json as it is now.
    /// The old ones finish in the background.
    fn reload(&self) {
        if self.env.lock().unwrap().is_none() {
            return;
        }
        let new = self.new_host(false);
        let old = std::mem::replace(&mut *self.host.write().unwrap(), new);
        if let Some(mut h) = old {
            std::thread::spawn(move || h.shutdown(Duration::from_secs(10)));
        }
    }

    /// `fresh`: a new recording (counts start over); else a restart within one.
    fn new_host(&self, fresh: bool) -> Option<PluginHost> {
        let (systems, capture_dir) = self.env.lock().unwrap().clone()?;
        let notes = self.notes();
        let file = match PluginsFile::load(&self.file_path) {
            Ok(f) => f,
            Err(e) => {
                notes(Note::Log { plugin: String::new(), level: Level::Error, text: e });
                return None;
            }
        };
        let specs = Spec::enabled(&file);
        {
            let mut rt = self.runtime.lock().unwrap();
            for (id, e) in &file.plugins {
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
        let host = PluginHost::start(specs, &file.audio, &systems, &capture_dir, notes);
        (!host.is_empty()).then_some(host)
    }

    fn publish_all_runtime(&self) {
        let rt = self.runtime.lock().unwrap().clone();
        for (id, r) in rt {
            publish(&self.hub, json!({ "type": "pluginRuntime", "id": id, "runtime": r }));
        }
    }

    /// What plugins say, as log lines, runtime updates and results for the interface.
    fn notes(&self) -> Notes {
        let (hub, runtime) = (self.hub.clone(), self.runtime.clone());
        let log = super::notes_to_hub(hub.clone());
        Arc::new(move |n: Note| {
            let id = match &n {
                Note::Log { plugin, .. } | Note::State { plugin, .. } | Note::Result { plugin, .. } => plugin.clone(),
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
                        r.state = match state {
                            State::Ok => "ok",
                            State::Warning => "warning",
                            State::Error => "error",
                        };
                        r.message = message.clone();
                    }
                    Note::Result { outcome, message, path, .. } => match outcome {
                        Outcome::Ok => r.ok += 1,
                        Outcome::Skipped => r.skipped += 1,
                        Outcome::Failed => {
                            r.failed += 1;
                            r.last_failure = format!("{path}: {message}");
                        }
                    },
                }
                publish(&hub, json!({ "type": "pluginRuntime", "id": id, "runtime": r }));
            }
            log(n);
        })
    }

    /// Change plugins.json with `f`, save it, and apply it if recording.
    fn change(&self, f: impl FnOnce(&mut PluginsFile) -> Result<(), String>) -> Result<(), String> {
        let _one = self.edit.lock().unwrap();
        let mut file = PluginsFile::load(&self.file_path)?;
        f(&mut file)?;
        file.save(&self.file_path).map_err(|e| format!("Couldn't save {}: {e}", self.file_path.display()))?;
        self.reload();
        Ok(())
    }

    /// `setPlugin {id, enabled?, config?, systems?}`
    pub fn set(&self, v: &Value) -> Result<(), String> {
        let id = v["id"].as_str().ok_or("no plugin id")?.to_string();
        self.change(|file| {
            let e = file.plugins.entry(id).or_default();
            if let Some(b) = v["enabled"].as_bool() {
                e.enabled = b;
            }
            if v.get("config").is_some() {
                e.config = v["config"].clone();
            }
            if let Some(s) = v["systems"].as_object() {
                e.systems = s.iter().filter(|(_, c)| !is_empty(c)).map(|(k, c)| (k.clone(), c.clone())).collect();
            }
            Ok(())
        })
    }

    /// `addPlugin {path}`: a plugin executable of the user's own (a build).
    pub fn add(&self, v: &Value) -> Result<String, String> {
        let path = v["path"].as_str().map(str::trim).filter(|p| !p.is_empty()).ok_or("Which executable?")?;
        let path = PathBuf::from(shellexpand_home(path));
        let path = std::fs::canonicalize(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.manifests.lock().unwrap().remove(&path);
        let m = self.manifest(&path)?;
        let id = m.id.clone();
        self.change(|file| {
            if let Some(e) = file.plugins.get(&id) {
                if executable(&id, e) != path {
                    return Err(format!("There's already a plugin called {id} ({})", executable(&id, e).display()));
                }
            }
            file.plugins.entry(id.clone()).or_default().path = path.display().to_string();
            Ok(())
        })?;
        Ok(format!("Added {} {}. Check its settings, then turn it on.", m.name, m.version))
    }

    /// `removePlugin {id}`: forget it; an installed copy is deleted too. Its data folder stays.
    pub fn remove(&self, v: &Value) -> Result<(), String> {
        let id = v["id"].as_str().ok_or("no plugin id")?.to_string();
        if id.is_empty() || id.contains(['/', '\\', '.']) {
            return Err(format!("bad plugin id {id:?}"));
        }
        self.change(|file| {
            let e = file.plugins.remove(&id).unwrap_or_default();
            if e.path.is_empty() {
                let dir = plugins_dir().join(&id);
                if dir.is_dir() {
                    std::fs::remove_dir_all(&dir).map_err(|err| format!("Couldn't delete {}: {err}", dir.display()))?;
                }
            }
            Ok(())
        })?;
        self.runtime.lock().unwrap().remove(&id);
        Ok(())
    }

    /// `setPluginAudio {encoder, bitrateKbps}`
    pub fn set_audio(&self, v: &Value) -> Result<(), String> {
        self.change(|file| {
            if let Some(e) = v["encoder"].as_str() {
                if !["auto", "ffmpeg", "afconvert", "fdkaac", "none"].contains(&e) {
                    return Err(format!("unknown encoder {e}"));
                }
                file.audio.encoder = e.to_string();
            }
            if let Some(k) = v["bitrateKbps"].as_u64() {
                file.audio.bitrate_kbps = (k as u32).clamp(8, 320);
            }
            Ok(())
        })
    }
}

/// Settings left entirely empty (don't keep `{}` or `{"apiKey": ""}` around).
fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Object(o) => o.values().all(is_empty),
        _ => false,
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
