//! Statistics and activity history, kept in `stats.db` beside the config.
//!
//! The engine thread hands events to [`Stats`] (never waiting: a full queue
//! drops them, counted); one writer thread owns the database and writes what
//! changed every few seconds, in one transaction. Queries use a connection of
//! their own. Everything is kept: units, talkgroups and who affiliated where
//! are a long-term record.
//!
//! Per hour, per system, channel, talkgroup and radio: calls, seconds of
//! audio, vocoder frames, FEC-corrected bit errors, coded bits and bad
//! (repeated or muted) frames. Bit error rate = errors / coded bits.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use trunk_recorder_plugin::{CallInfo, HostMessage, Status, UnitEvent};

const SCHEMA: i64 = 1;
const QUEUE: usize = 4096;
const FLUSH: Duration = Duration::from_secs(5);
const BACKUP_EVERY: Duration = Duration::from_secs(24 * 3600);

/// Coded bits per vocoder frame: P25 Phase 1 IMBE 144 (7200 b/s), Phase 2
/// and DMR AMBE+2 72 (3600 b/s per slot).
const BITS_IMBE: u64 = 144;
const BITS_AMBE: u64 = 72;

enum Msg {
    Event(Box<HostMessage>),
    Concluded(String),
    Stop,
}

pub struct Stats {
    tx: Mutex<Option<SyncSender<Msg>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
    path: PathBuf,
    problem: Option<String>,
    dropped: AtomicU64,
    /// Unix seconds this process started keeping stats (the "since restart" window).
    started: i64,
}

