//! The PostgreSQL backend, for servers: years of history, and the operator's
//! backups, replication and point-in-time recovery. `storage.url` names the
//! database (`postgres://user:password@host/db`, or `host=… dbname=…`).
//!
//! The same shapes as SQLite's (`schema.rs`), in PostgreSQL's dialect: a
//! call's record is `jsonb`, so the database can be queried by it too. One
//! recorder per database (or schema) for now: a session advisory lock plays
//! the lock file's part, so a second recorder pointed at it runs with its
//! store off. Connections that drop (a server restart) are opened again on
//! the next use. Plain TCP; TLS is a later step.

use std::sync::Mutex;

use postgres::types::ToSql;
use postgres::{Client, NoTls};
use serde_json::Value;

use super::read::{self, CallsFilter, Fetched, Page, Param, Where};
use super::row::{entry, CallRow};
use super::store::{CallStore, Change, StoreInfo, StoredRegistry};

/// Read connections kept.
const READERS: usize = 2;

/// The columns a call entry is made from (the record as text), then `id` and
/// `start_ms` for paging.
const COLUMNS: &str = "path, record::text, audio, json, id, start_ms";

/// One script per version; entry `n` takes a database from version `n` to
/// `n + 1`. Never edit a shipped script: add the next one. (The SQLite
/// schema's twin: keep them in step.)
const MIGRATIONS: &[&str] = &["
create table meta (key text primary key, value text not null);
create table calls (
    id bigserial primary key,
    path text not null unique,
    short_name text not null,
    nac text,
    site text,
    talkgroup bigint not null,
    start_ms bigint not null,
    stop_ms bigint not null,
    length_ms bigint not null,
    freq_hz bigint not null,
    emergency boolean not null,
    encrypted boolean not null,
    decrypted boolean not null,
    duplex boolean not null,
    priority bigint,
    audio_type text,
    signal double precision,
    noise double precision,
    snr double precision,
    clean_voice_pct double precision,
    audio boolean not null,
    json boolean not null,
    recorder text,
    record jsonb not null,
    indexed_ms bigint not null
);
create table call_transmissions (
    call_id bigint not null references calls(id) on delete cascade,
    seq bigint not null,
    unit bigint not null,
    start_ms bigint not null,
    length_ms bigint not null,
    emergency boolean not null,
    signal_system text,
    tag text,
    primary key (call_id, seq)
);
create table call_frequencies (
    call_id bigint not null references calls(id) on delete cascade,
    seq bigint not null,
    freq_hz bigint not null,
    start_ms bigint not null,
    length_ms bigint not null,
    error_count bigint not null,
    spike_count bigint not null,
    primary key (call_id, seq)
);
create index calls_start on calls(start_ms);
create index calls_system_start on calls(short_name, start_ms);
create index calls_tg_start on calls(talkgroup, start_ms);
create index tx_unit_start on call_transmissions(unit, start_ms);
create table kept (kind text not null, key text not null, value text not null, primary key (kind, key));
create table registry (system text primary key, whole text not null);
create table registry_changes (id bigserial primary key, system text not null, changes text not null);
"];

pub struct PostgresStore {
    url: String,
    /// The schema it lives in (tests: one each); None: the connection's own.
    schema: Option<String>,
    /// The database, said without its password.
    location: String,
    writer: Mutex<Client>,
    readers: Mutex<Vec<Client>>,
}

impl PostgresStore {
    /// Connect to the database `url` names, take it for this recorder, and
    /// migrate it.
    pub fn open(url: &str) -> Result<PostgresStore, String> {
        Self::open_in(url, None)
    }

    /// A fresh store in schema `schema` of `url`'s database, emptied first
    /// (tests: each its own).
    #[cfg(test)]
    pub fn open_fresh_schema(url: &str, schema: &str) -> Result<PostgresStore, String> {
        let mut c = Client::connect(url, NoTls).map_err(|e| e.to_string())?;
        c.batch_execute(&format!("drop schema if exists {schema} cascade; create schema {schema}")).map_err(|e| e.to_string())?;
        Self::open_in(url, Some(schema.to_string()))
    }

    fn open_in(url: &str, schema: Option<String>) -> Result<PostgresStore, String> {
        if url.trim().is_empty() {
            return Err("storage.url is empty: say which PostgreSQL database".into());
        }
        let location = redacted(url);
        let mut writer = connect(url, schema.as_deref()).map_err(|e| format!("{location}: {e}"))?;
        take(&mut writer).map_err(|e| format!("{location}: {e}"))?;
        migrate(&mut writer).map_err(|e| format!("{location}: {e}"))?;
        let readers = (0..READERS).map(|_| connect(url, schema.as_deref())).collect::<Result<Vec<_>, _>>().map_err(|e| format!("{location}: {e}"))?;
        Ok(PostgresStore { url: url.to_string(), schema, location, writer: Mutex::new(writer), readers: Mutex::new(readers) })
    }

    /// The writer's connection, opened again (and the database taken again)
    /// if the server dropped it.
    fn writer(&self) -> Result<std::sync::MutexGuard<'_, Client>, String> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if w.is_closed() {
            log::warn!("Call store: {}: the connection dropped; connecting again", self.location);
            let mut c = connect(&self.url, self.schema.as_deref()).map_err(|e| e.to_string())?;
            take(&mut c)?;
            *w = c;
        }
        Ok(w)
    }

    /// Run `f` on a read connection from the pool (a new one when all are
    /// out, or the one taken had dropped).
    fn read<T>(&self, f: impl FnOnce(&mut Client) -> Result<T, postgres::Error>) -> Result<T, String> {
        let pooled = self.readers.lock().unwrap_or_else(|e| e.into_inner()).pop().filter(|c| !c.is_closed());
        let mut c = match pooled {
            Some(c) => c,
            None => connect(&self.url, self.schema.as_deref()).map_err(|e| e.to_string())?,
        };
        let out = f(&mut c).map_err(|e| e.to_string());
        let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if !c.is_closed() && pool.len() < READERS {
            pool.push(c);
        }
        out
    }
}

