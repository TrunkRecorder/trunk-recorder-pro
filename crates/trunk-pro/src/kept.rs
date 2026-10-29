//! What's kept between runs besides the calls (research/storage-redesign.md):
//! each system's talkgroup file, talker aliases, band plans, the codes
//! conventional frequencies carried, AutoTune's scores, what sharing did,
//! and the radio registry. The store's, always: written by its writer, read
//! at start. A store that couldn't open keeps nothing meanwhile (the
//! interface says so). Reads block: never from async code.

use trunk_app::stats::radio::{RegistryChanges, SystemRadio};

use crate::db::{Change, Db};

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
}

/// `doc` as last kept.
pub fn read(db: &Db, doc: Doc) -> Option<String> {
    if db.is_disabled() {
        return None;
    }
    db.kept(doc.kind(), doc.key()).inspect_err(|e| log::warn!("Store: {} {} not read ({e})", doc.kind(), doc.key())).ok().flatten()
}

/// Keep `text` as `doc`.
pub fn write(db: &Db, doc: Doc, text: String) {
    db.send(Change::Kept { kind: doc.kind().into(), key: doc.key().into(), value: Some(text) });
}

/// The talkgroup files of `cfg`'s systems (trunked and conventional), as kept.
pub fn load_talkgroups(db: &Db, cfg: &trunk_app::Config) -> trunk_app::config::TalkgroupFiles {
    if db.is_disabled() {
        return Default::default();
    }
    let names = cfg.systems.iter().map(|s| &s.short_name).chain(cfg.conventional.iter().map(|c| &c.short_name));
    let read = |name: &String| db.talkgroups(name).inspect_err(|e| log::warn!("Store: talkgroups of {name} not read ({e})")).ok().flatten();
    names.filter_map(|name| read(name).map(|csv| (name.clone(), csv))).collect()
}

/// Keep system `system`'s talkgroup file, edited from `old` to `new` (""
/// for `old`: in place of what's kept; "" for `new`: none). The store
/// writes only the lines that changed, and commits at once.
pub fn write_talkgroups(db: &Db, system: &str, old: &str, new: String) {
    if let Some(system_id) = db.system_id(system) {
        db.send(Change::Talkgroups { system_id, old: old.to_string(), new });
    }
}

/// Keep a system's whole radio registry (turned into JSON by the writer: it
/// can be megabytes).
pub fn write_registry(db: &Db, system: String, radio: SystemRadio) {
    if let Some(system_id) = db.system_id(&system) {
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

/// Read the radio registry kept by earlier runs into `shared`; it's kept as
/// changes from here on ([`trunk_app::Session::keep_registry_as_changes`]).
/// A system with many since its last whole copy (a recorder restarted more
/// often than a day) is written whole now.
pub fn load_registry(db: &Db, shared: &trunk_app::stats::Shared) {
    let mut r = shared.radio.lock().unwrap();
    r.track_rows();
    if db.is_disabled() {
        return;
    }
    match db.registry() {
        Ok(systems) => {
            for s in systems {
                // (A system gone from the config: its registry is left.)
                let Some(name) = db.system_name(s.system_id) else { continue };
                let Some(radio) = from_stored(&s) else {
                    log::warn!("Store: radio registry of {name} not read");
                    continue;
                };
                if s.changes.len() >= (trunk_app::saving::REGISTRY_WHOLE_EVERY / 4) as usize {
                    db.send(Change::RegistryWhole { system_id: s.system_id, radio: Box::new(radio.clone()) });
                }
                r.put(&name, radio);
            }
        }
        Err(e) => log::warn!("Store: radio registry not read ({e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What's kept comes back from the store; with none (it couldn't open),
    /// nothing is read and nothing kept.
    #[test]
    fn kept_by_the_store_alone() {
        let mut reg = trunk_app::stats::radio::Registry::default();
        reg.concluded("x", &trunk_recorder_plugin::CallRecord { freq: 851_000_000, talkgroup: 300, ..Default::default() }, 1000);
        let mut cfg = trunk_app::Config::default();
        cfg.systems.push(trunk_app::config::System { short_name: "a".into(), ..Default::default() });
        for db in Db::for_each_backend("kept", std::time::Duration::from_millis(10)) {
            assert_eq!(read(&db, Doc::Units("a")), None, "{}", db.kind());
            write(&db, Doc::Units("a"), "1,Engine 1".into());
            write_talkgroups(&db, "a", "", "Decimal,Alpha Tag\n101,Fire\n".into());
            write_registry(&db, "x".into(), reg.systems["x"].clone());
            db.flush().unwrap();
            assert_eq!(read(&db, Doc::Units("a")).as_deref(), Some("1,Engine 1"));
            assert_eq!(load_talkgroups(&db, &cfg)["a"], "Decimal,Alpha Tag\n101,Fire\n");
            let shared = trunk_app::stats::Shared::new();
            load_registry(&db, &shared);
            assert_eq!(serde_json::to_value(&shared.radio.lock().unwrap().systems["x"]).unwrap(), serde_json::to_value(&reg.systems["x"]).unwrap(), "{}", db.kind());
        }
        let off = Db::failed("refused");
        assert_eq!(read(&off, Doc::Units("a")), None);
        assert!(load_talkgroups(&off, &cfg).is_empty());
        assert_eq!(off.failure(), Some("refused"));
    }
}
