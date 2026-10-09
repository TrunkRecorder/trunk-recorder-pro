//! `trunk-pro plugin …`: find plugins in the registry and install them, look
//! at the installed ones, and run one against calls already on disk — a
//! plugin author's test bench.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use trunk_recorder_plugin::{Level, Outcome, State, SystemInfo};

use super::store::{self, Listing};
use super::{describe, executable, plugins_dir, AudioSettings, Note, PluginHost, Spec};
use crate::config::{Config, PluginSetup};
use crate::{die, Args};

pub fn run(a: &Args) {
    // Plugins are kept beside the config (`--config`), as the app keeps them.
    config_path(a);
    match a.positional.first().map(String::as_str) {
        Some("list") => list(a),
        Some("describe") => {
            let exe = a.positional.get(1).unwrap_or_else(|| die("trunk-pro plugin describe <executable>"));
            match describe(Path::new(exe)) {
                Ok(m) => println!("{}", serde_json::to_string_pretty(&m).unwrap_or_default()),
                Err(e) => die(&e),
            }
        }
        Some("run") => run_calls(a),
        Some("search") => search(a),
        Some("install") => install(a),
        Some("update") => update(a),
        Some("uninstall") => uninstall(a),
        _ => die(USAGE),
    }
}

const USAGE: &str = "\
trunk-pro plugin search [text]
    The plugins in the registry (github.com/TrunkRecorder/plugins), or those
    whose name or description has <text>.
trunk-pro plugin install <id | GitHub repository> [--tag v1.2.0]
    Install a plugin from the registry, or the release of one that isn't in it
    (its latest, or --tag): https://github.com/<owner>/<repo>. It's installed
    off; turn it on on the Plugins page.
trunk-pro plugin update [id…]
    Install newer versions of installed plugins (all of them, or these).
trunk-pro plugin uninstall <id>
    Remove a plugin and forget its settings. Its data folder is kept.
trunk-pro plugin list [--config file.json]
    The installed plugins and those the config names, and what each is.
trunk-pro plugin describe <executable>
    A plugin's manifest, checked.
