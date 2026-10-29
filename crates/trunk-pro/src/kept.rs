//! What's kept between runs besides the calls (research/sqlite-plan.md,
//! "The flat-file stores"): each system's talkgroup file, talker aliases,
//! band plans, the codes conventional frequencies carried, AutoTune's
//! scores, what sharing did, and the radio registry. With the store on
//! they're its rows, written by its writer; with `storage.backend: "files"`
//! they're files, written by the saver. One or the other: nothing is carried
//! between them. A store asked for that could not open keeps nothing (files
//! written then would be lost when it opens again): the log says so.

use std::fs;
use std::path::{Path, PathBuf};

use trunk_app::stats::radio::{RegistryChanges, SystemRadio};

use crate::db::{Change, Db};
use crate::saver::Saver;

/// A document kept between runs.
pub enum Doc<'a> {
    /// A system's talker aliases (CSV).
    Units(&'a str),
    /// A site's band plan, by its key.
    Bandplan(&'a str),
    /// The codes conventional frequencies carried ([`trunk_app::heard`]).
    Heard,
    /// AutoTune's scores.
    Tune,
    /// What sharing with the directory did (each record's version, the last share).
    Sharing,
}

impl Doc<'_> {
    fn kind(&self) -> &'static str {
        match self {
            Doc::Units(_) => "units",
            Doc::Bandplan(_) => "bandplan",
            Doc::Heard => "heard",
            Doc::Tune => "tune",
            Doc::Sharing => "sharing",
        }
    }

    fn key(&self) -> &str {
        match self {
            Doc::Units(k) | Doc::Bandplan(k) => k,
            _ => "",
        }
    }

    /// Its file in `dir`, as `files` keeps it.
    fn file(&self, dir: &Path) -> PathBuf {
        match self {
            Doc::Units(s) => dir.join(format!("{s}.units.csv")),
            Doc::Bandplan(k) => dir.join(format!("{k}.bandplan")),
            Doc::Heard => dir.join(trunk_app::Session::HEARD_FILE),
            Doc::Tune => dir.join("tune.json"),
            Doc::Sharing => dir.join("sharing.json"),
        }
    }
}

/// `doc` as last kept. Blocks: never from async code.
pub fn read(db: &Db, doc: Doc) -> Option<String> {
    read_in(db, &crate::paths::data_dir(), doc)
}

fn read_in(db: &Db, dir: &Path, doc: Doc) -> Option<String> {
    if db.uses_files() {
        return fs::read_to_string(doc.file(dir)).ok();
    }
    db.kept(doc.kind(), doc.key()).inspect_err(|e| log::warn!("Call store: {} {} not read ({e})", doc.kind(), doc.key())).ok().flatten()
}

/// Keep `text` as `doc`.
pub fn write(db: &Db, saver: &Saver, doc: Doc, text: String) {
    if db.uses_files() {
        saver.text(doc.file(&crate::paths::data_dir()), text);
    } else {
        db.send(Change::Kept { kind: doc.kind().into(), key: doc.key().into(), value: Some(text) });
    }
}

/// System `system`'s talkgroup file in `dir`, as `files` keeps it.
fn talkgroups_file(dir: &Path, system: &str) -> PathBuf {
    trunk_app::config::talkgroups_path(dir, system)
}

/// The talkgroup files of `cfg`'s systems (trunked and conventional), as
/// kept. Blocks: never from async code.
pub fn load_talkgroups(db: &Db, cfg: &trunk_app::Config) -> trunk_app::config::TalkgroupFiles {
    load_talkgroups_in(db, &crate::paths::data_dir(), cfg)
}

