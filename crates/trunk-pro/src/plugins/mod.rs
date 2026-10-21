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
//! A system can override some of a plugin's settings for itself (another
//! server for it, say): `"openmhz": { "apiKey": "…", "override": { "server": "…" } }`.
//! The plugin is told of each system's settings, the plugin's with its
//! overrides, along with its own ([`host`] does the merging).
//!
//! A system can have several copies of a plugin (a list of settings in place
//! of one, `"openmhz": [{ "apiKey": "a" }, { "apiKey": "b" }]`, to upload it
//! to two places, say). Each copy after the first runs as a process of its
//! own. Every copy is told of every system; a system with fewer copies gets
//! null settings, as one that doesn't use the plugin does, so plugins needn't
//! know. Each copy that takes `call.concluded` is a taker the Archive waits on.
//!
//! An installed plugin is `<config dir>/plugins/<id>/<id>` (`.exe` on
//! Windows), put there by the plugin store ([`store`]) or by hand; `"path"`
//! in its entry runs another executable instead (a build of your own). Each
//! gets `<config dir>/plugin-data/<id>/` for its state.

mod archive;
pub mod cli;
pub mod delay;
mod encode;
pub mod hold;
mod host;
pub mod manage;
pub mod store;

pub use archive::{plan, Archive, FileRules};
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

/// A system's settings for a plugin, one per copy of it: most have one (an
/// object); a list is several.
fn copies_of(v: &Value) -> &[Value] {
    match v {
        Value::Array(a) => a,
        Value::Null => &[],
        v => std::slice::from_ref(v),
    }
}

/// How many copies of plugin `id` run: one, and more when a system has more.
pub fn copies(cfg: &Config, id: &str) -> usize {
    cfg.call_systems().iter().filter_map(|s| s.plugins.get(id)).map(|v| copies_of(v).len()).max().unwrap_or(0).max(1)
}

/// Copy `copy` (0: the first) of plugin `id`'s settings for each system
/// calls can come from that has some, by short name (every system's own:
/// [`Config::name_problem`]).
pub fn system_settings(cfg: &Config, id: &str, copy: usize) -> BTreeMap<String, Value> {
    cfg.call_systems().into_iter().filter_map(|s| Some((s.short_name.to_string(), copies_of(s.plugins.get(id)?).get(copy)?.clone()))).collect()
}

/// What a copy of plugin `id` goes by in its notes, process stats and data
/// folder: `id` for the first, `id#2` and on for the others.
pub fn copy_label(id: &str, copy: usize) -> String {
    if copy == 0 {
        id.to_string()
    } else {
        format!("{id}#{}", copy + 1)
    }
}

/// A label's plugin id and which copy (0: the first).
pub fn parse_label(label: &str) -> (&str, usize) {
    match label.rsplit_once('#') {
        Some((id, n)) => match n.parse::<usize>() {
            Ok(n) if n >= 2 => (id, n - 1),
            _ => (label, 0),
        },
        None => (label, 0),
    }
}

