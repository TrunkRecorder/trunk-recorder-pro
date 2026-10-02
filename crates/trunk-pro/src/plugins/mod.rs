//! Plugins: programs of their own that the recorder runs while it records and
//! tells what happens (see the `trunk-recorder-plugin` crate for the protocol).
//!
//! They're set up in the config: on or off and their settings for the whole
//! recorder at the top, their settings for each system in that system.
//!
//! ```json
//! {
//!   "plugins": { "openmhz": { "enabled": true, "settings": { "server": "https://api.openmhz.com" } } },
//!   "systems": [{ "shortName": "dcfd", …, "plugins": { "openmhz": { "apiKey": "…" } } }],
//!   "recording": { …, "m4a": { "encoder": "auto", "bitrateKbps": 32 } }
//! }
//! ```
//!
//! An installed plugin is `<config dir>/plugins/<id>/<id>` (`.exe` on
//! Windows), put there by the plugin store ([`store`]) or by hand; `"path"`
//! in its entry runs another executable instead (a build of your own). Each
//! gets `<config dir>/plugin-data/<id>/` for its state.
//!
//! Before, they were set up in `plugins.json` beside the config: [`migrate`]
//! moves that into the config once.

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

use serde::Deserialize;
use serde_json::Value;
use trunk_recorder_plugin::{Manifest, API_VERSION};

use crate::config::{Config, PluginSetup};

/// The M4A settings (the config's `recording.m4a`).
pub type AudioSettings = crate::config::M4a;

/// `plugins.json`, where plugins were set up before they moved into the config.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyFile {
    audio: Option<AudioSettings>,
    plugins: BTreeMap<String, LegacyEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyEntry {
    enabled: bool,
    path: String,
    config: Value,
    systems: BTreeMap<String, Value>,
    unlisted_from: String,
}

/// Move `plugins.json` (beside `config_path`) into the config, once: what the
/// config already has wins. The file is kept as `plugins.json.migrated`.
/// Returns what happened, for the log.
pub fn migrate(config_path: &Path) -> Option<String> {
    let old = config_path.with_file_name("plugins.json");
    let text = std::fs::read_to_string(&old).ok()?;
    let legacy: LegacyFile = match serde_json::from_str(&text) {
        Ok(l) => l,
        Err(e) => return Some(format!("{} isn't readable ({e}); its plugin settings weren't moved into the config", old.display())),
    };
    let mut cfg = Config::load(config_path);
    let mut lost = Vec::new();
    for (id, e) in legacy.plugins {
        let top = cfg.plugins.entry(id.clone()).or_insert_with(|| PluginSetup { enabled: e.enabled, ..Default::default() });
        if top.settings.is_null() && !e.config.is_null() {
            top.settings = e.config;
        }
        if top.path.is_empty() {
            top.path = e.path;
        }
        for (name, v) in e.systems {
            let slot = if let Some(s) = cfg.systems.iter_mut().find(|s| s.short_name == name) {
                &mut s.plugins
            } else if cfg.conventional.short_name == name {
                &mut cfg.conventional.plugins
            } else {
                lost.push(format!("{id} for {name}"));
                continue;
            };
            slot.entry(id.clone()).or_insert(v);
        }
        if !e.unlisted_from.is_empty() {
            store::note_unlisted(&id, &e.unlisted_from);
        }
    }
    if let Some(a) = legacy.audio {
        if cfg.recording.m4a == AudioSettings::default() {
            cfg.recording.m4a = a;
        }
    }
    if let Err(e) = cfg.save(config_path) {
        return Some(format!("Couldn't move plugins.json into {}: {e}", config_path.display()));
    }
    let _ = std::fs::rename(&old, old.with_extension("json.migrated"));
    let mut said = format!("Plugin settings moved from {} into {}", old.display(), config_path.display());
    if !lost.is_empty() {
        said += &format!(" (no system by those names any more, left out: {})", lost.join(", "));
    }
    Some(said)
}