fn load_talkgroups_in(db: &Db, dir: &Path, cfg: &trunk_app::Config) -> trunk_app::config::TalkgroupFiles {
    let names = cfg.systems.iter().map(|s| &s.short_name).chain(cfg.conventional.iter().map(|c| &c.short_name));
    let read = |name: &String| -> Option<String> {
        if db.uses_files() {
            return fs::read_to_string(talkgroups_file(dir, name)).ok();
        }
        db.talkgroups(name).inspect_err(|e| log::warn!("Call store: talkgroups of {name} not read ({e})")).ok().flatten()
    };
    names.filter_map(|name| read(name).map(|csv| (name.clone(), csv))).collect()
}

/// Keep system `system`'s talkgroup file, edited from `old` to `new` (""
/// for `old`: in place of what's kept; "" for `new`: none). The store
/// writes only the lines that changed, and commits at once.
pub fn write_talkgroups(db: &Db, saver: &Saver, system: &str, old: &str, new: String) {
    if !db.uses_files() {
        if let Some(system_id) = db.system_id(system) {
            db.send(Change::Talkgroups { system_id, old: old.to_string(), new });
        }
    } else if new.trim().is_empty() {
        saver.remove(talkgroups_file(&crate::paths::data_dir(), system));
    } else {
        saver.text(talkgroups_file(&crate::paths::data_dir(), system), new);
    }
}

/// A system's radio registry file (`files` keeps a file per system).
fn registry_path(short_name: &str) -> PathBuf {
    crate::paths::data_dir().join("radio").join(format!("{}.json", trunk_core::metrics::key_part(short_name)))
}

/// Keep a system's whole radio registry (turned into JSON by the writer: it
/// can be megabytes).
pub fn write_registry(db: &Db, saver: &Saver, system: String, radio: SystemRadio) {
    if db.uses_files() {
        saver.later(registry_path(&system), move || serde_json::to_string(&radio).ok());
    } else if let Some(system_id) = db.system_id(&system) {
        db.send(Change::RegistryWhole { system_id, radio: Box::new(radio) });
    }
}

/// A system's stored registry: its whole copy with the changes since made
/// over it (none: nothing whole or a change in it could be read).
pub(crate) fn from_stored(s: &crate::db::store::StoredRegistry) -> Option<SystemRadio> {
    let changes: Vec<RegistryChanges> = s.changes.iter().filter_map(|c| serde_json::from_str(c).ok()).collect();
    let mut radio = match &s.whole {
        Some(json) => serde_json::from_str(json).ok()?,
        None => SystemRadio::from_changes(changes.first()?),
    };
    for c in &changes {
        radio.apply(c);
    }
    Some(radio)
}

/// Read the radio registry kept by earlier runs into `shared`. With the
/// store on, it's kept as changes from here on
/// ([`trunk_app::Session::keep_registry_as_changes`]); a system with many
/// since its last whole copy (a recorder restarted more often than a day)
/// is written whole now.
pub fn load_registry(db: &Db, shared: &trunk_app::stats::Shared) {
    load_registry_in(db, &crate::paths::data_dir().join("radio"), shared)
}

