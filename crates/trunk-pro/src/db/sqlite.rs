//! The SQLite backend: one file, `<data>/trunk-pro.sqlite`, in WAL mode with
//! `synchronous=NORMAL`. The writer's connection is the only one that
//! writes; a small pool of read-only connections answers queries without
//! waiting on it. A lock file beside it keeps a second recorder on the same
//! data folder from becoming a second writer. A file that isn't a database is
//! moved aside (its WAL with it) and a fresh one started.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::types::Value as Sql;
use rusqlite::{params, params_from_iter, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::Value;

use super::read::{self, CallsFilter, Fetched, Page, Param, Where, COLUMNS};
use super::row::{entry, CallRow};
use super::schema;
use super::store::{CallStore, Change, StoreInfo, StoredRegistry};

/// Read-only connections opened up front, and the most the pool keeps.
const READERS: usize = 2;

pub struct SqliteStore {
    path: PathBuf,
    /// The writer's connection (only the writer thread uses it).
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    /// Held while open, so a second recorder on the same data folder runs
    /// with its store off rather than as a second writer.
    _lock: std::fs::File,
}

impl SqliteStore {
    /// Open (creating if need be) the database at `path` and migrate it. A
    /// file that is not a sound database is moved aside and replaced.
    pub fn open(path: &Path) -> Result<SqliteStore, String> {
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let lock = lock(path)?;
        let conn = match open_sound(path) {
            Ok(c) => c,
            Err(OpenError::Corrupt(e)) if path.exists() => {
                let aside = move_aside(path)?;
                log::warn!("Call store: {} is unusable ({e}); moved to {}", path.display(), aside.display());
                open_sound(path).map_err(|e| format!("open {}: {e}", path.display()))?
            }
            Err(e) => return Err(format!("open {}: {e}", path.display())),
        };
        let readers = (0..READERS).map(|_| open_reader(path)).collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("open reader {}: {e}", path.display()))?;
        Ok(SqliteStore { path: path.to_path_buf(), writer: Mutex::new(conn), readers: Mutex::new(readers), _lock: lock })
    }

    /// Run `f` on a read-only connection from the pool (one opened for the
    /// occasion when all are out).
    fn read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let pooled = self.readers.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let conn = match pooled {
            Some(c) => c,
            None => open_reader(&self.path).map_err(|e| format!("open reader: {e}"))?,
        };
        let out = f(&conn).map_err(|e| e.to_string());
        let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < READERS {
            pool.push(conn);
        }
        out
    }

    #[cfg(test)]
    pub fn readers_pooled(&self) -> usize {
        self.readers.lock().unwrap().len()
    }
}

fn sql(p: &Param) -> Sql {
    match p {
        Param::Int(i) => Sql::Integer(*i),
        Param::Text(t) => Sql::Text(t.clone()),
    }
}

impl CallStore for SqliteStore {
    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn location(&self) -> String {
        self.path.display().to_string()
    }

