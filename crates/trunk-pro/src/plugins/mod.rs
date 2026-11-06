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
mod timing;

pub use archive::{plan, Archive, FileRules};
pub use encode::Encoder;
pub use host::{Calls, Note, PluginHost, Saves, Spec};

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

/// What going from one config to another changes for plugins ([`change`]).
#[derive(Debug, PartialEq)]
pub enum Change {
    None,
    /// Only plugins' own settings: each changed plugin's id and new settings
    /// (a plugin that takes them as it runs needn't restart).
    Settings(Vec<(String, Value)>),
    /// What they run with: they restart.
    Restart,
}

/// What going from `a` to `b` changes for plugins.
pub fn change(a: &Config, b: &Config) -> Change {
    let per_system = |c: &Config| -> Vec<(String, BTreeMap<String, Value>)> {
        let mut v: Vec<_> = c.systems.iter().map(|s| (s.short_name.clone(), s.plugins.clone())).collect();
        v.extend(c.conventional.iter().map(|x| (x.short_name.clone(), x.plugins.clone())));
        v
    };
    // Each plugin's entry, but for its settings.
    let bare = |c: &Config| -> BTreeMap<String, PluginSetup> { c.plugins.iter().map(|(k, p)| (k.clone(), PluginSetup { settings: Value::Null, ..p.clone() })).collect() };
    if a.recording.m4a != b.recording.m4a || per_system(a) != per_system(b) || bare(a) != bare(b) {
        return Change::Restart;
    }
    let settings: Vec<_> = b.plugins.iter().filter(|(k, p)| a.plugins.get(*k).map(|o| &o.settings) != Some(&p.settings)).map(|(k, p)| (k.clone(), p.settings.clone())).collect();
    if settings.is_empty() { Change::None } else { Change::Settings(settings) }
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

/// How long `--describe` may take, try by try. The SDK prints the manifest
/// straight away, but a busy machine (one just booted, its disk cold) can be
/// slow to start a process, so one that runs out is asked again, for longer.
const DESCRIBE_LIMITS: [Duration; 2] = [Duration::from_secs(10), Duration::from_secs(30)];

/// Why an executable gave no manifest. `timed_out`: `--describe` didn't
/// finish, which says more about the machine than the plugin: worth asking
/// again later, not remembering.
#[derive(Debug, Clone)]
pub struct DescribeError {
    pub text: String,
    pub timed_out: bool,
}

/// Ask an executable for its manifest (`--describe`), and check that this
/// recorder can run it.
pub fn describe(exe: &Path) -> Result<Manifest, String> {
    try_describe(exe).map_err(|e| e.text)
}

/// [`describe`], saying whether it failed by running out of time.
pub fn try_describe(exe: &Path) -> Result<Manifest, DescribeError> {
    let fail = |text: String| DescribeError { text, timed_out: false };
    let mut late = Vec::new();
    let (status, out, err) = loop {
        match run_describe(exe, DESCRIBE_LIMITS[late.len()]) {
            Ok(r) => break r,
            Err(Run::Failed(e)) => return Err(fail(format!("{}: {e}", exe.display()))),
            Err(Run::TimedOut(what)) => {
                late.push(what);
                if late.len() == DESCRIBE_LIMITS.len() {
                    let text = format!("{}: --describe didn't finish ({}; {})", exe.display(), late.join("; asked again, "), machine_state());
                    return Err(DescribeError { text, timed_out: true });
                }
            }
        }
    };
    if !late.is_empty() {
        log::warn!("{}: --describe didn't finish the first time ({}; {}); it did when asked again", exe.display(), late.join("; "), machine_state());
    }
    if !status.success() {
        return Err(fail(format!("{}: --describe failed ({status}){}", exe.display(), said(&err))));
    }
    let m: Manifest = serde_json::from_str(&out).map_err(|e| fail(format!("{}: not a plugin manifest ({e}){}", exe.display(), said(&err))))?;
    if m.id.is_empty() {
        return Err(fail(format!("{}: its manifest has no id", exe.display())));
    }
    // The host speaks API_VERSION only. Plugins built for an older API are
    // accepted because there is only one so far: when API_VERSION goes up,
    // the host must speak each plugin's own `api` (or refuse older ones here).
    if m.api == 0 {
        return Err(fail(format!("{}: its manifest doesn't say which plugin API it speaks (\"api\"; this recorder has {API_VERSION})", m.id)));
    }
    if m.api > API_VERSION {
        return Err(fail(format!("{} needs plugin API {}; this recorder has {API_VERSION} — update Trunk Recorder Pro", m.id, m.api)));
    }
    Ok(m)
}

/// How one run of `--describe` went wrong.
enum Run {
    /// It couldn't be started, or waited on.
    Failed(String),
    /// It ran past its limit and was killed: what it had done by then.
    TimedOut(String),
}

/// Run `exe --describe` for at most `limit`: its exit status, stdout and stderr.
fn run_describe(exe: &Path, limit: Duration) -> Result<(std::process::ExitStatus, String, String), Run> {
    let t0 = Instant::now();
    let mut child = Command::new(exe)
        .arg("--describe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Run::Failed(e.to_string()))?;
    let spawned = t0.elapsed();
    // Both pipes are read as it goes, so neither can fill and stall it, and
    // what came is there even if something it started keeps a pipe open.
    type Pipe = (std::sync::Arc<std::sync::Mutex<Vec<u8>>>, std::sync::mpsc::Receiver<()>);
    let read = |pipe: Option<Box<dyn Read + Send>>| -> Pipe {
        let (buf, (tx, rx)) = (std::sync::Arc::new(std::sync::Mutex::new(Vec::new())), std::sync::mpsc::channel());
        if let Some(mut p) = pipe {
            let buf = buf.clone();
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while let Ok(n @ 1..) = p.read(&mut b) {
                    buf.lock().unwrap().extend_from_slice(&b[..n]);
                }
                let _ = tx.send(());
            });
        }
        (buf, rx)
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as _));
    let err = read(child.stderr.take().map(|p| Box::new(p) as _));
    // (The end of each pipe, waited on until `by`: not for whatever still holds one.)
    let got = |(buf, done): Pipe, by: Instant| {
        let _ = done.recv_timeout(by.saturating_duration_since(Instant::now()));
        let b = buf.lock().unwrap();
        String::from_utf8_lossy(&b).into_owned()
    };
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| Run::Failed(e.to_string()))? {
            break s;
        }
        if t0.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            let by = Instant::now() + Duration::from_secs(1);
            let (out, err) = (got(out, by), got(err, by));
            let printed = match out.len() {
                0 => "it printed nothing".to_string(),
                n => format!("it printed {n} bytes but didn't exit"),
            };
            return Err(Run::TimedOut(format!("killed after {:.1} s, started in {:.2} s, {printed}{}", limit.as_secs_f64(), spawned.as_secs_f64(), said(&err))));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let by = Instant::now() + Duration::from_secs(1);
    Ok((status, got(out, by), got(err, by)))
}

