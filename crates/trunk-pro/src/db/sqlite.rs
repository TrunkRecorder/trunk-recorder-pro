//! The SQLite backend: one file, `<data>/trunk-pro.sqlite`, in WAL mode with
//! `synchronous=NORMAL`. The writer's connection is the only one that
//! writes; a small pool of read-only connections answers queries without
//! waiting on it. A lock file beside it keeps a second recorder on the same
//! data folder from becoming a second writer. A file that isn't a database is
//! moved aside (its WAL with it) and a fresh one started.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::types::{Value as Lit, ValueRef};
use rusqlite::{params_from_iter, Connection, OpenFlags, TransactionBehavior};

use super::schema;
use super::sql::{question_marks, Dialect, Exec, Row, V};
use super::store::{Backend, Store};

/// Read-only connections opened up front, and the most the pool keeps.
const READERS: usize = 2;

pub struct SqliteBackend {
    path: PathBuf,
    /// The writer's connection (only the writer thread uses it).
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    /// Held while open, so a second recorder on the same data folder runs
    /// with its store off rather than as a second writer.
    _lock: std::fs::File,
}

/// The store in the SQLite database at `path` ([`SqliteBackend::open`]).
pub fn open(path: &Path) -> Result<Store, String> {
    SqliteBackend::open(path).map(|b| Store::new(Box::new(b)))
}

impl SqliteBackend {
    /// Open (creating if need be) the database at `path`, at this build's
    /// schema. A file that is not a sound database, or another version's, is
    /// moved aside and a fresh one made.
    pub fn open(path: &Path) -> Result<SqliteBackend, String> {
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let lock = lock(path)?;
        let conn = match open_sound(path) {
            Ok(c) => c,
            Err(OpenError::Corrupt(e)) if path.exists() => {
                let aside = move_aside(path, "corrupt")?;
                log::warn!("Call store: {} is unusable ({e}); moved to {}", path.display(), aside.display());
                open_sound(path).map_err(|e| format!("open {}: {e}", path.display()))?
            }
            // (Nothing is migrated while in development: another version's store starts afresh.)
            Err(OpenError::Version(v)) => {
                let aside = move_aside(path, &format!("v{v}"))?;
                log::warn!("Call store: {} has schema version {v}, not this build's {}; moved to {} and started afresh", path.display(), schema::SCHEMA_VERSION, aside.display());
                open_sound(path).map_err(|e| format!("open {}: {e}", path.display()))?
            }
            Err(e) => return Err(format!("open {}: {e}", path.display())),
        };
        let readers = (0..READERS).map(|_| open_reader(path)).collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("open reader {}: {e}", path.display()))?;
        Ok(SqliteBackend { path: path.to_path_buf(), writer: Mutex::new(conn), readers: Mutex::new(readers), _lock: lock })
    }

    #[cfg(test)]
    pub fn readers_pooled(&self) -> usize {
        self.readers.lock().unwrap().len()
    }
}

impl Backend for SqliteBackend {
    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn location(&self) -> String {
        self.path.display().to_string()
    }

    fn write(&self, f: &mut dyn FnMut(&mut dyn Exec) -> Result<(), String>) -> Result<(), String> {
        let mut conn = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| format!("could not begin a transaction: {e}"))?;
        f(&mut Lite(&tx))?;
        tx.commit().map_err(|e| format!("commit failed: {e}"))
    }

    /// `f` on a read-only connection from the pool (one opened for the
    /// occasion when all are out).
    fn read(&self, f: &mut dyn FnMut(&mut dyn Exec) -> Result<(), String>) -> Result<(), String> {
        let pooled = self.readers.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let conn = match pooled {
            Some(c) => c,
            None => open_reader(&self.path).map_err(|e| format!("open reader: {e}"))?,
        };
        let out = f(&mut Lite(&conn));
        let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < READERS {
            pool.push(conn);
        }
        out
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
}

/// Statements on a SQLite connection (a transaction's, or a reader's).
struct Lite<'a>(&'a Connection);

fn lit(v: &V) -> Lit {
    match v {
        V::Null => Lit::Null,
        V::Int(i) => Lit::Integer(*i),
        V::Real(r) => Lit::Real(*r),
        V::Bool(b) => Lit::Integer(i64::from(*b)),
        V::Text(t) => Lit::Text(t.clone()),
    }
}

fn value(v: ValueRef) -> V {
    match v {
        ValueRef::Null => V::Null,
        ValueRef::Integer(i) => V::Int(i),
        ValueRef::Real(r) => V::Real(r),
        ValueRef::Text(t) | ValueRef::Blob(t) => V::Text(String::from_utf8_lossy(t).into_owned()),
    }
}

impl Exec for Lite<'_> {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    fn execute(&mut self, sql: &str, ps: &[V]) -> Result<u64, String> {
        let mut st = self.0.prepare_cached(&question_marks(sql)).map_err(|e| e.to_string())?;
        st.execute(params_from_iter(ps.iter().map(lit))).map(|n| n as u64).map_err(|e| e.to_string())
    }

    fn query(&mut self, sql: &str, ps: &[V]) -> Result<Vec<Row>, String> {
        let mut st = self.0.prepare_cached(&question_marks(sql)).map_err(|e| e.to_string())?;
        let n = st.column_count();
        let mut rows = st.query(params_from_iter(ps.iter().map(lit))).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            out.push(Row((0..n).map(|i| r.get_ref(i).map_or(V::Null, value)).collect()));
        }
        Ok(out)
    }

    fn batch(&mut self, sql: &str) -> Result<(), String> {
        self.0.execute_batch(sql).map_err(|e| e.to_string())
    }
}