/// Forget plugin `id`: its entry, its settings for every system, and its
/// delays (a rule for it and others is for the others now; one for it alone
/// goes, rather than coming to be for every plugin).
pub fn forget(cfg: &mut Config, id: &str) {
    cfg.plugins.remove(id);
    let forget_delays = |rules: &mut Vec<crate::config::DelayRule>| {
        rules.retain_mut(|r| {
            let had = !r.plugins.is_empty();
            r.plugins.retain(|p| parse_label(p).0 != id);
            !had || !r.plugins.is_empty()
        })
    };
    forget_delays(&mut cfg.plugin_delays);
    for s in &mut cfg.systems {
        s.plugins.remove(id);
        forget_delays(&mut s.plugin_delays);
    }
    for c in &mut cfg.conventional {
        c.plugins.remove(id);
        forget_delays(&mut c.plugin_delays);
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
    use crate::config::{fcc_primary, FccLicenses};
    cfg.call_systems()
        .into_iter()
        .map(|s| {
            // Name, FCC licenses and sites: a trunked system's or a conventional one's.
            let sys = cfg.systems.iter().find(|y| y.short_name == s.short_name && s.kind != "conventional");
            let conv = cfg.conventional.iter().find(|y| y.short_name == s.short_name && s.kind == "conventional");
            let (name, fcc, ccs): (String, FccLicenses, Vec<f64>) = match (sys, conv) {
                (Some(y), _) => (y.name.clone(), y.fcc.clone(), y.active_sites().flat_map(|t| t.control_channels_hz.iter().copied()).collect()),
                (None, Some(c)) => (c.name.clone(), c.fcc.clone(), vec![]),
                _ => Default::default(),
            };
            let primary = fcc_primary(&fcc, &ccs).cloned().unwrap_or_default();
            trunk_recorder_plugin::SystemInfo {
                index: s.index,
                short_name: s.short_name.to_string(),
                kind: s.kind.into(),
                name,
                fcc: fcc.into_iter().map(|(k, l)| (k, trunk_recorder_plugin::FccLicense { call_sign: l.call_sign, name: l.name })).collect(),
                fcc_call_sign: primary.call_sign,
                fcc_name: primary.name,
                sites: sys.map_or_else(Vec::new, |y| y.active_sites().map(|t| trunk_recorder_plugin::SiteInfo { name: y.site_name(t).to_string() }).collect()),
                ..Default::default()
            }
        })
        .collect()
}

/// Plugin notes as log lines, which the log shows the browsers too
/// ([`crate::logging::text`]; the plugins view follows each plugin's state
/// and results in its `pluginRuntime`).
pub fn notes_to_log() -> host::Notes {
    use trunk_recorder_plugin::{Level, Outcome, State};
    let log = |plugin: &str, error: bool, text: &str| {
        let text = if plugin.is_empty() { format!("Plugins: {text}") } else { format!("[{plugin}] {text}") };
        if error {
            log::error!(target: "plugin", "{text}");
        } else {
            log::info!(target: "plugin", "{text}");
        }
    };
    std::sync::Arc::new(move |n: Note| match n {
        Note::Log { plugin, level, text } => {
            if level != Level::Debug {
                log(&plugin, level <= Level::Warn, &text);
            }
        }
        Note::State { plugin, state, message } => {
            if state == State::Error {
                log(&plugin, true, &message);
            }
        }
        Note::Result { plugin, path, outcome, message, .. } => {
            if outcome == Outcome::Failed {
                log(&plugin, true, &format!("{path}: {message}"));
            }
        }
        // (In the plugin's pluginRuntime.)
        Note::Metrics { .. } => {}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Allowing a plugin decrypted calls, or no longer, restarts the plugins.
    #[test]
    fn a_change_to_share_decrypted_reloads_the_plugins() {
        let a: Config = serde_json::from_value(json!({ "plugins": { "openmhz": { "enabled": true } } })).unwrap();
        let mut b = a.clone();
        b.plugins.get_mut("openmhz").unwrap().share_decrypted = true;
        assert!(changed(&a, &b) && changed(&b, &a));
    }

    /// A system's name, FCC licenses (and the one that names it) and sites are told to plugins, a
    /// conventional system's licenses too; left out, they're absent.
    #[test]
    fn plugins_are_told_a_systems_fcc_licenses_and_sites() {
        let c: Config = serde_json::from_value(json!({
            "systems": [
                { "shortName": "dcfd", "name": "DC Fire",
                  "fcc": { "859087500": { "callSign": "KNJA391", "from": "air" }, "857987500": { "callSign": "WPXT459", "name": "DISTRICT OF COLUMBIA" } },
                  "sites": [{ "controlChannelsHz": [857987500] }, { "name": "south", "controlChannelsHz": [858987500] }] },
                { "shortName": "wmata", "sites": [{ "controlChannelsHz": [489087500] }] }
            ],
            "conventional": [{ "shortName": "county", "fcc": { "155475000": { "callSign": "KQX123" } }, "channels": [{ "freqHz": 155475000, "mode": "fm" }] }]
        }))
        .unwrap();
        let s = systems_of(&c);
        assert_eq!(s[0].name, "DC Fire");
        assert_eq!((s[0].fcc_call_sign.as_str(), s[0].fcc_name.as_str()), ("WPXT459", "DISTRICT OF COLUMBIA"));
        assert_eq!(s[0].fcc["859087500"], trunk_recorder_plugin::FccLicense { call_sign: "KNJA391".into(), name: String::new() });
        assert_eq!(s[0].sites.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["dcfd", "south"]);
        let wire = serde_json::to_value(&s[1]).unwrap();
        assert!(wire.get("fcc").is_none() && wire.get("fcc_call_sign").is_none() && wire.get("name").is_none(), "{wire}");
        assert_eq!(wire["sites"][0]["name"], "wmata");
        let county = s.iter().find(|x| x.short_name == "county").unwrap();
        assert_eq!(county.fcc_call_sign, "KQX123");
    }

    #[test]
    fn what_plugins_get_from_the_config() {
        let c: Config = serde_json::from_value(json!({
            "systems": [
                { "shortName": "dcfd", "sites": [{ "controlChannelsHz": [857987500] }], "plugins": { "openmhz": { "apiKey": "k" } } },
                { "shortName": "wmata", "sites": [{ "controlChannelsHz": [489087500] }] }
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
        let mut with_delays = c.clone();
        with_delays.plugin_delays = serde_json::from_value(json!([{ "plugins": ["openmhz"], "delayS": 60 }, { "plugins": ["openmhz#2", "mine"], "delayS": 60 }, { "delayS": 5 }])).unwrap();
        forget(&mut with_delays, "openmhz");
        assert_eq!(with_delays.plugin_delays, serde_json::from_value::<Vec<crate::config::DelayRule>>(json!([{ "plugins": ["mine"], "delayS": 60 }, { "delayS": 5 }])).unwrap(), "a rule for it alone goes; it leaves the others");
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
                { "shortName": "old", "enabled": false, "sites": [{ "controlChannelsHz": [851000000] }], "plugins": { "openmhz": { "apiKey": "old" } } },
                { "shortName": "draft", "plugins": { "openmhz": { "apiKey": "draft" } } },
                { "shortName": "dcfd", "sites": [{ "controlChannelsHz": [857987500] }], "plugins": { "openmhz": { "apiKey": "dcfd" } } }
            ],
            "conventional": [{ "shortName": "county", "channels": [{ "freqHz": 154430000 }], "plugins": { "openmhz": { "apiKey": "conv" } } }],
            "plugins": { "openmhz": { "enabled": true } }
        }))
        .unwrap();
        let systems = systems_of(&c);
        assert_eq!(systems.iter().map(|s| (s.index, s.short_name.as_str(), s.kind.as_str())).collect::<Vec<_>>(), [(0, "dcfd", "p25"), (1, "county", "conventional")]);
        let e = c.engine_config(0.0, &Default::default());
        assert_eq!(e.systems.iter().map(|s| s.short_name.as_str()).collect::<Vec<_>>(), ["dcfd", "county"], "the engine numbers them so too");
        let specs = Spec::enabled(&c);
        assert_eq!(specs[0].systems.get("dcfd"), Some(&json!({ "apiKey": "dcfd" })));
        assert_eq!(specs[0].systems.get("county"), Some(&json!({ "apiKey": "conv" })));
        assert_eq!(specs[0].systems.len(), 2);
    }

    /// A system with two copies of a plugin: a second process, told only of
    /// the systems with a second copy.
    #[test]
    fn a_second_copy_runs_for_its_systems_only() {
        let c: Config = serde_json::from_value(json!({
            "systems": [
                { "shortName": "dcfd", "sites": [{ "controlChannelsHz": [857987500] }], "plugins": { "openmhz": [{ "apiKey": "a" }, { "apiKey": "b" }] } },
                { "shortName": "wmata", "sites": [{ "controlChannelsHz": [489087500] }], "plugins": { "openmhz": { "apiKey": "w" } } }
            ],
            "plugins": { "openmhz": { "enabled": true, "settings": { "server": "s" } } }
        }))
        .unwrap();
        assert_eq!(copies(&c, "openmhz"), 2);
        let specs = Spec::enabled(&c);
        assert_eq!(specs.iter().map(|s| (s.id.as_str(), s.copy)).collect::<Vec<_>>(), [("openmhz", 0), ("openmhz", 1)]);
        assert_eq!(specs[0].systems, BTreeMap::from([("dcfd".to_string(), json!({ "apiKey": "a" })), ("wmata".to_string(), json!({ "apiKey": "w" }))]));
        assert_eq!(specs[1].systems, BTreeMap::from([("dcfd".to_string(), json!({ "apiKey": "b" }))]));
        assert_eq!(specs[1].config, json!({ "server": "s" }), "every copy has the whole recorder's settings");
        assert_eq!(copy_label("openmhz", 1), "openmhz#2");
        assert_eq!(parse_label("openmhz#2"), ("openmhz", 1));
        assert_eq!(parse_label("openmhz"), ("openmhz", 0));
    }
}