    fn apply(&self, batch: Vec<Change>) -> Result<(), String> {
        let mut conn = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let mut tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| format!("could not begin a transaction: {e}"))?;
        for change in batch {
            match change {
                // (A call with its transmissions and frequencies, or none of it.)
                Change::Call { row, live } => {
                    let written = tx.savepoint().and_then(|sp| {
                        write_call(&sp, &row, live)?;
                        sp.commit()
                    });
                    if let Err(e) = written {
                        log::warn!("Call store: call write {} failed: {e}", row.path);
                    }
                }
                Change::CallFiles { path, audio, json } => {
                    if let Err(e) = set_files(&tx, &path, audio, json) {
                        log::warn!("Call store: files write {path} failed: {e}");
                    }
                }
                Change::Meta { key, value } => {
                    if let Err(e) = tx.execute("insert into meta (key, value) values (?1, ?2) on conflict(key) do update set value = excluded.value", params![key, value]) {
                        log::warn!("Call store: meta write {key} failed: {e}");
                    }
                }
                Change::Kept { kind, key, value } => {
                    let r = match &value {
                        Some(v) => tx.execute("insert into kept (kind, key, value) values (?1, ?2, ?3) on conflict(kind, key) do update set value = excluded.value", params![kind, key, v]),
                        None => tx.execute("delete from kept where kind = ?1 and key = ?2", params![kind, key]),
                    };
                    if let Err(e) = r {
                        log::warn!("Call store: {kind} {key} write failed: {e}");
                    }
                }
                Change::RegistryChanges { system, json } => {
                    if let Err(e) = tx.execute("insert into registry_changes (system, changes) values (?1, ?2)", params![system, json]) {
                        log::warn!("Call store: registry write {system} failed: {e}");
                    }
                }
                Change::Talkgroups { system, lines, len } => {
                    let written = tx.savepoint().and_then(|sp| {
                        write_talkgroups(&sp, &system, &lines, len)?;
                        sp.commit()
                    });
                    if let Err(e) = written {
                        log::warn!("Call store: talkgroups write {system} failed: {e}");
                    }
                }
                Change::RegistryWhole { system, json } => {
                    let written = tx.savepoint().and_then(|sp| {
                        sp.execute("insert into registry (system, whole) values (?1, ?2) on conflict(system) do update set whole = excluded.whole", params![system, json])?;
                        sp.execute("delete from registry_changes where system = ?1", [&system])?;
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
                let mut ps: Vec<Sql> = w.params.iter().map(sql).collect();
                ps.push(Sql::Integer(want.saturating_add(1)));
                let rows = (|| -> rusqlite::Result<(Vec<Fetched>, bool)> {
                    let mut stmt = c.prepare_cached(&w.select(COLUMNS, false))?;
                    let mut rows = stmt.query(params_from_iter(ps))?;
                    let (mut out, mut extra) = (Vec::new(), false);
                    while let Some(r) = rows.next()? {
                        if out.len() as i64 == want {
                            extra = true;
                            break;
                        }
                        out.push(Fetched { entry: entry(&r.get::<_, String>(0)?, &r.get::<_, String>(1)?, r.get(2)?, r.get(3)?), id: r.get(4)?, start_ms: r.get(5)? });
                    }
                    Ok((out, extra))
                })();
                rows.map_err(|e| e.to_string())
            };
            Ok(read::paged(f, &mut fetch))
        })?
    }

    fn one(&self, path: &str) -> Result<Option<Value>, String> {
        self.read(|c| {
            c.query_row("select path, record, audio, json from calls where path = ?1", [path], |r| Ok(entry(&r.get::<_, String>(0)?, &r.get::<_, String>(1)?, r.get(2)?, r.get(3)?))).optional()
        })
    }

    fn known(&self, path: &str) -> Result<bool, String> {
        self.read(|c| c.prepare_cached("select 1 from calls where path = ?1")?.exists([path]))
    }

    fn meta(&self, key: &str) -> Result<Option<String>, String> {
        self.read(|c| c.query_row("select value from meta where key = ?1", [key], |r| r.get(0)).optional())
    }

    fn kept(&self, kind: &str, key: &str) -> Result<Option<String>, String> {
        self.read(|c| c.query_row("select value from kept where kind = ?1 and key = ?2", [kind, key], |r| r.get(0)).optional())
    }

    fn registry(&self) -> Result<Vec<StoredRegistry>, String> {
        self.read(|c| {
            let whole = c.prepare("select system, whole from registry")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let changes = c.prepare("select system, changes from registry_changes order by id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(super::store::stored_registry(whole, changes))
        })
    }

    fn talkgroups(&self, system: &str) -> Result<Option<String>, String> {
        self.read(|c| Ok(super::store::talkgroup_file(c.prepare("select line from talkgroups where system = ?1 order by seq")?.query_map([system], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)))
    }

    fn retire(&self, before_ms: i64, max: usize) -> Result<usize, String> {
        let mut conn = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let n = tx.execute("delete from calls where id in (select id from calls where start_ms < ?1 order by start_ms limit ?2)", params![before_ms, max as i64]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(n)
    }

    fn newest_start(&self) -> Result<Option<i64>, String> {
        self.read(|c| c.query_row("select max(start_ms) from calls", [], |r| r.get(0)))
    }

    fn info(&self) -> Result<StoreInfo, String> {
        let (calls, oldest_ms, newest_ms) = self.read(|c| c.query_row("select count(*), min(start_ms), max(start_ms) from calls", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))))?;
        Ok(StoreInfo { location: self.path.display().to_string(), schema_version: schema::SCHEMA_VERSION, calls, oldest_ms, newest_ms })
    }

    fn checkpoint(&self) {
        if let Err(e) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(())) {
            log::warn!("Call store: checkpoint failed: {e}");
        }
    }

    fn close(&self) {
        // (The planner's statistics, kept current as SQLite advises, on the way out.)
        if let Err(e) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).execute_batch("pragma optimize") {
            log::debug!("Call store: optimize: {e}");
        }
    }