trunk-pro plugin run <executable | id> [<call.json | folder>…] [options]
    Run a plugin and send it calls already on disk (call.concluded), printing
    what it says; then stop it. A folder means the calls in it (the newest
    --limit, default 10). An id runs an installed plugin with its settings
    from the config (its `plugins` entry and each system's).
    --settings file.json   its settings: {\"config\": {…}, \"systems\": {\"<shortName>\": {…}}}
    --capture-dir dir      the calls' capture folder (default: the config's)
    --encoder auto|ffmpeg|afconvert|fdkaac|none   for M4A (default auto)
    --grace 30             seconds it gets to finish after the last call
    --data-dir dir         its data folder (default: one in the temp folder, kept
                           between runs — not the installed plugin's)
";

fn config_path(a: &Args) -> PathBuf {
    let path = crate::paths::config_path(a.get("config"));
    crate::paths::init(&path);
    path
}

fn load_config(a: &Args) -> (PathBuf, Config) {
    let path = config_path(a);
    let cfg = Config::load(&path).unwrap_or_else(|e| die(&e));
    (path, cfg)
}

/// The config's plugins and the installed ones.
fn all_plugins(cfg: &Config) -> BTreeMap<String, PluginSetup> {
    let mut all = cfg.plugins.clone();
    for d in std::fs::read_dir(plugins_dir()).into_iter().flatten().flatten() {
        let id = d.file_name().to_string_lossy().to_string();
        if store::valid_id(&id) && executable(&id, &PluginSetup::default()).is_file() {
            all.entry(id).or_default();
        }
    }
    all
}

fn list(a: &Args) {
    let (path, cfg) = load_config(a);
    println!("{}", path.display());
    let all = all_plugins(&cfg);
    if all.is_empty() {
        println!("  (no plugins)");
    }
    for (id, e) in &all {
        let exe = executable(id, e);
        let what = match describe(&exe) {
            Ok(m) => format!("{} {} — subscribes to {}", m.name, m.version, m.subscribe.join(", ")),
            Err(e) => format!("can't run: {e}"),
        };
        println!("  {id} [{}] {}\n    {what}", if e.enabled { "on" } else { "off" }, exe.display());
    }
    let enc = super::Encoder::find(&cfg.recording.m4a.encoder);
    println!("M4A encoder ({}): {}", cfg.recording.m4a.encoder, enc.as_ref().map_or("none found (install ffmpeg)", |e| e.name()));
}

/// The installed plugin's version, if it's installed and answers.
fn installed_version(id: &str) -> Option<String> {
    let exe = executable(id, &PluginSetup::default());
    exe.is_file().then(|| describe(&exe).ok().map(|m| m.version)).flatten()
}

fn say_source(cat: &store::Catalog) {
    match cat.source {
        "registry" => {}
        "saved" => eprintln!("(The registry couldn't be reached: {}. Using the list fetched before.)", cat.problem.as_deref().unwrap_or("")),
        _ => eprintln!("(The registry couldn't be reached: {}. Using the list built into this version.)", cat.problem.as_deref().unwrap_or("")),
    }
}

fn search(a: &Args) {
    let text = a.positional.get(1).map(|t| t.to_lowercase()).unwrap_or_default();
    let cat = store::catalog();
    say_source(&cat);
    let found: Vec<&Listing> = cat.index.plugins.iter().filter(|l| [&l.id, &l.name, &l.description].iter().any(|f| f.to_lowercase().contains(&text))).collect();
    if found.is_empty() {
        println!("No plugins{}.", if text.is_empty() { String::new() } else { format!(" match {text:?}") });
    }
    for l in found {
        let status = match (installed_version(&l.id), l.unavailable()) {
            (Some(v), _) if store::newer(&l.version, &v) => format!("installed {v}, update available"),
            (Some(v), _) => format!("installed {v}"),
            (None, Some(why)) => why,
            (None, None) => String::new(),
        };
        println!("{} {} [{}]{}\n    {}\n    {}", l.id, l.version, l.tier, if status.is_empty() { String::new() } else { format!(" — {status}") }, l.description, l.repository);
    }
}

/// The listing to install for `what`: an id in the registry, or a GitHub repository.
fn listing_for(what: &str, tag: Option<&str>, cat: &store::Catalog) -> Listing {
    if !what.contains('/') {
        return cat.find(what).cloned().unwrap_or_else(|| die(&format!("{what} isn't in the plugin registry (`trunk-pro plugin search`)")));
    }
    let l = store::release_listing(what, tag).unwrap_or_else(|e| die(&e));
    if let Some(listed) = cat.find(&l.id) {
        if !listed.repository.eq_ignore_ascii_case(&l.repository) {
            die(&format!("{} is the id of {} in the plugin registry ({}): `trunk-pro plugin install {}`", l.id, listed.name, listed.repository, l.id));
        }
    }
    eprintln!("{} {} isn't from the plugin registry: nobody has reviewed it. Install it only if you trust {}.", l.name, l.version, l.repository);
    l
}

/// Install `l`; returns false when it failed.
fn install_listing(cfg: &Config, l: &Listing) -> bool {
    if let Some(e) = cfg.plugins.get(&l.id) {
        if !e.path.is_empty() {
            eprintln!("{} is your build at {}: `trunk-pro plugin uninstall {}` first.", l.id, e.path, l.id);
            return false;
        }
    }
    let before = installed_version(&l.id);
    match store::install(l, &|stage| eprintln!("  {stage}…")) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            return false;
        }
    }
    match before {
        Some(old) if store::newer(&l.version, &old) => println!("Updated {} from {old} to {}.", l.name, l.version),
        Some(old) if old != l.version => println!("Installed {} {} in place of {old}.", l.name, l.version),
        Some(_) => println!("Reinstalled {} {}.", l.name, l.version),
        None => println!("Installed {} {} in {}. Set it up and turn it on on the Plugins page.", l.name, l.version, plugins_dir().join(&l.id).display()),
    }
    true
}

fn install(a: &Args) {
    let what = a.positional.get(1).unwrap_or_else(|| die("trunk-pro plugin install <id | GitHub repository> [--tag v1.2.0]"));
    let cat = store::catalog();
    say_source(&cat);
    let l = listing_for(what, a.get("tag"), &cat);
    let (_, cfg) = load_config(a);
    if !install_listing(&cfg, &l) {
        std::process::exit(1);
    }
}