/// Plugin `id`'s settings for each system that has some, by short name
/// (the conventional channels too, when there are any).
pub fn system_settings(cfg: &Config, id: &str) -> BTreeMap<String, Value> {
    let mut m: BTreeMap<String, Value> = cfg.systems.iter().filter_map(|s| Some((s.short_name.clone(), s.plugins.get(id)?.clone()))).collect();
    if !cfg.conventional.channels.is_empty() {
        if let Some(v) = cfg.conventional.plugins.get(id) {
            m.insert(cfg.conventional.short_name.clone(), v.clone());
        }
    }
    m
}

/// Forget plugin `id`: its entry and its settings for every system.
pub fn forget(cfg: &mut Config, id: &str) {
    cfg.plugins.remove(id);
    for s in &mut cfg.systems {
        s.plugins.remove(id);
    }
    cfg.conventional.plugins.remove(id);
}

/// Whether going from `a` to `b` changes what plugins run with.
pub fn changed(a: &Config, b: &Config) -> bool {
    let per_system = |c: &Config| -> Vec<(String, BTreeMap<String, Value>)> {
        let mut v: Vec<_> = c.systems.iter().map(|s| (s.short_name.clone(), s.plugins.clone())).collect();
        v.push((c.conventional.short_name.clone(), c.conventional.plugins.clone()));
        v
    };
    a.plugins != b.plugins || a.recording.m4a != b.recording.m4a || per_system(a) != per_system(b)
}

pub fn plugins_dir() -> PathBuf {
    crate::config::config_dir().join("plugins")
}

pub fn data_dir(id: &str) -> PathBuf {
    crate::config::config_dir().join("plugin-data").join(id)
}

/// The executable of plugin `id`.
pub fn executable(id: &str, setup: &PluginSetup) -> PathBuf {
    if !setup.path.is_empty() {
        return PathBuf::from(&setup.path);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plugins_json_moves_into_the_config() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.json");
        std::fs::write(
            &config_path,
            json!({ "systems": [{ "shortName": "dcfd" }], "conventional": { "shortName": "conv" },
                    "plugins": { "broadcastify": { "enabled": true, "settings": { "server": "mine" } } } })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("plugins.json"),
            json!({
                "audio": { "encoder": "ffmpeg", "bitrateKbps": 48 },
                "plugins": {
                    "openmhz": { "enabled": true, "config": { "server": "s" }, "systems": { "dcfd": { "apiKey": "k" }, "conv": { "apiKey": "c" }, "gone": { "apiKey": "x" } } },
                    "broadcastify": { "enabled": false, "config": { "server": "theirs" }, "systems": { "dcfd": { "apiKey": "b" } } },
                    "mine": { "enabled": false, "path": "/builds/mine" }
                }
            })
            .to_string(),
        )
        .unwrap();
        let said = migrate(&config_path).unwrap();
        assert!(said.contains("openmhz for gone"), "{said}");
        let c = Config::load(&config_path);
        assert_eq!(c.plugins["openmhz"], PluginSetup { enabled: true, settings: json!({ "server": "s" }), path: String::new() });
        // What the config had wins.
        assert!(c.plugins["broadcastify"].enabled);
        assert_eq!(c.plugins["broadcastify"].settings, json!({ "server": "mine" }));
        assert_eq!(c.plugins["mine"].path, "/builds/mine");
        assert_eq!(c.systems[0].plugins["openmhz"], json!({ "apiKey": "k" }));
        assert_eq!(c.systems[0].plugins["broadcastify"], json!({ "apiKey": "b" }));
        assert_eq!(c.conventional.plugins["openmhz"], json!({ "apiKey": "c" }));
        assert_eq!((c.recording.m4a.encoder.as_str(), c.recording.m4a.bitrate_kbps), ("ffmpeg", 48));
        assert!(!dir.join("plugins.json").exists() && dir.join("plugins.json.migrated").exists());
        // Once.
        assert!(migrate(&config_path).is_none());
        // And what plugins get from it.
        let specs = Spec::enabled(&c);
        assert_eq!(specs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["broadcastify", "openmhz"]);
        assert_eq!(specs[1].systems.get("dcfd"), Some(&json!({ "apiKey": "k" })));
        assert!(!specs[1].systems.contains_key("conv"), "no conventional channels, so no conventional system");
    }
}
