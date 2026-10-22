//! Interfaces besides the built-in one: web pages (HTML, JavaScript, CSS;
//! no server side) for people who listen rather than run the recorder, a
//! scanner wall or a player, say. Each is served on a port of its own, so
//! each is a site of its own to the browser: it can't read the built-in
//! interface's storage, nor another interface's. The recorder's API is
//! served beside it (`/api/ws`, `/api/client.js`, `/calls/…`), so a page
//! reaches it at the same address it would on the built-in interface's port,
//! with what its `access` allows ([`crate::access`]).
//!
//! An interface comes from the interface store ([`store`],
//! github.com/TrunkRecorder/interfaces; installed in `<config
//! dir>/interfaces/<id>/`) or is a folder of the user's own (`path`). The
//! config's `server.interfaces` says which are served, on which port, with
//! what access:
//!
//! ```json
//! "server": { "port": 8080, "interfaces": {
//!     "trunkamp": { "port": 8081, "enabled": true, "access": "listen" },
//!     "wall": { "port": 8082, "enabled": true, "access": "control", "path": "~/wall-display" } } }
//! ```
//!
//! [`serve`] keeps a listener running for each enabled one, and changes them
//! as the config changes: no restart. `trunk-pro --ui <folder>` serves a
//! folder for one run besides them (an author's test bench).
//!
//! Browser → server: `interfaces` (the list), `interfaceStore {refresh?}`,
//! `installInterface {id}` or `{repository, tag?}`, `addInterface {path}`,
//! `removeInterface {id}`. Server → browser: `interfaces`, `interfaceStore`,
//! `interfaceInstall {key, id, stage, message?}` as an install goes. The
//! rest (port, on or off, access) is the config's, saved as it's changed.

pub mod cli;
pub mod serve;
pub mod store;

use trunk_app::protocol::{body, Install, ToClient};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{ApiAccess, Config, Interface};
use crate::store::{Provenance, Shelf};

/// The API version interfaces are written against: the WebSocket protocol
/// (web/src/protocol.ts) and the files beside it. An interface that needs a
/// newer one isn't installed.
pub const API_VERSION: u32 = 1;

/// What an interface says it is: `interface.json` in its folder. Required
/// for one from the store; a folder of the user's own may go without.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// The API version it's written for ([`API_VERSION`]).
    pub api: u32,
    /// What it needs: "listen" (the default) or "control".
    pub access: ApiAccess,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub repository: String,
    /// What it looks like, shown before it's installed and on its card.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<Screenshot>,
}

/// A picture of an interface: a file in its folder.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Screenshot {
    /// Relative to the folder: `screenshots/player.png`.
    pub path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub caption: String,
}

pub const MANIFEST: &str = "interface.json";
/// At most this many screenshots, each at most [`MAX_SCREENSHOT`] bytes.
pub const MAX_SCREENSHOTS: usize = 6;
pub const MAX_SCREENSHOT: u64 = 2 << 20;

/// A screenshot's media type, from its name: PNG, JPEG or WebP only.
pub fn image_type(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.')?.1;
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

impl Manifest {
    /// The folder's `interface.json`: None when it has none.
    pub fn read(dir: &Path) -> Result<Option<Manifest>, String> {
        match std::fs::read_to_string(dir.join(MANIFEST)) {
            Ok(text) => Manifest::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", dir.join(MANIFEST).display())),
        }
    }

    pub fn parse(text: &str) -> Result<Manifest, String> {
        let m: Manifest = serde_json::from_str(text).map_err(|e| format!("{MANIFEST} isn't readable ({e})"))?;
        if !Interface::valid_id(&m.id) {
            return Err(format!("{MANIFEST}: \"{}\" can't be an id (lowercase letters, digits and -)", m.id));
        }
        if m.name.trim().is_empty() || !crate::store::valid_version(&m.version) {
            return Err(format!("{MANIFEST} needs a name and a version (1.0.0)"));
        }
        if m.api == 0 {
            return Err(format!("{MANIFEST} needs the API it's written for (\"api\": {API_VERSION})"));
        }
        for (what, url) in [("homepage", &m.homepage), ("repository", &m.repository)] {
            if !url.is_empty() && !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(format!("{MANIFEST}: {what} should be a web address (https://…)"));
            }
        }
        if m.screenshots.len() > MAX_SCREENSHOTS {
            return Err(format!("{MANIFEST}: at most {MAX_SCREENSHOTS} screenshots"));
        }
        let mut names = std::collections::BTreeSet::new();
        for shot in &m.screenshots {
            // Plain names between single slashes: each is released as a file of its own, by name.
            let plain = shot.path.split('/').all(|p| !p.is_empty() && !p.starts_with('.') && p != "..");
            if shot.path.is_empty() || !plain || image_type(&shot.path).is_none() || !names.insert(shot.path.rsplit('/').next().unwrap_or_default()) {
                return Err(format!("{MANIFEST}: screenshot \"{}\" should be a .png, .jpg or .webp in the interface's folder, its file name unlike the others'", shot.path));
            }
        }
        Ok(m)
    }

    /// Screenshot `n`'s file in `dir`, when it's there and small enough.
    pub fn screenshot(&self, dir: &Path, n: usize) -> Option<(PathBuf, &'static str)> {
        let shot = self.screenshots.get(n)?;
        let file = dir.join(&shot.path);
        let real = file.canonicalize().ok()?;
        let ok = real.starts_with(dir.canonicalize().ok()?) && std::fs::metadata(&real).is_ok_and(|m| m.is_file() && m.len() <= MAX_SCREENSHOT);
        ok.then(|| (real, image_type(&shot.path).unwrap()))
    }
}

