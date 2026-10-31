//! The call store: an index of recorded calls, kept by a backend chosen in
//! the config (`storage.backend`: research/sqlite-plan.md, "Backends").
//! Above the backend, everything is shared: one writer thread batches writes
//! into transactions (`writer.rs`), queries page the same way (`read.rs`),
//! and the backfill walks the capture folder (`backfill.rs`). Nothing here
//! is fatal: a store that cannot be opened is off, and the recorder carries
//! on from its live list.

pub mod backfill;
pub mod postgres;
pub mod read;
pub mod row;
pub mod schema;
pub mod sql;
pub mod sqlite;
pub mod store;
pub mod writer;

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;


use store::{Store, StoreInfo};

/// Writes the channel holds before `send` blocks.
const QUEUE_LEN: usize = 4096;
/// A day, in milliseconds.
pub(super) const DAY_MS: i64 = 86_400_000;

pub use store::Change;

/// What the writer thread takes: changes, and what to do around them.
pub(crate) enum Msg {
    Change(Change),
    /// Fold what's pending into the store proper (SQLite: the WAL).
    Checkpoint,
    /// Answered with the outcome of the commit that took every change sent
    /// before it.
    Flush(std::sync::mpsc::Sender<Result<(), String>>),
    /// Delete calls that started before this time, as the writer does by
    /// itself for `retentionDays` (tests).
    #[cfg(test)]
    Retention { before_ms: i64 },
}

/// A handle on the call store. Clones share one backend and one writer; the
/// writer stops when the last clone is dropped. A store that could not be
/// opened is off ([`Db::disabled`]): it drops every write and answers every
/// read with an error, and the recorder records as ever without it.
#[derive(Clone)]
pub struct Db(Option<Arc<Inner>>, Option<Arc<str>>);

struct Inner {
    store: Arc<Store>,
    recorder: Option<String>,
    /// Taken when the last handle goes, which closes the writer's channel.
    tx: Option<SyncSender<Msg>>,
    /// The writer thread, waited for when the last handle goes.
    writer: Option<JoinHandle<()>>,
    backfill_running: AtomicBool,
    /// Calls are indexed (not in low-write mode).
    index_calls: bool,
    /// Calls are indexed by radio too (`storage.radioIndex`), and by talkgroup (`storage.talkgroupIndex`).
    index_radios: bool,
    index_talkgroups: bool,
    /// The bytes the writer thread has written to disk (`u64::MAX`: not measured here).
    written: Arc<AtomicU64>,
    /// Who's told when a change people made couldn't be saved.
    report: Reporter,
    /// The config's systems: each short name its calls were filed under (a
    /// system's own, and its sites' from before it had them) to its id.
    systems: std::sync::RwLock<Systems>,
    #[cfg(test)]
    commits: Arc<AtomicU64>,
    /// The test's folder, removed once the store is closed.
    #[cfg(test)]
    dir: Option<TestDir>,
}

/// Who's told when a change people made couldn't be saved ([`Db::on_failure`]).
pub(crate) type Reporter = Arc<std::sync::Mutex<Option<Box<dyn Fn(String) + Send + Sync>>>>;

/// Commits are at least this far apart in low-write mode.
const LOW_WRITE_COMMIT_S: u32 = 3600;

/// The config's systems as the store knows them ([`Db::set_systems`]).
#[derive(Default)]
struct Systems {
    by_name: std::collections::HashMap<String, i64>,
    /// Each system's id and short name, in the config's order.
    all: Vec<(i64, String)>,
    /// (Tests: any name is a system, given an id the first time it's seen.)
    #[cfg(test)]
    any: bool,
}