    #[cfg(test)]
    fn scalar(&self, sql: &str) -> i64 {
        self.read(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
    }

    #[cfg(test)]
    fn refuse_unit(&self, unit: i64) {
        let sql = format!("create trigger refuse before insert on call_transmissions when new.unit = {unit} begin select raise(abort, 'refused'); end;");
        self.writer.lock().unwrap().execute_batch(&sql).unwrap();
    }
}

/// Why the writer's connection could not be opened.
enum OpenError {
    /// The file is not a sound database: it may be moved aside.
    Corrupt(String),
    /// Anything else (busy, disk full, a newer schema): the file is left alone.
    Other(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Corrupt(e) | Self::Other(e)) = self;
        f.write_str(e)
    }
}

impl From<rusqlite::Error> for OpenError {
    fn from(e: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
        match e.sqlite_error_code() {
            Some(NotADatabase | DatabaseCorrupt) => Self::Corrupt(e.to_string()),
            _ => Self::Other(e.to_string()),
        }
    }
}

/// Open the writer's connection: contents sound, pragmas set, schema current.
fn open_sound(path: &Path) -> Result<Connection, OpenError> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // (No integrity check at every start: it reads the whole file, minutes on
    // an SD card for a big store. A file that isn't a database fails below.)
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Checkpoints by size (16 MB of WAL): periodic sequential bursts.
    conn.pragma_update(None, "wal_autocheckpoint", 4096)?;
    conn.pragma_update(None, "journal_size_limit", 16 << 20)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    if let schema::Migrated::Newer(v) = schema::migrate(&conn)? {
        return Err(OpenError::Other(format!("its schema is version {v}, newer than this build's {} (written by a newer Trunk Recorder Pro)", schema::SCHEMA_VERSION)));
    }
    Ok(conn)
}

/// The lock beside `path` (`<name>.lock`), held exclusively while the store
/// is open; an error when another process holds it.
fn lock(path: &Path) -> Result<std::fs::File, String> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&name).map_err(|e| format!("lock {}: {e}", Path::new(&name).display()))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!("another Trunk Recorder Pro is using {}", path.display())),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("lock {}: {e}", Path::new(&name).display())),
    }
}

/// A read-only connection. It waits up to a second for a lock (a checkpoint
/// resetting the WAL) rather than failing at once.
fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(1))?;
    Ok(conn)
}

/// Rename the database and its `-wal`/`-shm` siblings to `<name>.corrupt-<unix
/// seconds>`, returning the new name of the database file. The siblings move
/// first and the database last; if any rename fails, what already moved is put
/// back and the move has failed, so a fresh database never adopts an old WAL
/// and a WAL's committed data is never deleted. Only a `-shm` that will not
/// rename is removed: it is an index that SQLite rebuilds.
fn move_aside(path: &Path) -> Result<PathBuf, String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let with = |base: &Path, suffix: &str| {
        let mut s = base.as_os_str().to_owned();
        s.push(suffix);
        PathBuf::from(s)
    };
    let aside = with(path, &format!(".corrupt-{secs}"));
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let undo = |moved: &[(PathBuf, PathBuf)]| {
        for (from, to) in moved.iter().rev() {
            if let Err(e) = std::fs::rename(to, from) {
                log::warn!("Call store: could not put {} back: {e}", from.display());
            }
        }
    };
    for suffix in ["-wal", "-shm"] {
        let from = with(path, suffix);
        if !from.exists() {
            continue;
        }
        let to = with(&aside, suffix);
        match std::fs::rename(&from, &to) {
            Ok(()) => moved.push((from, to)),
            Err(_) if suffix == "-shm" => {
                std::fs::remove_file(&from)
                    .map_err(|e| format!("move {} aside: {e}", from.display()))
                    .inspect_err(|_| undo(&moved))?;
            }
            Err(e) => {
                undo(&moved);
                return Err(format!("move {} aside: {e}", from.display()));
            }
        }
    }
    if let Err(e) = std::fs::rename(path, &aside) {
        undo(&moved);
        return Err(format!("move {} aside: {e}", path.display()));
    }
    Ok(aside)
}

