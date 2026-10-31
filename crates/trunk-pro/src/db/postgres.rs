//! The PostgreSQL backend, for servers: years of history, and the operator's
//! backups, replication and point-in-time recovery. `storage.url` names the
//! database (`postgres://user:password@host/db`, or `host=… dbname=…`).
//!
//! It only connects: the schema is `schema.rs`'s in PostgreSQL's types, and
//! every statement is `store.rs`'s. Values go to each parameter's own type,
//! which a statement says once prepared (statements are kept per
//! connection). One recorder per database (or schema) for now: a session
//! advisory lock plays the lock file's part, so a second recorder pointed at
//! it runs with its store off. Connections that drop (a server restart) are
//! opened again on the next use. Plain TCP; TLS is a later step.

use std::collections::HashMap;
use std::sync::Mutex;

use postgres::types::{ToSql, Type};
use postgres::{Client, GenericClient, NoTls, Statement};

use super::schema::{self, SCHEMA_VERSION};
use super::sql::{Dialect, Exec, Row, V};
use super::store::{Backend, Store};

/// Read connections kept.
const READERS: usize = 2;

/// A connection and the statements prepared on it.
struct Conn {
    client: Client,
    prepared: HashMap<String, Statement>,
}

pub struct PostgresBackend {
    url: String,
    /// The schema it lives in (tests: one each); None: the connection's own.
    schema: Option<String>,
    /// The database, said without its password.
    location: String,
    writer: Mutex<Conn>,
    readers: Mutex<Vec<Conn>>,
}

/// The store in the database `url` names ([`PostgresBackend::open`]).
pub fn open(url: &str) -> Result<Store, String> {
    PostgresBackend::open_in(url, None).map(|b| Store::new(Box::new(b)))
}

/// A fresh store in schema `schema` of `url`'s database, emptied first
/// (tests: each its own).
#[cfg(test)]
pub fn open_fresh_schema(url: &str, schema: &str) -> Result<Store, String> {
    let mut c = Client::connect(url, NoTls).map_err(said)?;
    c.batch_execute(&format!("drop schema if exists {schema} cascade; create schema {schema}")).map_err(said)?;
    PostgresBackend::open_in(url, Some(schema.to_string())).map(|b| Store::new(Box::new(b)))
}

impl PostgresBackend {
    /// Connect to the database `url` names (in `schema`), take it for this
    /// recorder, and make its tables if they aren't this build's.
    fn open_in(url: &str, schema: Option<String>) -> Result<PostgresBackend, String> {
        if url.trim().is_empty() {
            return Err("storage.url is empty: say which PostgreSQL database".into());
        }
        let location = redacted(url);
        let mut writer = connect(url, schema.as_deref()).map_err(|e| format!("{location}: {}", said(e)))?;
        take(&mut writer.client).map_err(|e| format!("{location}: {e}"))?;
        prepare(&mut writer.client, &location).map_err(|e| format!("{location}: {e}"))?;
        let readers = (0..READERS).map(|_| connect(url, schema.as_deref())).collect::<Result<Vec<_>, _>>().map_err(|e| format!("{location}: {}", said(e)))?;
        Ok(PostgresBackend { url: url.to_string(), schema, location, writer: Mutex::new(writer), readers: Mutex::new(readers) })
    }
}

impl Backend for PostgresBackend {
    fn kind(&self) -> &'static str {
        "postgres"
    }

    fn location(&self) -> String {
        self.location.clone()
    }

    /// `f` in a transaction on the writer's connection (opened again, and
    /// the database taken again, if the server dropped it).
    fn write(&self, f: &mut dyn FnMut(&mut dyn Exec) -> Result<(), String>) -> Result<(), String> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if w.client.is_closed() {
            log::warn!("Call store: {}: the connection dropped; connecting again", self.location);
            let mut c = connect(&self.url, self.schema.as_deref()).map_err(said)?;
            take(&mut c.client)?;
            *w = c;
        }
        let Conn { client, prepared } = &mut *w;
        let mut tx = client.transaction().map_err(|e| format!("could not begin a transaction: {}", said(e)))?;
        f(&mut Pg { c: &mut tx, prepared })?;
        tx.commit().map_err(|e| format!("commit failed: {}", said(e)))
    }

    /// `f` on a read connection from the pool (a new one when all are out,
    /// or the one taken had dropped).
    fn read(&self, f: &mut dyn FnMut(&mut dyn Exec) -> Result<(), String>) -> Result<(), String> {
        let pooled = self.readers.lock().unwrap_or_else(|e| e.into_inner()).pop().filter(|c| !c.client.is_closed());
        let mut c = match pooled {
            Some(c) => c,
            None => connect(&self.url, self.schema.as_deref()).map_err(said)?,
        };
        let out = f(&mut Pg { c: &mut c.client, prepared: &mut c.prepared });
        let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if !c.client.is_closed() && pool.len() < READERS {
            pool.push(c);
        }
        out
    }
}

