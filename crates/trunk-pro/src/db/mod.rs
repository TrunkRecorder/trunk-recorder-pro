//! The call store: a SQLite index of recorded calls. One writer thread
//! batches writes into transactions; a small pool of read-only connections
//! serves queries without ever waiting on it. Nothing here is fatal once the
//! file is open: errors are logged and the recorder carries on.

pub mod read;
pub mod row;
pub mod schema;
pub mod writer;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags};

use crate::runtime::Hub;

#[allow(unused_imports)] // read by later tasks
pub use read::{CallsFilter, Page, newest, older, one, query};
#[allow(unused_imports)] // read by later tasks
pub use row::{CallRow, call_row, entry};
#[allow(unused_imports)] // read by later tasks
pub use schema::SCHEMA_VERSION;

/// Writes the channel holds before `send` blocks.
const QUEUE_LEN: usize = 4096;
/// Read-only connections opened up front.
const READERS: usize = 4;

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
    /// Delete calls that started before this time.
    Retention { before_ms: i64 },
    /// Checkpoint the WAL into the database file.
    Checkpoint,
    /// Answered with the outcome of the commit that took every write sent
    /// before it.
    Flush(std::sync::mpsc::Sender<Result<(), String>>),
}

/// A handle on the call store. Clones share one database and one writer;
/// the writer stops when the last clone is dropped.
#[derive(Clone)]
pub struct Db(Arc<Inner>);

struct Inner {
    path: PathBuf,
    recorder: Option<String>,
    tx: SyncSender<Write>,
    queued: Arc<AtomicUsize>,
    commits: Arc<AtomicU64>,
    readers: Mutex<Vec<Connection>>,
}

impl Db {
    /// Open (creating if need be) the database at `path`, migrate it and start
    /// its writer. A file that is not a sound database is moved aside and
    /// replaced.
    pub fn open(path: &Path, hub: Hub) -> Result<Db, String> {
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let conn = match open_sound(path) {
            Ok(c) => c,
            Err(OpenError::Corrupt(e)) if path.exists() => {
                let aside = move_aside(path)?;
                log::warn!(
                    "db: {} is unusable ({e}); moved to {}",
                    path.display(),
                    aside.display()
                );
                open_sound(path).map_err(|e| format!("open {}: {e}", path.display()))?
            }
            Err(e) => return Err(format!("open {}: {e}", path.display())),
        };
        let recorder = std::fs::read_to_string(dir.join("directory-recorder-id"))
            .ok()
            .map(|s| s.trim().to_string());
        let readers = (0..READERS)
            .map(|_| open_reader(path))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("open reader {}: {e}", path.display()))?;
        let (tx, rx) = sync_channel(QUEUE_LEN);
        let queued = Arc::new(AtomicUsize::new(0));
        let commits = Arc::new(AtomicU64::new(0));
        writer::spawn(conn, hub, rx, commits.clone(), queued.clone());
        Ok(Db(Arc::new(Inner {
            path: path.to_path_buf(),
            recorder,
            tx,
            queued,
            commits,
            readers: Mutex::new(readers),
        })))
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// The directory recorder's id, if `directory-recorder-id` sits beside the
    /// database. Read once, at open.
    pub fn recorder(&self) -> Option<&str> {
        self.0.recorder.as_deref()
    }

    /// Queue a write. Blocks only while the queue is full.
    pub fn send(&self, w: Write) {
        self.0.queued.fetch_add(1, Ordering::Relaxed);
        if self.0.tx.send(w).is_err() {
            self.0.queued.fetch_sub(1, Ordering::Relaxed);
            log::warn!("db: the writer has stopped; write dropped");
        }
    }

    /// Writes queued and not yet taken up by the writer.
    pub fn queued(&self) -> usize {
        self.0.queued.load(Ordering::Relaxed)
    }

    /// Run `f` on a read-only connection.
    pub fn with_reader<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, String> {
        let pooled = self
            .0
            .readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop();
        let conn = match pooled {
            Some(c) => c,
            None => open_reader(&self.0.path).map_err(|e| format!("open reader: {e}"))?,
        };
        let out = f(&conn).map_err(|e| e.to_string());
        self.0
            .readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(conn);
        out
    }

    /// Return once every write sent before this call has been through a
    /// commit; `Err` if that commit failed or the writer is gone.
    pub fn flush(&self) -> Result<(), String> {
        let (done, wait) = std::sync::mpsc::channel();
        self.send(Write::Flush(done));
        wait.recv()
            .map_err(|_| "the writer has stopped".to_string())?
    }

    /// Transactions the writer has committed since open.
    pub fn commits(&self) -> u64 {
        self.0.commits.load(Ordering::Relaxed)
    }

    /// A fresh database under the temp folder, named for the test.
    #[cfg(test)]
    pub fn for_test(name: &str) -> (Db, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("trunk-pro-db-{name}-{}.sqlite", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let (hub, _) = tokio::sync::broadcast::channel(16);
        (Db::open(&path, hub).expect("open test db"), path)
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
    let check: String = conn.query_row("pragma quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(OpenError::Corrupt(check));
    }
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "wal_autocheckpoint", 16384)?;
    conn.pragma_update(None, "journal_size_limit", 67108864)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    schema::migrate(&conn)?;
    Ok(conn)
}

fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
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
                log::warn!("db: could not put {} back: {e}", from.display());
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
            .with_reader(|c| c.query_row("select version from schema_version", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
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
            [
                "call_frequencies",
                "call_transmissions",
                "calls",
                "meta",
                "schema_version"
            ]
        );
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_a_fresh_one_opened() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-db-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trunk-pro.sqlite");
        std::fs::write(&path, b"this is not a database").unwrap();
        let (hub, _) = tokio::sync::broadcast::channel(16);
        let db = Db::open(&path, hub).unwrap();
        let v: u32 = db
            .with_reader(|c| c.query_row("select version from schema_version", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(v, 1);
        let aside = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(aside, 1);
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
        let (db, path) = Db::for_test("newer");
        drop(db);
        // The writer thread exits once the last handle is gone; wait for it to
        // let go of the file before changing it from outside.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let conn = Connection::open(&path).unwrap();
        conn.execute("update schema_version set version = 99", [])
            .unwrap();
        drop(conn);
        let (hub, _) = tokio::sync::broadcast::channel(16);
        let err = Db::open(&path, hub)
            .err()
            .expect("a newer schema must be refused");
        assert!(err.contains("99"), "{err}");
        assert!(path.exists());
        let dir = path.parent().unwrap();
        let aside = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.contains("trunk-pro-db-newer") && n.contains(".corrupt-")
            })
            .count();
        assert_eq!(aside, 0);
    }

    #[test]
    fn a_path_that_cannot_be_opened_is_not_called_corrupt() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-db-notdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let path = file.join("trunk-pro.sqlite");
        assert!(matches!(open_sound(&path), Err(OpenError::Other(_))));
        let aside = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(aside, 0);
    }

    #[test]
    fn move_aside_keeps_the_wal_and_puts_everything_back_on_failure() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-db-aside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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