impl Stats {
    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path.with_file_name("stats.db")
    }

    /// Open (or create) the database and start the writer. A database that
    /// can't be opened is left as it is, and nothing is kept.
    pub fn open(path: PathBuf) -> Stats {
        let started = unix_now();
        let mk = |problem: Option<String>, tx, writer| Stats { tx: Mutex::new(tx), writer: Mutex::new(writer), path: path.clone(), problem, dropped: AtomicU64::new(0), started };
        let conn = match open_writer(&path) {
            Ok(c) => c,
            Err(e) => return mk(Some(format!("Statistics are off: {} ({e})", path.display())), None, None),
        };
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let p2 = path.clone();
        let writer = std::thread::Builder::new().name("stats".into()).spawn(move || writer(conn, rx, &p2)).ok();
        mk(None, Some(tx), writer)
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Events statistics are kept from (the session builds them only when asked).
    pub fn wants(&self) -> bool {
        self.problem.is_none()
    }

    pub fn event(&self, m: &HostMessage) {
        if matches!(m, HostMessage::CallStart(_) | HostMessage::CallEnd(_) | HostMessage::Unit(_) | HostMessage::Status(_)) {
            self.send(Msg::Event(Box::new(m.clone())));
        }
    }

    /// A recorded call's JSON.
    pub fn concluded(&self, json: &str) {
        self.send(Msg::Concluded(json.to_string()));
    }

    fn send(&self, m: Msg) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            if let Err(TrySendError::Full(_)) = tx.try_send(m) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Write what's pending and stop the writer.
    pub fn close(&self) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(Msg::Stop);
        }
        if let Some(w) = self.writer.lock().unwrap().take() {
            let _ = w.join();
        }
    }

    fn reader(&self) -> Result<Connection, String> {
        if let Some(p) = &self.problem {
            return Err(p.clone());
        }
        let c = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| e.to_string())?;
        c.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        Ok(c)
    }

    /// The earliest unix second of `window`: "restart", "24h", "7d", "30d" or "all".
    pub fn since(&self, window: &str) -> i64 {
        let now = unix_now();
        match window {
            "restart" => self.started,
            "24h" => now - 24 * 3600,
            "7d" => now - 7 * 24 * 3600,
            "30d" => now - 30 * 24 * 3600,
            _ => 0,
        }
    }

    /// The `stats` message: the totals, channels, talkgroups, top errors and
    /// charts of `systems` (one, or the sites of a multi-site system) over
    /// `window`. `key` names them in the answer.
    pub fn summary(&self, key: &str, systems: &[String], window: &str) -> Result<Value, String> {
        let c = self.reader()?;
        let sys = systems_param(systems);
        let since = self.since(window);
        let hour = since - since.rem_euclid(3600);
        let q = |e: rusqlite::Error| e.to_string();
        let totals = c
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(calls),0), COALESCE(SUM(seconds),0), COALESCE(SUM(frames),0), COALESCE(SUM(errors),0), COALESCE(SUM(bad_frames),0),
                        COALESCE(SUM(coded_bits),0), COALESCE(SUM(grants),0), COALESCE(SUM(encrypted),0), COALESCE(SUM(emergency),0), COALESCE(SUM(not_recorded),0)
                     FROM sys_hours WHERE sys {IN_SYSTEMS} AND hour >= ?2"
                ),
                params![sys, hour],
                |r| {
                    Ok(json!({
                        "calls": r.get::<_, i64>(0)?, "seconds": r.get::<_, f64>(1)?, "frames": r.get::<_, i64>(2)?, "errors": r.get::<_, i64>(3)?,
                        "badFrames": r.get::<_, i64>(4)?, "codedBits": r.get::<_, i64>(5)?, "grants": r.get::<_, i64>(6)?, "encrypted": r.get::<_, i64>(7)?,
                        "emergency": r.get::<_, i64>(8)?, "notRecorded": r.get::<_, i64>(9)?,
                    }))
                },
            )
            .map_err(q)?;
        let rows = |sql: &str, key: &str| -> Result<Vec<Value>, String> {
            let mut st = c.prepare(sql).map_err(q)?;
            let it = st
                .query_map(params![sys, hour], |r| {
                    Ok(json!({
                        key: r.get::<_, i64>(0)?, "alias": r.get::<_, Option<String>>(1)?.unwrap_or_default(), "calls": r.get::<_, i64>(2)?,
                        "seconds": r.get::<_, f64>(3)?, "frames": r.get::<_, i64>(4)?, "errors": r.get::<_, i64>(5)?, "badFrames": r.get::<_, i64>(6)?,
                        "codedBits": r.get::<_, i64>(7)?, "grants": r.get::<_, i64>(8)?,
                    }))
                })
                .map_err(q)?;
            it.collect::<Result<Vec<_>, _>>().map_err(q)
        };
        let agg = "SUM(x.calls), SUM(x.seconds), SUM(x.frames), SUM(x.errors), SUM(x.bad_frames), SUM(x.coded_bits), SUM(x.grants)";
        let channels = rows(&format!("SELECT x.freq, NULL, {agg} FROM freq_hours x WHERE x.sys {IN_SYSTEMS} AND x.hour >= ?2 GROUP BY x.freq ORDER BY x.freq"), "freq")?;
        let talkgroups = rows(
            &format!(
                "SELECT x.tg, MAX(t.alias), {agg} FROM tg_hours x LEFT JOIN talkgroups t ON t.sys = x.sys AND t.tg = x.tg
                 WHERE x.sys {IN_SYSTEMS} AND x.hour >= ?2 GROUP BY x.tg ORDER BY SUM(x.seconds) DESC, SUM(x.grants) DESC LIMIT 25"
            ),
            "talkgroup",
        )?;
        let tg_errors = rows(
            &format!(
                "SELECT x.tg, MAX(t.alias), {agg} FROM tg_hours x LEFT JOIN talkgroups t ON t.sys = x.sys AND t.tg = x.tg
                 WHERE x.sys {IN_SYSTEMS} AND x.hour >= ?2 GROUP BY x.tg HAVING SUM(x.errors) > 0 ORDER BY SUM(x.errors) DESC LIMIT 25"
            ),
            "talkgroup",
        )?;
        let unit_errors = rows(
            &format!(
                "SELECT x.unit, MAX(u.alias), {agg} FROM unit_hours x LEFT JOIN units u ON u.sys = x.sys AND u.unit = x.unit
                 WHERE x.sys {IN_SYSTEMS} AND x.hour >= ?2 GROUP BY x.unit HAVING SUM(x.errors) > 0 ORDER BY SUM(x.errors) DESC LIMIT 25"
            ),
            "unit",
        )?;
        Ok(json!({
            "type": "stats", "system": key, "window": window, "since": since, "totals": totals,
            "channels": channels, "talkgroups": talkgroups, "topErrors": { "talkgroups": tg_errors, "units": unit_errors },
            "rates": self.rates(&c, &sys, since)?, "hours": self.hours(&c, "sys_hours", "", &sys, 0, hour)?,
        }))
    }

    /// Decode rate and active calls: per minute up to a day, per hour beyond.
    /// Over several sites: the decode rate averaged, the calls added up.
    fn rates(&self, c: &Connection, sys: &str, since: i64) -> Result<Value, String> {
        let span = unix_now() - since;
        let (bucket, from) = if span <= 26 * 3600 { (60, since - since.rem_euclid(60)) } else { (3600, since - since.rem_euclid(3600)) };
        let mut st = c
            .prepare(&format!(
                "SELECT b, SUM(ds) / MAX(SUM(dn), 1), MIN(dmin), MAX(dmax), SUM(amax), SUM(rmax) FROM (
                     SELECT sys, (minute / ?3) * ?3 AS b, SUM(decode_sum) AS ds, SUM(decode_n) AS dn, MIN(decode_min) AS dmin, MAX(decode_max) AS dmax,
                            MAX(active_max) AS amax, MAX(recording_max) AS rmax
                     FROM sys_minutes WHERE sys {IN_SYSTEMS} AND minute >= ?2 GROUP BY sys, b
                 ) GROUP BY b ORDER BY b"
            ))
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map(params![sys, from, bucket], |r| {
                Ok(json!([r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?]))
            })
            .map_err(|e| e.to_string())?;
        let rows = it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        Ok(json!({ "bucket": bucket, "columns": ["t", "decode", "decodeMin", "decodeMax", "active", "recording"], "rows": rows }))
    }

    /// Hourly rows of `table` (for `key` = `id`, or the whole system).
    fn hours(&self, c: &Connection, table: &str, key: &str, sys: &str, id: i64, hour: i64) -> Result<Value, String> {
        let filter = if key.is_empty() { String::new() } else { format!(" AND {key} = ?3") };
        let sql = format!(
            "SELECT hour, SUM(calls), SUM(seconds), SUM(frames), SUM(errors), SUM(bad_frames), SUM(coded_bits), SUM(grants) FROM {table}
             WHERE sys {IN_SYSTEMS} AND hour >= ?2{filter} GROUP BY hour ORDER BY hour"
        );
        let mut st = c.prepare(&sql).map_err(|e| e.to_string())?;
        let map = |r: &rusqlite::Row| {
            Ok(json!([r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?, r.get::<_, i64>(7)?]))
        };
        let rows = if key.is_empty() { st.query_map(params![sys, hour], map) } else { st.query_map(params![sys, hour, id], map) }
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(json!({ "columns": ["hour", "calls", "seconds", "frames", "errors", "badFrames", "codedBits", "grants"], "rows": rows }))
    }

    /// The `statsHistory` message: hourly rows for a channel, talkgroup, radio or the system.
    pub fn history(&self, key: &str, systems: &[String], kind: &str, id: i64, window: &str) -> Result<Value, String> {
        let c = self.reader()?;
        let since = self.since(window);
        let hour = since - since.rem_euclid(3600);
        let (table, column) = match kind {
            "freq" => ("freq_hours", "freq"),
            "talkgroup" => ("tg_hours", "tg"),
            "unit" => ("unit_hours", "unit"),
            _ => ("sys_hours", ""),
        };
        let hours = self.hours(&c, table, column, &systems_param(systems), id, hour)?;
        Ok(json!({ "type": "statsHistory", "system": key, "kind": kind, "id": id, "window": window, "hours": hours }))
    }

    /// The `affiliations` message: a page of radios or talkgroups, newest first
    /// (or the one `id`). Over several sites, one row each, with the sites it
    /// was heard on; a radio's last talkgroup and on/off are its latest site's.
    pub fn affiliations(&self, key: &str, systems: &[String], page: &AffiliationPage) -> Result<Value, String> {
        let AffiliationPage { view, search, id, offset, limit } = *page;
        let c = self.reader()?;
        let sys = systems_param(systems);
        let like = format!("%{}%", search.trim().replace(['%', '_'], ""));
        let limit = limit.clamp(1, 500);
        let q = |e: rusqlite::Error| e.to_string();
        let tgs = view == "talkgroups";
        let (col, table) = if tgs { ("tg", "talkgroups") } else { ("unit", "units") };
        // ?5: one exact row, or -1 for any.
        // A search picks radios or talkgroups (by any site's name for them); their rows on every site count.
        let filter = format!(
            "x.sys {IN_SYSTEMS} AND (?5 < 0 OR x.{col} = ?5)
             AND (?2 = '%%' OR x.{col} IN (SELECT {col} FROM {table} WHERE sys {IN_SYSTEMS} AND (CAST({col} AS TEXT) LIKE ?2 OR alias LIKE ?2)))"
        );
        let sql = if tgs {
            format!(
                "SELECT x.tg, MAX(x.alias), MIN(x.first_seen), MAX(x.last_seen), SUM(x.calls), SUM(x.seconds), SUM(x.affiliations),
                        (SELECT COUNT(DISTINCT l.unit) FROM links l WHERE l.sys {IN_SYSTEMS} AND l.tg = x.tg), GROUP_CONCAT(DISTINCT x.sys)
                 FROM talkgroups x WHERE {filter} GROUP BY x.tg ORDER BY MAX(x.last_seen) DESC LIMIT ?3 OFFSET ?4"
            )
        } else {
            format!(
                "SELECT x.unit, MAX(x.alias), MIN(x.first_seen), MAX(x.last_seen), SUM(x.calls), SUM(x.affiliations), SUM(x.registrations),
                        (SELECT COUNT(DISTINCT l.tg) FROM links l WHERE l.sys {IN_SYSTEMS} AND l.unit = x.unit), GROUP_CONCAT(DISTINCT x.sys),
                        (SELECT y.last_tg FROM units y WHERE y.sys {IN_SYSTEMS} AND y.unit = x.unit ORDER BY y.last_seen DESC LIMIT 1),
                        (SELECT y.registered FROM units y WHERE y.sys {IN_SYSTEMS} AND y.unit = x.unit AND y.registered IS NOT NULL ORDER BY y.last_seen DESC LIMIT 1)
                 FROM units x WHERE {filter} GROUP BY x.unit ORDER BY MAX(x.last_seen) DESC LIMIT ?3 OFFSET ?4"
            )
        };
        // (SQLite numbers parameters by the highest used: ?3 and ?4 are simply unused here.)
        let count_sql = format!("SELECT COUNT(DISTINCT x.{col}) FROM {table} x WHERE {filter}");
        let p = params![sys, like, limit, offset.max(0), id.unwrap_or(-1)];
        let total: i64 = c.query_row(&count_sql, p, |r| r.get(0)).map_err(q)?;
        let mut st = c.prepare(&sql).map_err(q)?;
        let sites = |s: Option<String>| site_list(s);
        let rows = st
            .query_map(p, |r| {
                Ok(if tgs {
                    json!({ "talkgroup": r.get::<_, i64>(0)?, "alias": r.get::<_, Option<String>>(1)?.unwrap_or_default(), "firstSeen": r.get::<_, i64>(2)?,
                            "lastSeen": r.get::<_, i64>(3)?, "calls": r.get::<_, i64>(4)?, "seconds": r.get::<_, f64>(5)?, "affiliations": r.get::<_, i64>(6)?,
                            "units": r.get::<_, i64>(7)?, "sites": sites(r.get(8)?) })
                } else {
                    json!({ "unit": r.get::<_, i64>(0)?, "alias": r.get::<_, Option<String>>(1)?.unwrap_or_default(), "firstSeen": r.get::<_, i64>(2)?,
                            "lastSeen": r.get::<_, i64>(3)?, "calls": r.get::<_, i64>(4)?, "affiliations": r.get::<_, i64>(5)?,
                            "registrations": r.get::<_, i64>(6)?, "talkgroups": r.get::<_, i64>(7)?, "sites": sites(r.get(8)?),
                            "lastTalkgroup": r.get::<_, Option<i64>>(9)?, "registered": r.get::<_, Option<i64>>(10)?.map(|v| v != 0) })
                })
            })
            .map_err(q)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(q)?;
        Ok(json!({ "type": "affiliations", "system": key, "view": view, "search": search, "id": id, "offset": offset, "total": total, "rows": rows }))
    }

    /// The `affiliationLinks` message: the talkgroups a radio has met, or the
    /// radios a talkgroup has, how, and on which sites.
    pub fn links(&self, key: &str, systems: &[String], view: &str, id: i64) -> Result<Value, String> {
        let c = self.reader()?;
        let sql = if view == "talkgroups" {
            format!(
                "SELECT l.unit, MAX(u.alias), MIN(l.first_seen), MAX(l.last_seen), SUM(l.voice), SUM(l.affiliations), SUM(l.locations), GROUP_CONCAT(DISTINCT l.sys)
                 FROM links l LEFT JOIN units u ON u.sys = l.sys AND u.unit = l.unit
                 WHERE l.sys {IN_SYSTEMS} AND l.tg = ?2 GROUP BY l.unit ORDER BY MAX(l.last_seen) DESC LIMIT 500"
            )
        } else {
            format!(
                "SELECT l.tg, MAX(t.alias), MIN(l.first_seen), MAX(l.last_seen), SUM(l.voice), SUM(l.affiliations), SUM(l.locations), GROUP_CONCAT(DISTINCT l.sys)
                 FROM links l LEFT JOIN talkgroups t ON t.sys = l.sys AND t.tg = l.tg
                 WHERE l.sys {IN_SYSTEMS} AND l.unit = ?2 GROUP BY l.tg ORDER BY MAX(l.last_seen) DESC LIMIT 500"
            )
        };
        let mut st = c.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![systems_param(systems), id], |r| {
                let (voice, affiliations, locations) = (r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?);
                let sites = site_list(r.get(7)?);
                Ok(json!({ "id": r.get::<_, i64>(0)?, "alias": r.get::<_, Option<String>>(1)?.unwrap_or_default(), "firstSeen": r.get::<_, i64>(2)?,
                           "lastSeen": r.get::<_, i64>(3)?, "voice": voice, "affiliations": affiliations, "locations": locations,
                           "count": voice + affiliations + locations, "sites": sites }))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(json!({ "type": "affiliationLinks", "system": key, "view": view, "id": id, "rows": rows }))
    }

    /// Everything known about `systems`' radios, talkgroups and affiliations
    /// (the export), each row with its site.
    pub fn export(&self, systems: &[String]) -> Result<Value, String> {
        let c = self.reader()?;
        let sys = systems_param(systems);
        let q = |e: rusqlite::Error| e.to_string();
        let all = |sql: &str, cols: &[&str]| -> Result<Vec<Value>, String> {
            let mut st = c.prepare(sql).map_err(q)?;
            let it = st
                .query_map(params![sys], |r| {
                    let mut o = serde_json::Map::new();
                    for (i, k) in cols.iter().enumerate() {
                        let v: rusqlite::types::Value = r.get(i)?;
                        o.insert(k.to_string(), sql_json(v));
                    }
                    Ok(Value::Object(o))
                })
                .map_err(q)?;
            it.collect::<Result<Vec<_>, _>>().map_err(q)
        };
        Ok(json!({
            "systems": systems,
            "exported": unix_now(),
            "units": all(&format!("SELECT sys, unit, alias, first_seen, last_seen, calls, last_tg, affiliations, registrations, deregistrations, registered FROM units WHERE sys {IN_SYSTEMS} ORDER BY unit, sys"),
                &["system", "unit", "alias", "firstSeen", "lastSeen", "calls", "lastTalkgroup", "affiliations", "registrations", "deregistrations", "registered"])?,
            "talkgroups": all(&format!("SELECT sys, tg, alias, first_seen, last_seen, calls, seconds, affiliations FROM talkgroups WHERE sys {IN_SYSTEMS} ORDER BY tg, sys"),
                &["system", "talkgroup", "alias", "firstSeen", "lastSeen", "calls", "seconds", "affiliations"])?,
            "affiliations": all(&format!("SELECT sys, unit, tg, first_seen, last_seen, voice, affiliations, locations FROM links WHERE sys {IN_SYSTEMS} ORDER BY unit, tg, sys"),
                &["system", "unit", "talkgroup", "firstSeen", "lastSeen", "voice", "affiliations", "locations"])?,
        }))
    }

    /// Events dropped because the writer was behind (since startup).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Which radios or talkgroups an `affiliations` query wants.
#[derive(Clone, Copy)]
pub struct AffiliationPage<'a> {
    /// "units" or "talkgroups".
    pub view: &'a str,
    pub search: &'a str,
    /// Just this one.
    pub id: Option<i64>,
    pub offset: i64,
    pub limit: i64,
}