/// Statements on a connection or a transaction of it.
struct Pg<'a, C: GenericClient> {
    c: &'a mut C,
    prepared: &'a mut HashMap<String, Statement>,
}

impl<C: GenericClient> Pg<'_, C> {
    /// `sql` prepared (once per connection), and `ps` as its parameters' types.
    fn statement(&mut self, sql: &str, ps: &[V]) -> Result<(Statement, Vec<Box<dyn ToSql + Sync>>), String> {
        let st = match self.prepared.get(sql) {
            Some(st) => st.clone(),
            None => {
                let st = self.c.prepare(sql).map_err(said)?;
                self.prepared.insert(sql.to_string(), st.clone());
                st
            }
        };
        let args = ps.iter().zip(st.params()).map(|(v, t)| param(v, t)).collect::<Result<Vec<_>, _>>()?;
        Ok((st, args))
    }
}

impl<C: GenericClient> Exec for Pg<'_, C> {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    fn execute(&mut self, sql: &str, ps: &[V]) -> Result<u64, String> {
        let (st, args) = self.statement(sql, ps)?;
        let refs: Vec<&(dyn ToSql + Sync)> = args.iter().map(|a| &**a).collect();
        self.c.execute(&st, &refs).map_err(said)
    }

    fn query(&mut self, sql: &str, ps: &[V]) -> Result<Vec<Row>, String> {
        let (st, args) = self.statement(sql, ps)?;
        let refs: Vec<&(dyn ToSql + Sync)> = args.iter().map(|a| &**a).collect();
        Ok(self.c.query(&st, &refs).map_err(said)?.iter().map(row).collect())
    }

    fn batch(&mut self, sql: &str) -> Result<(), String> {
        self.c.batch_execute(sql).map_err(said)
    }
}

/// `v` as a parameter of type `t`.
fn param(v: &V, t: &Type) -> Result<Box<dyn ToSql + Sync>, String> {
    let bad = || format!("{v:?} can't be a {t}");
    Ok(match v {
        V::Null => match *t {
            Type::INT2 => Box::new(None::<i16>),
            Type::INT4 => Box::new(None::<i32>),
            Type::INT8 => Box::new(None::<i64>),
            Type::FLOAT4 => Box::new(None::<f32>),
            Type::FLOAT8 => Box::new(None::<f64>),
            Type::BOOL => Box::new(None::<bool>),
            Type::JSON | Type::JSONB => Box::new(None::<serde_json::Value>),
            _ => Box::new(None::<String>),
        },
        V::Int(i) => match *t {
            Type::INT2 => Box::new(i16::try_from(*i).map_err(|_| bad())?),
            Type::INT4 => Box::new(i32::try_from(*i).map_err(|_| bad())?),
            Type::INT8 => Box::new(*i),
            Type::FLOAT4 => Box::new(*i as f32),
            Type::FLOAT8 => Box::new(*i as f64),
            Type::BOOL => Box::new(*i != 0),
            Type::TEXT | Type::VARCHAR => Box::new(i.to_string()),
            _ => return Err(bad()),
        },
        V::Real(r) => match *t {
            Type::FLOAT4 => Box::new(*r as f32),
            Type::FLOAT8 => Box::new(*r),
            Type::INT8 => Box::new(*r as i64),
            _ => return Err(bad()),
        },
        V::Bool(b) => match *t {
            Type::BOOL => Box::new(*b),
            Type::INT2 => Box::new(i16::from(*b)),
            Type::INT4 => Box::new(i32::from(*b)),
            Type::INT8 => Box::new(i64::from(*b)),
            _ => return Err(bad()),
        },
        V::Text(s) => match *t {
            Type::JSON | Type::JSONB => Box::new(serde_json::from_str::<serde_json::Value>(s).map_err(|e| format!("not JSON: {e}"))?),
            _ => Box::new(s.clone()),
        },
    })
}