/// Where installed interfaces are kept: `<config dir>/interfaces/`.
pub fn interfaces_dir() -> PathBuf {
    crate::paths::data_dir().join("interfaces")
}

/// Interface `id`'s folder: its `path` (absolute, `~/…`, or relative to the
/// config file's folder), else its installed copy.
pub fn folder(config_path: &Path, id: &str, i: &Interface) -> PathBuf {
    let path = i.path.trim();
    if path.is_empty() {
        return interfaces_dir().join(id);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return trunk_app::config::home().join(rest);
    }
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        config_path.parent().unwrap_or(Path::new(".")).join(p)
    }
}

/// Why a folder can't be served, if it can't.
fn folder_problem(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        Some(format!("The folder {} isn't there.", dir.display()))
    } else if !dir.join("index.html").is_file() {
        Some(format!("{} has no index.html.", dir.display()))
    } else {
        None
    }
}

/// What an install leaves for the config.
pub struct Installed {
    pub id: String,
    /// The access to give it when it's new: what a reviewed listing asks; a
    /// release nobody reviewed may only listen until the user says otherwise.
    pub access: ApiAccess,
    /// It came from another repository than what was installed under its id
    /// before (or nothing was): access given to that isn't this one's, so the
    /// config's entry takes `access` again.
    pub new_source: bool,
}

impl Installed {
    /// Put it in the config: a new entry on the first free port, or the
    /// entry there with its access reset when it's from another source.
    pub fn apply(&self, c: &mut Config, taken: &[u16]) {
        let port = c.server.free_port(taken);
        let e = c.server.interfaces.entry(self.id.clone()).or_insert(Interface { port, access: self.access, ..Default::default() });
        if self.new_source {
            e.access = self.access;
        }
    }
}

/// `--ui <folder>`: an interface served this run only, with control access.
#[derive(Clone, Debug)]
pub struct Dev {
    pub id: String,
    pub folder: PathBuf,
    pub port: u16,
}

/// The interfaces' part of the app's shared state.
pub struct Interfaces {
    shelf: Shelf<store::Index>,
    /// The listeners running, by id ([`serve`]).
    pub(crate) serving: Mutex<BTreeMap<String, serve::Serving>>,
    /// Why one that should be served isn't (its port is taken, say), by id.
    pub(crate) problems: Mutex<BTreeMap<String, String>>,
    pub dev: Option<Dev>,
    /// The built-in interface's port this run (`--port` may differ from the config's).
    builtin_port: u16,
}

impl Interfaces {
    pub fn new(dev: Option<Dev>, builtin_port: u16) -> Interfaces {
        Interfaces { shelf: Shelf::new(&store::REGISTRY), serving: Mutex::new(BTreeMap::new()), problems: Mutex::new(BTreeMap::new()), dev, builtin_port }
    }

    /// Ports in use this run that the config doesn't know of: the built-in
    /// interface's (`--port`) and `--ui`'s.
    pub fn taken(&self) -> Vec<u16> {
        [Some(self.builtin_port), self.dev.as_ref().map(|d| d.port)].into_iter().flatten().collect()
    }

    /// Why interface `id` can't be served as things are, if it can't: the
    /// config's reasons, or a port this run uses otherwise.
    pub fn problem(&self, cfg: &Config, id: &str) -> Option<String> {
        if self.dev.as_ref().is_some_and(|d| d.id == id) {
            return None;
        }
        let port = cfg.server.interfaces.get(id)?.port;
        cfg.server.interface_problem(id).or_else(|| {
            if port == self.builtin_port {
                Some(format!("Port {port} is the built-in interface's."))
            } else if self.dev.as_ref().is_some_and(|d| d.port == port) {
                Some(format!("Port {port} is --ui's this run."))
            } else {
                None
            }
        })
    }

