//! What's kept between runs besides the calls (research/storage-redesign.md):
//! each system's talkgroups and talker aliases (rows of their own), band plans, the codes
//! conventional frequencies carried, AutoTune's scores, what sharing did,
//! and the radio registry. The store's, always: written by its writer, read
//! at start. A store that couldn't open keeps nothing meanwhile (the
//! interface says so). Reads block: never from async code.

use trunk_app::saving::Kind;
use trunk_app::stats::radio::RegistryChanges;
use trunk_core::trunk::{TalkgroupRow, UnitAliases};

use crate::db::{Change, Db};

/// Document `kind` (`key`: which: a site's band plan; "" for the others)
/// as last kept.
pub fn read(db: &Db, kind: Kind, key: &str) -> Option<String> {
    if db.is_disabled() {
        return None;
    }
    db.kept(kind.as_str(), key).inspect_err(|e| log::warn!("Store: {} {key} not read ({e})", kind.as_str())).ok().flatten()
}

/// Keep `text` as document `kind` (`key`: as [`read`]).
pub fn write(db: &Db, kind: Kind, key: &str, text: String) {
    db.send(Change::Kept { kind: kind.as_str().into(), key: key.into(), value: Some(text) });
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

/// Keep what changed in system `system`'s radio registry (the store keeps
/// it so: [`crate::db::store::COMPACT_AFTER`]).
pub fn write_registry(db: &Db, system: &str, changes: Box<RegistryChanges>) {
    if let Some(system_id) = db.system_id(system) {
        db.send(Change::Registry { system_id, changes });
    }
}

/// Read the radio registry kept by earlier runs into `shared`.
pub fn load_registry(db: &Db, shared: &trunk_app::stats::Shared) {
    if db.is_disabled() {
        return;
    }
    let mut r = shared.radio.lock().unwrap();
    match db.registry() {
        Ok(systems) => {
            for (system_id, radio) in systems {
                // (A system gone from the config: its registry is left.)
                let Some(name) = db.system_name(system_id) else { continue };
                match radio {
                    Some(radio) => r.put(&name, radio),
                    None => log::warn!("Store: radio registry of {name} not read"),
                }
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
        let (_, changes) = reg.take_changes().remove(0);
        let mut cfg = trunk_app::Config::default();
        cfg.systems.push(trunk_app::config::System { short_name: "a".into(), ..Default::default() });
        for db in Db::for_each_backend("kept", std::time::Duration::from_millis(10)) {
            assert_eq!(read(&db, Kind::Tune, ""), None, "{}", db.kind());
            assert_eq!(read_units(&db, "a"), None);
            write(&db, Kind::Tune, "", "{}".into());
            let units = "1,Engine 1,ota,1000,BEE00,1F7,101\n";
            write_units(&db, "a", units);
            write_talkgroups(&db, "a", trunk_core::trunk::parse_rows("Decimal,Alpha Tag\n# the fire one\n101,Fire\n102,EMS\n"));
            let mut ems = trunk_core::trunk::parse_rows("Decimal,Alpha Tag,Ignore\n102,EMS,x\n").remove(0);
            ems.comment = String::new();
            write_talkgroup(&db, "a", ems, 1);
            write_registry(&db, "x", Box::new(changes.clone()));
            db.flush().unwrap();
            assert_eq!(read(&db, Kind::Tune, "").as_deref(), Some("{}"));
            assert_eq!(read_units(&db, "a").map(|c| UnitAliases::parse_csv(&c).entries().len()), Some(1));
            assert_eq!(load_talkgroups(&db, &cfg)["a"], trunk_core::trunk::rows_csv(&trunk_core::trunk::parse_rows("Decimal,Alpha Tag,Ignore\n# the fire one\n101,Fire,\n102,EMS,x\n")), "{}", db.kind());
            let shared = trunk_app::stats::Shared::new();
            load_registry(&db, &shared);
            assert_eq!(serde_json::to_value(&shared.radio.lock().unwrap().systems["x"]).unwrap(), serde_json::to_value(&reg.systems["x"]).unwrap(), "{}", db.kind());
        }
        let off = Db::failed("refused");
        assert_eq!(read(&off, Kind::Tune, ""), None);
        assert!(load_talkgroups(&off, &cfg).is_empty());
        assert_eq!(off.failure(), Some("refused"));
    }
}
