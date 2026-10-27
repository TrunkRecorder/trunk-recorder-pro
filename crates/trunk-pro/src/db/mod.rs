//! The call store: a SQLite index of recorded calls. One writer thread
//! batches writes into transactions; a small pool of read-only connections
//! serves queries without ever waiting on it. Nothing here is fatal: a store
//! that cannot be opened is off, and once it is open errors are logged and
//! the recorder carries on.

pub mod backfill;
pub mod read;
pub mod row;
pub mod schema;
pub mod writer;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use row::CallRow;

/// Writes the channel holds before `send` blocks.
const QUEUE_LEN: usize = 4096;
/// Read-only connections opened up front, and the most the pool keeps.
const READERS: usize = 2;
/// A day, in milliseconds.
pub(super) const DAY_MS: i64 = 86_400_000;

/// A change for the writer thread to make.
#[allow(clippy::large_enum_variant)] // one row per call; boxing it would only add a hop
pub enum Write {
    /// Insert or update a call's row.
    Call { row: CallRow, live: bool },
    /// Mark which of a call's files exist.
    CallFiles {
        path: String,
        audio: bool,
        json: bool,
    },
    /// Set a `meta` value.
    Meta { key: String, value: String },
    /// Delete calls that started before this time (the writer also does so
    /// by itself, for `retentionDays`).
    Retention { before_ms: i64 },
    /// Checkpoint the WAL into the database file.
    Checkpoint,
    /// Answered with the outcome of the commit that took every write sent
    /// before it.
    Flush(std::sync::mpsc::Sender<Result<(), String>>),
}

/// A handle on the call store. Clones share one database and one writer;
/// the writer stops when the last clone is dropped. A store that could not be
/// opened is off ([`Db::disabled`]): it drops every write and answers every
/// read with an error, and the recorder records as ever without it.
#[derive(Clone)]
pub struct Db(Option<Arc<Inner>>);

struct Inner {
    path: PathBuf,
    recorder: Option<String>,
    /// Taken when the last handle goes, which closes the writer's channel.
    tx: Option<SyncSender<Write>>,
    /// The writer thread, waited for when the last handle goes.
    writer: Option<JoinHandle<()>>,
    backfill_running: AtomicBool,
    #[cfg(test)]
    commits: Arc<AtomicU64>,
    readers: Mutex<Vec<Connection>>,
    /// Held while open, so a second recorder on the same data folder runs
    /// with its store off rather than as a second writer.
    _lock: std::fs::File,
    /// The test's folder, removed once the connections above are closed.
    #[cfg(test)]
    dir: Option<TestDir>,
}

