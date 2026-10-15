//! Key containers unlocked with a password typed into the interface: a key source whose file is a
//! KFDtool container with no `passwordFile` ([`Config::lockable`]) starts locked. Its keys are held
//! in memory only, until Lock or the app quits; the password is never kept. Browsers are told which
//! sources are locked, never a key.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use trunk_core::p25::crypt::Keyring;

use crate::config::Config;
use crate::runtime::{Ctx, EngineCmd};

/// Unlocked key sources: name → (the file it was unlocked from, its keys).
pub type Unlocked = Mutex<BTreeMap<String, (PathBuf, Arc<Keyring>)>>;

/// The keys unlocked for source `name` of `cfg`, while it still names the same file.
pub fn held(ctx: &Ctx, cfg: &Config, name: &str) -> Option<Arc<Keyring>> {
    let file = trunk_app::keys::path(&cfg.keys.get(name)?.file, &ctx.config_path)?;
    ctx.unlocked.lock().unwrap().get(name).filter(|(f, _)| *f == file).map(|(_, r)| r.clone())
}

/// The `keys` message: each key source that is unlocked here, the systems that use it, and whether it is.
pub fn status_json(ctx: &Ctx) -> Value {
    let cfg = ctx.config.lock().unwrap().clone();
    let sources: Vec<Value> =
        cfg.lockable(&ctx.config_path).into_iter().map(|(name, _, systems)| json!({ "name": name, "systems": systems, "unlocked": held(ctx, &cfg, &name).is_some() })).collect();
    json!({ "type": "keys", "sources": sources })
}

/// Open key source `name`'s container with `password`: its systems decrypt with its keys from now
/// on, calls in progress too.
pub fn unlock(ctx: &Ctx, name: &str, password: &str) -> Result<(), String> {
    let cfg = ctx.config.lock().unwrap().clone();
    let Some((_, file, systems)) = cfg.lockable(&ctx.config_path).into_iter().find(|l| l.0 == name) else {
        return Err(format!("Key source {name} isn't a key container waiting for a password."));
    };
    let loaded = trunk_app::keys::open(&file, password).map_err(|e| format!("Key source {name}: {e}"))?;
    if loaded.ring.is_empty() {
        return Err(format!("Key source {name}: the container holds no key this program can use."));
    }
    for s in &loaded.skipped {
        log::warn!("Key source {name}: key left out: {s}");
    }
    let ring = Arc::new(loaded.ring);
    log::info!("Key source {name}: unlocked, {} key{}, for {}", ring.keys().len(), if ring.keys().len() == 1 { "" } else { "s" }, systems.join(", "));
    ctx.unlocked.lock().unwrap().insert(name.to_string(), (file, ring.clone()));
    apply(ctx, &systems, Some(ring));
    Ok(())
}

/// Forget key source `name`'s keys: its systems stop decrypting at once.
pub fn lock(ctx: &Ctx, name: &str) {
    if ctx.unlocked.lock().unwrap().remove(name).is_some() {
        log::info!("Key source {name}: locked");
    }
    let cfg = ctx.config.lock().unwrap().clone();
    let systems = cfg.lockable(&ctx.config_path).into_iter().find(|l| l.0 == name).map(|l| l.2).unwrap_or_default();
    apply(ctx, &systems, None);
}

/// The config changed: keys unlocked for a source it no longer has (or that names another file) are
/// forgotten, and browsers are told what is locked now.
pub fn config_changed(ctx: &Ctx, cfg: &Config) {
    ctx.unlocked.lock().unwrap().retain(|name, (file, _)| cfg.keys.get(name).and_then(|s| trunk_app::keys::path(&s.file, &ctx.config_path)).is_some_and(|f| f == *file));
    crate::runtime::publish(&ctx.hub, status_json(ctx));
}

/// `systems` decrypt with `ring` from now on (when recording), and browsers are told.
fn apply(ctx: &Ctx, systems: &[String], ring: Option<Arc<Keyring>>) {
    if ctx.runner.lock().unwrap().is_some() {
        let mut cmds = ctx.engine_cmds.lock().unwrap();
        for s in systems {
            cmds.push(EngineCmd::Keys { short_name: s.clone(), ring: ring.clone() });
        }
    }
    crate::runtime::publish(&ctx.hub, status_json(ctx));
}
