//! The call store's schema, as numbered migration scripts. `migrate` brings
//! any database, new or old, up to the last one.

use rusqlite::Connection;

/// The version a fully migrated database reports.
pub const SCHEMA_VERSION: u32 = 1;

/// One script per version; entry `n` takes a database from version `n` to
/// `n + 1`. Never edit a shipped script: add the next one.
pub const MIGRATIONS: &[&str] = &["
create table schema_version (version integer not null);
create table meta (key text primary key, value text not null);
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
);
create table call_frequencies (
    call_id integer not null references calls(id) on delete cascade,
    seq integer not null,
    freq_hz integer not null,
    start_ms integer not null,
    length_ms integer not null,
    error_count integer not null,
    spike_count integer not null,
    primary key (call_id, seq)
);
create index calls_start on calls(start_ms);
create index calls_system_tg_start on calls(short_name, talkgroup, start_ms);
create index calls_tg_start on calls(talkgroup, start_ms);
create index tx_unit_start on call_transmissions(unit, start_ms);
"];

/// Apply every migration past the database's version, all in one
/// transaction, so a failure leaves the file as it was.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let have: usize = conn
        .query_row("select version from schema_version", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|v| v as usize)
        .unwrap_or(0);
    if have >= MIGRATIONS.len() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for script in &MIGRATIONS[have..] {
        tx.execute_batch(script)?;
    }
    tx.execute("delete from schema_version", [])?;
    tx.execute(
        "insert into schema_version (version) values (?1)",
        [MIGRATIONS.len() as i64],
    )?;
    tx.commit()
}
