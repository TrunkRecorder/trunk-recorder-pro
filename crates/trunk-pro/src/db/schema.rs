//! The store's schema, and its version (SQLite's own `user_version`). While
//! in development nothing is migrated: the version is raised whenever the
//! schema changes, and a store of any other version is set aside and made
//! afresh (`sqlite.rs`; PostgreSQL's tables dropped and made again).
//!
//! Indexes follow the queries: calls by start (every page), by system and
//! start (a system's page, without scanning the busier systems' calls), by
//! talkgroup and start (a talkgroup's calls; its system is filtered on top),
//! and transmissions by radio. Each costs page writes on every commit
//! (research/storage.md); a setup that must write the least has
//! `storage.backend: "files"`. The child tables are `without rowid`: their
//! primary key is the table, not a second index beside it.

use rusqlite::Connection;

/// This build's schema. Raise it with any change to [`SCHEMA`] (or
/// PostgreSQL's twin): a store of another version is made afresh.
pub const SCHEMA_VERSION: u32 = 2;

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
create index calls_tg_start on calls(talkgroup, start_ms);
create index tx_unit_start on call_transmissions(unit, start_ms);
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

    /// The queries' plans use the indexes the schema keeps: a page of every
    /// call, of a talkgroup (in a system), and of a radio, without sorting
    /// the whole table.
    #[test]
    fn the_queries_use_the_indexes() {
        let c = Connection::open_in_memory().unwrap();
        prepare(&c).unwrap();
        let plan = |sql: &str| -> String {
            let mut st = c.prepare(&format!("explain query plan {sql}")).unwrap();
            let rows: Vec<String> = st.query_map([], |r| r.get::<_, String>(3)).unwrap().map(Result::unwrap).collect();
            rows.join(" | ")
        };
        let order = "order by start_ms desc, id desc limit 101";
        assert!(plan(&format!("select path from calls where start_ms < 5 {order}")).contains("calls_start"));
        let tg = plan(&format!("select path from calls where short_name in ('a') and talkgroup = 1 {order}"));
        assert!(tg.contains("calls_tg_start"), "{tg}");
        let unit = plan("select call_id from call_transmissions where unit = 7");
        assert!(unit.contains("tx_unit_start"), "{unit}");
        let sys = plan(&format!("select path from calls where short_name in ('a') {order}"));
        assert!(sys.contains("calls_system_start") && !sys.contains("TEMP B-TREE"), "a system's page walks its own index: {sys}");
    }
}
