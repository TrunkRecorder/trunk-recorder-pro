//! The dashboard's history kept: each minute's [`Rollup`]s (a minute may
//! have several — the engine's, the platform's, the plugins' — merged when
//! read), read into the in-memory [`History`] in the background at startup.
//! Memory keeps [`STORE_MEMORY_MINUTES`]; the store keeps the minutes in
//! `stats_minutes` [`STORE_MINUTE_DAYS`] (to fill memory at start; none in
//! low-write mode) and each series' hours in `stats_hours`, written as each
//! hour ends and kept `storage.statsDays`. A chart further back than memory
//! adds the store's hours to memory's minutes (`server::stats_query`). With
//! no store (it couldn't open), memory alone: a week.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use trunk_app::stats::{History, Rollup};

use crate::db::{Change, Db};

/// Minutes the history keeps in memory with the store on: a day and a bit
/// (the 24 h charts and "the usual"); anything further back is the store's hours.
pub const STORE_MEMORY_MINUTES: usize = 26 * 60;

/// Minutes of each series the in-memory history keeps: a week with no store
/// to ask; with one, [`STORE_MEMORY_MINUTES`] (the store's hours further back).
pub fn memory_minutes(db: &Db) -> usize {
    if db.is_disabled() { trunk_app::stats::history::WEEK_MINUTES } else { STORE_MEMORY_MINUTES }
}

/// Days of minutes the store keeps: enough to fill memory and write the
/// hours of a day the recorder was stopped part of.
const STORE_MINUTE_DAYS: i64 = 2;
/// An hour is written this long after it ends: a minute's rollups come from several places.
const HOUR_GRACE_S: i64 = 120;
/// The `meta` key of the last hour written to the store.
const HOURS_DONE: &str = "stats.hoursDone";
/// No hour written yet (the history still loading).
const NOT_YET: i64 = i64::MIN;

/// Hands rollups to the writer thread.
#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Rollup>,
}

impl Store {
    /// Start the writer (and the reader of what's kept) filling `history`
    /// from and into the store `db`, keeping hours `stats_days` (0: all) and
    /// the minutes too unless `low_write`.
    pub fn start(history: Arc<Mutex<History>>, db: Db, stats_days: u32, low_write: bool) -> Store {
        let (tx, rx) = mpsc::channel::<Rollup>();
        history.lock().unwrap().loading = true;
        let done = Arc::new(AtomicI64::new(NOT_YET));
        let (h2, db2, done2) = (history.clone(), db.clone(), done.clone());
        let _ = std::thread::Builder::new().name("stats-load".into()).spawn(move || {
            if !db2.is_disabled() {
                load_store(&db2, &h2, &done2);
            }
            h2.lock().unwrap().loading = false;
        });
        let _ = std::thread::Builder::new().name("stats-store".into()).spawn(move || write_store(&db, &history, rx, &done, stats_days, low_write));
        Store { tx }
    }

    pub fn add(&self, r: Rollup) {
        if !r.values.is_empty() {
            let _ = self.tx.send(r);
        }
    }
}

/// `rollups` into `history`, a few hundred at a time (the live writer
/// shares the lock).
fn insert_all(history: &Mutex<History>, rollups: impl Iterator<Item = Rollup>) {
    let mut batch = Vec::new();
    for r in rollups {
        batch.push(r);
        if batch.len() >= 500 {
            let mut h = history.lock().unwrap();
            batch.drain(..).for_each(|r| h.insert(&r));
        }
    }
    let mut h = history.lock().unwrap();
    batch.drain(..).for_each(|r| h.insert(&r));
}

/// Read the store's minutes into `history`, then write the hours it has
/// that the store hasn't, and say which hour was the last (`done`) to the writer.
fn load_store(db: &Db, history: &Mutex<History>, done: &AtomicI64) {
    let now = crate::clock::unix_s() as i64;
    let keep = history.lock().unwrap().cap_minutes() as i64 * 60;
    match db.stats_minutes(now - keep) {
        Ok(lines) => insert_all(history, lines.iter().filter_map(|l| serde_json::from_str(l).ok().as_ref().and_then(Rollup::from_json))),
        Err(e) => log::warn!("Store: the dashboard's history not read ({e})"),
    }
    let last = db.meta(HOURS_DONE).ok().flatten().and_then(|v| v.parse::<i64>().ok());
    let first = history.lock().unwrap().first().map(|t| t.div_euclid(3600) * 3600 - 3600);
    let from = last.or(first).unwrap_or(now.div_euclid(3600) * 3600 - 3600);
    done.store(write_hours(db, history, from, now), Ordering::SeqCst);
}

/// Write each hour after `done` that has ended by `now` (with
/// [`HOUR_GRACE_S`]) from `history`; the last one written (else `done`).
fn write_hours(db: &Db, history: &Mutex<History>, mut done: i64, now: i64) -> i64 {
    let mut wrote = false;
    while done + 2 * 3600 + HOUR_GRACE_S <= now {
        done += 3600;
        let rows = history.lock().unwrap().hour_rows(done);
        if !rows.is_empty() {
            db.send(Change::StatsHours(rows));
        }
        wrote = true;
    }
    if wrote {
        db.send(Change::Meta { key: HOURS_DONE.into(), value: done.to_string() });
    }
    done
}