/// What a plugin wrote to stderr, the end of it, to add to an error ("" for nothing).
fn said(stderr: &str) -> String {
    let s = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        return String::new();
    }
    let n = s.chars().count();
    let tail: String = s.chars().skip(n.saturating_sub(300)).collect();
    format!("; it said \"{}{tail}\"", if n > 300 { "…" } else { "" })
}

/// How busy the machine is, for errors that may be its fault: load and
/// uptime where the system says (Linux), the CPU count everywhere.
fn machine_state() -> String {
    let cpus = std::thread::available_parallelism().map_or(0, |n| n.get());
    let load = std::fs::read_to_string("/proc/loadavg").ok().map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(" "));
    let up = std::fs::read_to_string("/proc/uptime").ok().and_then(|u| u.split_whitespace().next()?.parse::<f64>().ok());
    let mut s = format!("{cpus} CPUs");
    if let Some(l) = load {
        s += &format!(", load average {l}");
    }
    if let Some(u) = up {
        let u = u as u64;
        s += &match u {
            0..=119 => format!(", the system up {u} s"),
            120..=7199 => format!(", the system up {} min", u / 60),
            _ => format!(", the system up {} h", u / 3600),
        };
    }
    s
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

/// A plugin's `settings.save` into its settings (`settings`, an object or null): each key given
/// replaces that setting, a null removes it, the others stay. The keys that changed.
pub fn merge_settings(settings: &mut serde_json::Value, given: serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    use serde_json::Value;
    let mut have = match std::mem::take(settings) {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    let mut changed = Vec::new();
    for (k, v) in given {
        let was = if v.is_null() { have.remove(&k) } else { have.insert(k.clone(), v.clone()) };
        if was.as_ref() != Some(&v) && !(v.is_null() && was.is_none()) {
            changed.push(k);
        }
    }
    *settings = Value::Object(have);
    changed
}

/// Plugin notes as log lines, which the log shows the browsers too
/// ([`crate::logging::plugin_note`]; the plugins view follows each plugin's
/// state and results in its `pluginRuntime`).
pub fn notes_to_log() -> host::Notes {
    use trunk_recorder_plugin::{Level, Outcome, State};
    let log = |plugin: &str, error: bool, text: &str| {
        use trunk_app::log::Level as L;
        crate::logging::plugin_note(plugin, if error { L::Error } else { L::Info }, text);
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

    /// A plugin's save replaces the keys it gives, removes those it gives as null, keeps the rest.
    #[test]
    fn a_plugins_save_merges_into_its_settings() {
        let mut s = json!({ "token": "t", "servers": [{ "id": "1" }], "old": 1 });
        let given = json!({ "servers": [{ "id": "1", "name": "LolWut" }], "old": null, "gone": null, "token": "t" });
        let mut changed = merge_settings(&mut s, given.as_object().unwrap().clone());
        changed.sort();
        assert_eq!(s, json!({ "token": "t", "servers": [{ "id": "1", "name": "LolWut" }] }));
        assert_eq!(changed, ["old", "servers"], "only what changed is said");
        let mut none = serde_json::Value::Null;
        merge_settings(&mut none, json!({ "a": 1 }).as_object().unwrap().clone());
        assert_eq!(none, json!({ "a": 1 }));
    }

    /// Allowing a plugin decrypted calls, or no longer, restarts the plugins.
    #[test]
    fn a_change_to_share_decrypted_reloads_the_plugins() {
        let a: Config = serde_json::from_value(json!({ "plugins": { "openmhz": { "enabled": true } } })).unwrap();
        let mut b = a.clone();
        b.plugins.get_mut("openmhz").unwrap().share_decrypted = true;
        assert_eq!((change(&a, &b), change(&b, &a)), (Change::Restart, Change::Restart));
    }

    /// A plugin's own settings alone changed: they may be sent to it as it
    /// runs; with anything more (a system's settings of it, turned off), a restart.
    #[test]
    fn its_own_settings_alone_are_a_settings_change() {
        let a: Config = serde_json::from_value(json!({ "plugins": { "notify": { "enabled": true, "settings": { "rules": [] } }, "openmhz": { "enabled": true } } })).unwrap();
        assert_eq!(change(&a, &a.clone()), Change::None);
        let mut b = a.clone();
        b.plugins.get_mut("notify").unwrap().settings = json!({ "rules": [{ "name": "Station 1" }] });
        assert_eq!(change(&a, &b), Change::Settings(vec![("notify".into(), json!({ "rules": [{ "name": "Station 1" }] }))]));
        b.plugins.get_mut("openmhz").unwrap().enabled = false;
        assert_eq!(change(&a, &b), Change::Restart);
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
        assert_eq!((change(&c, &gone), change(&c, &c.clone())), (Change::Restart, Change::None));
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

    /// A plugin that hangs on `--describe` is killed at the limit, and the
    /// error says what it had done (printed, said on stderr); one that fails
    /// says what it said too.
    #[cfg(unix)]
    #[test]
    fn describe_says_what_the_plugin_did() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("trunk-pro-describe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let hangs = script("hangs", "echo '{\"id\":'; echo 'waiting on something' >&2; sleep 5");
        let t0 = Instant::now();
        let Err(Run::TimedOut(what)) = run_describe(&hangs, Duration::from_secs(2)) else { panic!("it should time out") };
        assert!(t0.elapsed() < Duration::from_secs(4), "killed, not waited out");
        assert!(what.contains("killed after") && what.contains("printed 7 bytes but didn't exit"), "{what}");
        assert!(what.contains("it said \"waiting on something\""), "{what}");
        let fails = script("fails", "echo 'no config here' >&2; exit 3");
        let e = try_describe(&fails).unwrap_err();
        assert!(!e.timed_out && e.text.contains("--describe failed") && e.text.contains("it said \"no config here\""), "{}", e.text);
        assert!(machine_state().contains("CPUs"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