fn connect(url: &str, schema: Option<&str>) -> Result<Client, postgres::Error> {
    let mut c = Client::connect(url, NoTls)?;
    if let Some(s) = schema {
        c.batch_execute(&format!("set search_path to {s}"))?;
    }
    Ok(c)
}

/// Take the database (its schema) for this recorder: a session advisory
/// lock, let go when the connection closes.
fn take(c: &mut Client) -> Result<(), String> {
    let got: bool = c.query_one("select pg_try_advisory_lock(hashtext('trunk-pro:' || current_schema()))", &[]).map_err(|e| e.to_string())?.get(0);
    if got { Ok(()) } else { Err("another Trunk Recorder Pro is using this database".into()) }
}

/// Bring the schema up to the last migration, in one transaction. A
/// database a newer build wrote is refused.
fn migrate(c: &mut Client) -> Result<(), String> {
    let mut tx = c.transaction().map_err(|e| e.to_string())?;
    tx.batch_execute("create table if not exists schema_version (version integer not null)").map_err(|e| e.to_string())?;
    let have: i32 = tx.query_opt("select version from schema_version", &[]).map_err(|e| e.to_string())?.map_or(0, |r| r.get(0));
    let want = MIGRATIONS.len() as i32;
    if have > want {
        return Err(format!("its schema is version {have}, newer than this build's {want} (written by a newer Trunk Recorder Pro)"));
    }
    for script in &MIGRATIONS[have as usize..] {
        tx.batch_execute(script).map_err(|e| e.to_string())?;
    }
    tx.execute("delete from schema_version", &[]).map_err(|e| e.to_string())?;
    tx.execute("insert into schema_version (version) values ($1)", &[&want]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// `url` without its password, to show and log.
fn redacted(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        let rest = match rest.split_once('@') {
            Some((user, host)) => format!("{}@{host}", user.split(':').next().unwrap_or_default()),
            None => rest.to_string(),
        };
        return format!("{scheme}://{rest}");
    }
    url.split_whitespace().filter(|kv| !kv.starts_with("password=")).collect::<Vec<_>>().join(" ")
}

fn boxed(p: &Param) -> Box<dyn ToSql + Sync> {
    match p {
        Param::Int(i) => Box::new(*i),
        Param::Text(t) => Box::new(t.clone()),
    }
}

impl CallStore for PostgresStore {
    fn kind(&self) -> &'static str {
        "postgres"
    }

    fn location(&self) -> String {
        self.location.clone()
    }

    fn apply(&self, batch: Vec<Change>) -> Result<(), String> {
        let mut w = self.writer()?;
        let mut tx = w.transaction().map_err(|e| format!("could not begin a transaction: {e}"))?;
        for change in batch {
            match change {
                // (A call with its transmissions and frequencies, or none of it.)
                Change::Call { row, live } => {
                    let written = tx.transaction().and_then(|mut sp| {
                        write_call(&mut sp, &row, live)?;
                        sp.commit()
                    });
                    if let Err(e) = written {
                        log::warn!("Call store: call write {} failed: {e}", row.path);
                    }
                }
                Change::CallFiles { path, audio, json } => {
                    if let Err(e) = tx.execute("update calls set audio = $2, json = $3 where path = $1", &[&path, &audio, &json]) {
                        log::warn!("Call store: files write {path} failed: {e}");
                    }
                }
                Change::Meta { key, value } => {
                    if let Err(e) = tx.execute("insert into meta (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", &[&key, &value]) {
                        log::warn!("Call store: meta write {key} failed: {e}");
                    }
                }
                Change::Kept { kind, key, value } => {
                    let r = match &value {
                        Some(v) => tx.execute("insert into kept (kind, key, value) values ($1, $2, $3) on conflict (kind, key) do update set value = excluded.value", &[&kind, &key, v]),
                        None => tx.execute("delete from kept where kind = $1 and key = $2", &[&kind, &key]),
                    };
                    if let Err(e) = r {
                        log::warn!("Call store: {kind} {key} write failed: {e}");
                    }
                }
                Change::RegistryChanges { system, json } => {
                    if let Err(e) = tx.execute("insert into registry_changes (system, changes) values ($1, $2)", &[&system, &json]) {
                        log::warn!("Call store: registry write {system} failed: {e}");
                    }
                }
                Change::RegistryWhole { system, json } => {
                    let written = tx.transaction().and_then(|mut sp| {
                        sp.execute("insert into registry (system, whole) values ($1, $2) on conflict (system) do update set whole = excluded.whole", &[&system, &json])?;
                        sp.execute("delete from registry_changes where system = $1", &[&system])?;
                        sp.commit()
                    });
                    if let Err(e) = written {
                        log::warn!("Call store: registry write {system} failed: {e}");
                    }
                }
            }
        }
        tx.commit().map_err(|e| format!("commit failed: {e}"))
    }

    fn page(&self, f: &CallsFilter) -> Result<Page, String> {
        self.read(|c| {
            let mut fetch = |w: &Where, limit: usize| -> Result<(Vec<Fetched>, bool), String> {
                let want = i64::try_from(limit).unwrap_or(i64::MAX - 1);
                let mut ps: Vec<Box<dyn ToSql + Sync>> = w.params.iter().map(boxed).collect();
                ps.push(Box::new(want.saturating_add(1)));
                let refs: Vec<&(dyn ToSql + Sync)> = ps.iter().map(|p| &**p).collect();
                let rows = c.query(&w.select(COLUMNS, true), &refs).map_err(|e| e.to_string())?;
                let extra = rows.len() as i64 > want;
                let out = rows
                    .iter()
                    .take(want as usize)
                    .map(|r| Fetched { entry: entry(r.get::<_, &str>(0), r.get::<_, &str>(1), r.get(2), r.get(3)), id: r.get(4), start_ms: r.get(5) })
                    .collect();
                Ok((out, extra))
            };
            Ok(read::paged(f, &mut fetch))
        })?
    }

    fn one(&self, path: &str) -> Result<Option<Value>, String> {
        self.read(|c| Ok(c.query_opt("select path, record::text, audio, json from calls where path = $1", &[&path])?.map(|r| entry(r.get::<_, &str>(0), r.get::<_, &str>(1), r.get(2), r.get(3)))))
    }

    fn known(&self, path: &str) -> Result<bool, String> {
        self.read(|c| Ok(c.query_opt("select 1 from calls where path = $1", &[&path])?.is_some()))
    }

    fn meta(&self, key: &str) -> Result<Option<String>, String> {
        self.read(|c| Ok(c.query_opt("select value from meta where key = $1", &[&key])?.map(|r| r.get(0))))
    }

    fn kept(&self, kind: &str, key: &str) -> Result<Option<String>, String> {
        self.read(|c| Ok(c.query_opt("select value from kept where kind = $1 and key = $2", &[&kind, &key])?.map(|r| r.get(0))))
    }

    fn registry(&self) -> Result<Vec<StoredRegistry>, String> {
        self.read(|c| {
            let whole = c.query("select system, whole from registry", &[])?.iter().map(|r| (r.get(0), r.get(1))).collect();
            let changes = c.query("select system, changes from registry_changes order by id", &[])?.iter().map(|r| (r.get(0), r.get(1))).collect();
            Ok(super::store::stored_registry(whole, changes))
        })
    }

    fn retire(&self, before_ms: i64, max: usize) -> Result<usize, String> {
        let mut w = self.writer()?;
        let n = w.execute("delete from calls where id in (select id from calls where start_ms < $1 order by start_ms limit $2)", &[&before_ms, &(max as i64)]).map_err(|e| e.to_string())?;
        Ok(n as usize)
    }

    fn newest_start(&self) -> Result<Option<i64>, String> {
        self.read(|c| Ok(c.query_one("select max(start_ms) from calls", &[])?.get(0)))
    }

    fn info(&self) -> Result<StoreInfo, String> {
        let (calls, oldest_ms, newest_ms) = self.read(|c| {
            let r = c.query_one("select count(*), min(start_ms), max(start_ms) from calls", &[])?;
            Ok((r.get(0), r.get(1), r.get(2)))
        })?;
        Ok(StoreInfo { location: self.location.clone(), schema_version: MIGRATIONS.len() as u32, calls, oldest_ms, newest_ms })
    }

    #[cfg(test)]
    fn scalar(&self, sql: &str) -> i64 {
        self.read(|c| Ok(c.query_one(sql, &[])?.get::<_, i64>(0))).unwrap()
    }

    #[cfg(test)]
    fn refuse_unit(&self, unit: i64) {
        let sql = format!(
            "create or replace function refuse_unit() returns trigger as $$ begin if new.unit = {unit} then raise exception 'refused'; end if; return new; end $$ language plpgsql;
             create trigger refuse before insert on call_transmissions for each row execute function refuse_unit();"
        );
        self.writer().unwrap().batch_execute(&sql).unwrap();
    }
}

