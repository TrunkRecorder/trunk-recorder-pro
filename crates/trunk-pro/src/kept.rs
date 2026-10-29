//! What a recording keeps between runs besides its calls (research/sqlite-plan.md,
//! "Flat files"): each system's talker aliases, band plans, the codes
//! conventional frequencies carried, AutoTune's scores, what sharing did,
//! and the radio registry. With `storage.backend: "files"` (or a store that
//! could not open) they're the files they always were, written by the
//! saver; otherwise they're rows in the store, written by its writer.
//!
//! A file found while the store is on was written while it was off (or by a
//! release before it): it's newer than the store's copy, so it's imported
//! and renamed `<name>.imported`. A store that can't be read falls back on
//! that last copy.

use std::fs;
use std::path::{Path, PathBuf};

use trunk_app::stats::radio::{RegistryChanges, SystemRadio};

use crate::db::{Db, Write};
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

/// `path` once imported.
fn imported(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".imported");
    PathBuf::from(s)
}

/// Rename `path` once the store has committed what was read from it.
fn retire_file(db: &Db, path: &Path) {
    match db.flush() {
        Ok(()) => {
            if let Err(e) = fs::rename(path, imported(path)) {
                log::warn!("{}: imported into the store, but not renamed: {e}", path.display());
            } else {
                log::info!("{}: imported into the store", path.display());
            }
        }
        Err(e) => log::warn!("{}: not imported into the store ({e}); read again next start", path.display()),
    }
}

/// `doc` as last kept. Blocks: never from async code.
pub fn read(db: &Db, doc: Doc) -> Option<String> {
    read_in(db, &crate::paths::data_dir(), doc)
}

fn read_in(db: &Db, dir: &Path, doc: Doc) -> Option<String> {
    let path = doc.file(dir);
    if db.is_disabled() {
        return fs::read_to_string(&path).or_else(|_| fs::read_to_string(imported(&path))).ok();
    }
    if let Ok(text) = fs::read_to_string(&path) {
        db.send(Write::Kept { kind: doc.kind().into(), key: doc.key().into(), value: Some(text.clone()) });
        retire_file(db, &path);
        return Some(text);
    }
    match db.kept(doc.kind(), doc.key()) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Call store: {} {} not read ({e}); using its last file", doc.kind(), doc.key());
            fs::read_to_string(imported(&path)).ok()
        }
    }
}

/// Keep `text` as `doc`.
pub fn write(db: &Db, saver: &Saver, doc: Doc, text: String) {
    if db.is_disabled() {
        saver.text(doc.file(&crate::paths::data_dir()), text);
    } else {
        db.send(Write::Kept { kind: doc.kind().into(), key: doc.key().into(), value: Some(text) });
    }
}

/// A system's radio registry file (`files` keeps a file per system).
fn registry_path(short_name: &str) -> PathBuf {
    crate::paths::data_dir().join("radio").join(format!("{}.json", trunk_core::metrics::key_part(short_name)))
}