fn load_registry_in(db: &Db, radio_dir: &Path, shared: &trunk_app::stats::Shared) {
    let mut r = shared.radio.lock().unwrap();
    if db.uses_files() {
        for p in fs::read_dir(radio_dir).into_iter().flatten().flatten().map(|e| e.path()) {
            if let (Some("json"), Some(name), Ok(text)) = (p.extension().and_then(|x| x.to_str()), p.file_stem().and_then(|n| n.to_str()), fs::read_to_string(&p)) {
                r.load(name, &text);
            }
        }
        return;
    }
    match db.registry() {
        Ok(systems) => {
            for s in systems {
                // (A system gone from the config: its registry is left.)
                let Some(name) = db.system_name(s.system_id) else { continue };
                let Some(radio) = from_stored(&s) else {
                    log::warn!("Call store: radio registry of {name} not read");
                    continue;
                };
                if s.changes.len() >= (trunk_app::saving::REGISTRY_WHOLE_EVERY / 4) as usize {
                    db.send(Change::RegistryWhole { system_id: s.system_id, radio: Box::new(radio.clone()) });
                }
                r.put(&name, radio);
            }
        }
        Err(e) => log::warn!("Call store: radio registry not read ({e})"),
    }
    r.track_rows();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TestDir;

    /// With the store on, what's kept is the store's, and files beside it are
    /// not read; with it off, the files are.
    #[test]
    fn the_store_or_the_files_not_both() {
        let dir = TestDir::new("kept");
        fs::write(Doc::Units("a").file(&dir), "1,From a file").unwrap();
        let radio = dir.join("radio");
        fs::create_dir_all(&radio).unwrap();
        let mut reg = trunk_app::stats::radio::Registry::default();
        reg.concluded("x", &trunk_recorder_plugin::CallRecord { freq: 851_000_000, talkgroup: 300, ..Default::default() }, 1000);
        fs::write(radio.join("x.json"), serde_json::to_string(&reg.systems["x"]).unwrap()).unwrap();
        let mut cfg = trunk_app::Config::default();
        cfg.systems.push(trunk_app::config::System { short_name: "a".into(), ..Default::default() });
        fs::write(talkgroups_file(&dir, "a"), "Decimal,Alpha Tag\n101,From a file\n").unwrap();

        let off = Db::disabled();
        assert_eq!(read_in(&off, &dir, Doc::Units("a")).as_deref(), Some("1,From a file"));
        assert_eq!(load_talkgroups_in(&off, &dir, &cfg)["a"], "Decimal,Alpha Tag\n101,From a file\n");
        let shared = trunk_app::stats::Shared::new();
        load_registry_in(&off, &radio, &shared);
        assert!(shared.radio.lock().unwrap().systems.contains_key("x"));

        for db in Db::for_each_backend("kept", std::time::Duration::from_millis(10)) {
            assert_eq!(read_in(&db, &dir, Doc::Units("a")), None, "the file isn't read ({})", db.kind());
            assert!(load_talkgroups_in(&db, &dir, &cfg).is_empty());
            let shared = trunk_app::stats::Shared::new();
            load_registry_in(&db, &radio, &shared);
            assert!(shared.radio.lock().unwrap().systems.is_empty());
            db.send(Change::Kept { kind: "units".into(), key: "a".into(), value: Some("1,Engine 1".into()) });
            db.send(Change::Talkgroups { system_id: db.system_id("a").unwrap(), old: String::new(), new: "Decimal,Alpha Tag\n101,Fire\n".into() });
            db.send(Change::RegistryWhole { system_id: db.system_id("x").unwrap(), radio: Box::new(reg.systems["x"].clone()) });
            db.flush().unwrap();
            assert_eq!(read_in(&db, &dir, Doc::Units("a")).as_deref(), Some("1,Engine 1"));
            assert_eq!(load_talkgroups_in(&db, &dir, &cfg)["a"], "Decimal,Alpha Tag\n101,Fire\n");
            let shared = trunk_app::stats::Shared::new();
            load_registry_in(&db, &radio, &shared);
            assert_eq!(serde_json::to_value(&shared.radio.lock().unwrap().systems["x"]).unwrap(), serde_json::to_value(&reg.systems["x"]).unwrap(), "{}", db.kind());
        }
        assert!(Doc::Units("a").file(&dir).exists(), "files are left as they are");

        // A store asked for that couldn't open: nothing kept, the files neither read nor written.
        let failed = Db::failed("refused");
        assert_eq!(read_in(&failed, &dir, Doc::Units("a")), None);
        assert!(load_talkgroups_in(&failed, &dir, &cfg).is_empty());
        let shared = trunk_app::stats::Shared::new();
        load_registry_in(&failed, &radio, &shared);
        assert!(shared.radio.lock().unwrap().systems.is_empty());
        assert!(failed.is_disabled() && !failed.uses_files() && Db::disabled().uses_files());
    }
}