/// Write a call's row and its children. A live call replaces what is there;
/// a backfilled one only fills a gap, and brings its children only if it
/// landed.
fn write_call(tx: &mut postgres::Transaction<'_>, row: &CallRow, live: bool) -> Result<(), postgres::Error> {
    let conflict = if live {
        "on conflict (path) do update set
            short_name = excluded.short_name, nac = excluded.nac, site = excluded.site,
            talkgroup = excluded.talkgroup, start_ms = excluded.start_ms, stop_ms = excluded.stop_ms,
            length_ms = excluded.length_ms, freq_hz = excluded.freq_hz, emergency = excluded.emergency,
            encrypted = excluded.encrypted, decrypted = excluded.decrypted, duplex = excluded.duplex,
            priority = excluded.priority, audio_type = excluded.audio_type, signal = excluded.signal,
            noise = excluded.noise, snr = excluded.snr, clean_voice_pct = excluded.clean_voice_pct,
            audio = excluded.audio, json = excluded.json, recorder = excluded.recorder,
            record = excluded.record, indexed_ms = excluded.indexed_ms"
    } else {
        "on conflict (path) do nothing"
    };
    let sql = format!(
        "insert into calls (path, short_name, nac, site, talkgroup, start_ms, stop_ms, length_ms, freq_hz,
            emergency, encrypted, decrypted, duplex, priority, audio_type, signal, noise, snr,
            clean_voice_pct, audio, json, recorder, record, indexed_ms)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19,
            $20, $21, $22, ($23::text)::jsonb, $24)
         {conflict} returning id"
    );
    let landed = tx.query_opt(
        &sql,
        &[
            &row.path, &row.short_name, &row.nac, &row.site, &row.talkgroup, &row.start_ms, &row.stop_ms, &row.length_ms, &row.freq_hz, &row.emergency, &row.encrypted, &row.decrypted, &row.duplex, &row.priority, &row.audio_type, &row.signal, &row.noise, &row.snr,
            &row.clean_voice_pct, &row.audio, &row.json, &row.recorder, &row.record, &row.indexed_ms,
        ],
    )?;
    let Some(landed) = landed else { return Ok(()) };
    let id: i64 = landed.get(0);
    tx.execute("delete from call_transmissions where call_id = $1", &[&id])?;
    tx.execute("delete from call_frequencies where call_id = $1", &[&id])?;
    for (seq, t) in row.transmissions.iter().enumerate() {
        tx.execute(
            "insert into call_transmissions (call_id, seq, unit, start_ms, length_ms, emergency, signal_system, tag) values ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[&id, &(seq as i64), &t.unit, &t.start_ms, &t.length_ms, &t.emergency, &t.signal_system, &t.tag],
        )?;
    }
    for (seq, f) in row.frequencies.iter().enumerate() {
        tx.execute(
            "insert into call_frequencies (call_id, seq, freq_hz, start_ms, length_ms, error_count, spike_count) values ($1, $2, $3, $4, $5, $6, $7)",
            &[&id, &(seq as i64), &f.freq_hz, &f.start_ms, &f.length_ms, &f.error_count, &f.spike_count],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_said_without_its_password() {
        assert_eq!(redacted("postgres://trp:secret@db.example:5432/trunk"), "postgres://trp@db.example:5432/trunk");
        assert_eq!(redacted("postgresql://db/trunk"), "postgresql://db/trunk");
        assert_eq!(redacted("host=db user=trp password=secret dbname=trunk"), "host=db user=trp dbname=trunk");
    }

    /// The database is this recorder's: a second store on it is refused, and
    /// a newer schema is too. (With `TRP_PG_URL`.)
    #[test]
    fn a_second_recorder_and_a_newer_schema_are_refused() {
        let Ok(url) = std::env::var("TRP_PG_URL") else { return };
        let first = PostgresStore::open_fresh_schema(&url, "t_pg_lock").unwrap();
        let err = PostgresStore::open_in(&url, Some("t_pg_lock".into())).err().expect("refused");
        assert!(err.contains("another Trunk Recorder Pro"), "{err}");
        first.writer().unwrap().batch_execute("update schema_version set version = 99").unwrap();
        drop(first);
        let err = PostgresStore::open_in(&url, Some("t_pg_lock".into())).err().expect("newer refused");
        assert!(err.contains("99"), "{err}");
    }
}