/// The live minutes: into `history` and (unless `low_write`) the store;
/// each hour into the store as it ends (once the history is loaded:
/// `done`), and what's past keeping let go. With no store, memory alone.
fn write_store(db: &Db, history: &Mutex<History>, rx: mpsc::Receiver<Rollup>, done: &AtomicI64, stats_days: u32, low_write: bool) {
    for r in rx {
        history.lock().unwrap().insert(&r);
        if db.is_disabled() {
            continue;
        }
        if !low_write {
            db.send(Change::StatsMinute { t: r.t, json: r.to_json().to_string() });
        }
        let was = done.load(Ordering::SeqCst);
        if was == NOT_YET {
            continue;
        }
        let now = r.t + 60;
        let last = write_hours(db, history, was, now);
        if last != was {
            done.store(last, Ordering::SeqCst);
            let hours_before = (stats_days > 0).then(|| now - i64::from(stats_days) * 86400);
            db.send(Change::RetireStats { minutes_before: now - STORE_MINUTE_DAYS * 86400, hours_before });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn wait(h: &Arc<Mutex<History>>) {
        for _ in 0..200 {
            if !h.lock().unwrap().loading {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("still loading");
    }

    /// The minutes read back at the next start, each ended hour written
    /// once, and the writer going on from the last.
    #[test]
    fn the_store_keeps_minutes_and_each_hour() {
        let now = crate::clock::unix_s() as i64;
        let t0 = now.div_euclid(3600) * 3600 - 4 * 3600;
        let (db, _) = Db::for_test_with_batch("stats-store", std::time::Duration::from_millis(10));
        for t in [t0, t0 + 3900, t0 + 7200] {
            db.send(Change::StatsMinute { t, json: Rollup { t, values: BTreeMap::from([("a".to_string(), [2.0, 1.0, 3.0])]) }.to_json().to_string() });
        }
        db.flush().unwrap();
        let h = Arc::new(Mutex::new(History::new(memory_minutes(&db))));
        let store = Store::start(h.clone(), db.clone(), 30, false);
        wait(&h);
        db.flush().unwrap();
        assert_eq!(h.lock().unwrap().keys(), ["a"]);
        assert_eq!(db.count("select count(*) from stats_hours"), 3, "the three hours with minutes");
        let ended = (now - HOUR_GRACE_S).div_euclid(3600) * 3600 - 3600;
        assert_eq!(db.meta(HOURS_DONE).unwrap(), Some(ended.to_string()), "the last hour that has ended");
        // Live: a minute well into the next hour writes the one before.
        let next = now.div_euclid(3600) * 3600 + 3600;
        store.add(Rollup { t: next + 30 * 60, values: BTreeMap::from([("a".to_string(), [6.0, 6.0, 6.0])]) });
        store.add(Rollup { t: next + 3600 + 5 * 60, values: BTreeMap::from([("a".to_string(), [6.0, 6.0, 6.0])]) });
        drop(store);
        std::thread::sleep(std::time::Duration::from_millis(100));
        db.flush().unwrap();
        assert_eq!(db.count("select count(*) from stats_hours"), 4);
        assert_eq!(db.meta(HOURS_DONE).unwrap(), Some(next.to_string()));
        // The next start: from the store, no hour written twice.
        let again = Arc::new(Mutex::new(History::new(memory_minutes(&db))));
        let _store = Store::start(again.clone(), db.clone(), 30, false);
        wait(&again);
        db.flush().unwrap();
        assert_eq!(again.lock().unwrap().query(&["a".into()], t0, t0, 10)["a"]["v"], serde_json::json!([2.0]));
        assert_eq!(db.count("select count(*) from stats_minutes"), 5);
        assert_eq!(db.count("select count(*) from stats_hours"), 4);
    }

    /// Low-write mode keeps the hours, not the minutes.
    #[test]
    fn low_write_keeps_hours_alone() {
        let (db, _) = Db::for_test_with_batch("stats-low", std::time::Duration::from_millis(10));
        let h = Arc::new(Mutex::new(History::new(memory_minutes(&db))));
        let store = Store::start(h.clone(), db.clone(), 30, true);
        wait(&h);
        let next = (crate::clock::unix_s() as i64).div_euclid(3600) * 3600 + 3600;
        store.add(Rollup { t: next + 30 * 60, values: BTreeMap::from([("a".to_string(), [6.0, 6.0, 6.0])]) });
        store.add(Rollup { t: next + 3600 + 5 * 60, values: BTreeMap::from([("a".to_string(), [6.0, 6.0, 6.0])]) });
        drop(store);
        std::thread::sleep(std::time::Duration::from_millis(100));
        db.flush().unwrap();
        assert_eq!((db.count("select count(*) from stats_minutes"), db.count("select count(*) from stats_hours")), (0, 1));
    }
}