/// Why the writer's connection could not be opened.
enum OpenError {
    /// The file is not a sound database: it may be moved aside.
    Corrupt(String),
    /// Another version's schema: it's moved aside.
    Version(u32),
    /// Anything else (busy, disk full): the file is left alone.
    Other(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(e) | Self::Other(e) => f.write_str(e),
            Self::Version(v) => write!(f, "schema version {v}"),
        }
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
    if let schema::Prepared::Other(v) = schema::prepare(&conn)? {
        return Err(OpenError::Version(v));
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
fn move_aside(path: &Path, why: &str) -> Result<PathBuf, String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let with = |base: &Path, suffix: &str| {
        let mut s = base.as_os_str().to_owned();
        s.push(suffix);
        PathBuf::from(s)
    };
    let aside = with(path, &format!(".{why}-{secs}"));
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





#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{seed_unopenable, Db, TestDir};
    use std::time::Duration;

    fn aside_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".corrupt-")).count()
    }

    #[test]
    fn opening_creates_the_file_in_wal_at_the_current_version() {
        let dir = TestDir::new("sqlite-open");
        let store = open(&dir.join("trunk-pro.sqlite")).unwrap();
        assert!(dir.join("trunk-pro.sqlite").exists());
        assert_eq!(store.scalar("pragma user_version"), i64::from(schema::SCHEMA_VERSION));
        assert_eq!(store.texts("pragma journal_mode"), ["wal"]);
        let tables = store.texts("select name from sqlite_master where type='table' order by name");
        assert_eq!(tables, ["call_frequencies", "call_transmissions", "call_units", "calls", "kept", "meta", "registry", "registry_changes", "stats_hours", "stats_minutes", "talkgroups"]);
    }

    /// A second opener of the same store (another recorder on the same data
    /// folder) is refused, so there's only ever one writer; once the first
    /// lets go, it opens.
    #[test]
    fn a_second_opener_is_refused_while_the_first_holds_it() {
        let (db, path) = Db::for_test("lock");
        let err = open(&path).err().expect("refused");
        assert!(err.contains("another Trunk Recorder Pro"), "{err}");
        let dir = TestDir::new("db-lock-2");
        let other = dir.join("trunk-pro.sqlite");
        std::fs::copy(&path, &other).unwrap();
        drop(db);
        assert!(open(&other).is_ok());
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_a_fresh_one_opened() {
        let dir = TestDir::new("db-corrupt");
        let path = dir.join("trunk-pro.sqlite");
        std::fs::write(&path, b"this is not a database").unwrap();
        let store = open(&path).unwrap();
        assert_eq!(store.scalar("pragma user_version"), i64::from(schema::SCHEMA_VERSION));
        assert_eq!(aside_count(&dir), 1);
    }

    /// Another version's store (older or newer: nothing is migrated) is
    /// moved aside and a fresh one made; one that can't be opened at all
    /// leaves the store off.
    #[test]
    fn another_version_s_store_is_set_aside_and_made_afresh() {
        let dir = TestDir::new("db-version");
        let path = dir.join("trunk-pro.sqlite");
        drop(open(&path).unwrap());
        rusqlite::Connection::open(&path).unwrap().pragma_update(None, "user_version", 99).unwrap();
        let store = open(&path).unwrap();
        assert_eq!(store.scalar("pragma user_version"), i64::from(schema::SCHEMA_VERSION));
        let aside = std::fs::read_dir(&*dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".v99-")).count();
        assert_eq!(aside, 1, "kept beside, not deleted");
        drop(store);
        let off = dir.join("off.sqlite");
        seed_unopenable(&off);
        assert!(Db::open_or_off(&off, Duration::from_secs(1), 0).is_disabled(), "the recorder carries on with the store off");
    }

    #[test]
    fn the_reader_pool_keeps_no_more_than_it_opened() {
        let dir = TestDir::new("sqlite-pool");
        let b = SqliteBackend::open(&dir.join("trunk-pro.sqlite")).unwrap();
        // Three at once: the third is opened for the occasion.
        fn nest(b: &SqliteBackend, depth: usize) -> Result<(), String> {
            if depth == 0 {
                return Ok(());
            }
            b.read(&mut |_| nest(b, depth - 1))
        }
        nest(&b, READERS + 1).unwrap();
        assert_eq!(b.readers_pooled(), READERS);
        let store = Store::new(Box::new(b));
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
        let aside = move_aside(&path, "corrupt").unwrap();
        assert!(!path.exists());
        let mut wal = aside.as_os_str().to_owned();
        wal.push("-wal");
        assert_eq!(std::fs::read(PathBuf::from(wal)).unwrap(), b"wal");
        // The database file is missing, so the final rename fails: the WAL must be back where it was.
        let path2 = dir.join("y.sqlite");
        std::fs::write(dir.join("y.sqlite-wal"), b"wal2").unwrap();
        assert!(move_aside(&path2, "corrupt").is_err());
        assert_eq!(std::fs::read(dir.join("y.sqlite-wal")).unwrap(), b"wal2");
    }
}
