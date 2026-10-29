//! The store's schema, and its version (SQLite's own `user_version`). While
//! in development nothing is migrated: the version is raised whenever the
//! schema changes, and a store of any other version is set aside and made
//! afresh (`sqlite.rs`; PostgreSQL's tables dropped and made again).
//!
//! Indexes follow the queries, each page an index walked in order to its
//! limit (`read::Where`): calls by start (every page), by system and start
//! (a system's page), by system, talkgroup and start (a talkgroup's), and
//! `call_units`, a radio's calls by system and the call's start. Each costs page writes on every commit
//! (research/storage.md); a setup that must write the least has
//! `storage.backend: "files"`. The child tables are `without rowid`: their
//! primary key is the table, not a second index beside it.

use rusqlite::Connection;

/// This build's schema. Raise it with any change to [`SCHEMA`] (or
/// PostgreSQL's twin): a store of another version is made afresh.
pub const SCHEMA_VERSION: u32 = 3;

/// The tables, made in a new database.
pub const SCHEMA: &str = "
create table meta (key text primary key, value text not null) without rowid;
create table calls (
    id integer primary key,
    path text not null unique,
    short_name text not null,
    nac text,
    site text,
    talkgroup integer not null,
    start_ms integer not null,
    stop_ms integer not null,
    length_ms integer not null,
    freq_hz integer not null,
    emergency integer not null,
    encrypted integer not null,
    decrypted integer not null,
    duplex integer not null,
    priority integer,
    audio_type text,
    signal real,
    noise real,
    snr real,
    clean_voice_pct real,
    audio integer not null,
    json integer not null,
    recorder text,
    record text not null,
    indexed_ms integer not null
);
create table call_transmissions (
    call_id integer not null references calls(id) on delete cascade,
    seq integer not null,
    unit integer not null,
    start_ms integer not null,
    length_ms integer not null,
    emergency integer not null,
    signal_system text,
    tag text,
    primary key (call_id, seq)
) without rowid;
create table call_frequencies (
    call_id integer not null references calls(id) on delete cascade,
    seq integer not null,
    freq_hz integer not null,
    start_ms integer not null,
    length_ms integer not null,
    error_count integer not null,
    spike_count integer not null,
    primary key (call_id, seq)
) without rowid;
create index calls_start on calls(start_ms);
create index calls_system_start on calls(short_name, start_ms);
create index calls_system_tg_start on calls(short_name, talkgroup, start_ms);
create table call_units (
    short_name text not null,
    unit integer not null,
    start_ms integer not null,
    call_id integer not null references calls(id) on delete cascade,
    primary key (short_name, unit, start_ms, call_id)
) without rowid;
create table kept (kind text not null, key text not null, value text not null, primary key (kind, key)) without rowid;
create table registry (system text primary key, whole text not null);
create table registry_changes (id integer primary key, system text not null, changes text not null);
create table talkgroups (
    system text not null,
    seq integer not null,
    line text not null,
    number integer,
    alpha_tag text,
    description text,
    tag text,
    category text,
    mode text,
    priority integer,
    ignore integer,
    primary key (system, seq)
) without rowid;
create index talkgroups_number on talkgroups(system, number);
create table stats_minutes (id integer primary key, t integer not null, s text not null);
create index stats_minutes_t on stats_minutes(t);
create table stats_hours (
    series text not null,
    t integer not null,
    avg real not null,
    lo real not null,
    hi real not null,
    n integer not null,
    primary key (series, t)
) without rowid;
";

/// How a database stands after [`prepare`].
pub enum Prepared {
    /// This build's schema (made now, if the database was new).
    Current,
    /// Another version's (older or newer): for the caller to set aside.
    Other(u32),
}

/// A new database (no version, nothing in it) made this schema, in one
/// transaction; one of this version left as it is.
pub fn prepare(conn: &Connection) -> rusqlite::Result<Prepared> {
    let have: u32 = conn.query_row("pragma user_version", [], |r| r.get(0))?;
    if have == SCHEMA_VERSION {
        return Ok(Prepared::Current);
    }
    let tables: i64 = conn.query_row("select count(*) from sqlite_master", [], |r| r.get(0))?;
    if have != 0 || tables > 0 {
        return Ok(Prepared::Other(have));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(Prepared::Current)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every page the query builder makes walks an index in order to its
    /// limit, never sorting all the calls it matches: every call, a system,
    /// several, a talkgroup, a radio, and the paging cursor's parts.
    #[test]
    fn every_page_walks_an_index_without_sorting() {
        use crate::db::read::{CallsFilter, Where, COLUMNS};
        let c = Connection::open_in_memory().unwrap();
        prepare(&c).unwrap();
        let plan = |f: CallsFilter| -> String {
            let (sql, ps) = Where::of(&f).query(COLUMNS, 101, false);
            let ps: Vec<rusqlite::types::Value> = ps.into_iter().map(|p| match p {
                crate::db::read::Param::Int(i) => i.into(),
                crate::db::read::Param::Text(t) => t.into(),
            }).collect();
            let mut st = c.prepare(&format!("explain query plan {sql}")).unwrap();
            let rows: Vec<String> = st.query_map(rusqlite::params_from_iter(ps), |r| r.get::<_, String>(3)).unwrap().map(Result::unwrap).collect();
            rows.join(" | ")
        };
        let f = |systems: &[&str], tg: Option<u32>, unit: Option<i64>| CallsFilter { systems: systems.iter().map(|s| s.to_string()).collect(), talkgroup: tg, unit, before_ms: Some(5), limit: 100, ..Default::default() };
        for (what, filter, index) in [
            ("every call", f(&[], None, None), "calls_start"),
            ("a system", f(&["a"], None, None), "calls_system_start"),
            ("two systems", f(&["a", "b"], None, None), "calls_system_start"),
            ("a talkgroup", f(&["a"], Some(1), None), "calls_system_tg_start"),
            ("a talkgroup on two", f(&["a", "b"], Some(1), None), "calls_system_tg_start"),
            // (call_units, as `u`: its key is system, radio, start.)
            ("a radio", f(&["a"], None, Some(7)), "SEARCH u USING PRIMARY KEY (short_name=? AND unit=? AND start_ms<?)"),
            ("a radio on two", f(&["a", "b"], None, Some(7)), "SEARCH u USING PRIMARY KEY (short_name=? AND unit=? AND start_ms<?)"),
        ] {
            let p = plan(filter);
            assert!(p.contains(index), "{what}: {p}");
            // A sort only ever of a system's part once it's cut to the limit (the union
            // merging them), right after that part's scan: never of the calls matched.
            let steps: Vec<&str> = p.split(" | ").collect();
            for (i, s) in steps.iter().enumerate() {
                if s.contains("TEMP B-TREE") {
                    assert!(i > 0 && steps[i - 1].starts_with("SCAN a"), "{what} sorts what it matched: {p}");
                }
            }
        }
    }
}