    /// What's to be served: the config's interfaces and `--ui`'s, by id, with their folders.
    pub fn all(&self, cfg: &Config, config_path: &Path) -> BTreeMap<String, (Interface, PathBuf)> {
        let mut all: BTreeMap<String, (Interface, PathBuf)> = cfg.server.interfaces.iter().map(|(id, i)| (id.clone(), (i.clone(), folder(config_path, id, i)))).collect();
        if let Some(d) = &self.dev {
            all.insert(d.id.clone(), (Interface { port: d.port, enabled: true, access: ApiAccess::Control, path: d.folder.display().to_string() }, d.folder.clone()));
        }
        all
    }

    /// What interface `id` may do through the API now; None once it's gone or off.
    pub fn access(&self, cfg: &Config, id: &str) -> Option<ApiAccess> {
        match &self.dev {
            Some(d) if d.id == id => Some(ApiAccess::Control),
            _ => cfg.server.interfaces.get(id).filter(|i| i.enabled).map(|i| i.access),
        }
    }

    /// The `interfaces` message: each one, what it is, and how it's doing.
    pub fn list(&self, cfg: &Config, config_path: &Path) -> ToClient {
        let serving = self.serving.lock().unwrap();
        let problems = self.problems.lock().unwrap();
        let list: Vec<Value> = self
            .all(cfg, config_path)
            .into_iter()
            .map(|(id, (i, dir))| {
                let (manifest, bad) = match Manifest::read(&dir) {
                    Ok(m) => (m, None),
                    Err(e) => (None, Some(e)),
                };
                let installed = i.path.is_empty();
                let from = if installed { Provenance::read(&dir) } else { None };
                let problem = folder_problem(&dir).or(bad).or_else(|| self.problem(cfg, &id)).or_else(|| problems.get(&id).cloned());
                json!({
                    "id": id,
                    "folder": dir.display().to_string(),
                    "installed": installed,
                    "dev": self.dev.as_ref().is_some_and(|d| d.id == id),
                    "manifest": manifest,
                    "repository": from.as_ref().map(|p| p.repository.clone()).filter(|r| !r.is_empty()),
                    "reviewed": from.as_ref().is_some_and(|p| p.reviewed),
                    "port": i.port,
                    "access": i.access,
                    "serving": serving.get(&id).is_some_and(|s| s.port == i.port),
                    "problem": if i.enabled { problem } else { None },
                })
            })
            .collect();
        ToClient::Interfaces(body(json!({ "interfaces": list, "api": API_VERSION })))
    }

    // ─── The store ─────────────────────────────────────────────────────────

    /// The `interfaceStore` message.
    pub fn store(&self, refresh: bool) -> ToClient {
        store::message(&self.shelf.catalog(refresh))
    }

    /// `install {kind: "interface", id}` or `{…, repository, tag?}`: says how
    /// it goes to `report`; blocks until done. Returns what the config should
    /// have for it once installed ([`Installed`]).
    pub fn install(&self, what: &Install, cfg: &Config, report: &dyn Fn(crate::store::Progress)) -> Option<Installed> {
        let mut done = None;
        self.shelf.install(what.key(), report, |id_out, stage| {
            let listing = match what {
                Install::Listed { id } => self.shelf.catalog(false).find(id).cloned().ok_or_else(|| format!("{id} isn't in the interface store"))?,
                Install::Release { repository, tag } => {
                    let repo = Some(repository.trim()).filter(|r| !r.is_empty()).ok_or("Which interface? Its GitHub repository, or its release's page.")?;
                    let l = store::release_listing(repo, tag.as_deref())?;
                    if let Some(listed) = self.shelf.catalog(false).find(&l.id) {
                        if !listed.repository.eq_ignore_ascii_case(&l.repository) {
                            return Err(format!("{} is the id of {} in the interface store ({}). Install that one from the list.", l.id, listed.name, listed.repository));
                        }
                    }
                    l
                }
            };
            *id_out = listing.id.clone();
            if let Some(i) = cfg.server.interfaces.get(&listing.id).filter(|i| !i.path.is_empty()) {
                return Err(format!("{} is your folder {}: remove it first.", listing.id, i.path));
            }
            let dir = interfaces_dir().join(&listing.id);
            let before = Manifest::read(&dir).ok().flatten().map(|m| m.version);
            let from = Provenance::read(&dir).map(|p| p.repository);
            store::install(&listing, &|s| stage(&listing.id, s))?;
            done = Some(Installed {
                id: listing.id.clone(),
                access: if listing.tier == "unlisted" { ApiAccess::Listen } else { listing.access },
                new_source: from.is_none_or(|r| !r.eq_ignore_ascii_case(&listing.repository)),
            });
            let (name, version) = (&listing.name, &listing.version);
            Ok(match before {
                Some(old) if crate::store::newer(version, &old) => format!("Updated {name} from {old} to {version}."),
                Some(old) if &old != version => format!("Installed {name} {version} in place of {old}."),
                Some(_) => format!("Reinstalled {name} {version}."),
                None => format!("Installed {name} {version}."),
            })
        });
        done
    }

