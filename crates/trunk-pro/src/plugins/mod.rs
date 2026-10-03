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

mod archive;
pub mod cli;
mod encode;
mod host;
pub mod manage;
pub mod store;

pub use archive::{Archive, FileRules};
pub use encode::Encoder;
pub use host::{Note, PluginHost, Spec};

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use trunk_recorder_plugin::{Manifest, API_VERSION};

use crate::config::{Config, PluginSetup};

/// The M4A settings (the config's `recording.m4a`).
pub type AudioSettings = crate::config::M4a;

/// Plugin `id`'s settings for each system calls can come from that has
/// some, by short name (every system's own: [`Config::name_problem`]).
pub fn system_settings(cfg: &Config, id: &str) -> BTreeMap<String, Value> {
    cfg.call_systems().into_iter().filter_map(|s| Some((s.short_name.to_string(), s.plugins.get(id)?.clone()))).collect()
}

/// Forget plugin `id`: its entry and its settings for every system.
pub fn forget(cfg: &mut Config, id: &str) {
    cfg.plugins.remove(id);
    for s in &mut cfg.systems {
        s.plugins.remove(id);
    }
    for c in &mut cfg.conventional {
        c.plugins.remove(id);
    }
}

/// Whether going from `a` to `b` changes what plugins run with.
pub fn changed(a: &Config, b: &Config) -> bool {
    let per_system = |c: &Config| -> Vec<(String, BTreeMap<String, Value>)> {
        let mut v: Vec<_> = c.systems.iter().map(|s| (s.short_name.clone(), s.plugins.clone())).collect();
        v.extend(c.conventional.iter().map(|x| (x.short_name.clone(), x.plugins.clone())));
        v
    };
    a.plugins != b.plugins || a.recording.m4a != b.recording.m4a || per_system(a) != per_system(b)
}

pub fn plugins_dir() -> PathBuf {
    crate::paths::data_dir().join("plugins")
}

pub fn data_dir(id: &str) -> PathBuf {
    crate::paths::data_dir().join("plugin-data").join(id)
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
    // The host speaks API_VERSION only. Plugins built for an older API are
    // accepted because there is only one so far: when API_VERSION goes up,
    // the host must speak each plugin's own `api` (or refuse older ones here).
    if m.api == 0 {
        return Err(format!("{}: its manifest doesn't say which plugin API it speaks (\"api\"; this recorder has {API_VERSION})", m.id));
    }
    if m.api > API_VERSION {
        return Err(format!("{} needs plugin API {}; this recorder has {API_VERSION} — update Trunk Recorder Pro", m.id, m.api));
    }
    Ok(m)
}

/// Every system calls can come from, as plugins know them (settings not filled in).
pub fn systems_of(cfg: &crate::config::Config) -> Vec<trunk_recorder_plugin::SystemInfo> {
    cfg.call_systems()
        .into_iter()
        .map(|s| trunk_recorder_plugin::SystemInfo { index: s.index, short_name: s.short_name.to_string(), kind: s.kind.into(), config: Value::Null })
        .collect()
}

/// Plugin notes as interface messages: log lines, and `pluginState` /
/// `pluginResult` for the plugins view.
pub fn notes_to_hub(hub: crate::runtime::Hub) -> host::Notes {
    use serde_json::json;
    use trunk_recorder_plugin::{Level, Outcome, State};
    let log = move |hub: &crate::runtime::Hub, plugin: &str, error: bool, text: &str| {
        let text = if plugin.is_empty() { format!("Plugins: {text}") } else { format!("[{plugin}] {text}") };
        if error {
            log::error!("{text}");
        } else {
            log::info!("{text}");
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
        // (In the plugin's pluginRuntime.)
        Note::Metrics { .. } => {}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_plugins_get_from_the_config() {
        let c: Config = serde_json::from_value(json!({
            "systems": [
                { "shortName": "dcfd", "controlChannels": [857987500], "plugins": { "openmhz": { "apiKey": "k" } } },
                { "shortName": "wmata", "controlChannels": [489087500] }
            ],
            "conventional": [{ "shortName": "conv", "plugins": { "openmhz": { "apiKey": "c" } } }],
            "plugins": {
                "openmhz": { "enabled": true, "settings": { "server": "s" } },
                "mine": { "enabled": false, "path": "/builds/mine" }
            }
        }))
        .unwrap();
        let specs = Spec::enabled(&c);
        assert_eq!(specs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["openmhz"]);
        assert_eq!(specs[0].config, json!({ "server": "s" }));
        assert_eq!(specs[0].systems.get("dcfd"), Some(&json!({ "apiKey": "k" })));
        assert!(!specs[0].systems.contains_key("wmata"), "wmata has no settings");
        assert_eq!(specs[0].systems.len(), 1, "no conventional channels, so no conventional system");
        assert_eq!(executable("mine", &c.plugins["mine"]), PathBuf::from("/builds/mine"));
        let mut gone = c.clone();
        forget(&mut gone, "openmhz");
        assert!(!gone.plugins.contains_key("openmhz") && gone.systems[0].plugins.is_empty() && gone.conventional[0].plugins.is_empty());
        assert!(changed(&c, &gone) && !changed(&c, &c.clone()));
    }

    /// Plugins know systems by short name; the number events carry is the
    /// one calls carry. A system that isn't recorded (disabled, or no
    /// control channel) isn't one of them.
    #[test]
    fn plugins_know_systems_by_short_name() {
        let c: Config = serde_json::from_value(json!({
            "systems": [
                { "shortName": "old", "enabled": false, "controlChannels": [851000000], "plugins": { "openmhz": { "apiKey": "old" } } },
                { "shortName": "draft", "plugins": { "openmhz": { "apiKey": "draft" } } },
                { "shortName": "dcfd", "controlChannels": [857987500], "plugins": { "openmhz": { "apiKey": "dcfd" } } }
            ],
            "conventional": [{ "shortName": "county", "channels": [{ "freqHz": 154430000 }], "plugins": { "openmhz": { "apiKey": "conv" } } }],
            "plugins": { "openmhz": { "enabled": true } }
        }))
        .unwrap();
        let systems = systems_of(&c);
        let conv = trunk_core::trunk::conventional_system(0);
        assert_eq!(systems.iter().map(|s| (s.index, s.short_name.as_str(), s.kind.as_str())).collect::<Vec<_>>(), [(0, "dcfd", "p25"), (conv, "county", "conventional")]);
        assert_eq!(c.engine_config(0.0).systems[0].short_name, "dcfd", "the engine numbers dcfd 0 too");
        let specs = Spec::enabled(&c);
        assert_eq!(specs[0].systems.get("dcfd"), Some(&json!({ "apiKey": "dcfd" })));
        assert_eq!(specs[0].systems.get("county"), Some(&json!({ "apiKey": "conv" })));
        assert_eq!(specs[0].systems.len(), 2);
    }
}
