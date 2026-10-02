//! Plugins: programs of their own that the recorder runs while it records and
//! tells what happens (see the `trunk-recorder-plugin` crate for the protocol).
//!
//! Their settings are kept in `plugins.json` next to the config:
//!
//! ```json
//! {
//!   "audio": { "encoder": "auto", "bitrateKbps": 32 },
//!   "plugins": {
//!     "openmhz": {
//!       "enabled": true,
//!       "config": { "server": "https://api.openmhz.com" },
//!       "systems": { "dcfd": { "apiKey": "…" } }
//!     }
//!   }
//! }
//! ```
//!
//! An installed plugin is `<config dir>/plugins/<id>/<id>` (`.exe` on
//! Windows), put there by the plugin store ([`store`]) or by hand; `"path"`
//! in its entry runs another executable instead (a build of your own). Each
//! gets `<config dir>/plugin-data/<id>/` for its state.

pub mod cli;
mod encode;
mod host;
pub mod manage;
pub mod store;

pub use encode::Encoder;
pub use host::{Note, PluginHost, Spec};

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trunk_recorder_plugin::{Manifest, API_VERSION};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PluginsFile {
    pub audio: AudioSettings,
    pub plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioSettings {
    /// "auto" | "ffmpeg" | "afconvert" | "fdkaac" | "none"
    pub encoder: String,
    pub bitrate_kbps: u32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings { encoder: "auto".into(), bitrate_kbps: 32 }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PluginEntry {
    pub enabled: bool,
    /// Run this executable instead of the installed one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// The plugin's settings.
    pub config: Value,
    /// Its settings for each system, by short name.
    pub systems: BTreeMap<String, Value>,
    /// The GitHub repository it was installed from when that wasn't the
    /// registry: nobody reviewed it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub unlisted_from: String,
}

impl PluginsFile {
    /// `plugins.json` beside the config file.
    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path.with_file_name("plugins.json")
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// Missing: no plugins. Unreadable: an error (not silently none).
    pub fn load(path: &Path) -> Result<PluginsFile, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PluginsFile::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

pub fn plugins_dir() -> PathBuf {
    crate::config::config_dir().join("plugins")
}

pub fn data_dir(id: &str) -> PathBuf {
    crate::config::config_dir().join("plugin-data").join(id)
}

/// The executable of plugin `id`.
pub fn executable(id: &str, entry: &PluginEntry) -> PathBuf {
    if !entry.path.is_empty() {
        return PathBuf::from(&entry.path);
    }
    plugins_dir().join(id).join(executable_name(id))
}

/// `<id>`, or `<id>.exe` on Windows.
pub fn executable_name(id: &str) -> String {
    if cfg!(windows) {
        format!("{id}.exe")
    } else {
        id.to_string()
    }
}

/// Ask an executable for its manifest (`--describe`), and check that this
/// recorder can run it.
pub fn describe(exe: &Path) -> Result<Manifest, String> {
    let mut child = Command::new(exe)
        .arg("--describe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let mut out = String::new();
    let reader = child.stdout.take().map(|mut o| {
        std::thread::spawn(move || {
            let _ = o.read_to_string(&mut out);
            out
        })
    });
    let t0 = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if t0.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{}: --describe didn't finish", exe.display()));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    if !status.success() {
        return Err(format!("{}: --describe failed ({status})", exe.display()));
    }
    let m: Manifest = serde_json::from_str(&out).map_err(|e| format!("{}: not a plugin manifest ({e})", exe.display()))?;
    if m.id.is_empty() {
        return Err(format!("{}: its manifest has no id", exe.display()));
    }
    if m.api == 0 || m.api > API_VERSION {
        return Err(format!("{} needs plugin API {}; this recorder has {API_VERSION} — update Trunk Recorder Pro", m.id, m.api));
    }
    Ok(m)
}

/// Every system calls can come from, as plugins know them (settings not filled in).
pub fn systems_of(cfg: &crate::config::Config) -> Vec<trunk_recorder_plugin::SystemInfo> {
    let mut v: Vec<trunk_recorder_plugin::SystemInfo> = cfg
        .systems
        .iter()
        .enumerate()
        .map(|(i, s)| trunk_recorder_plugin::SystemInfo {
            index: i as u16,
            short_name: s.short_name.clone(),
            kind: if s.is_smartnet() { "smartnet" } else if s.is_dmr() { "dmr" } else { "p25" }.into(),
            config: Value::Null,
        })
        .collect();
    if !cfg.conventional.channels.is_empty() {
        v.push(trunk_recorder_plugin::SystemInfo {
            index: trunk_recorder_plugin::CONVENTIONAL,
            short_name: cfg.conventional.short_name.clone(),
            kind: "conventional".into(),
            config: Value::Null,
        });
    }
    v
}

/// Plugin notes as interface messages: log lines, and `pluginState` /
/// `pluginResult` for the plugins view.
pub fn notes_to_hub(hub: crate::runtime::Hub) -> host::Notes {
    use serde_json::json;
    use trunk_recorder_plugin::{Level, Outcome, State};
    let log = move |hub: &crate::runtime::Hub, plugin: &str, error: bool, text: &str| {
        let text = if plugin.is_empty() { format!("Plugins: {text}") } else { format!("[{plugin}] {text}") };
        if error {
            eprintln!("{text}");
        }
        crate::runtime::publish(hub, json!({ "type": "log", "lines": [{ "timeS": 0, "kind": if error { "error" } else { "plugin" }, "text": text }] }));
    };
    std::sync::Arc::new(move |n: Note| match n {
        Note::Log { plugin, level, text } => {
            if level != Level::Debug {
                log(&hub, &plugin, level <= Level::Warn, &text);
            }
        }
        Note::State { plugin, state, message } => {
            if state == State::Error {
                log(&hub, &plugin, true, &message);
            }
            crate::runtime::publish(&hub, json!({ "type": "pluginState", "id": plugin, "state": state, "message": message }));
        }
        Note::Result { plugin, path, outcome, message, url } => {
            if outcome == Outcome::Failed {
                log(&hub, &plugin, true, &format!("{path}: {message}"));
            }
            crate::runtime::publish(&hub, json!({ "type": "pluginResult", "id": plugin, "path": path, "outcome": outcome, "message": message, "url": url }));
        }
    })
}