    /// A store listing's screenshot, by its SHA-256: fetched once (only a
    /// URL a listing names), checked, and kept in the config folder.
    pub fn store_screenshot(&self, sha256: &str) -> Result<(Vec<u8>, &'static str), String> {
        let sha = sha256.to_ascii_lowercase();
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("not a checksum".into());
        }
        let cat = self.shelf.catalog(false);
        let shot = cat.index.interfaces.iter().flat_map(|l| &l.screenshots).find(|s| s.sha256 == sha).ok_or("no listing has that screenshot")?;
        let mime = image_type(&shot.url).ok_or("not a picture")?;
        let cache = crate::paths::data_dir().join("interface-screenshots").join(&sha);
        if let Ok(data) = std::fs::read(&cache) {
            if crate::store::sha256_hex(&data) == sha {
                return Ok((data, mime));
            }
        }
        let data = crate::store::download(&shot.url, &sha, MAX_SCREENSHOT, "a screenshot")?;
        let _ = std::fs::create_dir_all(cache.parent().unwrap());
        let _ = crate::config::write_atomic(&cache, &data);
        Ok((data, mime))
    }

    /// Screenshot `n` of interface `id`, from its folder.
    pub fn screenshot(&self, cfg: &Config, config_path: &Path, id: &str, n: usize) -> Option<(PathBuf, &'static str)> {
        let (_, dir) = self.all(cfg, config_path).remove(id)?;
        Manifest::read(&dir).ok()??.screenshot(&dir, n)
    }

    /// `addInterface {path}`: a folder of the user's own. Returns its id
    /// and its entry, for the config.
    pub fn add(&self, path: &str, cfg: &Config, config_path: &Path) -> Result<(String, Interface), String> {
        let path = Some(path.trim()).filter(|p| !p.is_empty()).ok_or("Which folder?")?;
        let entry = Interface { path: path.to_string(), port: cfg.server.free_port(&self.taken()), ..Default::default() };
        let dir = folder(config_path, "", &entry);
        if let Some(p) = folder_problem(&dir) {
            return Err(p);
        }
        let m = Manifest::read(&dir)?;
        let id = match &m {
            Some(m) => m.id.clone(),
            None => slug(&dir.canonicalize().unwrap_or(dir.clone()).file_name().unwrap_or_default().to_string_lossy()),
        };
        if cfg.server.interfaces.contains_key(&id) {
            return Err(format!("There's already an interface called {id}."));
        }
        Ok((id, Interface { access: m.map(|m| m.access).unwrap_or_default(), ..entry }))
    }

    /// `removeInterface {id}`: an installed copy is deleted (a folder of the
    /// user's own isn't). The server then forgets it in the config.
    pub fn remove(&self, id: &str, cfg: &Config) -> Result<(), String> {
        if !Interface::valid_id(id) {
            return Err(format!("bad interface id {id:?}"));
        }
        if cfg.server.interfaces.get(id).is_none_or(|i| i.path.is_empty()) {
            let dir = interfaces_dir().join(id);
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir).map_err(|e| format!("Couldn't delete {}: {e}", dir.display()))?;
            }
        }
        Ok(())
    }
}

