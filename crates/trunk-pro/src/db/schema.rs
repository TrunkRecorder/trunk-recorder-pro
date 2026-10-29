//! The call store's schema, as numbered migration scripts. `migrate` brings
//! any database, new or old, up to the last one. The version is SQLite's own
//! `user_version`, which changes in the same transaction as the tables.
//!
//! Indexes follow the queries: calls by start (every page), by system and
//! start (a system's page, without scanning the busier systems' calls), by
//! talkgroup and start (a talkgroup's calls; its system is filtered on top),
//! and transmissions by radio. Each costs page writes on every commit
//! (research/storage.md); a setup that must write the least has
//! `storage.backend: "files"`. The child tables are `without rowid`: their
//! primary key is the table, not a second index beside it.

use rusqlite::Connection;

/// The version a fully migrated database reports.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// One script per version; entry `n` takes a database from version `n` to
/// `n + 1`. Never edit a shipped script: add the next one.
pub const MIGRATIONS: &[&str] = &["
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
"];

/// How a database stands after [`migrate`].
pub enum Migrated {
    Current,
    /// A newer build wrote it (this version): it's left as it is.
    Newer(u32),
}

/// Apply every migration past the database's version, all in one
/// transaction, so a failure leaves the file as it was.
pub fn migrate(conn: &Connection) -> rusqlite::Result<Migrated> {
    let have: u32 = conn.query_row("pragma user_version", [], |r| r.get(0))?;
    if have > SCHEMA_VERSION {
        return Ok(Migrated::Newer(have));
    }
    if have < SCHEMA_VERSION {
        let tx = conn.unchecked_transaction()?;
        for script in &MIGRATIONS[have as usize..] {
            tx.execute_batch(script)?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
    }
    Ok(Migrated::Current)
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
        migrate(&c).unwrap();
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
