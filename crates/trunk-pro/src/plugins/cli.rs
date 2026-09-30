//! `trunk-pro plugin …`: look at plugins, and run one against calls already
//! on disk — a plugin author's test bench.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use trunk_recorder_plugin::{Level, Outcome, State, SystemInfo};

use super::{describe, executable, AudioSettings, Note, PluginEntry, PluginHost, PluginsFile, Spec};
use crate::{die, Args};

pub fn run(a: &Args) {
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
        _ => die(USAGE),
    }
}

const USAGE: &str = "\
trunk-pro plugin list [--config file.json]
    The plugins in plugins.json (beside the config), and what each is.
trunk-pro plugin describe <executable>
    A plugin's manifest, checked.
trunk-pro plugin run <executable | id> [<call.json | folder>…] [options]
    Run a plugin and send it calls already on disk (call.concluded), printing
    what it says; then stop it. A folder means the calls in it (the newest
    --limit, default 10). An id runs an installed plugin with its settings
    from plugins.json.
    --settings file.json   its settings: {\"config\": {…}, \"systems\": {\"<shortName>\": {…}}}
    --capture-dir dir      the calls' capture folder (default: the config's)
    --encoder auto|ffmpeg|afconvert|fdkaac|none   for M4A (default auto)
    --grace 30             seconds it gets to finish after the last call
    --data-dir dir         its data folder (default: one in the temp folder, kept
                           between runs — not the installed plugin's)
";

fn config_path(a: &Args) -> PathBuf {
    a.get("config").map(PathBuf::from).unwrap_or_else(|| crate::config::config_dir().join("config.json"))
}

fn list(a: &Args) {
    let path = PluginsFile::path_for(&config_path(a));
    let file = PluginsFile::load(&path).unwrap_or_else(|e| die(&e));
    println!("{}", path.display());
    if file.plugins.is_empty() {
        println!("  (no plugins)");
    }
    for (id, e) in &file.plugins {
        let exe = executable(id, e);
        let what = match describe(&exe) {
            Ok(m) => format!("{} {} — subscribes to {}", m.name, m.version, m.subscribe.join(", ")),
            Err(e) => format!("can't run: {e}"),
        };
        println!("  {id} [{}] {}\n    {what}", if e.enabled { "on" } else { "off" }, exe.display());
    }
    let enc = super::Encoder::find(&file.audio.encoder);
    println!("M4A encoder ({}): {}", file.audio.encoder, enc.as_ref().map_or("none found (install ffmpeg)", |e| e.name()));
}

fn run_calls(a: &Args) {
    let target = a.positional.get(1).unwrap_or_else(|| die("trunk-pro plugin run <executable | id> [calls…]"));
    let cfg_path = config_path(a);
    let cfg = crate::config::Config::load(&cfg_path);
    // An executable, or an installed plugin's id (with its settings).
    let (id, entry) = if Path::new(target).is_file() {
        let m = describe(Path::new(target)).unwrap_or_else(|e| die(&e));
        (m.id, PluginEntry { enabled: true, path: target.clone(), ..Default::default() })
    } else {
        let file = PluginsFile::load(&PluginsFile::path_for(&cfg_path)).unwrap_or_else(|e| die(&e));
        match file.plugins.get(target) {
            Some(e) => (target.clone(), PluginEntry { path: executable(target, e).display().to_string(), ..e.clone() }),
            None => die(&format!("{target}: no such file, and no plugin by that id in plugins.json")),
        }
    };
    // Not the plugin's real data folder: a test run mustn't leave work for the installed plugin.
    let data_dir = match a.get("data-dir") {
        Some(d) => PathBuf::from(d),
        None => std::env::temp_dir().join(format!("trunk-pro-plugin-run-{id}")),
    };
    eprintln!("Data folder: {}", data_dir.display());
    let mut spec = Spec { id, exe: PathBuf::from(&entry.path), config: entry.config, systems: entry.systems, data_dir: Some(data_dir) };
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
            let index = systems.iter().filter(|s| s.index != trunk_recorder_plugin::CONVENTIONAL).count() as u16;
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
        host.concluded(system, &rel.to_string_lossy().replace('\\', "/"), json);
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