/// SQL: the row's system is one of parameter ?1 ([`systems_param`]).
const IN_SYSTEMS: &str = "IN (SELECT value FROM json_each(?1))";

/// GROUP_CONCAT's sites, in order.
fn site_list(s: Option<String>) -> Vec<String> {
    let mut v: Vec<String> = s.map(|s| s.split(',').map(String::from).collect()).unwrap_or_default();
    v.sort();
    v
}

/// Short names as the JSON array [`IN_SYSTEMS`] reads.
fn systems_param(systems: &[String]) -> String {
    serde_json::to_string(systems).unwrap_or_else(|_| "[]".into())
}

fn sql_json(v: rusqlite::types::Value) -> Value {
    use rusqlite::types::Value as V;
    match v {
        V::Null => Value::Null,
        V::Integer(i) => json!(i),
        V::Real(f) => json!(f),
        V::Text(t) => json!(t),
        V::Blob(_) => Value::Null,
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn open_writer(path: &Path) -> Result<Connection, String> {
    let exists = path.exists();
    let c = Connection::open(path).map_err(|e| e.to_string())?;
    // A file that isn't our database is left alone.
    c.query_row("PRAGMA schema_version", [], |_| Ok(())).map_err(|e| e.to_string())?;
    c.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA foreign_keys = OFF;").map_err(|e| e.to_string())?;
    c.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let version: Option<i64> = if exists {
        c.query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| r.get::<_, String>(0)).optional().ok().flatten().and_then(|v| v.parse().ok())
    } else {
        None
    };
    match version {
        Some(v) if v > SCHEMA => return Err(format!("made by a newer version (schema {v})")),
        Some(_) => {}
        None if exists && has_tables(&c) => return Err("not a statistics database".into()),
        None => create(&c).map_err(|e| e.to_string())?,
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(c)
}

fn has_tables(c: &Connection) -> bool {
    c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name != 'meta'", [], |r| r.get::<_, i64>(0)).unwrap_or(1) > 0
}

fn create(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(&format!(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE units (sys TEXT, unit INTEGER, alias TEXT, first_seen INTEGER, last_seen INTEGER, last_tg INTEGER,
             calls INTEGER DEFAULT 0, affiliations INTEGER DEFAULT 0, registrations INTEGER DEFAULT 0, deregistrations INTEGER DEFAULT 0,
             registered INTEGER, PRIMARY KEY (sys, unit)) WITHOUT ROWID;
         CREATE INDEX units_seen ON units (sys, last_seen);
         CREATE TABLE talkgroups (sys TEXT, tg INTEGER, alias TEXT, first_seen INTEGER, last_seen INTEGER,
             calls INTEGER DEFAULT 0, seconds REAL DEFAULT 0, affiliations INTEGER DEFAULT 0, PRIMARY KEY (sys, tg)) WITHOUT ROWID;
         CREATE INDEX talkgroups_seen ON talkgroups (sys, last_seen);
         CREATE TABLE links (sys TEXT, unit INTEGER, tg INTEGER, first_seen INTEGER, last_seen INTEGER,
             voice INTEGER DEFAULT 0, affiliations INTEGER DEFAULT 0, locations INTEGER DEFAULT 0,
             PRIMARY KEY (sys, unit, tg)) WITHOUT ROWID;
         CREATE INDEX links_tg ON links (sys, tg);
         {h}
         CREATE TABLE sys_minutes (sys TEXT, minute INTEGER, decode_sum REAL, decode_n INTEGER, decode_min REAL, decode_max REAL,
             active_max INTEGER, recording_max INTEGER, PRIMARY KEY (sys, minute)) WITHOUT ROWID;
         INSERT OR REPLACE INTO meta VALUES ('schema', '{SCHEMA}');
         COMMIT;",
        h = ["sys_hours (sys TEXT, hour INTEGER", "freq_hours (sys TEXT, freq INTEGER, hour INTEGER", "tg_hours (sys TEXT, tg INTEGER, hour INTEGER", "unit_hours (sys TEXT, unit INTEGER, hour INTEGER"]
            .iter()
            .map(|t| {
                let key = t.split('(').nth(1).unwrap().split(',').map(|c| c.split_whitespace().next().unwrap()).collect::<Vec<_>>().join(", ");
                format!(
                    "CREATE TABLE {t}, calls INTEGER DEFAULT 0, seconds REAL DEFAULT 0, frames INTEGER DEFAULT 0, errors INTEGER DEFAULT 0,
                     bad_frames INTEGER DEFAULT 0, coded_bits INTEGER DEFAULT 0, grants INTEGER DEFAULT 0, encrypted INTEGER DEFAULT 0,
                     emergency INTEGER DEFAULT 0, not_recorded INTEGER DEFAULT 0, PRIMARY KEY ({key})) WITHOUT ROWID;"
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

// ─── The writer ────────────────────────────────────────────────────────────

/// Counts added to an hour row.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Tally {
    calls: i64,
    seconds: f64,
    frames: i64,
    errors: i64,
    bad_frames: i64,
    coded_bits: i64,
    grants: i64,
    encrypted: i64,
    emergency: i64,
    not_recorded: i64,
}

impl Tally {
    fn add(&mut self, o: &Tally) {
        self.calls += o.calls;
        self.seconds += o.seconds;
        self.frames += o.frames;
        self.errors += o.errors;
        self.bad_frames += o.bad_frames;
        self.coded_bits += o.coded_bits;
        self.grants += o.grants;
        self.encrypted += o.encrypted;
        self.emergency += o.emergency;
        self.not_recorded += o.not_recorded;
    }
}

#[derive(Default, Debug)]
struct UnitDelta {
    alias: Option<String>,
    first: i64,
    last: i64,
    last_tg: Option<i64>,
    calls: i64,
    affiliations: i64,
    registrations: i64,
    deregistrations: i64,
    /// On or off as last heard (None: not said).
    registered: Option<bool>,
}

/// Each way a radio and a talkgroup meet.
#[derive(Clone, Copy)]
enum Meeting {
    /// On a call (a channel grant).
    Voice,
    /// Joined the talkgroup.
    Affiliation,
    /// A location registration for it.
    Location,
}

#[derive(Default, Debug)]
struct LinkDelta {
    first: i64,
    last: i64,
    voice: i64,
    affiliations: i64,
    locations: i64,
}

#[derive(Default, Debug)]
struct TgDelta {
    alias: Option<String>,
    first: i64,
    last: i64,
    calls: i64,
    seconds: f64,
    affiliations: i64,
}

#[derive(Default, Debug)]
struct Minute {
    sum: f64,
    n: i64,
    min: f64,
    max: f64,
    active: i64,
    recording: i64,
}

/// What changed since the last write.
#[derive(Default)]
struct Pending {
    sys: HashMap<(String, i64), Tally>,
    freq: HashMap<(String, i64, i64), Tally>,
    tg: HashMap<(String, i64, i64), Tally>,
    unit: HashMap<(String, i64, i64), Tally>,
    units: HashMap<(String, i64), UnitDelta>,
    talkgroups: HashMap<(String, i64), TgDelta>,
    links: HashMap<(String, i64, i64), LinkDelta>,
    minutes: HashMap<(String, i64), Minute>,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.sys.is_empty() && self.units.is_empty() && self.talkgroups.is_empty() && self.minutes.is_empty() && self.links.is_empty()
    }

    fn unit(&mut self, sys: &str, unit: i64, t: i64) -> &mut UnitDelta {
        let u = self.units.entry((sys.to_string(), unit)).or_insert_with(|| UnitDelta { first: t, last: t, ..Default::default() });
        u.first = u.first.min(t);
        u.last = u.last.max(t);
        u
    }

    fn talkgroup(&mut self, sys: &str, tg: i64, t: i64) -> &mut TgDelta {
        let g = self.talkgroups.entry((sys.to_string(), tg)).or_insert_with(|| TgDelta { first: t, last: t, ..Default::default() });
        g.first = g.first.min(t);
        g.last = g.last.max(t);
        g
    }

    fn call_start(&mut self, c: &CallInfo) {
        let t = c.start_time as i64;
        let hour = t - t.rem_euclid(3600);
        let sys = c.short_name.as_str();
        let tally = Tally { grants: 1, encrypted: c.encrypted as i64, emergency: c.emergency as i64, not_recorded: (!c.recording) as i64, ..Default::default() };
        self.sys.entry((sys.into(), hour)).or_default().add(&tally);
        self.tg.entry((sys.into(), c.talkgroup as i64, hour)).or_default().add(&tally);
        let g = self.talkgroup(sys, c.talkgroup as i64, t);
        if !c.talkgroup_tag.is_empty() {
            g.alias = Some(c.talkgroup_tag.clone());
        }
        for &u in c.units.iter().filter(|&&u| u > 0) {
            self.unit(sys, u as i64, t).last_tg = Some(c.talkgroup as i64);
        }
    }

    /// A radio and a talkgroup met: the link between them, both of their
    /// "last heard", and the radio is on.
    fn meet(&mut self, sys: &str, unit: i64, tg: i64, t: i64, how: Meeting) {
        // 0 is "no radio" and "no talkgroup", not one.
        if unit <= 0 || tg <= 0 {
            return;
        }
        let u = self.unit(sys, unit, t);
        u.last_tg = Some(tg);
        u.registered = Some(true);
        let g = self.talkgroup(sys, tg, t);
        if matches!(how, Meeting::Affiliation) {
            g.affiliations += 1;
        }
        let l = self.links.entry((sys.into(), unit, tg)).or_insert_with(|| LinkDelta { first: t, last: t, ..Default::default() });
        l.first = l.first.min(t);
        l.last = l.last.max(t);
        match how {
            Meeting::Voice => l.voice += 1,
            Meeting::Affiliation => l.affiliations += 1,
            Meeting::Location => l.locations += 1,
        }
    }

    /// A call ended (recorded or not): a call for its talkgroup, and one for
    /// each radio heard on it, which met the talkgroup.
    fn call_end(&mut self, c: &CallInfo) {
        let (t, tg) = (c.start_time as i64, c.talkgroup as i64);
        if tg <= 0 {
            return;
        }
        self.talkgroup(&c.short_name, tg, t).calls += 1;
        // A radio that keys up twice is listed twice: one call all the same.
        let mut units: Vec<i64> = c.units.iter().map(|&u| u as i64).filter(|&u| u > 0).collect();
        units.sort_unstable();
        units.dedup();
        for u in units {
            self.meet(&c.short_name, u, tg, t, Meeting::Voice);
            self.unit(&c.short_name, u, t).calls += 1;
        }
    }

    fn unit_event(&mut self, e: &UnitEvent) {
        // 0 is "no radio", not one.
        if e.unit == 0 {
            return;
        }
        let t = e.time as i64;
        let sys = e.short_name.as_str();
        let u = self.unit(sys, e.unit as i64, t);
        match e.kind.as_str() {
            "registration" => {
                u.registrations += 1;
                u.registered = Some(true);
            }
            "deregistration" => {
                u.deregistrations += 1;
                u.registered = Some(false);
            }
            "affiliation" => {
                u.affiliations += 1;
                if let Some(tg) = e.talkgroup {
                    self.meet(sys, e.unit as i64, tg as i64, t, Meeting::Affiliation);
                }
            }
            "location" => {
                if let Some(tg) = e.talkgroup {
                    self.meet(sys, e.unit as i64, tg as i64, t, Meeting::Location);
                }
            }
            // (Answer requests and call alerts name another radio, not a talkgroup.)
            _ => {}
        }
    }

    fn status(&mut self, s: &Status) {
        let t = s.time as i64;
        let minute = t - t.rem_euclid(60);
        for y in &s.systems {
            let m = self.minutes.entry((y.short_name.clone(), minute)).or_insert(Minute { min: f64::MAX, ..Default::default() });
            // Searching for a control channel decodes nothing: still a sample.
            m.sum += y.decode_rate;
            m.n += 1;
            m.min = m.min.min(y.decode_rate);
            m.max = m.max.max(y.decode_rate);
            m.active = m.active.max(y.active_calls as i64);
            m.recording = m.recording.max(y.recording as i64);
        }
    }

    /// A recorded call (its JSON).
    fn concluded(&mut self, j: &Value) {
        let sys = j["short_name"].as_str().unwrap_or_default().to_string();
        if sys.is_empty() {
            return;
        }
        let start = j["start_time"].as_i64().unwrap_or(0);
        let hour = start - start.rem_euclid(3600);
        let tg = j["talkgroup"].as_i64().unwrap_or(0);
        let freq = j["freq"].as_i64().unwrap_or(0);
        let length = j["call_length_ms"].as_f64().map(|ms| ms / 1000.0).or_else(|| j["call_length"].as_f64()).unwrap_or(0.0);
        let bits = if j["audio_type"] == "analog" {
            0
        } else if flag(&j["phase2_tdma"]) || j["color_code"].as_i64().is_some_and(|c| c >= 0) {
            BITS_AMBE
        } else {
            BITS_IMBE
        };
        // (pos, len, frames, errors, bad frames) of each stretch of audio.
        let intervals: Vec<(f64, f64, i64, i64, i64)> = j["errorList"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| (e["pos"].as_f64().unwrap_or(0.0), e["len"].as_f64().unwrap_or(0.0), e["frames"].as_i64().unwrap_or(0), e["error_count"].as_i64().unwrap_or(0), e["bad_frames"].as_i64().unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default();
        let frames: i64 = intervals.iter().map(|i| i.2).sum();
        let tally = Tally {
            calls: 1,
            seconds: length,
            frames,
            errors: intervals.iter().map(|i| i.3).sum(),
            bad_frames: intervals.iter().map(|i| i.4).sum(),
            coded_bits: frames * bits as i64,
            ..Default::default()
        };
        self.sys.entry((sys.clone(), hour)).or_default().add(&tally);
        self.freq.entry((sys.clone(), freq, hour)).or_default().add(&tally);
        self.tg.entry((sys.clone(), tg, hour)).or_default().add(&tally);
        // (Calls are counted when they end; this is the audio recorded.)
        let g = self.talkgroup(&sys, tg, start);
        g.seconds += length;
        if let Some(a) = j["talkgroup_tag"].as_str().filter(|a| !a.is_empty()) {
            g.alias = Some(a.to_string());
        }
        // Each radio's transmissions: from its position to the next one's,
        // with the errors of the audio it overlaps.
        let srcs = j["srcList"].as_array().cloned().unwrap_or_default();
        for (i, s) in srcs.iter().enumerate() {
            let unit = s["src"].as_i64().unwrap_or(0);
            if unit <= 0 {
                continue;
            }
            let pos = s["pos"].as_f64().unwrap_or(0.0);
            let end = srcs.get(i + 1).and_then(|n| n["pos"].as_f64()).unwrap_or(length).max(pos);
            let mut t = Tally { calls: 1, seconds: end - pos, ..Default::default() };
            for &(ip, il, f, e, b) in &intervals {
                let overlap = (end.min(ip + il) - pos.max(ip)).max(0.0);
                if il > 0.0 && overlap > 0.0 {
                    let share = overlap / il;
                    let frames = (f as f64 * share).round() as i64;
                    t.frames += frames;
                    t.errors += (e as f64 * share).round() as i64;
                    t.bad_frames += (b as f64 * share).round() as i64;
                    t.coded_bits += frames * bits as i64;
                }
            }
            self.unit.entry((sys.clone(), unit, hour)).or_default().add(&t);
            let at = s["time"].as_i64().unwrap_or(start);
            let u = self.unit(&sys, unit, at);
            u.last_tg = Some(tg);
            let alias = s["tag"].as_str().filter(|a| !a.is_empty()).or_else(|| s["tag_ota"].as_str().filter(|a| !a.is_empty()));
            if let Some(a) = alias {
                u.alias = Some(a.to_string());
            }
        }
    }

    fn write(&mut self, c: &mut Connection) -> rusqlite::Result<()> {
        let tx = c.transaction()?;
        {
            let tally_sql = |table: &str, cols: &str| {
                let n = cols.split(',').count();
                let ph: Vec<String> = (1..=n + 10).map(|i| format!("?{i}")).collect();
                format!(
                    "INSERT INTO {table} ({cols}, calls, seconds, frames, errors, bad_frames, coded_bits, grants, encrypted, emergency, not_recorded)
                     VALUES ({}) ON CONFLICT DO UPDATE SET calls = calls + excluded.calls, seconds = seconds + excluded.seconds,
                     frames = frames + excluded.frames, errors = errors + excluded.errors, bad_frames = bad_frames + excluded.bad_frames,
                     coded_bits = coded_bits + excluded.coded_bits, grants = grants + excluded.grants, encrypted = encrypted + excluded.encrypted,
                     emergency = emergency + excluded.emergency, not_recorded = not_recorded + excluded.not_recorded",
                    ph.join(", ")
                )
            };
            macro_rules! tallies {
                ($map:expr, $table:expr, $cols:expr, |$k:ident| $keys:expr) => {{
                    let mut st = tx.prepare_cached(&tally_sql($table, $cols))?;
                    for ($k, t) in $map.drain() {
                        let mut p: Vec<rusqlite::types::Value> = $keys;
                        p.extend([
                            t.calls.into(), t.seconds.into(), t.frames.into(), t.errors.into(), t.bad_frames.into(), t.coded_bits.into(),
                            t.grants.into(), t.encrypted.into(), t.emergency.into(), t.not_recorded.into(),
                        ]);
                        st.execute(rusqlite::params_from_iter(p))?;
                    }
                }};
            }
            tallies!(self.sys, "sys_hours", "sys, hour", |k| vec![k.0.into(), k.1.into()]);
            tallies!(self.freq, "freq_hours", "sys, freq, hour", |k| vec![k.0.into(), k.1.into(), k.2.into()]);
            tallies!(self.tg, "tg_hours", "sys, tg, hour", |k| vec![k.0.into(), k.1.into(), k.2.into()]);
            tallies!(self.unit, "unit_hours", "sys, unit, hour", |k| vec![k.0.into(), k.1.into(), k.2.into()]);

            let mut st = tx.prepare_cached(
                "INSERT INTO units (sys, unit, alias, first_seen, last_seen, last_tg, calls, affiliations, registrations, deregistrations, registered)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT DO UPDATE SET
                 registered = COALESCE(excluded.registered, registered),
                 alias = COALESCE(excluded.alias, alias), first_seen = MIN(first_seen, excluded.first_seen),
                 last_tg = CASE WHEN excluded.last_seen >= last_seen THEN COALESCE(excluded.last_tg, last_tg) ELSE last_tg END,
                 last_seen = MAX(last_seen, excluded.last_seen), calls = calls + excluded.calls, affiliations = affiliations + excluded.affiliations,
                 registrations = registrations + excluded.registrations, deregistrations = deregistrations + excluded.deregistrations",
            )?;
            for ((sys, unit), u) in self.units.drain() {
                st.execute(params![sys, unit, u.alias, u.first, u.last, u.last_tg, u.calls, u.affiliations, u.registrations, u.deregistrations, u.registered])?;
            }
            let mut st = tx.prepare_cached(
                "INSERT INTO talkgroups (sys, tg, alias, first_seen, last_seen, calls, seconds, affiliations) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT DO UPDATE SET alias = COALESCE(excluded.alias, alias), first_seen = MIN(first_seen, excluded.first_seen),
                 last_seen = MAX(last_seen, excluded.last_seen), calls = calls + excluded.calls, seconds = seconds + excluded.seconds,
                 affiliations = affiliations + excluded.affiliations",
            )?;
            for ((sys, tg), g) in self.talkgroups.drain() {
                st.execute(params![sys, tg, g.alias, g.first, g.last, g.calls, g.seconds, g.affiliations])?;
            }
            let mut st = tx.prepare_cached(
                "INSERT INTO links (sys, unit, tg, first_seen, last_seen, voice, affiliations, locations) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT DO UPDATE SET first_seen = MIN(first_seen, excluded.first_seen), last_seen = MAX(last_seen, excluded.last_seen),
                 voice = voice + excluded.voice, affiliations = affiliations + excluded.affiliations, locations = locations + excluded.locations",
            )?;
            for ((sys, unit, tg), l) in self.links.drain() {
                st.execute(params![sys, unit, tg, l.first, l.last, l.voice, l.affiliations, l.locations])?;
            }
            let mut st = tx.prepare_cached(
                "INSERT INTO sys_minutes (sys, minute, decode_sum, decode_n, decode_min, decode_max, active_max, recording_max) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT DO UPDATE SET decode_sum = decode_sum + excluded.decode_sum, decode_n = decode_n + excluded.decode_n,
                 decode_min = MIN(decode_min, excluded.decode_min), decode_max = MAX(decode_max, excluded.decode_max),
                 active_max = MAX(active_max, excluded.active_max), recording_max = MAX(recording_max, excluded.recording_max)",
            )?;
            for ((sys, minute), m) in self.minutes.drain() {
                st.execute(params![sys, minute, m.sum, m.n, m.min, m.max, m.active, m.recording])?;
            }
        }
        tx.commit()
    }
}

fn flag(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| v.as_i64().is_some_and(|n| n != 0))
}

/// A control channel says each radio message several times over: a repeat
/// within this many seconds is the same message.
const REPEAT_S: f64 = 3.0;

/// Radio messages heard lately, to tell a repeat from a new one.
#[derive(Default)]
struct Repeats {
    last: HashMap<(String, String, u32, Option<u32>), f64>,
}

impl Repeats {
    /// Whether `e` is the same as one heard within [`REPEAT_S`].
    fn repeat(&mut self, e: &UnitEvent) -> bool {
        if self.last.len() > 20_000 {
            self.last.retain(|_, t| e.time - *t <= REPEAT_S);
        }
        let key = (e.short_name.clone(), e.kind.clone(), e.unit, e.talkgroup);
        let prev = self.last.insert(key, e.time);
        prev.is_some_and(|t| (e.time - t).abs() <= REPEAT_S)
    }
}

fn writer(mut c: Connection, rx: Receiver<Msg>, path: &Path) {
    let mut p = Pending::default();
    let mut repeats = Repeats::default();
    let mut last_write = Instant::now();
    let mut last_backup = backup_age(path);
    let mut failing = false;
    loop {
        let stop = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Msg::Event(m)) => {
                match *m {
                    HostMessage::CallStart(ci) => p.call_start(&ci),
                    HostMessage::CallEnd(ci) => p.call_end(&ci),
                    HostMessage::Unit(e) => {
                        if !repeats.repeat(&e) {
                            p.unit_event(&e)
                        }
                    }
                    HostMessage::Status(s) => p.status(&s),
                    _ => {}
                }
                false
            }
            Ok(Msg::Concluded(text)) => {
                if let Ok(j) = serde_json::from_str::<Value>(&text) {
                    p.concluded(&j);
                }
                false
            }
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => true,
            Err(RecvTimeoutError::Timeout) => false,
        };
        if (stop || last_write.elapsed() >= FLUSH) && !p.is_empty() {
            last_write = Instant::now();
            match p.write(&mut c) {
                Ok(()) => {
                    failing = false;
                    private(path);
                }
                Err(e) => {
                    // Kept for the next try (a busy or full disk), said once.
                    if !failing {
                        log::error!("Statistics: couldn't write {} ({e})", path.display());
                    }
                    failing = true;
                }
            }
        }
        if last_backup.is_none_or(|t| t.elapsed() >= BACKUP_EVERY) {
            last_backup = Some(Instant::now());
            backup(&c, path);
        }
        if stop {
            let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            return;
        }
    }
}

/// The database and its journal files, readable by their owner only.
fn private(path: &Path) {
    #[cfg(unix)]
    for ext in ["db", "db-wal", "db-shm"] {
        use std::os::unix::fs::PermissionsExt;
        let p = path.with_extension(ext);
        if std::fs::metadata(&p).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("db.bak")
}

/// When the backup was made, as an Instant (None: there's none).
fn backup_age(path: &Path) -> Option<Instant> {
    let m = std::fs::metadata(backup_path(path)).and_then(|m| m.modified()).ok()?;
    let age = m.elapsed().unwrap_or_default();
    Instant::now().checked_sub(age)
}

/// A daily copy beside the database (whole, consistent, owner-only).
fn backup(c: &Connection, path: &Path) {
    let bak = backup_path(path);
    let tmp = path.with_extension("db.bak.tmp");
    let _ = std::fs::remove_file(&tmp);
    if c.execute("VACUUM INTO ?1", params![tmp.to_string_lossy()]).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        let _ = std::fs::rename(&tmp, &bak);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::SystemStatus;

    fn one(sys: &str) -> Vec<String> {
        vec![sys.to_string()]
    }

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("trunk-pro-stats-{}-{}", std::process::id(), crate::auth::random_hex(4)));
        std::fs::create_dir_all(&d).unwrap();
        d.join("stats.db")
    }

    fn call_json(tg: i64, start: i64, phase2: bool) -> String {
        json!({
            "short_name": "east", "talkgroup": tg, "talkgroup_tag": "Fire Disp", "freq": 851012500, "start_time": start, "stop_time": start + 20,
            "call_length": 20, "audio_type": if phase2 { "digital tdma" } else { "digital" }, "phase2_tdma": phase2 as u8, "color_code": -1,
            "errorList": [
                { "pos": 0.0, "len": 10.0, "frames": 500, "error_count": 100, "bad_frames": 4, "max_frame_errors": 3 },
                { "pos": 10.0, "len": 10.0, "frames": 500, "error_count": 300, "bad_frames": 6, "max_frame_errors": 5 }
            ],
            "srcList": [
                { "src": 1001, "time": start, "pos": 0.0, "tag": "", "tag_ota": "ENGINE 1" },
                { "src": 1002, "time": start + 15, "pos": 15.0, "tag": "", "tag_ota": "" }
            ]
        })
        .to_string()
    }

    #[test]
    fn a_call_is_counted_everywhere() {
        let path = temp();
        let s = Stats::open(path.clone());
        assert!(s.problem().is_none());
        let now = unix_now();
        for start in [now - 60, now - 30] {
            s.event(&HostMessage::CallEnd(CallInfo { short_name: "east".into(), talkgroup: 101, start_time: start as f64, units: vec![1001, 1002], ..Default::default() }));
        }
        s.concluded(&call_json(101, now - 60, false));
        s.concluded(&call_json(101, now - 30, true));
        s.event(&HostMessage::Unit(UnitEvent { system: 0, short_name: "east".into(), kind: "affiliation".into(), unit: 1002, talkgroup: Some(101), time: now as f64 }));
        s.event(&HostMessage::Unit(UnitEvent { system: 0, short_name: "east".into(), kind: "affiliation".into(), unit: 1002, talkgroup: Some(101), time: now as f64 }));
        s.event(&HostMessage::CallStart(CallInfo { short_name: "east".into(), talkgroup: 202, encrypted: true, start_time: now as f64, ..Default::default() }));
        s.event(&HostMessage::Status(Status {
            time: now as f64,
            systems: vec![SystemStatus { index: 0, short_name: "east".into(), decode_rate: 30.0, active_calls: 2, ..Default::default() }],
        }));
        s.close();

        let r = Stats::open(path);
        let v = r.summary("east", &one("east"), "all").unwrap();
        let t = &v["totals"];
        assert_eq!((t["calls"].as_i64(), t["seconds"].as_f64(), t["frames"].as_i64(), t["errors"].as_i64()), (Some(2), Some(40.0), Some(2000), Some(800)));
        // Phase 1 frames carry 144 coded bits, Phase 2 72.
        assert_eq!(t["codedBits"].as_i64(), Some(1000 * 144 + 1000 * 72));
        assert_eq!((t["grants"].as_i64(), t["encrypted"].as_i64(), t["notRecorded"].as_i64()), (Some(1), Some(1), Some(1)));
        assert_eq!(v["channels"][0]["freq"], 851012500);
        assert_eq!(v["talkgroups"][0]["alias"], "Fire Disp");
        // Radio 1002 talked over the last 5 s of each call: half of the second stretch.
        let units = v["topErrors"]["units"].as_array().unwrap();
        let u2 = units.iter().find(|u| u["unit"] == 1002).unwrap();
        assert_eq!((u2["errors"].as_i64(), u2["frames"].as_i64(), u2["seconds"].as_f64()), (Some(300), Some(500), Some(10.0)));
        assert_eq!(units.iter().find(|u| u["unit"] == 1001).unwrap()["alias"], "ENGINE 1");
        assert_eq!(v["rates"]["rows"][0][1], 30.0);

        let a = r.affiliations("east", &one("east"), &AffiliationPage { view: "units", search: "", id: None, offset: 0, limit: 50 }).unwrap();
        assert_eq!(a["total"], 2);
        let u = a["rows"].as_array().unwrap().iter().find(|x| x["unit"] == 1002).unwrap();
        // (The second affiliation, at the same moment, is the control channel repeating itself.)
        assert_eq!((u["affiliations"].as_i64(), u["calls"].as_i64(), u["lastTalkgroup"].as_i64()), (Some(1), Some(2), Some(101)));
        let l = r.links("east", &one("east"), "units", 1002).unwrap();
        // Two calls and a join.
        assert_eq!((l["rows"][0]["id"].as_i64(), l["rows"][0]["voice"].as_i64(), l["rows"][0]["count"].as_i64()), (Some(101), Some(2), Some(3)));
        let tgs = r.affiliations("east", &one("east"), &AffiliationPage { view: "talkgroups", search: "fire", id: None, offset: 0, limit: 50 }).unwrap();
        // Both radios were heard on its calls.
        assert_eq!((tgs["total"].as_i64(), tgs["rows"][0]["units"].as_i64(), tgs["rows"][0]["calls"].as_i64()), (Some(1), Some(2), Some(2)));
        let h = r.history("east", &one("east"), "talkgroup", 101, "all").unwrap();
        assert_eq!(h["hours"]["rows"].as_array().unwrap().iter().map(|r| r[1].as_i64().unwrap()).sum::<i64>(), 2);
        let ex = r.export(&one("east")).unwrap();
        let link = ex["affiliations"].as_array().unwrap().iter().find(|x| x["unit"] == 1002).unwrap();
        assert_eq!((link["affiliations"].as_i64(), link["voice"].as_i64()), (Some(1), Some(2)));
        r.close();
    }

    fn unit_event(kind: &str, unit: u32, talkgroup: Option<u32>, time: i64) -> HostMessage {
        HostMessage::Unit(UnitEvent { system: 0, short_name: "east".into(), kind: kind.into(), unit, talkgroup, time: time as f64 })
    }

    #[test]
    fn every_way_a_radio_meets_a_talkgroup_is_tracked() {
        let path = temp();
        let s = Stats::open(path.clone());
        let now = unix_now();
        s.event(&unit_event("affiliation", 7, Some(10), now - 50));
        s.event(&unit_event("location", 7, Some(11), now - 40));
        // A call heard radios 7 and 8 on talkgroup 12 (and one with no radio at all).
        s.event(&HostMessage::CallEnd(CallInfo { short_name: "east".into(), talkgroup: 12, start_time: (now - 30) as f64, units: vec![7, 8], ..Default::default() }));
        s.event(&HostMessage::CallEnd(CallInfo { short_name: "east".into(), talkgroup: 13, start_time: (now - 30) as f64, units: vec![], ..Default::default() }));
        // These name another radio, not a talkgroup.
        s.event(&unit_event("answer_request", 7, Some(99), now - 20));
        s.event(&unit_event("call_alert", 7, Some(98), now - 20));
        // Nothing to link without a radio or a talkgroup.
        s.event(&unit_event("affiliation", 0, Some(14), now - 20));
        s.event(&unit_event("affiliation", 15, Some(0), now - 20));
        // On, then off.
        s.event(&unit_event("registration", 9, None, now - 15));
        s.event(&unit_event("deregistration", 9, None, now - 10));
        // Off, then heard again: on.
        s.event(&unit_event("deregistration", 7, None, now - 9));
        s.event(&unit_event("affiliation", 7, Some(10), now - 5));
        s.event(&unit_event("acknowledge", 20, None, now - 5));
        s.close();

        let r = Stats::open(path);
        let a = r.affiliations("east", &one("east"), &AffiliationPage { view: "units", search: "", id: None, offset: 0, limit: 50 }).unwrap();
        let row = |unit: u64| a["rows"].as_array().unwrap().iter().find(|x| x["unit"] == unit).cloned().unwrap_or(Value::Null);
        // Radio 7: talkgroups 10 (twice), 11 and 12.
        assert_eq!(row(7)["talkgroups"], 3);
        assert_eq!(row(7)["registered"], true, "heard after it went off");
        assert_eq!(row(9)["registered"], false);
        assert_eq!(row(8)["registered"], true, "heard on a call");
        assert_eq!(row(20)["registered"], Value::Null, "an acknowledgement says nothing either way");
        assert_eq!(row(0), Value::Null);
        assert_eq!(row(15)["talkgroups"], 0);
        let l = r.links("east", &one("east"), "units", 7).unwrap();
        let link = |id: u64| l["rows"].as_array().unwrap().iter().find(|x| x["id"] == id).cloned().unwrap_or(Value::Null);
        assert_eq!((link(10)["affiliations"].as_i64(), link(10)["voice"].as_i64(), link(10)["count"].as_i64()), (Some(2), Some(0), Some(2)));
        assert_eq!((link(11)["locations"].as_i64(), link(11)["count"].as_i64()), (Some(1), Some(1)));
        assert_eq!((link(12)["voice"].as_i64(), link(12)["count"].as_i64()), (Some(1), Some(1)));
        assert_eq!(link(99), Value::Null);
        assert_eq!(link(98), Value::Null);
        // The same links from the talkgroup's side.
        let tg = r.links("east", &one("east"), "talkgroups", 12).unwrap();
        let mut units: Vec<i64> = tg["rows"].as_array().unwrap().iter().map(|x| x["id"].as_i64().unwrap()).collect();
        units.sort();
        assert_eq!(units, [7, 8]);
        let t = r.affiliations("east", &one("east"), &AffiliationPage { view: "talkgroups", search: "", id: None, offset: 0, limit: 50 }).unwrap();
        let t10 = t["rows"].as_array().unwrap().iter().find(|x| x["talkgroup"] == 10).unwrap();
        assert_eq!((t10["units"].as_i64(), t10["affiliations"].as_i64()), (Some(1), Some(2)));
        let ex = r.export(&one("east")).unwrap();
        assert_eq!(ex["affiliations"].as_array().unwrap().len(), 4);
        r.close();
    }

    #[test]
    fn a_radio_keying_up_twice_is_one_call() {
        let path = temp();
        let s = Stats::open(path.clone());
        let now = unix_now();
        s.event(&HostMessage::CallEnd(CallInfo { short_name: "east".into(), talkgroup: 5, start_time: now as f64, units: vec![7, 8, 7], ..Default::default() }));
        s.event(&HostMessage::CallEnd(CallInfo { short_name: "east".into(), talkgroup: 5, start_time: now as f64, units: vec![], ..Default::default() }));
        s.close();
        let r = Stats::open(path);
        let a = r.affiliations("east", &one("east"), &AffiliationPage { view: "units", search: "", id: None, offset: 0, limit: 50 }).unwrap();
        let u7 = a["rows"].as_array().unwrap().iter().find(|x| x["unit"] == 7).unwrap();
        assert_eq!(u7["calls"], 1);
        assert_eq!(r.links("east", &one("east"), "units", 7).unwrap()["rows"][0]["voice"], 1);
        let t = r.affiliations("east", &one("east"), &AffiliationPage { view: "talkgroups", search: "", id: None, offset: 0, limit: 50 }).unwrap();
        assert_eq!((t["rows"][0]["calls"].as_i64(), t["rows"][0]["units"].as_i64()), (Some(2), Some(2)));
        r.close();
    }

    #[test]
    fn a_multi_site_system_is_seen_as_one() {
        let path = temp();
        let s = Stats::open(path.clone());
        let now = unix_now();
        let ev = |sys: &str, kind: &str, unit: u32, tg: Option<u32>, t: i64| {
            HostMessage::Unit(UnitEvent { system: 0, short_name: sys.into(), kind: kind.into(), unit, talkgroup: tg, time: t as f64 })
        };
        // Radio 7 joins talkgroup 10 on site A, then talks on it on site B, then goes off on B.
        s.event(&ev("site-a", "affiliation", 7, Some(10), now - 60));
        s.event(&HostMessage::CallEnd(CallInfo { short_name: "site-b".into(), talkgroup: 10, start_time: (now - 30) as f64, units: vec![7], ..Default::default() }));
        s.event(&ev("site-b", "deregistration", 7, None, now - 10));
        // Site A knows its name.
        s.concluded(
            &json!({ "short_name": "site-a", "talkgroup": 10, "start_time": now - 50, "call_length": 2, "audio_type": "digital",
                     "srcList": [{ "src": 7, "time": now - 50, "pos": 0.0, "tag": "MEDIC 7" }] })
            .to_string(),
        );
        // Another system's radio 7 isn't this one.
        s.event(&ev("elsewhere", "affiliation", 7, Some(99), now - 5));
        s.close();

        let r = Stats::open(path);
        let both = vec!["site-a".to_string(), "site-b".to_string()];
        let a = r.affiliations("all", &both, &AffiliationPage { view: "units", search: "", id: None, offset: 0, limit: 50 }).unwrap();
        assert_eq!(a["total"], 1);
        let u = &a["rows"][0];
        assert_eq!((u["unit"].as_i64(), u["calls"].as_i64(), u["affiliations"].as_i64(), u["talkgroups"].as_i64()), (Some(7), Some(1), Some(1), Some(1)));
        assert_eq!(u["sites"], json!(["site-a", "site-b"]));
        assert_eq!(u["registered"], false, "its latest site says off");
        assert_eq!(u["alias"], "MEDIC 7");
        // Found by a name only one site knows, and still both sites' counts.
        let named = r.affiliations("all", &both, &AffiliationPage { view: "units", search: "medic", id: None, offset: 0, limit: 50 }).unwrap();
        assert_eq!((named["total"].as_i64(), named["rows"][0]["affiliations"].as_i64(), named["rows"][0]["calls"].as_i64()), (Some(1), Some(1), Some(1)));
        // The link, met both ways, on both sites.
        let l = r.links("all", &both, "units", 7).unwrap();
        assert_eq!(l["rows"].as_array().unwrap().len(), 1);
        let link = &l["rows"][0];
        assert_eq!((link["voice"].as_i64(), link["affiliations"].as_i64(), link["count"].as_i64()), (Some(1), Some(1), Some(2)));
        assert_eq!(link["sites"], json!(["site-a", "site-b"]));
        // One radio or talkgroup by its ID.
        let exact = r.affiliations("all", &both, &AffiliationPage { view: "talkgroups", search: "", id: Some(10), offset: 0, limit: 50 }).unwrap();
        assert_eq!((exact["total"].as_i64(), exact["rows"][0]["talkgroup"].as_i64(), exact["rows"][0]["units"].as_i64()), (Some(1), Some(10), Some(1)));
        assert_eq!(r.affiliations("all", &both, &AffiliationPage { view: "talkgroups", search: "", id: Some(99), offset: 0, limit: 50 }).unwrap()["total"], 0);
        // Each site on its own.
        assert_eq!(r.affiliations("site-a", &one("site-a"), &AffiliationPage { view: "units", search: "", id: None, offset: 0, limit: 50 }).unwrap()["rows"][0]["registered"], true);
        let ex = r.export(&both).unwrap();
        assert_eq!(ex["affiliations"].as_array().unwrap().len(), 2, "a row per site");
        r.close();
    }

    #[test]
    fn a_message_said_twice_counts_once() {
        let path = temp();
        let s = Stats::open(path.clone());
        let now = unix_now();
        // The control channel repeats it within a second or two…
        s.event(&unit_event("affiliation", 7, Some(10), now - 30));
        s.event(&unit_event("affiliation", 7, Some(10), now - 29));
        // …but the radio joining again later is another join.
        s.event(&unit_event("affiliation", 7, Some(10), now - 10));
        // As is a different talkgroup at the same moment.
        s.event(&unit_event("affiliation", 7, Some(11), now - 10));
        s.close();
        let r = Stats::open(path);
        let l = r.links("east", &one("east"), "units", 7).unwrap();
        let joined = |tg: u64| l["rows"].as_array().unwrap().iter().find(|x| x["id"] == tg).unwrap()["affiliations"].as_i64();
        assert_eq!((joined(10), joined(11)), (Some(2), Some(1)));
        r.close();
    }

    #[test]
    fn something_else_is_left_alone() {
        let path = temp();
        std::fs::write(&path, b"not a database at all, but someone's file").unwrap();
        let s = Stats::open(path.clone());
        assert!(s.problem().is_some());
        s.concluded(&call_json(1, 0, false));
        s.close();
        assert_eq!(std::fs::read(&path).unwrap(), b"not a database at all, but someone's file");
        // Another program's SQLite database too.
        let other = temp();
        Connection::open(&other).unwrap().execute_batch("CREATE TABLE notes (x TEXT); INSERT INTO notes VALUES ('keep');").unwrap();
        assert!(Stats::open(other.clone()).problem().is_some());
        let n: String = Connection::open(&other).unwrap().query_row("SELECT x FROM notes", [], |r| r.get(0)).unwrap();
        assert_eq!(n, "keep");
    }

    #[test]
    fn a_daily_backup_is_made() {
        let path = temp();
        let s = Stats::open(path.clone());
        s.concluded(&call_json(7, unix_now(), false));
        s.close();
        let bak = backup_path(&path);
        assert!(bak.exists());
        let n: i64 = Connection::open(&bak).unwrap().query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
