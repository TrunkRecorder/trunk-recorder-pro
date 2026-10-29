//! What's kept between runs besides the calls (research/storage-redesign.md):
//! each system's talkgroups and talker aliases (rows of their own), band plans, the codes
//! conventional frequencies carried, AutoTune's scores, what sharing did,
//! and the radio registry. The store's, always: written by its writer, read
//! at start. A store that couldn't open keeps nothing meanwhile (the
//! interface says so). Reads block: never from async code.

use trunk_app::stats::radio::{RegistryChanges, SystemRadio};
use trunk_core::trunk::{TalkgroupRow, UnitAliases};

use crate::db::{Change, Db};

/// A document kept between runs.
pub enum Doc<'a> {
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
            Doc::Bandplan(_) => "bandplan",
            Doc::Heard => "heard",
            Doc::Tune => "tune",
            Doc::Sharing => "sharing",
        }
    }

    fn key(&self) -> &str {
        match self {
            Doc::Bandplan(k) => k,
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

/// The talkgroups of `cfg`'s systems (trunked and conventional), as kept:
/// each system's as a talkgroup file ([`trunk_core::trunk::rows_csv`]), as
/// the recording and browsers take them.
pub fn load_talkgroups(db: &Db, cfg: &trunk_app::Config) -> trunk_app::config::TalkgroupFiles {
    if db.is_disabled() {
        return Default::default();
    }
    let names = cfg.systems.iter().map(|s| &s.short_name).chain(cfg.conventional.iter().map(|c| &c.short_name));
    let read = |name: &String| db.talkgroups(name).inspect_err(|e| log::warn!("Store: talkgroups of {name} not read ({e})")).ok().filter(|rows| !rows.is_empty());
    names.filter_map(|name| read(name).map(|rows| (name.clone(), trunk_core::trunk::rows_csv(&rows)))).collect()
}

/// Keep system `system`'s talkgroups (none: it has none now), in place of
/// what's kept. Committed at once.
pub fn write_talkgroups(db: &Db, system: &str, rows: Vec<TalkgroupRow>) {
    if let Some(system_id) = db.system_id(system) {
        db.send(Change::Talkgroups { system_id, rows });
    }
}

/// Keep one talkgroup of system `system`, `order`th among them. Committed at once.
pub fn write_talkgroup(db: &Db, system: &str, row: TalkgroupRow, order: usize) {
    if let Some(system_id) = db.system_id(system) {
        db.send(Change::Talkgroup { system_id, row, order });
    }
}

/// System `system`'s talker aliases as kept, as their CSV (none: none kept).
pub fn read_units(db: &Db, system: &str) -> Option<String> {
    if db.is_disabled() {
        return None;
    }
    let aliases = db.units(system).inspect_err(|e| log::warn!("Store: talker aliases of {system} not read ({e})")).ok()?;
    (!aliases.is_empty()).then(|| UnitAliases::from_entries(aliases).to_csv())
}

/// Keep system `system`'s talker aliases, `csv` (only the rows that changed are written).
pub fn write_units(db: &Db, system: &str, csv: &str) {
    if let Some(system_id) = db.system_id(system) {
        let aliases = UnitAliases::parse_csv(csv).entries().into_iter().map(|(u, a)| (u, a.clone())).collect();
        db.send(Change::Units { system_id, aliases });
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
            assert_eq!(read(&db, Doc::Tune), None, "{}", db.kind());
            assert_eq!(read_units(&db, "a"), None);
            write(&db, Doc::Tune, "{}".into());
            let units = "1,Engine 1,ota,1000,BEE00,1F7,101\n";
            write_units(&db, "a", units);
            write_talkgroups(&db, "a", trunk_core::trunk::parse_rows("Decimal,Alpha Tag\n# the fire one\n101,Fire\n102,EMS\n"));
            let mut ems = trunk_core::trunk::parse_rows("Decimal,Alpha Tag,Ignore\n102,EMS,x\n").remove(0);
            ems.comment = String::new();
            write_talkgroup(&db, "a", ems, 1);
            write_registry(&db, "x".into(), reg.systems["x"].clone());
            db.flush().unwrap();
            assert_eq!(read(&db, Doc::Tune).as_deref(), Some("{}"));
            assert_eq!(read_units(&db, "a").map(|c| UnitAliases::parse_csv(&c).entries().len()), Some(1));
            assert_eq!(load_talkgroups(&db, &cfg)["a"], trunk_core::trunk::rows_csv(&trunk_core::trunk::parse_rows("Decimal,Alpha Tag,Ignore\n# the fire one\n101,Fire,\n102,EMS,x\n")), "{}", db.kind());
            let shared = trunk_app::stats::Shared::new();
            load_registry(&db, &shared);
            assert_eq!(serde_json::to_value(&shared.radio.lock().unwrap().systems["x"]).unwrap(), serde_json::to_value(&reg.systems["x"]).unwrap(), "{}", db.kind());
        }
        let off = Db::failed("refused");
        assert_eq!(read(&off, Doc::Tune), None);
        assert!(load_talkgroups(&off, &cfg).is_empty());
        assert_eq!(off.failure(), Some("refused"));
    }
}