/// Write a system's talkgroup lines, and remove those past `len`.
fn write_talkgroups(conn: &Connection, system: &str, lines: &[super::store::TalkgroupLine], len: i64) -> rusqlite::Result<()> {
    let mut up = conn.prepare_cached(
        "insert into talkgroups (system, seq, line, number, alpha_tag, description, tag, category, mode, priority, ignore) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         on conflict(system, seq) do update set line = excluded.line, number = excluded.number, alpha_tag = excluded.alpha_tag, description = excluded.description, tag = excluded.tag,
         category = excluded.category, mode = excluded.mode, priority = excluded.priority, ignore = excluded.ignore",
    )?;
    for l in lines {
        let t = l.tg.as_ref();
        up.execute(params![system, l.seq, l.line, t.map(|t| t.number), t.map(|t| &t.alpha_tag), t.map(|t| &t.description), t.map(|t| &t.tag), t.map(|t| &t.group), t.map(|t| &t.mode), t.map(|t| t.priority), t.map(|t| t.ignore)])?;
    }
    conn.execute("delete from talkgroups where system = ?1 and seq >= ?2", params![system, len])?;
    Ok(())
}

/// Write a call's row and its children. A live call replaces what is there;
/// a backfilled one only fills a gap, and brings its children only if it
/// landed.
fn write_call(conn: &Connection, row: &CallRow, live: bool) -> rusqlite::Result<()> {
    let conflict = if live {
        "on conflict(path) do update set
            short_name = excluded.short_name, nac = excluded.nac, site = excluded.site,
            talkgroup = excluded.talkgroup, start_ms = excluded.start_ms,
            stop_ms = excluded.stop_ms, length_ms = excluded.length_ms,
            freq_hz = excluded.freq_hz, emergency = excluded.emergency,
            encrypted = excluded.encrypted, decrypted = excluded.decrypted,
            duplex = excluded.duplex, priority = excluded.priority,
            audio_type = excluded.audio_type, signal = excluded.signal,
            noise = excluded.noise, snr = excluded.snr,
            clean_voice_pct = excluded.clean_voice_pct, audio = excluded.audio,
            json = excluded.json, recorder = excluded.recorder,
            record = excluded.record, indexed_ms = excluded.indexed_ms"
    } else {
        "on conflict(path) do nothing"
    };
    let landed = conn.execute(
        &format!(
            "insert into calls (path, short_name, nac, site, talkgroup, start_ms, stop_ms,
                length_ms, freq_hz, emergency, encrypted, decrypted, duplex, priority,
                audio_type, signal, noise, snr, clean_voice_pct, audio, json, recorder,
                record, indexed_ms)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
             {conflict}"
        ),
        params![
            row.path,
            row.short_name,
            row.nac,
            row.site,
            row.talkgroup,
            row.start_ms,
            row.stop_ms,
            row.length_ms,
            row.freq_hz,
            row.emergency,
            row.encrypted,
            row.decrypted,
            row.duplex,
            row.priority,
            row.audio_type,
            row.signal,
            row.noise,
            row.snr,
            row.clean_voice_pct,
            row.audio,
            row.json,
            row.recorder,
            row.record,
            row.indexed_ms,
        ],
    )? > 0;
    if !landed {
        return Ok(());
    }
    let id: i64 = conn.query_row("select id from calls where path = ?1", [&row.path], |r| {
        r.get(0)
    })?;
    conn.execute("delete from call_transmissions where call_id = ?1", [id])?;
    conn.execute("delete from call_frequencies where call_id = ?1", [id])?;
    let mut tx = conn.prepare_cached(
        "insert into call_transmissions
            (call_id, seq, unit, start_ms, length_ms, emergency, signal_system, tag)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for (seq, t) in row.transmissions.iter().enumerate() {
        tx.execute(params![
            id,
            seq as i64,
            t.unit,
            t.start_ms,
            t.length_ms,
            t.emergency,
            t.signal_system,
            t.tag
        ])?;
    }
    let mut freq = conn.prepare_cached(
        "insert into call_frequencies
            (call_id, seq, freq_hz, start_ms, length_ms, error_count, spike_count)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for (seq, f) in row.frequencies.iter().enumerate() {
        freq.execute(params![
            id,
            seq as i64,
            f.freq_hz,
            f.start_ms,
            f.length_ms,
            f.error_count,
            f.spike_count
        ])?;
    }
    Ok(())
}

/// Set the call's file flags; true if they changed. A call not in the store
/// is no change.
fn set_files(conn: &Connection, path: &str, audio: bool, json: bool) -> rusqlite::Result<bool> {
    let had: Option<(bool, bool)> = conn
        .query_row(
            "select audio, json from calls where path = ?1",
            [path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if had.is_none_or(|h| h == (audio, json)) {
        return Ok(false);
    }
    conn.execute(
        "update calls set audio = ?2, json = ?3 where path = ?1",
        params![path, audio, json],
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{seed_newer, Db, TestDir};
    use std::time::Duration;

    fn aside_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".corrupt-")).count()
    }

    #[test]
    fn opening_creates_the_file_in_wal_at_the_current_version() {
        let dir = TestDir::new("sqlite-open");
        let store = SqliteStore::open(&dir.join("trunk-pro.sqlite")).unwrap();
        assert!(dir.join("trunk-pro.sqlite").exists());
        assert_eq!(store.scalar("pragma user_version"), i64::from(schema::SCHEMA_VERSION));
        let mode: String = store.read(|c| c.query_row("pragma journal_mode", [], |r| r.get(0))).unwrap();
        assert_eq!(mode, "wal");
        let tables: Vec<String> = store.read(|c| c.prepare("select name from sqlite_master where type='table' order by name")?.query_map([], |r| r.get(0))?.collect()).unwrap();
        assert_eq!(tables, ["call_frequencies", "call_transmissions", "calls", "kept", "meta", "registry", "registry_changes", "talkgroups"]);
    }

    /// A second opener of the same store (another recorder on the same data
    /// folder) is refused, so there's only ever one writer; once the first
    /// lets go, it opens.
    #[test]
    fn a_second_opener_is_refused_while_the_first_holds_it() {
        let (db, path) = Db::for_test("lock");
        let err = SqliteStore::open(&path).err().expect("refused");
        assert!(err.contains("another Trunk Recorder Pro"), "{err}");
        let dir = TestDir::new("db-lock-2");
        let other = dir.join("trunk-pro.sqlite");
        std::fs::copy(&path, &other).unwrap();
        drop(db);
        assert!(SqliteStore::open(&other).is_ok());
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_a_fresh_one_opened() {
        let dir = TestDir::new("db-corrupt");
        let path = dir.join("trunk-pro.sqlite");
        std::fs::write(&path, b"this is not a database").unwrap();
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.scalar("pragma user_version"), 1);
        assert_eq!(aside_count(&dir), 1);
    }

    #[test]
    fn a_database_from_a_newer_version_is_refused_not_moved_aside() {
        let dir = TestDir::new("db-newer");
        let path = dir.join("trunk-pro.sqlite");
        seed_newer(&path);
        let err = SqliteStore::open(&path).err().expect("a newer schema must be refused");
        assert!(err.contains("99"), "{err}");
        assert!(path.exists());
        assert_eq!(aside_count(&dir), 0);
        assert!(Db::open_or_off(&path, Duration::from_secs(1), 0).is_disabled(), "the recorder carries on with the store off");
    }

    #[test]
    fn the_reader_pool_keeps_no_more_than_it_opened() {
        let dir = TestDir::new("sqlite-pool");
        let store = SqliteStore::open(&dir.join("trunk-pro.sqlite")).unwrap();
        // Three at once: the third is opened for the occasion.
        fn nest(s: &SqliteStore, depth: usize) -> rusqlite::Result<()> {
            if depth == 0 {
                return Ok(());
            }
            s.read(|_| nest(s, depth - 1)).map_err(rusqlite::Error::InvalidParameterName)
        }
        nest(&store, READERS + 1).unwrap();
        assert_eq!(store.readers_pooled(), READERS);
        assert_eq!(store.scalar("pragma busy_timeout"), 1000, "a reader waits a second for a lock");
    }

    #[test]
    fn a_path_that_cannot_be_opened_is_not_called_corrupt() {
        let dir = TestDir::new("db-notdir");
        let file = dir.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(open_sound(&file.join("trunk-pro.sqlite")), Err(OpenError::Other(_))));
        assert_eq!(aside_count(&dir), 0);
    }

    #[test]
    fn move_aside_keeps_the_wal_and_puts_everything_back_on_failure() {
        let dir = TestDir::new("db-aside");
        let path = dir.join("x.sqlite");
        std::fs::write(&path, b"db").unwrap();
        std::fs::write(dir.join("x.sqlite-wal"), b"wal").unwrap();
        let aside = move_aside(&path).unwrap();
        assert!(!path.exists());
        let mut wal = aside.as_os_str().to_owned();
        wal.push("-wal");
        assert_eq!(std::fs::read(PathBuf::from(wal)).unwrap(), b"wal");
        // The database file is missing, so the final rename fails: the WAL must be back where it was.
        let path2 = dir.join("y.sqlite");
        std::fs::write(dir.join("y.sqlite-wal"), b"wal2").unwrap();
        assert!(move_aside(&path2).is_err());
        assert_eq!(std::fs::read(dir.join("y.sqlite-wal")).unwrap(), b"wal2");
    }
}