impl Drop for Inner {
    /// Close the writer's channel and wait for its last commit, so the store
    /// is let go of when the last handle is.
    fn drop(&mut self) {
        self.tx.take();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

/// The directory recorder's id, if `directory-recorder-id` in `dir` says one.
pub fn recorder_id(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("directory-recorder-id")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Db {
    /// The store `storage` asks for (`data` is the data folder). One that
    /// can't be opened is left off ([`Db::failure`] says why, and the
    /// interface shows it); recording goes on either way. Low-write mode
    /// keeps no call index and commits at most once an hour.
    pub fn from_settings(storage: &trunk_app::config::StorageSettings, data: &Path) -> Db {
        use trunk_app::config::StorageBackend;
        let range = trunk_app::config::StorageSettings::COMMIT_SECONDS;
        let commit = storage.commit_seconds.clamp(*range.start(), *range.end());
        if commit != storage.commit_seconds {
            log::warn!("storage.commitSeconds {} is outside {} to {}; using {commit}", storage.commit_seconds, range.start(), range.end());
        }
        let commit = if storage.low_write { commit.max(LOW_WRITE_COMMIT_S) } else { commit };
        let batch = Duration::from_secs(u64::from(commit));
        let opened = match storage.backend {
            StorageBackend::Sqlite => sqlite::open(&data.join("trunk-pro.sqlite")),
            StorageBackend::Postgres => postgres::open(&storage.url),
        };
        match opened {
            Ok(store) => {
                // (A store that doesn't index calls by radio or talkgroup drops
                // that index; one that does again makes it, which can take a while.)
                let radios = storage.radio_index && !storage.low_write;
                let talkgroups = storage.talkgroup_index && !storage.low_write;
                for (name, on) in [(schema::RADIO_INDEX, radios), (schema::TALKGROUP_INDEX, talkgroups)] {
                    if let Err(e) = store.optional_index(name, on) {
                        log::warn!("Store: index {name} not {} ({e})", if on { "made" } else { "dropped" });
                    }
                }
                let mut db = Db::with_store(Arc::new(store), batch, storage.retention_days, recorder_id(data));
                if let Some(inner) = db.0.as_mut().and_then(Arc::get_mut) {
                    inner.index_calls = !storage.low_write;
                    inner.index_radios = radios;
                    inner.index_talkgroups = talkgroups;
                }
                db
            }
            Err(e) => {
                log::error!("Store: off: {e}. Nothing is kept until it opens: not the calls' index, talkgroups, aliases, the radio registry or the dashboard's history");
                Db::failed(&e)
            }
        }
    }

    /// The store over `store`, its writer committing at most once per
    /// `batch` under load and keeping `retention_days` of calls (0: all).
    pub fn with_store(store: Arc<Store>, batch: Duration, retention_days: u32, recorder: Option<String>) -> Db {
        let (tx, rx) = sync_channel(QUEUE_LEN);
        let commits = Arc::new(AtomicU64::new(0));
        let written = Arc::new(AtomicU64::new(u64::MAX));
        let report: Reporter = Default::default();
        let writer = writer::spawn(store.clone(), rx, commits.clone(), written.clone(), batch, retention_days, report.clone());
        Db(
            Some(Arc::new(Inner {
            store,
            recorder,
            tx: Some(tx),
            writer,
            index_calls: true,
            index_radios: true,
            index_talkgroups: true,
            report,
            backfill_running: AtomicBool::new(false),
            systems: Default::default(),
            written,
            #[cfg(test)]
            commits,
            #[cfg(test)]
            dir: None,
        })),
            None,
        )
    }

    /// A SQLite store at `path` (the default backend; tests).
    #[cfg(test)]
    pub fn open(path: &Path, batch: Duration, retention_days: u32) -> Result<Db, String> {
        let store = sqlite::open(path)?;
        let dir = path.parent().unwrap_or(Path::new("."));
        Ok(Db::with_store(Arc::new(store), batch, retention_days, recorder_id(dir)))
    }

    /// [`Db::open`], or the store off when it cannot be opened (tests).
    #[cfg(test)]
    pub fn open_or_off(path: &Path, batch: Duration, retention_days: u32) -> Db {
        Db::open(path, batch, retention_days).unwrap_or_else(|e| {
            log::error!("Call store: off: {e}");
            Db::disabled()
        })
    }

    /// No store (tests): writes are dropped without a word, reads are
    /// errors, and nothing runs in the background.
    #[cfg(test)]
    pub fn disabled() -> Db {
        Db(None, None)
    }

    /// The store asked for, which could not be opened (`why`): off, and
    /// nothing kept in its place (not files: what's written then would be
    /// lost when it opens again).
    pub fn failed(why: &str) -> Db {
        Db(None, Some(why.into()))
    }

    /// Why the store asked for is off, if it is.
    pub fn failure(&self) -> Option<&str> {
        self.1.as_deref()
    }

    /// There is no store: it couldn't be opened (or a test has none).
    pub fn is_disabled(&self) -> bool {
        self.0.is_none()
    }

    /// The store keeps the call index (not in low-write mode).
    pub fn indexes_calls(&self) -> bool {
        self.0.as_deref().is_some_and(|i| i.index_calls)
    }

    /// Whether a radio's calls can be paged from the store (`storage.radioIndex`).
    pub fn indexes_radios(&self) -> bool {
        self.indexes_calls() && self.0.as_deref().is_some_and(|i| i.index_radios)
    }

    /// Whether a talkgroup's calls can be paged from the store (`storage.talkgroupIndex`).
    pub fn indexes_talkgroups(&self) -> bool {
        self.indexes_calls() && self.0.as_deref().is_some_and(|i| i.index_talkgroups)
    }

    /// Tell `f` of a change people made that couldn't be saved (a talkgroup
    /// edit: what to say to them).
    pub fn on_failure(&self, f: impl Fn(String) + Send + Sync + 'static) {
        if let Some(i) = self.0.as_deref() {
            *i.report.lock().unwrap() = Some(Box::new(f));
        }
    }

    fn store(&self) -> Result<&Store, String> {
        self.0.as_deref().map(|i| &*i.store).ok_or_else(|| "the call store is off".to_string())
    }

    /// Which backend, and where: `sqlite /path/trunk-pro.sqlite`; None when
    /// off. No query: safe to call from async code.
    pub fn describe(&self) -> Option<String> {
        let s = self.store().ok()?;
        Some(format!("{} {}", s.kind(), s.location()))
    }

    /// The backend's name (`off` when there's none). Safe from async code.
    pub fn backend(&self) -> &'static str {
        self.store().map_or("off", |s| s.kind())
    }

    /// The directory recorder's id, read once at open.
    pub fn recorder(&self) -> Option<&str> {
        self.0.as_deref()?.recorder.as_deref()
    }

    /// Queue a change. Blocks only while the queue is full. False when it
    /// was dropped: the writer has stopped, the store is off, or (a call) its
    /// system isn't the config's.
    pub fn send(&self, mut c: Change) -> bool {
        // (Low-write mode keeps no call index.)
        if matches!(c, Change::Call { .. } | Change::CallFiles { .. }) && !self.indexes_calls() {
            return false;
        }
        if let Change::Call { row, .. } = &mut c {
            let Some(id) = self.system_id(&row.short_name) else {
                log::debug!("Call store: {} is of a system not in the config ({}); not indexed", row.path, row.short_name);
                return false;
            };
            row.system_id = id;
        }
        self.post(Msg::Change(c))
    }

    /// The config's systems are now `cfg`'s: what names are resolved by,
    /// and the `systems` table (written first, before anything that refers
    /// to it).
    pub fn set_systems(&self, cfg: &trunk_app::Config) {
        let Some(inner) = self.0.as_deref() else { return };
        let mut by_name = std::collections::HashMap::new();
        let mut rows = Vec::new();
        for s in &cfg.systems {
            let id = i64::from(s.id);
            for name in std::iter::once(s.short_name.clone()).chain(trunk_app::migrate::old_names(s)) {
                by_name.entry(name).or_insert(id);
            }
            let kind = serde_json::to_value(s.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            rows.push((id, s.short_name.clone(), kind));
        }
        for c in &cfg.conventional {
            by_name.entry(c.short_name.clone()).or_insert(i64::from(c.id));
            rows.push((i64::from(c.id), c.short_name.clone(), "conventional".into()));
        }
        {
            let mut s = inner.systems.write().unwrap();
            s.by_name = by_name;
            s.all = rows.iter().map(|(id, n, _)| (*id, n.clone())).collect();
        }
        self.send(Change::Systems(rows));
    }

    /// The id of the system its calls file as `name`.
    pub fn system_id(&self, name: &str) -> Option<i64> {
        let inner = self.0.as_deref()?;
        if let Some(id) = inner.systems.read().unwrap().by_name.get(name) {
            return Some(*id);
        }
        #[cfg(test)]
        {
            let mut s = inner.systems.write().unwrap();
            if s.any {
                let id = 1000 + s.all.len() as i64;
                s.by_name.insert(name.to_string(), id);
                s.all.push((id, name.to_string()));
                drop(s);
                self.post(Msg::Change(Change::Systems(vec![(id, name.to_string(), "p25".into())])));
                return Some(id);
            }
        }
        None
    }

    /// The short name of system `id`, as the config has it now.
    pub fn system_name(&self, id: i64) -> Option<String> {
        self.0.as_deref()?.systems.read().unwrap().all.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone())
    }

    /// Every system of the config, by id.
    pub fn system_ids(&self) -> Vec<i64> {
        self.0.as_deref().map(|i| i.systems.read().unwrap().all.iter().map(|(id, _)| *id).collect()).unwrap_or_default()
    }

    /// Fold what's pending into the store proper (SQLite: the WAL), after
    /// what's queued.
    pub fn checkpoint(&self) -> bool {
        self.post(Msg::Checkpoint)
    }

    /// Have the writer cut calls started before `before_ms` (tests).
    #[cfg(test)]
    pub fn retire_for_test(&self, before_ms: i64) {
        self.post(Msg::Retention { before_ms });
    }

    fn post(&self, m: Msg) -> bool {
        let Some(tx) = self.0.as_deref().and_then(|i| i.tx.as_ref()) else { return false };
        if tx.send(m).is_err() {
            log::warn!("Call store: the writer has stopped; write dropped");
            return false;
        }
        true
    }

    /// A page of the calls `q` asks for, of `systems` (none: any), as
    /// committed ([`Store::calls`]).
    pub fn calls(&self, q: &trunk_app::calls::CallsQuery, systems: &[i64]) -> Result<trunk_app::calls::CallsPage, String> {
        self.store()?.calls(q, systems)
    }

    /// The call filed under `path`, as committed.
    pub fn one(&self, path: &str) -> Result<Option<trunk_app::calls::StoredCall>, String> {
        self.store()?.one(path)
    }

    pub fn known(&self, path: &str) -> Result<bool, String> {
        self.store()?.known(path)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>, String> {
        self.store()?.meta(key)
    }

    /// A kept document (`crate::kept`).
    pub fn kept(&self, kind: &str, key: &str) -> Result<Option<String>, String> {
        self.store()?.kept(kind, key)
    }

    /// The dashboard's minutes kept from `since` (Unix s) on, as JSON lines.
    pub fn stats_minutes(&self, since: i64) -> Result<Vec<String>, String> {
        self.store()?.stats_minutes(since)
    }

    /// The dashboard's hours, added up by bucket.
    pub fn stats_hours(&self, q: &store::HoursQuery) -> Result<Vec<(String, i64, trunk_app::stats::Bucket)>, String> {
        self.store()?.stats_hours(q)
    }

    /// A system's stored talkgroups, in their order.
    pub fn talkgroups(&self, system: &str) -> Result<Vec<trunk_core::trunk::TalkgroupRow>, String> {
        let Some(id) = self.system_id(system) else { return Ok(Vec::new()) };
        self.store()?.talkgroups(id)
    }

    /// A system's stored talker aliases.
    pub fn units(&self, system: &str) -> Result<Vec<(u32, trunk_core::trunk::UnitAlias)>, String> {
        let Some(id) = self.system_id(system) else { return Ok(Vec::new()) };
        self.store()?.units(id)
    }

    /// Every system's stored radio registry.
    pub fn registry(&self) -> Result<Vec<(i64, Option<trunk_app::stats::radio::SystemRadio>)>, String> {
        self.store()?.registry()
    }

    /// The bytes the store has written to disk since it opened: SQLite's
    /// writer thread's own count (Linux). None: not measured on this OS, or
    /// a PostgreSQL store (its server writes: [`Db::server_wal_bytes`]).
    pub fn written(&self) -> Option<u64> {
        let b = self.0.as_deref()?.written.load(Ordering::Relaxed);
        (b != u64::MAX && self.backend() == "sqlite").then_some(b)
    }

    /// PostgreSQL: the bytes its server's write-ahead log has had since its
    /// statistics were reset (the whole server's: every database on it).
    /// Blocks: never from async code.
    pub fn server_wal_bytes(&self) -> Option<i64> {
        self.store().ok()?.server_wal_bytes()
    }

    /// Each table's and index's size (blocks: never from async code).
    pub fn sizes(&self) -> Result<store::StoreSizes, String> {
        self.store()?.sizes()
    }

    pub fn info(&self) -> Result<StoreInfo, String> {
        self.store()?.info()
    }

    /// The time retention and the backfill count back from: `now_ms`, but at
    /// most a day past the newest call in the store, so a clock that
    /// jumps forward neither empties the store nor leaves the archive out.
    pub fn now_ms(&self, now_ms: i64) -> i64 {
        match self.store().and_then(|s| s.newest_start()) {
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
        self.0.as_deref().is_some_and(|i| i.backfill_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok())
    }

    /// Give the right to run a backfill back.
    pub fn end_backfill(&self) {
        if let Some(i) = self.0.as_deref() {
            i.backfill_running.store(false, Ordering::Release);
        }
    }

    /// Return once every write sent before this call has been through a
    /// commit; `Err` if that commit failed or the writer is gone. At once
    /// when the store is off.
    pub fn flush(&self) -> Result<(), String> {
        if self.is_disabled() {
            return Ok(());
        }
        let (done, wait) = std::sync::mpsc::channel();
        self.post(Msg::Flush(done));
        wait.recv().map_err(|_| "the writer has stopped".to_string())?
    }

    /// Transactions the writer has committed since open.
    #[cfg(test)]
    pub fn commits(&self) -> u64 {
        self.0.as_deref().map_or(0, |i| i.commits.load(Ordering::Relaxed))
    }

    /// A number from a plain SQL query (tests).
    #[cfg(test)]
    pub fn count(&self, sql: &str) -> i64 {
        self.store().unwrap().scalar(sql)
    }

    /// Make system `system_id`'s registry changes over its whole copy now (tests).
    #[cfg(test)]
    pub fn compact_registry_for_test(&self, system_id: i64) {
        self.store().unwrap().compact_registry(system_id)
    }

    /// Make writing rows of `table` whose `column` is `value` (an SQL literal) fail (tests).
    #[cfg(test)]
    pub fn refuse_for_test(&self, table: &str, column: &str, value: &str) {
        self.store().unwrap().refuse(table, column, value)
    }

    /// A fresh SQLite store in a folder of its own under the temp folder,
    /// named for the test; the folder is removed with the last handle.
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
            inner.systems.get_mut().unwrap().any = true;
        }
        (db, path)
    }

    /// A fresh store of each backend there is to test: SQLite always, and
    /// PostgreSQL when `TRP_PG_URL` names a database (a schema of its own,
    /// named for the test). What every backend must do is tested on each.
    #[cfg(test)]
    pub fn for_each_backend(name: &str, batch: Duration) -> Vec<Db> {
        // (Two tests of one name would share a PostgreSQL schema, each dropping the other's tables.)
        static NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let mut names = NAMES.lock().unwrap();
        assert!(!names.iter().any(|n| n == name), "two tests' stores are named {name:?}");
        names.push(name.to_string());
        drop(names);
        let mut v = vec![Db::for_test_with_batch(name, batch).0];
        if let Ok(url) = std::env::var("TRP_PG_URL") {
            let store = postgres::open_fresh_schema(&url, &format!("t_{name}").replace('-', "_")).expect("open the test PostgreSQL schema");
            let mut db = Db::with_store(Arc::new(store), batch, 0, None);
            if let Some(inner) = db.0.as_mut().and_then(Arc::get_mut) {
                inner.systems.get_mut().unwrap().any = true;
            }
            v.push(db);
        }
        v
    }

    /// The backend's name (tests).
    #[cfg(test)]
    pub fn kind(&self) -> &'static str {
        self.backend()
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

/// Something at `path` no SQLite store can be opened from (a folder): the
/// store stays off (tests).
#[cfg(test)]
pub fn seed_unopenable(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dashboard's minutes come back from a time on; its hours, rewritten
    /// in place, add up by bucket for names and prefixes; retention cuts both.
    #[test]
    fn stats_minutes_and_hours_come_back() {
        use trunk_app::stats::{HourRow, Span};
        for db in Db::for_each_backend("stats", std::time::Duration::from_millis(10)) {
            for t in [60, 120, 7200] {
                db.send(Change::StatsMinute { t, json: format!("{{\"t\":{t}}}") });
            }
            let row = |series: &str, t: i64, avg: f32, n: u32| HourRow { series: series.into(), t, avg, lo: avg - 1.0, hi: avg + 1.0, n };
            db.send(Change::StatsHours(vec![row("all/calls", 0, 9.0, 60), row("all/airMs", 0, 2.0, 60), row("cpu", 0, 50.0, 60)]));
            db.send(Change::StatsHours(vec![row("all/calls", 0, 4.0, 60), row("all/calls", 3600, 2.0, 30), row("all/calls", 4 * 3600, 1.0, 60)]));
            db.flush().unwrap();
            assert_eq!(db.stats_minutes(100).unwrap(), ["{\"t\":120}", "{\"t\":7200}"], "{}", db.kind());
            // Two-hour buckets over four hours.
            let span = Span::of(0, 4 * 3600 - 1, 2, 60);
            assert_eq!((span.step, span.n), (120, 2));
            let q = store::HoursQuery { patterns: &["all/*".to_string()], span, from: 0, before: 4 * 3600 };
            let mut got: Vec<_> = db.stats_hours(&q).unwrap().into_iter().map(|(k, i, h)| (k, i, h.sum, h.n, h.lo, h.hi)).collect();
            got.sort_by(|a, b| (a.0.as_str(), a.1).cmp(&(b.0.as_str(), b.1)));
            assert_eq!(got, [("all/airMs".to_string(), 0, 120.0, 60, 1.0, 3.0), ("all/calls".to_string(), 0, 4.0 * 60.0 + 2.0 * 30.0, 90, 1.0, 5.0)], "{}", db.kind());
            let exact = db.stats_hours(&store::HoursQuery { patterns: &["cpu".to_string()], span, from: 0, before: 4 * 3600 }).unwrap();
            assert_eq!(exact.len(), 1);
            assert_eq!(db.stats_hours(&store::HoursQuery { patterns: &["*".to_string()], span, from: 0, before: 4 * 3600 }).unwrap().len(), 3);
            assert!(db.stats_hours(&store::HoursQuery { patterns: &[], span, from: 0, before: 4 * 3600 }).unwrap().is_empty());
            db.send(Change::RetireStats { minutes_before: 7200, hours_before: Some(3600) });
            db.flush().unwrap();
            assert_eq!(db.stats_minutes(0).unwrap().len(), 1);
            assert_eq!(db.count("select count(*) from stats_hours"), 2, "{}", db.kind());
        }
    }

    /// What's kept is kept by the config's system id: a renamed system
    /// keeps its talkgroups and calls, and a name not the config's has none.
    #[test]
    fn a_renamed_system_keeps_what_is_kept_by_its_id() {
        let cfg = |name: &str| trunk_app::Config::from_json(&format!(r#"{{"systems":[{{"shortName":"{name}","id":5}}]}}"#)).unwrap();
        for db in Db::for_each_backend("rename", Duration::from_millis(10)) {
            db.set_systems(&cfg("old"));
            assert_eq!(db.system_id("old"), Some(5));
            db.send(Change::Talkgroups { system_id: 5, rows: trunk_core::trunk::parse_rows("Decimal,Alpha Tag\n1,A\n") });
            let mut r = row::tests::record(1, 1000);
            r.short_name = "old".into();
            db.send(Change::Call { row: row::call_row("old/1", &r, true, true, None, 0), live: true });
            db.flush().unwrap();
            db.set_systems(&cfg("new"));
            db.flush().unwrap();
            assert_eq!(db.talkgroups("new").unwrap().iter().map(|r| r.tg.alpha_tag.as_str()).collect::<Vec<_>>(), ["A"], "{}", db.kind());
            assert_eq!(db.calls(&Default::default(), &[db.system_id("new").unwrap()]).unwrap().entries.len(), 1);
            assert_eq!(db.count("select count(*) from systems where id = 5 and short_name = 'new'"), 1);
        }
    }

    /// A system's talkgroups come back as rows in their order, with each
    /// row's comment and unknown columns, through a one-row edit (committed
    /// at once), a shorter list and none.
    #[test]
    fn talkgroups_come_back_as_rows() {
        use trunk_core::trunk::{edit_rows, parse_rows};
        for db in Db::for_each_backend("talkgroups", std::time::Duration::from_secs(60)) {
            let (a, b) = (db.system_id("a").unwrap(), db.system_id("b").unwrap());
            let mut rows = parse_rows("# county\nDecimal;Alpha Tag;Ignore;Unit\n# dispatch\n201;\"Fire; Disp\";;E\n102;Tac;;\n");
            db.send(Change::Talkgroups { system_id: a, rows: rows.clone() });
            let t = std::time::Instant::now();
            db.send(Change::Talkgroups { system_id: b, rows: parse_rows("1,x\n") });
            let edited = edit_rows(&mut rows, 102, &[("Ignore", "x")]).unwrap();
            db.send(Change::Talkgroup { system_id: a, row: edited, order: 1 });
            // (People's edits commit at once, whatever the interval.)
            while db.talkgroups("a").unwrap() != rows {
                assert!(t.elapsed() < std::time::Duration::from_secs(5), "an edit waits for the interval ({})", db.kind());
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let back = db.talkgroups("a").unwrap();
            assert_eq!(back.iter().map(|r| r.tg.number).collect::<Vec<_>>(), [201, 102], "in their order, not by number ({})", db.kind());
            assert_eq!((back[0].comment.as_str(), back[0].extra.clone()), ("# dispatch", vec![("Unit".to_string(), "E".to_string())]));
            assert_eq!(db.count(&format!("select count(*) from talkgroups where system_id = {a} and number = 102 and ignore")), 1);
            db.send(Change::Talkgroups { system_id: a, rows: parse_rows("Decimal;Alpha Tag\n201;Fire\n") });
            db.send(Change::Talkgroups { system_id: b, rows: Vec::new() });
            db.flush().unwrap();
            assert_eq!(db.talkgroups("a").unwrap().len(), 1);
            assert!(db.talkgroups("b").unwrap().is_empty(), "cleared");
            assert!(db.talkgroups("c").unwrap().is_empty());
        }
    }

    /// Talker aliases are kept as rows, and an alias heard again (no change)
    /// is no write.
    #[test]
    fn talker_aliases_write_what_changed() {
        use trunk_core::trunk::UnitAlias;
        for db in Db::for_each_backend("units", std::time::Duration::from_millis(10)) {
            let a = db.system_id("a").unwrap();
            let alias = |s: &str, t: i64| UnitAlias { alias: s.into(), source: "ota".into(), time: t, talkgroup: Some(9), ..Default::default() };
            db.send(Change::Units { system_id: a, aliases: vec![(1, alias("E1", 10)), (2, alias("E2", 10))] });
            db.flush().unwrap();
            assert_eq!(db.units("a").unwrap(), vec![(1, alias("E1", 10)), (2, alias("E2", 10))], "{}", db.kind());
            // (A trigger that refuses unit 1's row: unchanged, it isn't written.)
            db.refuse_for_test("units", "unit", "1");
            db.send(Change::Units { system_id: a, aliases: vec![(1, alias("E1", 10)), (3, alias("E3", 20))] });
            db.flush().unwrap();
            assert_eq!(db.units("a").unwrap(), vec![(1, alias("E1", 10)), (3, alias("E3", 20))], "2 gone, 3 new ({})", db.kind());
        }
    }

    /// Kept documents come back as they went in; the radio registry, kept
    /// as changes, comes back as it was, its logs to their caps; written
    /// whole, its changes go.
    #[test]
    fn kept_documents_and_the_registry_come_back() {
        use trunk_app::stats::radio::{Registry, SystemRadio, MAX_AFFILIATIONS};
        use trunk_core::trunk::{Message, MessageType};
        for db in Db::for_each_backend("kept-docs", std::time::Duration::from_millis(10)) {
            db.send(Change::Kept { kind: "units".into(), key: "a".into(), value: Some("1,Engine 1".into()) });
            db.send(Change::Kept { kind: "units".into(), key: "a".into(), value: Some("1,Engine 2".into()) });
            db.send(Change::Kept { kind: "tune".into(), key: "".into(), value: Some("{}".into()) });
            db.send(Change::Kept { kind: "tune".into(), key: "".into(), value: None });
            db.flush().unwrap();
            assert_eq!(db.kept("units", "a").unwrap().as_deref(), Some("1,Engine 2"), "{}", db.kind());
            assert_eq!(db.kept("units", "b").unwrap(), None);
            assert_eq!(db.kept("tune", "").unwrap(), None, "removed");

            let mut r = Registry::default();
            let x = db.system_id("x").unwrap();
            let save = |r: &mut Registry| {
                for (system, c) in r.take_changes() {
                    db.send(Change::Registry { system_id: db.system_id(&system).unwrap(), changes: Box::new(c) });
                }
            };
            let affiliate = |r: &mut Registry, from: usize, n: usize| {
                for i in from..from + n {
                    r.message("x", &Message { kind: MessageType::Affiliation, source: 1 + i as i64 % 50, talkgroup: 300 + i as u32 % 7, ..Default::default() }, 1000 + i as i64);
                }
            };
            affiliate(&mut r, 0, MAX_AFFILIATIONS - 5);
            r.concluded("x", &trunk_recorder_plugin::CallRecord { freq: 851_000_000, talkgroup: 300, ..Default::default() }, 1000);
            save(&mut r);
            affiliate(&mut r, MAX_AFFILIATIONS - 5, 15);
            save(&mut r);
            db.flush().unwrap();
            let back = db.registry().unwrap();
            assert_eq!((back.len(), db.count("select count(*) from registry"), db.count("select count(*) from registry_changes")), (1, 0, 2));
            let again: &SystemRadio = back[0].1.as_ref().unwrap();
            assert_eq!(again.affiliations.len(), MAX_AFFILIATIONS, "the log kept to its cap ({})", db.kind());
            assert_eq!(serde_json::to_value(again).unwrap(), serde_json::to_value(&r.systems["x"]).unwrap(), "{}", db.kind());

            // A day's changes are made over the whole copy and let go.
            for i in 2..store::COMPACT_AFTER {
                affiliate(&mut r, MAX_AFFILIATIONS + 10 + i as usize, 1);
                save(&mut r);
            }
            db.flush().unwrap();
            assert_eq!((db.count(&format!("select count(*) from registry where system_id = {x}")), db.count("select count(*) from registry_changes")), (1, 0), "{}", db.kind());
            affiliate(&mut r, 10_000, 3);
            save(&mut r);
            db.flush().unwrap();
            let back = db.registry().unwrap();
            assert_eq!(db.count("select count(*) from registry_changes"), 1);
            assert_eq!(serde_json::to_value(back[0].1.as_ref().unwrap()).unwrap(), serde_json::to_value(&r.systems["x"]).unwrap(), "{}", db.kind());
        }
    }

    /// The reporting views give a call, its radios and the dashboard's
    /// hours with names and timestamps, on either backend.
    #[test]
    fn the_reporting_views_name_things() {
        for db in Db::for_each_backend("views", Duration::from_millis(10)) {
            let a = db.system_id("ohmarcs").unwrap();
            db.send(Change::Talkgroups { system_id: a, rows: trunk_core::trunk::parse_rows("Decimal,Alpha Tag,Category\n9179,FD Disp,Fire\n") });
            let alias = trunk_core::trunk::UnitAlias { alias: "ENGINE 1".into(), ..Default::default() };
            db.send(Change::Units { system_id: a, aliases: vec![(9001644, alias)] });
            // (Noon UTC: the same date in any server's time zone.)
            db.send(Change::Call { row: row::call_row("ohmarcs/v", &row::tests::record(9179, 1_699_963_200_123), true, true, None, 0), live: true });
            db.send(Change::StatsHours(vec![trunk_app::stats::HourRow { series: "cpu".into(), t: 1_699_963_200, avg: 1.0, lo: 0.0, hi: 2.0, n: 60 }]));
            db.flush().unwrap();
            let store = db.store().unwrap();
            let one = |sql: &str| store.texts(sql).join("|");
            assert_eq!(one("select system || ' ' || talkgroup_name || ' ' || talkgroup_category from calls_report"), "ohmarcs FD Disp Fire", "{}", db.kind());
            assert!(one("select cast(start_at as text) from calls_report").starts_with("2023-11-14") && one("select cast(start_at as text) from calls_report").contains("12:00:00.123"), "{}", one("select cast(start_at as text) from calls_report"));
            assert_eq!(one("select unit_name from transmissions_report order by seq"), "ENGINE 1|");
            assert_eq!(one("select series from stats_report"), "cpu");
        }
    }

    #[test]
    fn the_last_handle_removes_a_test_folder() {
        let (db, path) = Db::for_test("cleanup");
        db.send(Change::Meta { key: "a".into(), value: "1".into() });
        drop(db);
        assert!(!path.parent().unwrap().exists(), "the writer has let go and the folder is gone");
    }

    #[test]
    fn retention_counts_back_from_the_newest_call_when_the_clock_is_ahead() {
        for db in Db::for_each_backend("retention-clock", Duration::from_secs(1)) {
            assert_eq!(db.retention_cutoff(120, 1000 * DAY_MS), 880 * DAY_MS, "an empty store counts back from now ({})", db.kind());
            let r = row::tests::record(1, 10 * DAY_MS);
            db.send(Change::Call { row: row::call_row("s/a", &r, true, true, None, 0), live: true });
            db.flush().unwrap();
            // The clock jumped 990 days ahead of the newest call.
            let cut = db.retention_cutoff(120, 1000 * DAY_MS);
            assert_eq!(cut, 11 * DAY_MS - 120 * DAY_MS);
            assert_eq!(db.retention_cutoff(120, 10 * DAY_MS), -110 * DAY_MS, "a clock that is right counts from now");
        }
    }

    #[test]
    fn writes_are_batched_into_few_transactions() {
        for db in Db::for_each_backend("batch", Duration::from_secs(1)) {
            for i in 0..1000 {
                db.send(Change::Meta { key: format!("k{i}"), value: "v".into() });
            }
            db.flush().unwrap();
            assert_eq!(db.count("select count(*) from meta"), 1000);
            assert!(db.commits() <= 10, "{} commits for 1000 writes ({})", db.commits(), db.kind());
        }
    }

    #[test]
    fn a_quiet_write_commits_promptly() {
        for db in Db::for_each_backend("prompt", Duration::from_secs(1)) {
            let t = std::time::Instant::now();
            db.send(Change::Meta { key: "a".into(), value: "1".into() });
            db.flush().unwrap();
            assert!(t.elapsed() < Duration::from_millis(1500));
            assert_eq!(db.meta("a").unwrap().as_deref(), Some("1"));
        }
    }

    #[test]
    fn a_store_that_is_off_takes_writes_and_answers_reads_with_an_error() {
        let db = Db::disabled();
        assert!(db.is_disabled());
        assert!(!db.checkpoint());
        assert_eq!(db.calls(&Default::default(), &[]).unwrap_err(), "the call store is off");
        assert!(db.flush().is_ok());
        assert_eq!((db.recorder(), db.describe()), (None, None));
        assert!(!db.try_begin_backfill());
        assert_eq!(db.now_ms(5), 5);
    }

    /// Low-write mode: a store, keeping no call index (a call sent is
    /// dropped) and the rest as ever; the backup keeps what can't be rebuilt.
    /// SQLite's writer counts what it writes to disk where the OS counts a
    /// thread's writes (Linux); elsewhere it says it can't.
    #[test]
    fn the_writer_counts_what_it_writes() {
        let (db, _path) = Db::for_test_with_batch("written", Duration::from_millis(10));
        for i in 0..50 {
            db.send(Change::Call { row: row::call_row(&format!("ohmarcs/w{i}"), &row::tests::record(1, 1000 + i), true, true, None, 0), live: true });
        }
        db.flush().unwrap();
        db.checkpoint();
        db.flush().unwrap();
        if cfg!(target_os = "linux") {
            assert!(db.written().is_some_and(|b| b > 0), "{:?}", db.written());
        } else {
            assert_eq!(db.written(), None);
        }
    }

    /// Each table and index is measured, with its table and rows, in either backend.
    #[test]
    fn tables_and_indexes_are_measured() {
        for db in Db::for_each_backend("sizes", Duration::from_millis(10)) {
            db.send(Change::Call { row: row::call_row("ohmarcs/s", &row::tests::record(1, 1000), true, true, None, 0), live: true });
            db.flush().unwrap();
            let s = db.sizes().unwrap();
            let get = |n: &str| s.objects.iter().find(|o| o.name == n).unwrap_or_else(|| panic!("{n} in {:?} ({})", s.objects, db.kind())).clone();
            let calls = get("calls");
            assert!(!calls.index && calls.bytes > 0, "{}", db.kind());
            if db.kind() == "sqlite" {
                assert_eq!(calls.rows, Some(1));
                assert_eq!(db.server_wal_bytes(), None);
            } else {
                assert!(db.server_wal_bytes().is_some_and(|b| b > 0), "pg_stat_wal");
            }
            let i = get("calls_start");
            assert!(i.index && i.table == "calls" && i.bytes > 0);
            assert!(s.file_bytes > 0);
        }
    }

    /// Without the radio or talkgroup index (`storage.radioIndex`,
    /// `storage.talkgroupIndex` off) the store drops it; turned on again,
    /// it's made again from what's stored.
    #[test]
    fn the_radio_and_talkgroup_indexes_are_optional() {
        let dir = TestDir::new("db-optional-indexes");
        let has = |db: &Db, i: &str| db.count(&format!("select count(*) from sqlite_master where name = '{i}'"));
        let off = trunk_app::config::StorageSettings { radio_index: false, talkgroup_index: false, ..Default::default() };
        let db = Db::from_settings(&off, &dir);
        assert!(db.indexes_calls() && !db.indexes_radios() && !db.indexes_talkgroups());
        assert_eq!((has(&db, schema::RADIO_INDEX), has(&db, schema::TALKGROUP_INDEX)), (0, 0));
        drop(db);
        let db = Db::from_settings(&trunk_app::config::StorageSettings { talkgroup_index: false, ..Default::default() }, &dir);
        assert!(db.indexes_radios() && !db.indexes_talkgroups());
        assert_eq!((has(&db, schema::RADIO_INDEX), has(&db, schema::TALKGROUP_INDEX)), (1, 0));
        drop(db);
        let db = Db::from_settings(&Default::default(), &dir);
        assert_eq!((has(&db, schema::RADIO_INDEX), has(&db, schema::TALKGROUP_INDEX)), (1, 1));
    }

    #[test]
    fn low_write_keeps_no_call_index() {
        let dir = TestDir::new("db-low-write");
        let storage = trunk_app::config::StorageSettings { low_write: true, ..Default::default() };
        let db = Db::from_settings(&storage, &dir);
        assert_eq!(db.kind(), "sqlite");
        assert!(!db.indexes_calls() && !db.is_disabled());
        let r = row::tests::record(1, 1000);
        assert!(!db.send(Change::Call { row: row::call_row("ohmarcs/a", &r, true, true, None, 0), live: true }));
        db.send(Change::Kept { kind: "tune".into(), key: String::new(), value: Some("{}".into()) });
        db.flush().unwrap();
        assert_eq!((db.count("select count(*) from calls"), db.count("select count(*) from kept")), (0, 1));
        let to = db.store().unwrap().backup().unwrap().unwrap();
        let b = rusqlite::Connection::open(&to).unwrap();
        assert_eq!(b.query_row("select count(*) from kept", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "kept in the backup");
        assert!(b.query_row("select count(*) from calls", [], |r| r.get::<_, i64>(0)).is_err(), "calls aren't: they're rebuilt from their files");
        drop(db);
        assert!(Db::from_settings(&Default::default(), &dir).indexes_calls());
    }

    #[test]
    fn an_empty_recorder_id_is_none() {
        let dir = TestDir::new("db-recorder-id");
        std::fs::write(dir.join("directory-recorder-id"), b" \n").unwrap();
        assert_eq!(recorder_id(&dir), None);
        std::fs::write(dir.join("directory-recorder-id"), b"rec-1\n").unwrap();
        let db = Db::open(&dir.join("trunk-pro.sqlite"), Duration::from_secs(1), 0).unwrap();
        assert_eq!(db.recorder(), Some("rec-1"));
    }

    /// What the store sends to disk for an hour of a busy system (a call a
    /// second, three talkers among 10,000 radios, 2,500 talkgroups),
    /// committed every 60 s and every 300 s: phase 1's budget is under 1 GB
    /// a day (research/sqlite-plan.md). Linux only (it reads /proc/self/io);
    /// run it on the disk to measure: `TRP_BENCH_DIR=<dir> cargo test
    /// --release -p trunk-pro write_budget -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn write_budget() {
        let written = || -> u64 {
            let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
            io.lines().find_map(|l| l.strip_prefix("write_bytes: ").and_then(|v| v.trim().parse().ok())).unwrap_or(0)
        };
        let dir = std::env::var("TRP_BENCH_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % u64::from(n)) as u32
        };
        for every in [60, 300] {
            let path = dir.join(format!("trp-write-budget-{every}.sqlite"));
            for x in ["", "-wal", "-shm", ".lock"] {
                let _ = std::fs::remove_file(format!("{}{x}", path.display()));
            }
            // (Commits as the writer makes them under load: one per interval, here by flushing.)
            let db = Db::open(&path, Duration::from_secs(3600), 0).unwrap();
            db.set_systems(&trunk_app::Config::from_json(r#"{"systems":[{"shortName":"ohmarcs","id":1}]}"#).unwrap());
            let (at, before) = (std::time::Instant::now(), written());
            let mut size = 0;
            for second in 0..3600i64 {
                let mut r = row::tests::record(10_000 + next(2500), 1_790_000_000_000 + second * 1000);
                r.src_list.push(r.src_list[0].clone());
                for (k, t) in r.src_list.iter_mut().enumerate() {
                    t.src = 1_000_000 + i64::from(next(10_000));
                    t.pos = k as f64 * 2.0;
                }
                let json = r.to_json();
                size = json.len();
                db.send(Change::Call { row: row::call_row(&format!("big/{second}"), &r, true, true, None, 0), live: true });
                if (second + 1) % every == 0 {
                    db.flush().unwrap();
                }
            }
            db.flush().unwrap();
            let bytes = (written() - before) as f64;
            assert_eq!(db.count("select count(*) from calls"), 3600);
            println!("commit every {every:>3} s: {:6.1} MB an hour, {:.2} GB a day ({size} B records, {:.1?})", bytes / 1e6, bytes * 24.0 / 1e9, at.elapsed());
            drop(db);
            for x in ["", "-wal", "-shm", ".lock"] {
                let _ = std::fs::remove_file(format!("{}{x}", path.display()));
            }
        }
    }

    /// What the dashboard's history writes in the store: two rollups a minute
    /// (the engine's 88 series and the platform's 48, as the live recorder
    /// has them) and each hour's 136 series rows, committed every 300 s as
    /// the writer does under load. Linux only, as `write_budget`.
    #[test]
    #[ignore]
    fn stats_write_budget() {
        use trunk_app::stats::{HourRow, Rollup};
        let written = || -> u64 {
            let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
            io.lines().find_map(|l| l.strip_prefix("write_bytes: ").and_then(|v| v.trim().parse().ok())).unwrap_or(0)
        };
        let dir = std::env::var("TRP_BENCH_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let path = dir.join("trp-stats-budget.sqlite");
        let gone = || {
            for x in ["", "-wal", "-shm", ".lock"] {
                let _ = std::fs::remove_file(format!("{}{x}", path.display()));
            }
        };
        gone();
        let db = Db::open(&path, Duration::from_secs(3600), 0).unwrap();
        let rollup = |t: i64, prefix: &str, n: usize| Rollup { t, values: (0..n).map(|i| (format!("{prefix}/series{i}"), [1.234 * i as f32, 0.5, 9.87])).collect() };
        let t0 = 1_790_000_000i64.div_euclid(3600) * 3600;
        let (at, before) = (std::time::Instant::now(), written());
        let hours = 6;
        for m in 0..hours * 60 {
            let t = t0 + m * 60;
            for r in [rollup(t, "engine", 88), rollup(t, "plat", 48)] {
                db.send(Change::StatsMinute { t, json: r.to_json().to_string() });
            }
            if (m + 1) % 60 == 0 {
                let h = t0 + (m / 60) * 3600;
                db.send(Change::StatsHours((0..136).map(|i| HourRow { series: format!("s/series{i}"), t: h, avg: 1.0, lo: 0.0, hi: 2.0, n: 60 }).collect()));
            }
            if (m + 1) % 5 == 0 {
                db.flush().unwrap();
            }
        }
        db.flush().unwrap();
        let bytes = (written() - before) as f64 * 24.0 / hours as f64;
        println!("dashboard history in the store: {:.1} MB a day at 300 s commits ({:.1?}); the files wrote about 9 MB a day", bytes / 1e6, at.elapsed());
        drop(db);
        gone();
    }

    /// What the store writes in a day for a busy system, standard against
    /// low-write mode (`storage.lowWrite`), everything together: a call a
    /// second (three talkers among 10,000 radios, 2,500 talkgroups) and an
    /// affiliation, the registry's changes every 15 minutes, the dashboard's
    /// minutes and hours; committed every 300 s, or every hour in low-write
    /// mode (which also drops the calls and the minutes, as the recorder
    /// does), the registry folded into its whole copy as it does live. An
    /// hour heard first, then a whole day measured (checkpoints come every
    /// few hours: a shorter run can miss them). Linux only, as
    /// `write_budget`; `TRP_BENCH_DIR=<dir> cargo test --release -p
    /// trunk-pro day_write_budget -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn day_write_budget() {
        use trunk_app::stats::radio::Registry;
        use trunk_app::stats::{HourRow, Rollup};
        use trunk_core::trunk::{Call, CallSource, Message, MessageType};
        let written = || -> u64 {
            let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
            io.lines().find_map(|l| l.strip_prefix("write_bytes: ").and_then(|v| v.trim().parse().ok())).unwrap_or(0)
        };
        let dir = std::env::var("TRP_BENCH_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let cfg = trunk_app::Config::from_json(r#"{"systems":[{"shortName":"ohmarcs","id":1}]}"#).unwrap();
        for (mode, low_write, radio_index) in [("standard", false, true), ("no radio index", false, false), ("low-write", true, true)] {
            let mut seed = 0x2545_f491_4f6c_dd1du64;
            let mut next = |n: u32| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed % u64::from(n)) as u32
            };
            let here = dir.join(format!("trp-day-{}", mode.replace(' ', "-")));
            let _ = std::fs::remove_dir_all(&here);
            std::fs::create_dir_all(&here).unwrap();
            let db = Db::from_settings(&trunk_app::config::StorageSettings { low_write, radio_index, ..Default::default() }, &here);
            db.set_systems(&cfg);
            let every = if low_write { LOW_WRITE_COMMIT_S as i64 } else { 300 };
            let mut r = Registry::default();
            let t0 = 1_790_000_000i64.div_euclid(3600) * 3600;
            let (mut at, mut before) = (std::time::Instant::now(), written());
            for second in 0..25 * 3600i64 {
                if second == 3600 {
                    db.flush().unwrap();
                    (at, before) = (std::time::Instant::now(), written());
                }
                let t = t0 + second;
                let mut rec = row::tests::record(10_000 + next(2500), t * 1000);
                rec.src_list.push(rec.src_list[0].clone());
                for (k, x) in rec.src_list.iter_mut().enumerate() {
                    x.src = 1_000_000 + i64::from(next(10_000));
                    x.pos = k as f64 * 2.0;
                }
                let sources = rec.src_list.iter().map(|x| CallSource { src: x.src as u32, time_s: x.pos, ..Default::default() }).collect();
                r.call_end("ohmarcs", &Call { talkgroup: rec.talkgroup, start_s: 0.0, last_update_s: 4.0, sources, ..Default::default() }, t);
                r.concluded("ohmarcs", &rec, t);
                r.message("ohmarcs", &Message { kind: MessageType::Affiliation, source: i64::from(1_000_000 + next(10_000)), talkgroup: 10_000 + next(2500), ..Default::default() }, t);
                db.send(Change::Call { row: row::call_row(&format!("ohmarcs/{second}"), &rec, true, true, None, 0), live: true });
                if (second + 1) % 60 == 0 && !low_write {
                    for roll in ["engine", "plat"].map(|p| Rollup { t, values: (0..68).map(|i| (format!("{p}/series{i}"), [1.234 * i as f32, 0.5, 9.87])).collect() }) {
                        db.send(Change::StatsMinute { t, json: roll.to_json().to_string() });
                    }
                }
                if (second + 1) % 900 == 0 {
                    for (system, changes) in r.take_changes() {
                        db.send(Change::Registry { system_id: db.system_id(&system).unwrap(), changes: Box::new(changes) });
                    }
                }
                if (second + 1) % 3600 == 0 {
                    db.send(Change::StatsHours((0..136).map(|i| HourRow { series: format!("s/series{i}"), t: t - 3599, avg: 1.0, lo: 0.0, hi: 2.0, n: 60 }).collect()));
                }
                // (Commits as the writer makes them under load: one per interval, here by flushing.)
                if (second + 1) % every == 0 {
                    db.flush().unwrap();
                }
            }
            db.flush().unwrap();
            let day = (written() - before) as f64;
            println!("{mode:>14}: {:.2} GB a day ({:.1?})", day / 1e9, at.elapsed());

            drop(db);
            let _ = std::fs::remove_dir_all(&here);
        }
    }

    /// What keeping a busy system's radio registry as changes writes,
    /// against the whole file every 15 minutes (`files`): six hours heard
    /// first (written whole), then an hour of a call and an affiliation a
    /// second over 2,500 talkgroups and 10,000 radios, its changes appended
    /// every 15 minutes as the session does (and written whole once a day,
    /// counted in). Linux only, as `write_budget`.
    #[test]
    #[ignore]
    fn registry_write_budget() {
        use trunk_app::stats::radio::Registry;
        use trunk_core::trunk::{Call, CallSource, Message, MessageType};
        let written = || -> u64 {
            let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
            io.lines().find_map(|l| l.strip_prefix("write_bytes: ").and_then(|v| v.trim().parse().ok())).unwrap_or(0)
        };
        let dir = std::env::var("TRP_BENCH_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % u64::from(n)) as u32
        };
        let t0 = 1_790_000_000i64;
        let mut second = |r: &mut Registry, t: i64| {
            let (tg, a, b) = (10_000 + next(2500), 1_000_000 + next(10_000), 1_000_000 + next(10_000));
            let sources = vec![CallSource { src: a, time_s: 0.0, ..Default::default() }, CallSource { src: b, time_s: 2.0, ..Default::default() }];
            r.call_end("big", &Call { talkgroup: tg, start_s: 0.0, last_update_s: 4.0, sources, ..Default::default() }, t);
            r.concluded("big", &trunk_recorder_plugin::CallRecord { freq: 851_000_000 + u64::from(next(20)) * 25_000, talkgroup: tg.into(), ..Default::default() }, t);
            r.message("big", &Message { kind: MessageType::Affiliation, source: i64::from(1_000_000 + next(10_000)), talkgroup: 10_000 + next(2500), ..Default::default() }, t);
        };
        let mut r = Registry::default();
        for t in 0..6 * 3600 {
            second(&mut r, t0 + t);
        }
        let file = serde_json::to_string(&r.systems["big"]).unwrap().len() as f64;
        let path = dir.join("trp-registry-budget.sqlite");
        let gone = || {
            for x in ["", "-wal", "-shm", ".lock"] {
                let _ = std::fs::remove_file(format!("{}{x}", path.display()));
            }
        };
        gone();
        let db = Db::open(&path, Duration::from_secs(3600), 0).unwrap();
        db.set_systems(&trunk_app::Config::from_json(r#"{"systems":[{"shortName":"big","id":1}]}"#).unwrap());
        for (system, c) in r.take_changes() {
            db.send(Change::Registry { system_id: db.system_id(&system).unwrap(), changes: Box::new(c) });
        }
        db.flush().unwrap();
        db.compact_registry_for_test(db.system_id("big").unwrap());
        let (at, before) = (std::time::Instant::now(), written());
        let mut rows = 0;
        for t in 6 * 3600..7 * 3600 {
            second(&mut r, t0 + t);
            if (t + 1) % 900 == 0 {
                for (system, c) in r.take_changes() {
                    rows += c.tgs.len() + c.units.len() + c.pairs.len() + c.freqs.len();
                    db.send(Change::Registry { system_id: db.system_id(&system).unwrap(), changes: Box::new(c) });
                }
                db.flush().unwrap();
            }
        }
        let hour = (written() - before) as f64;
        // (The day's whole copy.)
        let before = written();
        db.compact_registry_for_test(db.system_id("big").unwrap());
        let bytes = hour + (written() - before) as f64 / 24.0;
        let stored = db.registry().unwrap();
        assert_eq!(serde_json::to_value(stored[0].1.as_ref().unwrap()).unwrap(), serde_json::to_value(&r.systems["big"]).unwrap());
        println!(
            "registry as changes: {:.1} MB an hour, {:.2} GB a day ({rows} rows changed, {:.1?}); whole file every 15 min: {:.1} MB an hour, {:.2} GB a day ({:.1} MB file)",
            bytes / 1e6,
            bytes * 24.0 / 1e9,
            at.elapsed(),
            file * 4.0 / 1e6,
            file * 96.0 / 1e9,
            file / 1e6
        );
        drop(db);
        gone();
    }
}
