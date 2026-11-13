//! `trunk-pro access …`: who may sign in, and signing in on or off — the
//! first user and their access token on a recorder with no browser nearby,
//! and the way back in for whoever lost theirs. They're written to the config; a running recorder
//! takes them once it's restarted.

use std::path::{Path, PathBuf};

use crate::config::{ApiAccess, Config};
use crate::{die, Args};

pub fn run(a: &Args) {
    match a.positional.first().map(String::as_str) {
        Some("list") => list(a),
        Some("add") => add(a),
        Some("remove") => remove(a),
        Some("passkey") if a.positional.get(1).map(String::as_str) == Some("remove") => remove_passkey(a),
        Some("on") => on(a, true),
        Some("off") => on(a, false),
        _ => die(USAGE),
    }
}

const USAGE: &str = "\
trunk-pro access list [--config file.json]
    Whether signing in is on, and the users.
trunk-pro access add <name> [--access listen|control]
    A new user (control by default) with an access token, whose secret is
    printed this once.
trunk-pro access remove <name>
    Remove a user: whoever signed in as them is signed out.
trunk-pro access passkey remove <name> <passkey>
    Remove one of a user's passkeys (by its name, as list shows it): whoever
    signed in with it is signed out. (Passkeys are added from a browser.)
trunk-pro access on | off
    Signing in on (it needs a control user first) or off.
A running recorder takes the change when it's restarted.
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

fn list(a: &Args) {
    let (path, cfg) = load_config(a);
    let auth = &cfg.server.auth;
    println!("{}", path.display());
    println!("  signing in {}, anonymous {}", if auth.enabled { "on" } else { "off" }, auth.anonymous.map_or("none", ApiAccess::name));
    let day = |t: i64| chrono::DateTime::from_timestamp(t, 0).map_or(String::new(), |d| format!(" {}", d.format("%Y-%m-%d")));
    for (name, u) in &auth.users {
        let token = u.token.as_ref().map_or("no token".to_string(), |t| format!("token{}", day(t.created)));
        println!("  {name} [{}] {token}", u.access.name());
        for p in &u.passkeys {
            println!("    passkey \"{}\"{}", p.name, day(p.created));
        }
    }
}

fn add(a: &Args) {
    let name = a.positional.get(1).unwrap_or_else(|| die("trunk-pro access add <name> [--access listen|control]"));
    let access = match a.get("access").unwrap_or("control") {
        "control" => ApiAccess::Control,
        "listen" => ApiAccess::Listen,
        x => die(&format!("--access {x}: listen or control")),
    };
    let (path, mut cfg) = load_config(a);
    let secret = super::add_user(&mut cfg.server.auth, name, access).unwrap_or_else(|e| die(&e));
    save(&path, &cfg);
    eprintln!("{} [{}]: their access token's secret, shown this once:", name.trim(), access.name());
    println!("{}", secret.expose());
}

fn remove(a: &Args) {
    let name = a.positional.get(1).unwrap_or_else(|| die("trunk-pro access remove <name>"));
    let (path, mut cfg) = load_config(a);
    if cfg.server.auth.users.remove(name.trim()).is_none() {
        die(&format!("There's no user named \"{}\".", name.trim()));
    }
    if let Some(why) = cfg.server.auth.problem() {
        die(&format!("{why} (Turn signing in off first: trunk-pro access off.)"));
    }
    save(&path, &cfg);
    println!("{} removed.", name.trim());
}

fn remove_passkey(a: &Args) {
    let (Some(name), Some(label)) = (a.positional.get(2), a.positional.get(3)) else { die("trunk-pro access passkey remove <name> <passkey>") };
    let (path, mut cfg) = load_config(a);
    let Some(u) = cfg.server.auth.users.get_mut(name.trim()) else { die(&format!("There's no user named \"{}\".", name.trim())) };
    let before = u.passkeys.len();
    u.passkeys.retain(|p| p.name != label.trim());
    if u.passkeys.len() == before {
        let have: Vec<&str> = u.passkeys.iter().map(|p| p.name.as_str()).collect();
        die(&format!("{} has no passkey named \"{}\" ({}).", name.trim(), label.trim(), if have.is_empty() { "none at all".into() } else { format!("theirs: {}", have.join(", ")) }));
    }
    if let Some(why) = cfg.server.auth.problem() {
        die(&format!("{why} (Turn signing in off first: trunk-pro access off.)"));
    }
    save(&path, &cfg);
    println!("{}'s passkey \"{}\" removed.", name.trim(), label.trim());
}

fn on(a: &Args, enabled: bool) {
    let (path, mut cfg) = load_config(a);
    cfg.server.auth.enabled = enabled;
    if let Some(why) = cfg.server.auth.problem() {
        die(&format!("{why} (trunk-pro access add <name>)"));
    }
    save(&path, &cfg);
    println!("Signing in {}.", if enabled { "on" } else { "off" });
}