/// A row's values, by each column's type (JSON as its text).
fn row(r: &postgres::Row) -> Row {
    Row(r
        .columns()
        .iter()
        .enumerate()
        .map(|(i, c)| match *c.type_() {
            Type::INT2 => r.get::<_, Option<i16>>(i).map_or(V::Null, |v| V::Int(v.into())),
            Type::INT4 => r.get::<_, Option<i32>>(i).map_or(V::Null, |v| V::Int(v.into())),
            Type::INT8 => r.get::<_, Option<i64>>(i).map_or(V::Null, V::Int),
            Type::FLOAT4 => r.get::<_, Option<f32>>(i).map_or(V::Null, |v| V::Real(v.into())),
            Type::FLOAT8 => r.get::<_, Option<f64>>(i).map_or(V::Null, V::Real),
            Type::BOOL => r.get::<_, Option<bool>>(i).map_or(V::Null, V::Bool),
            Type::JSON | Type::JSONB => r.get::<_, Option<serde_json::Value>>(i).map_or(V::Null, |v| V::Text(v.to_string())),
            _ => r.get::<_, Option<String>>(i).map_or(V::Null, V::Text),
        })
        .collect())
}

fn connect(url: &str, schema: Option<&str>) -> Result<Conn, postgres::Error> {
    let mut client = Client::connect(url, NoTls)?;
    if let Some(s) = schema {
        client.batch_execute(&format!("set search_path to {s}"))?;
    }
    Ok(Conn { client, prepared: HashMap::new() })
}

/// Take the database (its schema) for this recorder: a session advisory
/// lock, let go when the connection closes.
fn take(c: &mut Client) -> Result<(), String> {
    let got: bool = c.query_one("select pg_try_advisory_lock(hashtext('trunk-pro:' || current_schema()))", &[]).map_err(said)?.get(0);
    if got { Ok(()) } else { Err("another Trunk Recorder Pro is using this database".into()) }
}

/// The tables at this build's schema, in one transaction: kept if they are,
/// otherwise (nothing migrated while in development) dropped and made afresh.
fn prepare(c: &mut Client, location: &str) -> Result<(), String> {
    let mut tx = c.transaction().map_err(said)?;
    tx.batch_execute("create table if not exists schema_version (version integer not null)").map_err(said)?;
    let have: Option<i32> = tx.query_opt("select version from schema_version", &[]).map_err(said)?.map(|r| r.get(0));
    let want = SCHEMA_VERSION as i32;
    if have == Some(want) {
        return tx.commit().map_err(said);
    }
    if let Some(v) = have {
        log::warn!("Call store: {location} has schema version {v}, not this build's {want}; its tables are made afresh");
    }
    tx.batch_execute(&format!("drop table if exists {} cascade", schema::tables_to_drop())).map_err(said)?;
    tx.batch_execute(&schema::ddl(Dialect::Postgres)).map_err(said)?;
    tx.execute("delete from schema_version", &[]).map_err(said)?;
    tx.execute("insert into schema_version (version) values ($1)", &[&want]).map_err(said)?;
    tx.commit().map_err(said)
}

/// What went wrong, as the server said it (the driver's own words for a
/// server's error are just "db error").
fn said(e: postgres::Error) -> String {
    match e.as_db_error() {
        Some(d) => format!("{} ({})", d.message(), d.code().code()),
        None => e.to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_said_without_its_password() {
        assert_eq!(redacted("postgres://trp:secret@db.example:5432/trunk"), "postgres://trp@db.example:5432/trunk");
        assert_eq!(redacted("postgresql://db/trunk"), "postgresql://db/trunk");
        assert_eq!(redacted("host=db user=trp password=secret dbname=trunk"), "host=db user=trp dbname=trunk");
    }

    /// The database is this recorder's: a second store on it is refused;
    /// another version's tables are made afresh. (With `TRP_PG_URL`.)
    #[test]
    fn a_second_recorder_is_refused_and_another_version_made_afresh() {
        let Ok(url) = std::env::var("TRP_PG_URL") else { return };
        let first = open_fresh_schema(&url, "t_pg_lock").unwrap();
        let err = PostgresBackend::open_in(&url, Some("t_pg_lock".into())).err().expect("refused");
        assert!(err.contains("another Trunk Recorder Pro"), "{err}");
        first.apply(vec![crate::db::Change::Meta { key: "a".into(), value: "1".into() }]).unwrap();
        first.batch_for_test("update schema_version set version = 99");
        drop(first);
        let again = Store::new(Box::new(PostgresBackend::open_in(&url, Some("t_pg_lock".into())).unwrap()));
        assert_eq!(again.scalar("select max(version)::bigint from schema_version"), i64::from(SCHEMA_VERSION));
        assert_eq!(again.scalar("select count(*) from meta"), 0, "made afresh");
    }
}