/// "My Wall_Display" → "my-wall-display": a folder's name as an id.
pub fn slug(name: &str) -> String {
    let s: String = name.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let s: String = s.chars().take(64).collect();
    if s.is_empty() {
        "interface".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders() {
        let cfg = Path::new("/etc/trunk-pro/config.json");
        let at = |p: &str| folder(cfg, "wall", &Interface { path: p.into(), ..Default::default() });
        assert_eq!(at("ui/wall"), Path::new("/etc/trunk-pro/ui/wall"));
        assert_eq!(at("/srv/wall"), Path::new("/srv/wall"));
        assert!(at("~/wall").ends_with("wall") && !at("~/wall").starts_with("/etc"));
        assert!(at("").ends_with("interfaces/wall"));
    }

    /// An install from somewhere else than what had its id keeps no access given to that.
    #[test]
    fn a_new_source_takes_its_own_access() {
        let mut c = Config::default();
        c.server.interfaces.insert("wall".into(), Interface { port: 8081, access: ApiAccess::Control, ..Default::default() });
        let unreviewed = |new_source| Installed { id: "wall".into(), access: ApiAccess::Listen, new_source };
        unreviewed(false).apply(&mut c, &[]);
        assert_eq!(c.server.interfaces["wall"].access, ApiAccess::Control, "an update from the same repository keeps it");
        unreviewed(true).apply(&mut c, &[]);
        assert_eq!(c.server.interfaces["wall"].access, ApiAccess::Listen);
        assert_eq!(c.server.interfaces["wall"].port, 8081);
        // A new one skips ports this run uses that the config doesn't know.
        Installed { id: "b".into(), access: ApiAccess::Listen, new_source: true }.apply(&mut c, &[8080, 8082]);
        assert_eq!(c.server.interfaces["b"].port, 8083);
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("My Wall_Display"), "my-wall-display");
        assert_eq!(slug("--"), "interface");
        assert!(Interface::valid_id(&slug("Trunkamp (v2)")));
    }

    #[test]
    fn manifests() {
        let m = Manifest::parse(r#"{ "id": "trunkamp", "name": "Trunkamp", "version": "1.0.0", "api": 1 }"#).unwrap();
        assert_eq!(m.access, ApiAccess::Listen);
        assert!(Manifest::parse(r#"{ "id": "../x", "name": "x", "version": "1" }"#).is_err());
        assert!(Manifest::parse(r#"{ "id": "x", "version": "1.0.0", "api": 1 }"#).is_err());
        let m = Manifest::parse(r#"{ "id": "wall", "name": "Wall", "version": "1.0.0", "api": 1, "access": "control" }"#).unwrap();
        assert_eq!(m.access, ApiAccess::Control);
        let shots = |p: &str| Manifest::parse(&format!(r#"{{ "id": "w", "name": "W", "version": "1.0.0", "api": 1, "screenshots": [{{ "path": "{p}" }}] }}"#));
        assert!(shots("shots/a.png").is_ok() && shots("b.webp").is_ok());
        assert!(Manifest::parse(r#"{ "id": "w", "name": "W", "version": "1.0.0", "api": 1, "homepage": "javascript:alert(1)" }"#).is_err());
        assert!(shots("a//b.png").is_err() && shots("./a.png").is_err());
        assert!(Manifest::parse(r#"{ "id": "w", "name": "W", "version": "1.0.0", "api": 1, "screenshots": [{ "path": "a/x.png" }, { "path": "b/x.png" }] }"#).is_err());
        assert!(Manifest::parse(r#"{ "id": "w", "name": "W", "version": "one", "api": 1 }"#).is_err());
        assert!(shots("../a.png").is_err() && shots("/etc/a.png").is_err() && shots(".hidden/a.png").is_err() && shots("a.svg").is_err());
    }

    #[test]
    fn ports_and_old_configs() {
        // A config from before: a list, and `home` (upgraded as it's read: trunk_app::migrate).
        let mut c = Config::from_json(r#"{ "server": { "port": 8080, "interfaces": [{ "name": "Wall_1", "path": "~/wall" }], "home": "Wall_1" } }"#).unwrap();
        assert!(!c.server.other.contains_key("home"));
        c.server.assign_ports();
        assert_eq!(c.server.interfaces["wall-1"].port, 8081);
        assert_eq!(c.server.interfaces["wall-1"].access, ApiAccess::Listen);
        assert!(c.server.interfaces["wall-1"].enabled);
        c.server.interfaces.insert("b".into(), Interface { port: 8081, ..Default::default() });
        assert!(c.server.interface_problem("b").is_none(), "the first of two on one port is served");
        assert!(c.server.interface_problem("wall-1").unwrap().contains("b's"));
        assert_eq!(c.server.free_port(&[8082]), 8083);
        c.server.interfaces.insert("c".into(), Interface { port: 8080, ..Default::default() });
        assert!(c.server.interface_problem("c").unwrap().contains("built-in"));
        let saved = serde_json::to_value(&c.server).unwrap();
        assert!(saved.get("home").is_none());
        assert_eq!(saved["interfaces"]["b"], json!({ "port": 8081, "enabled": true, "access": "listen" }));
    }
}
