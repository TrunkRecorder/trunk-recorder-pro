//! `trunk-pro interface …`: find interfaces in the store and install them,
//! add folders of your own, and list them — for a recorder with no browser
//! nearby. They're written to the config; a running recorder serves them
//! once it's restarted (its Interfaces page does it at once).

use trunk_app::protocol::Install;
use std::path::{Path, PathBuf};


use super::store;
use super::{folder, interfaces_dir, Installed, Interfaces, Manifest};
use crate::config::{ApiAccess, Config};
use crate::store::Provenance;
use crate::{die, Args};

pub fn run(a: &Args) {
    match a.positional.first().map(String::as_str) {
        Some("list") => list(a),
        Some("search") => search(a),
        Some("install") => install(a),
        Some("update") => update(a),
        Some("add") => add(a),
        Some("uninstall" | "remove") => uninstall(a),
        _ => die(USAGE),
    }
}

const USAGE: &str = "\
trunk-pro interface search [text]
    The interfaces in the store (github.com/TrunkRecorder/interfaces), or
    those whose name or description has <text>.
trunk-pro interface install <id | GitHub repository> [--tag v1.2.0] [--port N]
    Install an interface from the store, or the release of one that isn't in
    it (its latest, or --tag). It's served on the next free port (or --port).
trunk-pro interface update [id…]
    Install newer versions of installed interfaces (all of them, or these).
trunk-pro interface add <folder> [--port N]
    Serve a folder of your own (an index.html, and an interface.json if it
    has one) as an interface.
trunk-pro interface uninstall <id>
    Stop serving an interface and forget it; an installed copy is deleted.
trunk-pro interface list [--config file.json]
    The interfaces, their ports and access, and what each is.
";

fn load_config(a: &Args) -> (PathBuf, Config) {
    let path = crate::paths::config_path(a.get("config"));
    crate::paths::init(&path);
    let cfg = Config::load(&path).unwrap_or_else(|e| die(&e));
    (path, cfg)
}

fn save(path: &Path, cfg: &Config) {
    cfg.save(path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
}

fn port_arg(a: &Args, cfg: &Config) -> u16 {
    match a.get("port") {
        Some(p) => p.parse().ok().filter(|p| *p != 0 && *p != cfg.server.port).unwrap_or_else(|| die(&format!("--port {p}: a port number other than {}", cfg.server.port))),
        None => cfg.server.free_port(&[]),
    }
}

fn list(a: &Args) {
    let (path, cfg) = load_config(a);
    println!("{}", path.display());
    println!("  built-in [control] port {}", cfg.server.port);
    let host = if cfg.server.bind == "0.0.0.0" { "localhost" } else { &cfg.server.bind };
    for (id, i) in &cfg.server.interfaces {
        let dir = folder(&path, id, i);
        let what = match Manifest::read(&dir) {
            Ok(Some(m)) => format!("{} {}", m.name, m.version),
            Ok(None) => "(no interface.json)".into(),
            Err(e) => e,
        };
        let access = if i.access == ApiAccess::Control { "control" } else { "listen" };
        let state = cfg.server.interface_problem(id).unwrap_or_else(|| if i.enabled { format!("http://{host}:{}/", i.port) } else { "off".into() });
        println!("  {id} [{access}] port {} — {state}\n    {what} · {}", i.port, dir.display());
    }
}

fn installed_version(id: &str) -> Option<String> {
    Manifest::read(&interfaces_dir().join(id)).ok().flatten().map(|m| m.version)
}

fn search(a: &Args) {
    let cat = store::catalog();
    cat.say_source();
    cat.search(a.positional.get(1).map_or("", String::as_str), &installed_version);
}

/// Install `what` (an id in the store, or a GitHub repository), headless:
/// the same as the Interfaces page does, without the browser.
fn install_one(what: &str, tag: Option<&str>, cfg: &Config) -> Option<Installed> {
    let v = if what.contains('/') { Install::Release { repository: what.into(), tag: tag.map(String::from) } } else { Install::Listed { id: what.into() } };
    Interfaces::new(None, 0).install(&v, cfg, &|p| match p.stage {
        "done" => println!("{}", p.message.unwrap_or("")),
        "failed" => eprintln!("{}", p.message.unwrap_or("")),
        stage => eprintln!("  {stage}…"),
    })
}

fn install(a: &Args) {
    let what = a.positional.get(1).unwrap_or_else(|| die("trunk-pro interface install <id | GitHub repository> [--tag v1.2.0] [--port N]"));
    let (path, mut cfg) = load_config(a);
    if what.contains('/') {
        eprintln!("(Not from the interface store: nobody has reviewed it. It may only listen until you give it control.)");
    }
    let port = port_arg(a, &cfg);
    let Some(done) = install_one(what, a.get("tag"), &cfg) else { std::process::exit(1) };
    let new = !cfg.server.interfaces.contains_key(&done.id);
    done.apply(&mut cfg, &[]);
    let i = cfg.server.interfaces.get_mut(&done.id).unwrap();
    if new {
        i.port = port;
    }
    println!("{}: port {}{}", done.id, i.port, if i.access == ApiAccess::Control { ", control access" } else { "" });
    save(&path, &cfg);
}

fn update(a: &Args) {
    let (path, mut cfg) = load_config(a);
    let mut ids: Vec<String> = a.positional.iter().skip(1).cloned().collect();
    if ids.is_empty() {
        ids = cfg.server.interfaces.iter().filter(|(_, i)| i.path.is_empty()).map(|(id, _)| id.clone()).collect();
    }
    let cat = store::catalog();
    cat.say_source();
    let mut failed = false;
    for id in &ids {
        let Some(have) = installed_version(id) else {
            println!("{id}: not installed from the store");
            continue;
        };
        let from = Provenance::read(&interfaces_dir().join(id)).filter(|p| !p.reviewed).map(|p| p.repository);
        if cat.newer_release(id, &have, from.as_deref(), &|repo| store::release_listing(repo, None)).is_some() {
            match install_one(from.as_deref().unwrap_or(id), None, &cfg) {
                Some(done) => done.apply(&mut cfg, &[]),
                None => failed = true,
            }
        }
    }
    save(&path, &cfg);
    if failed {
        std::process::exit(1);
    }
}

fn add(a: &Args) {
    let dir = a.positional.get(1).unwrap_or_else(|| die("trunk-pro interface add <folder> [--port N]"));
    let (path, mut cfg) = load_config(a);
    let abs = std::fs::canonicalize(dir).unwrap_or_else(|e| die(&format!("{dir}: {e}")));
    let (id, mut entry) = Interfaces::new(None, 0).add(&abs.display().to_string(), &cfg, &path).unwrap_or_else(|e| die(&e));
    entry.port = port_arg(a, &cfg);
    println!("Added {id}: {} on port {}.", abs.display(), entry.port);
    cfg.server.interfaces.insert(id, entry);
    save(&path, &cfg);
}

fn uninstall(a: &Args) {
    let id = a.positional.get(1).unwrap_or_else(|| die("trunk-pro interface uninstall <id>"));
    let (path, mut cfg) = load_config(a);
    if !cfg.server.interfaces.contains_key(id) && !interfaces_dir().join(id).is_dir() {
        die(&format!("{id} isn't an interface here"));
    }
    Interfaces::new(None, 0).remove(id, &cfg).unwrap_or_else(|e| die(&e));
    cfg.server.interfaces.remove(id);
    save(&path, &cfg);
    println!("{id} removed.");
}