impl Drop for Inner {
    /// Close the writer's channel and wait for its last commit, so the file
    /// is let go of when the last handle is.
    fn drop(&mut self) {
        self.tx.take();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

impl Db {
    /// Open (creating if need be) the database at `path`, migrate it and start
    /// its writer. A file that is not a sound database is moved aside and
    /// replaced. The writer commits at most once per `batch` under load, and
    /// keeps `retention_days` of calls (0: all).
    pub fn open(path: &Path, batch: Duration, retention_days: u32) -> Result<Db, String> {
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let lock = lock(path)?;
        log::debug!("Call store: schema version {}", schema::SCHEMA_VERSION);
        let conn = match open_sound(path) {
            Ok(c) => c,
            Err(OpenError::Corrupt(e)) if path.exists() => {
                let aside = move_aside(path)?;
                log::warn!(
                    "Call store: {} is unusable ({e}); moved to {}",
                    path.display(),
                    aside.display()
                );
                open_sound(path).map_err(|e| format!("open {}: {e}", path.display()))?
            }
            Err(e) => return Err(format!("open {}: {e}", path.display())),
        };
        let recorder = std::fs::read_to_string(dir.join("directory-recorder-id"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let readers = (0..READERS)
            .map(|_| open_reader(path))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("open reader {}: {e}", path.display()))?;
        let (tx, rx) = sync_channel(QUEUE_LEN);
        let commits = Arc::new(AtomicU64::new(0));
        let writer = writer::spawn(conn, rx, commits.clone(), batch, retention_days);
        Ok(Db(Some(Arc::new(Inner {
            path: path.to_path_buf(),
            recorder,
            tx: Some(tx),
            writer,
            backfill_running: AtomicBool::new(false),
            #[cfg(test)]
            commits,
            readers: Mutex::new(readers),
            _lock: lock,
            #[cfg(test)]
            dir: None,
        }))))
    }

    /// [`Db::open`], or the store off when it cannot be opened (a newer
    /// schema, a full disk, no permission, another instance holding it): the
    /// one error logged says why, and recording goes on without it.
    pub fn open_or_off(path: &Path, batch: Duration, retention_days: u32) -> Db {
        Db::open(path, batch, retention_days).unwrap_or_else(|e| {
            log::error!("Call store: off: {e}");
            Db::disabled()
        })
    }

    /// The store off: writes are dropped without a word, reads are errors,
    /// and nothing runs in the background.
    pub fn disabled() -> Db {
        Db(None)
    }

    /// The store could not be opened and is off.
    pub fn is_disabled(&self) -> bool {
        self.0.is_none()
    }

    /// The database file; `None` when the store is off.
    pub fn path(&self) -> Option<&Path> {
        self.0.as_deref().map(|i| i.path.as_path())
    }

    /// The directory recorder's id, if `directory-recorder-id` sits beside the
    /// database and is not empty. Read once, at open.
    pub fn recorder(&self) -> Option<&str> {
        self.0.as_deref()?.recorder.as_deref()
    }

    /// Queue a write. Blocks only while the queue is full. False when the
    /// write was dropped: the writer has stopped, or the store is off.
    pub fn send(&self, w: Write) -> bool {
        let Some(inner) = self.0.as_deref() else {
            return false;
        };
        let Some(tx) = &inner.tx else {
            return false;
        };
        if tx.send(w).is_err() {
            log::warn!("Call store: the writer has stopped; write dropped");
            return false;
        }
        true
    }

    /// The time retention and the backfill count back from: `now_ms`, but at
    /// most a day past the newest call in the store, so a clock that
    /// jumps forward neither empties the store nor leaves the archive out.
    pub fn now_ms(&self, now_ms: i64) -> i64 {
        let newest = self.with_reader(|c| {
            c.query_row("select max(start_ms) from calls", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
        });
        match newest {
            Ok(Some(newest)) => now_ms.min(newest.saturating_add(DAY_MS)),
            _ => now_ms,
        }
    }

    /// Where keeping `days` days of calls cuts at `now_ms`: calls that
    /// started before it go.
    pub fn retention_cutoff(&self, days: u32, now_ms: i64) -> i64 {
        self.now_ms(now_ms) - i64::from(days) * DAY_MS
    }

    /// Claim the right to run a backfill: false when one is already running
    /// or the store is off.
    pub fn try_begin_backfill(&self) -> bool {
        self.0.as_deref().is_some_and(|i| {
            i.backfill_running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
    }

    /// Give the right to run a backfill back.
    pub fn end_backfill(&self) {
        if let Some(i) = self.0.as_deref() {
            i.backfill_running.store(false, Ordering::Release);
        }
    }

    /// Run `f` on a read-only connection; an error when the store is off.
    pub fn with_reader<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, String> {
        let Some(inner) = self.0.as_deref() else {
            return Err("the call store is off".into());
        };
        let pooled = inner
            .readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop();
        let conn = match pooled {
            Some(c) => c,
            None => open_reader(&inner.path).map_err(|e| format!("open reader: {e}"))?,
        };
        let out = f(&conn).map_err(|e| e.to_string());
        let mut pool = inner.readers.lock().unwrap_or_else(|e| e.into_inner());
        // A connection opened while the pool was empty is closed, not kept.
        if pool.len() < READERS {
            pool.push(conn);
        }
        out
    }

    /// Return once every write sent before this call has been through a
    /// commit; `Err` if that commit failed or the writer is gone. At once
    /// when the store is off.
    pub fn flush(&self) -> Result<(), String> {
        if self.is_disabled() {
            return Ok(());
        }
        let (done, wait) = std::sync::mpsc::channel();
        self.send(Write::Flush(done));
        wait.recv()
            .map_err(|_| "the writer has stopped".to_string())?
    }

    /// Transactions the writer has committed since open.
    #[cfg(test)]
    pub fn commits(&self) -> u64 {
        self.0
            .as_deref()
            .map_or(0, |i| i.commits.load(Ordering::Relaxed))
    }

    /// A fresh database in a folder of its own under the temp folder, named
    /// for the test; the folder is removed with the last handle.
    #[cfg(test)]
    pub fn for_test(name: &str) -> (Db, PathBuf) {
        Db::for_test_with_batch(name, Duration::from_secs(1))
    }

    /// As [`Db::for_test`], committing every `batch`.
    #[cfg(test)]
    pub fn for_test_with_batch(name: &str, batch: Duration) -> (Db, PathBuf) {
        let dir = TestDir::new(&format!("db-{name}"));
        let path = dir.join("trunk-pro.sqlite");
        let mut db = Db::open(&path, batch, 0).expect("open test db");
        if let Some(inner) = db.0.as_mut().and_then(Arc::get_mut) {
            inner.dir = Some(dir);
        }
        (db, path)
    }
}

/// A test's folder under the temp folder: made empty, removed on drop.
#[cfg(test)]
pub struct TestDir(PathBuf);

#[cfg(test)]
impl TestDir {
    pub fn new(name: &str) -> TestDir {
        let p = std::env::temp_dir().join(format!("trunk-pro-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("make the test folder");
        TestDir(p)
    }
}

#[cfg(test)]
impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A database at `path` whose schema version says a newer build wrote it.
#[cfg(test)]
pub fn seed_newer(path: &Path) {
    drop(Db::open(path, Duration::from_secs(1), 0).unwrap());
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_creates_the_file_and_schema_version_1() {
        let (db, path) = Db::for_test("open");
        assert!(path.exists());
        let v: u32 = db
            .with_reader(|c| c.query_row("pragma user_version", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(v, schema::SCHEMA_VERSION);
        let mode: String = db
            .with_reader(|c| c.query_row("pragma journal_mode", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(mode, "wal");
        let tables: Vec<String> = db
            .with_reader(|c| {
                c.prepare("select name from sqlite_master where type='table' order by name")?
                    .query_map([], |r| r.get(0))?
                    .collect()
            })
            .unwrap();
        assert_eq!(
            tables,
            ["call_frequencies", "call_transmissions", "calls", "meta"]
        );
    }

    #[test]
    fn the_last_handle_removes_a_test_folder() {
        let (db, path) = Db::for_test("cleanup");
        db.send(Write::Meta {
            key: "a".into(),
            value: "1".into(),
        });
        drop(db);
        assert!(
            !path.parent().unwrap().exists(),
            "the writer has let go and the folder is gone"
        );
    }

    #[test]
    fn retention_counts_back_from_the_newest_call_when_the_clock_is_ahead() {
        let (db, _) = Db::for_test("retention-clock");
        assert_eq!(
            db.retention_cutoff(120, 1000 * DAY_MS),
            880 * DAY_MS,
            "an empty store counts back from now"
        );
        let r = row::tests::record(1, 10 * DAY_MS);
        db.send(Write::Call {
            row: row::call_row("s/a", &r, &r.to_json(), true, true, None, 0),
            live: true,
        });
        db.flush().unwrap();
        // The clock jumped 990 days ahead of the newest call.
        let cut = db.retention_cutoff(120, 1000 * DAY_MS);
        assert_eq!(cut, 11 * DAY_MS - 120 * DAY_MS);
        assert!(cut < 10 * DAY_MS, "the call is kept");
        assert_eq!(
            db.retention_cutoff(120, 10 * DAY_MS),
            -110 * DAY_MS,
            "a clock that is right counts from now"
        );
    }

    /// A second opener of the same store (another recorder on the same data
    /// folder) is refused, so there's only ever one writer; once the first
    /// lets go, it opens.
    #[test]
    fn a_second_opener_is_refused_while_the_first_holds_it() {
        let (db, path) = Db::for_test("lock");
        let err = Db::open(&path, Duration::from_secs(1), 0).err().expect("refused");
        assert!(err.contains("another Trunk Recorder Pro"), "{err}");
        let dir = TestDir::new("db-lock-2");
        let other = dir.join("trunk-pro.sqlite");
        std::fs::copy(&path, &other).unwrap();
        drop(db);
        assert!(Db::open(&other, Duration::from_secs(1), 0).is_ok());
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_a_fresh_one_opened() {
        let dir = TestDir::new("db-corrupt");
        let path = dir.join("trunk-pro.sqlite");
        std::fs::write(&path, b"this is not a database").unwrap();
        let db = Db::open(&path, Duration::from_secs(1), 0).unwrap();
        let v: u32 = db
            .with_reader(|c| c.query_row("pragma user_version", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(v, 1);
        let aside = std::fs::read_dir(&*dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(aside, 1);
        drop(db);
    }

    #[test]
    fn writes_are_batched_into_few_transactions() {
        let (db, _) = Db::for_test("batch");
        for i in 0..1000 {
            db.send(Write::Meta {
                key: format!("k{i}"),
                value: "v".into(),
            });
        }
        db.flush().unwrap();
        let n: u32 = db
            .with_reader(|c| c.query_row("select count(*) from meta", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 1000);
        assert!(
            db.commits() <= 10,
            "{} commits for 1000 writes",
            db.commits()
        );
    }

    #[test]
    fn a_quiet_write_commits_promptly() {
        let (db, _) = Db::for_test("prompt");
        let t = std::time::Instant::now();
        db.send(Write::Meta {
            key: "a".into(),
            value: "1".into(),
        });
        db.flush().unwrap();
        assert!(t.elapsed() < std::time::Duration::from_millis(1500));
        let v: String = db
            .with_reader(|c| c.query_row("select value from meta where key='a'", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(v, "1");
    }

    #[test]
    fn a_database_from_a_newer_version_is_refused_not_moved_aside() {
        let dir = TestDir::new("db-newer");
        let path = dir.join("trunk-pro.sqlite");
        seed_newer(&path);
        let err = Db::open(&path, Duration::from_secs(1), 0)
            .err()
            .expect("a newer schema must be refused");
        assert!(err.contains("99"), "{err}");
        assert!(path.exists());
        let aside = std::fs::read_dir(&*dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(aside, 0);
        assert!(
            Db::open_or_off(&path, Duration::from_secs(1), 0).is_disabled(),
            "the recorder carries on with the store off"
        );
    }

    #[test]
    fn a_store_that_is_off_takes_writes_and_answers_reads_with_an_error() {
        let db = Db::disabled();
        assert!(db.is_disabled());
        assert!(!db.send(Write::Checkpoint));
        assert_eq!(
            db.with_reader(|_| Ok(())).unwrap_err(),
            "the call store is off"
        );
        assert!(db.flush().is_ok());
        assert_eq!((db.recorder(), db.path()), (None, None));
        assert!(!db.try_begin_backfill());
        assert_eq!(db.now_ms(5), 5);
    }

    #[test]
    fn an_empty_recorder_id_is_none() {
        let dir = TestDir::new("db-recorder-id");
        std::fs::write(dir.join("directory-recorder-id"), b" \n").unwrap();
        let db = Db::open(&dir.join("trunk-pro.sqlite"), Duration::from_secs(1), 0).unwrap();
        assert_eq!(db.recorder(), None);
        drop(db);
        std::fs::write(dir.join("directory-recorder-id"), b"rec-1\n").unwrap();
        let db = Db::open(&dir.join("trunk-pro.sqlite"), Duration::from_secs(1), 0).unwrap();
        assert_eq!(db.recorder(), Some("rec-1"));
    }

    #[test]
    fn the_reader_pool_keeps_no_more_than_it_opened() {
        let (db, _) = Db::for_test("pool");
        // Five at once: the fifth is opened for the occasion.
        fn nest(db: &Db, depth: usize) -> rusqlite::Result<()> {
            if depth == 0 {
                return Ok(());
            }
            db.with_reader(|_| nest(db, depth - 1))
                .map_err(rusqlite::Error::InvalidParameterName)
        }
        nest(&db, READERS + 1).unwrap();
        let pooled = db.0.as_ref().unwrap().readers.lock().unwrap().len();
        assert_eq!(pooled, READERS);
        let wait: i64 = db
            .with_reader(|c| c.query_row("pragma busy_timeout", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(wait, 1000, "a reader waits a second for a lock");
    }

    #[test]
    fn a_path_that_cannot_be_opened_is_not_called_corrupt() {
        let dir = TestDir::new("db-notdir");
        let file = dir.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let path = file.join("trunk-pro.sqlite");
        assert!(matches!(open_sound(&path), Err(OpenError::Other(_))));
        let aside = std::fs::read_dir(&*dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(aside, 0);
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
        // The database file is missing, so the final rename fails: the WAL
        // must be back where it was.
        let path2 = dir.join("y.sqlite");
        std::fs::write(dir.join("y.sqlite-wal"), b"wal2").unwrap();
        assert!(move_aside(&path2).is_err());
        assert_eq!(std::fs::read(dir.join("y.sqlite-wal")).unwrap(), b"wal2");
    }
}