fn update(a: &Args) {
    let (_, cfg) = load_config(a);
    let mut ids: Vec<String> = a.positional.iter().skip(1).cloned().collect();
    if ids.is_empty() {
        // Every installed plugin (not builds of your own).
        ids = std::fs::read_dir(plugins_dir()).into_iter().flatten().flatten().map(|d| d.file_name().to_string_lossy().to_string()).filter(|id| installed_version(id).is_some()).collect();
        ids.sort();
    }
    let cat = store::catalog();
    say_source(&cat);
    let mut failed = false;
    for id in &ids {
        let entry = cfg.plugins.get(id).cloned().unwrap_or_default();
        if !entry.path.is_empty() {
            println!("{id}: your build ({}), not updated", entry.path);
            continue;
        }
        let Some(have) = installed_version(id) else {
            println!("{id}: not installed");
            continue;
        };
        // From the registry, or from the repository it came from.
        let unlisted = store::unlisted_from(id);
        let latest = if unlisted.is_none() {
            match cat.find(id) {
                Some(l) => l.clone(),
                None => {
                    println!("{id} {have}: not in the registry");
                    continue;
                }
            }
        } else {
            match store::release_listing(unlisted.as_deref().unwrap_or_default(), None) {
                Ok(l) => l,
                Err(e) => {
                    println!("{id} {have}: {e}");
                    continue;
                }
            }
        };
        if !store::newer(&latest.version, &have) {
            println!("{id} {have}: up to date");
            continue;
        }
        println!("{id}: {have} → {}", latest.version);
        failed |= !install_listing(&cfg, &latest);
    }
    if failed {
        std::process::exit(1);
    }
}