/// Keep a system's whole radio registry (turned into JSON by the writer: it
/// can be megabytes).
pub fn write_registry(db: &Db, saver: &Saver, system: String, radio: SystemRadio) {
    if db.is_disabled() {
        saver.later(registry_path(&system), move || serde_json::to_string(&radio).ok());
    } else {
        db.send(Write::RegistryWhole { system, radio: Box::new(radio) });
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
    let files = |also_imported: bool| {
        fs::read_dir(radio_dir).into_iter().flatten().flatten().map(|e| e.path()).filter(move |p| p.extension().is_some_and(|x| x == "json" || (also_imported && x == "imported")))
    };
    let stem = |p: &Path| -> Option<String> {
        let name = p.file_name()?.to_str()?;
        Some(name.strip_suffix(".imported").unwrap_or(name).strip_suffix(".json")?.to_string())
    };
    // Each file, an imported one first (a plain one is newer).
    let load_files = |r: &mut trunk_app::stats::radio::Registry| {
        let mut paths: Vec<PathBuf> = files(true).collect();
        paths.sort_by_key(|p| p.extension().is_some_and(|x| x == "json"));
        for p in paths {
            if let (Some(name), Ok(text)) = (stem(&p), fs::read_to_string(&p)) {
                r.load(&name, &text);
            }
        }
    };
    if db.is_disabled() {
        load_files(&mut r);
        return;
    }
    match db.registry() {
        Ok(systems) => {
            for s in systems {
                let Some(radio) = from_stored(&s) else {
                    log::warn!("Call store: radio registry {} not read", s.system);
                    continue;
                };
                if s.changes.len() >= (trunk_app::saving::REGISTRY_WHOLE_EVERY / 4) as usize {
                    db.send(Write::RegistryWhole { system: s.system.clone(), radio: Box::new(radio.clone()) });
                }
                r.put(&s.system, radio);
            }
        }
        Err(e) => {
            // (Kept by row from its last files: what changes is written as ever.)
            log::warn!("Call store: radio registry not read ({e}); using its last files");
            load_files(&mut r);
            r.track_rows();
            return;
        }
    }
    r.track_rows();
    for p in files(false).collect::<Vec<_>>() {
        let (Some(name), Ok(text)) = (stem(&p), fs::read_to_string(&p)) else { continue };
        r.load(&name, &text);
        let Some(radio) = r.systems.get(&name).cloned() else { continue };
        db.send(Write::RegistryWhole { system: name, radio: Box::new(radio) });
        retire_file(db, &p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TestDir;

    /// With the store on, a file is imported once (it was written while the
    /// store was off) and renamed; with it off, the files are read as ever,
    /// an imported one as the last copy.
    #[test]
    fn files_are_imported_once() {
        let dir = TestDir::new("kept-import");
        let units = Doc::Units("a").file(&dir);
        fs::write(&units, "1,Engine 1").unwrap();
        let radio = dir.join("radio");
        fs::create_dir_all(&radio).unwrap();
        let mut whole = trunk_app::stats::radio::Registry::default();
        whole.concluded("x", &trunk_recorder_plugin::CallRecord { freq: 851_000_000, talkgroup: 300, ..Default::default() }, 1000);
        let json = serde_json::to_string(&whole.systems["x"]).unwrap();

        let off = Db::disabled();
        assert_eq!(read_in(&off, &dir, Doc::Units("a")).as_deref(), Some("1,Engine 1"));
        assert!(units.exists(), "off: left as it is");

        for db in Db::for_each_backend("kept-import", std::time::Duration::from_millis(10)) {
            fs::write(&units, "1,Engine 1").unwrap();
            fs::write(radio.join("x.json"), &json).unwrap();
            assert_eq!(read_in(&db, &dir, Doc::Units("a")).as_deref(), Some("1,Engine 1"));
            assert!(!units.exists() && imported(&units).exists(), "renamed once imported ({})", db.kind());
            assert_eq!(read_in(&db, &dir, Doc::Units("a")).as_deref(), Some("1,Engine 1"), "from the store");
            assert_eq!(read_in(&db, &dir, Doc::Tune), None);

            let shared = trunk_app::stats::Shared::new();
            load_registry_in(&db, &radio, &shared);
            assert!(!radio.join("x.json").exists());
            let again = trunk_app::stats::Shared::new();
            load_registry_in(&db, &radio, &again);
            let (a, b) = (shared.radio.lock().unwrap(), again.radio.lock().unwrap());
            assert_eq!(serde_json::to_value(&b.systems["x"]).unwrap(), serde_json::to_value(&whole.systems["x"]).unwrap(), "{}", db.kind());
            assert_eq!(serde_json::to_value(&a.systems["x"]).unwrap(), serde_json::to_value(&b.systems["x"]).unwrap());
        }

        // Off again: the imported copies are the last.
        let shared = trunk_app::stats::Shared::new();
        load_registry_in(&off, &radio, &shared);
        assert!(shared.radio.lock().unwrap().systems.contains_key("x"));
        assert_eq!(read_in(&off, &dir, Doc::Units("a")).as_deref(), Some("1,Engine 1"));
    }
}