fn uninstall(a: &Args) {
    let id = a.positional.get(1).unwrap_or_else(|| die("trunk-pro plugin uninstall <id>"));
    if !store::valid_id(id) {
        die(&format!("{id:?} isn't a plugin id"));
    }
    let (path, mut cfg) = load_config(a);
    let entry = cfg.plugins.get(id).cloned();
    let dir = plugins_dir().join(id);
    let custom = entry.as_ref().is_some_and(|e| !e.path.is_empty());
    if entry.is_none() && !dir.is_dir() {
        die(&format!("{id} isn't installed"));
    }
    if !custom && dir.is_dir() {
        std::fs::remove_dir_all(&dir).unwrap_or_else(|e| die(&format!("Couldn't delete {}: {e}", dir.display())));
    }
    super::forget(&mut cfg, id);
    cfg.save(&path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
    println!("{id} {}. Its data folder, {}, is kept.", if custom { "removed (your build isn't touched)" } else { "uninstalled" }, super::data_dir(id).display());
}

fn run_calls(a: &Args) {
    let target = a.positional.get(1).unwrap_or_else(|| die("trunk-pro plugin run <executable | id> [calls…]"));
    let (_, cfg) = load_config(a);
    // An executable, or an installed plugin's id (with its settings from the config).
    let (id, exe, config, systems) = if Path::new(target).is_file() {
        let m = describe(Path::new(target)).unwrap_or_else(|e| die(&e));
        (m.id, PathBuf::from(target), Value::Null, BTreeMap::new())
    } else {
        match all_plugins(&cfg).get(target) {
            Some(e) => (target.clone(), executable(target, e), e.settings.clone(), super::system_settings(&cfg, target, 0)),
            None => die(&format!("{target}: no such file, and no plugin by that id")),
        }
    };
    // Not the plugin's real data folder: a test run mustn't leave work for the installed plugin.
    let data_dir = match a.get("data-dir") {
        Some(d) => PathBuf::from(d),
        None => std::env::temp_dir().join(format!("trunk-pro-plugin-run-{id}")),
    };
    eprintln!("Data folder: {}", data_dir.display());
    let mut spec = Spec { id, copy: 0, exe, config, systems, data_dir: Some(data_dir) };
    if let Some(s) = a.get("settings") {
        let v: Value = std::fs::read_to_string(s).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| die(&format!("{s}: not JSON")));
        spec.config = v.get("config").cloned().unwrap_or(Value::Null);
        spec.systems = v.get("systems").and_then(|s| serde_json::from_value(s.clone()).ok()).unwrap_or_default();
    }
    let capture_dir = PathBuf::from(a.get("capture-dir").map(String::from).unwrap_or(cfg.recording.capture_dir.clone()));
    let limit: usize = a.get("limit").and_then(|s| s.parse().ok()).unwrap_or(10);
    let mut calls: Vec<PathBuf> = Vec::new();
    for p in a.positional.iter().skip(2) {
        let p = PathBuf::from(p);
        if p.is_dir() {
            calls.extend(newest_calls(&p, limit));
        } else {
            calls.push(p);
        }
    }
    // The systems: the config's, and any other the calls name.
    let mut systems = super::systems_of(&cfg);
    let loaded: Vec<(PathBuf, String)> =
        calls.iter().map(|p| (p.clone(), std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("{}: {e}", p.display()))))).collect();
    for (_, j) in &loaded {
        let v: Value = serde_json::from_str(j).unwrap_or_default();
        let name = v["short_name"].as_str().unwrap_or("").to_string();
        if !systems.iter().any(|s| s.short_name == name) {
            let index = systems.iter().filter(|s| trunk_core::trunk::conventional_index(s.index).is_none()).count() as u16;
            systems.push(SystemInfo { index, short_name: name, kind: "p25".into(), config: Value::Null });
        }
    }
    let notes: super::host::Notes = Arc::new(print_note);
    let audio = AudioSettings { encoder: a.get("encoder").unwrap_or("auto").to_string(), ..Default::default() };
    let mut host = PluginHost::start(vec![spec], &audio, &systems, &capture_dir, notes);
    if host.is_empty() {
        std::process::exit(1);
    }
    eprintln!("M4A: {}", host.encoder().unwrap_or("none"));
    for (p, json) in &loaded {
        let v: Value = serde_json::from_str(json).unwrap_or_default();
        let system = systems.iter().find(|s| Some(s.short_name.as_str()) == v["short_name"].as_str()).map_or(0, |s| s.index);
        // Its key: relative to the capture folder (absolute when it isn't in there).
        let base = p.with_extension("");
        let rel = base.strip_prefix(&capture_dir).map(Path::to_path_buf).unwrap_or(base.clone());
        host.concluded(system, &rel.to_string_lossy().replace('\\', "/"), json, super::host::CallAudio::on_disk(&base));
    }
    let grace: f64 = a.get("grace").and_then(|s| s.parse().ok()).unwrap_or(30.0);
    host.shutdown(Duration::from_secs_f64(grace));
}

fn print_note(n: Note) {
    match n {
        Note::Log { plugin, level, text } => {
            let who = if plugin.is_empty() { "host".to_string() } else { plugin };
            let lv = match level {
                Level::Error => "error",
                Level::Warn => "warn",
                Level::Info => "info",
                Level::Debug => "debug",
            };
            eprintln!("[{who}] {lv}: {text}");
        }
        Note::State { plugin, state, message } => {
            let s = match state {
                State::Ok => "ok",
                State::Warning => "warning",
                State::Error => "error",
            };
            eprintln!("[{plugin}] status {s}: {message}");
        }
        Note::Result { plugin, path, outcome, message, url } => {
            let mark = match outcome {
                Outcome::Ok => "✓",
                Outcome::Skipped => "–",
                Outcome::Failed => "✗",
            };
            let extra = [message, url].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
            println!("[{plugin}] {mark} {path} {extra}");
        }
        Note::Metrics { plugin, metrics } => {
            eprintln!("[{plugin}] metrics: {}", serde_json::to_string(&metrics).unwrap_or_default());
        }
    }
}

/// The newest `limit` call JSONs under `dir`.
fn newest_calls(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut found: BTreeMap<std::time::SystemTime, Vec<PathBuf>> = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "json") && p.with_extension("wav").is_file() {
                let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                found.entry(t).or_default().push(p);
            }
        }
    }
    let mut v: Vec<PathBuf> = found.into_values().rev().flatten().take(limit).collect();
    v.reverse();
    v
}
