//! The dashboard's history kept: each minute's [`Rollup`]s (a minute may
//! have several — the engine's, the platform's, the plugins' — merged when
//! read), read into the in-memory [`History`] in the background at startup.
//!
//! - **The store on** (`storage.backend` `sqlite` or `postgres`): memory
//!   keeps [`STORE_MEMORY_MINUTES`]; the minutes in `stats_minutes`, kept
//!   [`STORE_MINUTE_DAYS`] (to fill memory at start), and each series' hours
//!   in `stats_hours`, written as each hour ends and kept `storage.statsDays`.
//!   A chart further back than memory adds the store's hours to memory's
//!   minutes (`server::stats_query`). Stats files beside it aren't read.
//! - **`files`**: JSON lines in `<data>/stats/YYYY-MM-DD.jsonl` (UTC days),
//!   appended, never rewritten (an SD card's friend); a finished day is
//!   gzipped, and days past [`KEEP_DAYS`] deleted.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use trunk_app::stats::{History, Rollup};

use crate::db::{Change, Db};

/// Days of minutes kept as files (a week, and the day being written).
pub const KEEP_DAYS: i64 = 8;
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
    /// Start the writer (and the reader of what's kept) filling `history`:
    /// the store `db`, keeping hours `stats_days` (0: all), or with it off
    /// the files in `dir`.
    pub fn start(dir: PathBuf, history: Arc<Mutex<History>>, db: Db, stats_days: u32) -> Store {
        let (tx, rx) = mpsc::channel::<Rollup>();
        history.lock().unwrap().loading = true;
        let done = Arc::new(AtomicI64::new(NOT_YET));
        let (d2, h2, db2, done2) = (dir.clone(), history.clone(), db.clone(), done.clone());
        let _ = std::thread::Builder::new().name("stats-load".into()).spawn(move || {
            if db2.uses_files() {
                load(&d2, &h2);
            } else {
                load_store(&db2, &h2, &done2);
            }
            h2.lock().unwrap().loading = false;
        });
        let _ = std::thread::Builder::new().name("stats-store".into()).spawn(move || {
            if db.uses_files() {
                write(&dir, &history, rx);
            } else {
                write_store(&db, &history, rx, &done, stats_days);
            }
        });
        Store { tx }
    }

    pub fn add(&self, r: Rollup) {
        if !r.values.is_empty() {
            let _ = self.tx.send(r);
        }
    }
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn day_name(day: i64) -> String {
    let (y, m, d) = civil(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The UTC day a file is for, from its name (`YYYY-MM-DD.jsonl[.gz]`).
fn file_day(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".jsonl.gz").or_else(|| name.strip_suffix(".jsonl"))?;
    let mut p = stem.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, d) = (p.next()??, p.next()??, p.next()??);
    // days_from_civil
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn files(dir: &Path) -> Vec<(i64, PathBuf)> {
    let mut v: Vec<(i64, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            file_day(&name).map(|d| (d, e.path()))
        })
        .collect();
    v.sort();
    v
}

/// The files' minutes from the last [`KEEP_DAYS`] into `history`, oldest first.
fn load(dir: &Path, history: &Mutex<History>) {
    let today = chrono::Utc::now().timestamp().div_euclid(86400);
    for (day, path) in files(dir) {
        if day < today - KEEP_DAYS {
            continue;
        }
        let Ok(f) = fs::File::open(&path) else { continue };
        let reader: Box<dyn Read> = if path.extension().is_some_and(|e| e == "gz") { Box::new(flate2::read::GzDecoder::new(f)) } else { Box::new(f) };
        let lines = BufReader::new(reader).lines().map_while(Result::ok);
        insert_all(history, lines.filter_map(|line| serde_json::from_str(&line).ok().as_ref().and_then(Rollup::from_json)));
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
        Err(e) => log::warn!("Call store: the dashboard's history not read ({e})"),
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

/// The live minutes: into `history` and the store; each hour into the
/// store as it ends (once the history is loaded: `done`), and what's past
/// keeping let go.
fn write_store(db: &Db, history: &Mutex<History>, rx: mpsc::Receiver<Rollup>, done: &AtomicI64, stats_days: u32) {
    for r in rx {
        history.lock().unwrap().insert(&r);
        db.send(Change::StatsMinute { t: r.t, json: r.to_json().to_string() });
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

/// Gzip finished days and delete old ones (everything but `today`'s plain file).
fn tidy(dir: &Path, today: i64) {
    for (day, path) in files(dir) {
        if day < today - KEEP_DAYS {
            let _ = fs::remove_file(&path);
        } else if day < today && path.extension().is_some_and(|e| e == "jsonl") {
            let gz = path.with_extension("jsonl.gz");
            let ok = (|| -> std::io::Result<()> {
                let mut src = fs::File::open(&path)?;
                let mut enc = flate2::write::GzEncoder::new(fs::File::create(&gz)?, flate2::Compression::default());
                std::io::copy(&mut src, &mut enc)?;
                enc.finish()?.sync_all()
            })();
            match ok {
                Ok(()) => {
                    let _ = fs::remove_file(&path);
                }
                Err(e) => {
                    log::warn!("Couldn't compress {}: {e}", path.display());
                    let _ = fs::remove_file(&gz);
                }
            }
        }
    }
}

fn write(dir: &Path, history: &Mutex<History>, rx: mpsc::Receiver<Rollup>) {
    let _ = fs::create_dir_all(dir);
    let mut day = chrono::Utc::now().timestamp().div_euclid(86400);
    tidy(dir, day);
    let mut file: Option<(i64, fs::File)> = None;
    for r in rx {
        history.lock().unwrap().insert(&r);
        let d = r.t.div_euclid(86400);
        if d > day {
            day = d;
            file = None;
            tidy(dir, day);
        }
        if file.as_ref().is_none_or(|(fd, _)| *fd != d) {
            let path = dir.join(format!("{}.jsonl", day_name(d)));
            file = fs::OpenOptions::new().create(true).append(true).open(&path).ok().map(|f| (d, f));
        }
        if let Some((_, f)) = file.as_mut() {
            let mut line = r.to_json().to_string();
            line.push('\n');
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn day_names_round_trip() {
        for d in [0i64, 19_999, 20_366, 20_729] {
            assert_eq!(file_day(&format!("{}.jsonl", day_name(d))), Some(d));
        }
        assert_eq!(day_name(20_366), "2025-10-05");
        assert_eq!(file_day("notes.txt"), None);
    }

    #[test]
    fn written_minutes_are_read_back() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-stats-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let now = chrono::Utc::now().timestamp() / 60 * 60;
        let h = Arc::new(Mutex::new(History::default()));
        let store = Store::start(dir.clone(), h.clone(), Db::disabled(), 0);
        store.add(Rollup { t: now, values: BTreeMap::from([("a".to_string(), [1.0, 0.0, 2.0])]) });
        store.add(Rollup { t: now, values: BTreeMap::from([("b".to_string(), [5.0, 5.0, 5.0])]) });
        drop(store);
        for _ in 0..100 {
            if h.lock().unwrap().keys().len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        let fresh = Arc::new(Mutex::new(History::default()));
        load(&dir, &fresh);
        let q = fresh.lock().unwrap().query(&["*".into()], now, now, 10);
        assert_eq!(q["a"]["v"], serde_json::json!([1.0]));
        assert_eq!(q["b"]["v"], serde_json::json!([5.0]));
        let _ = fs::remove_dir_all(&dir);
    }

    /// With the store on: the minutes read back at the next start, each
    /// ended hour written once, the writer going on from the last, and stats
    /// files beside it not read.
    #[test]
    fn the_store_keeps_minutes_and_each_hour() {
        let dir = crate::db::TestDir::new("stats-store");
        let stats = dir.join("stats");
        fs::create_dir_all(&stats).unwrap();
        let now = crate::clock::unix_s() as i64;
        let t0 = now.div_euclid(3600) * 3600 - 4 * 3600;
        let line = Rollup { t: t0, values: BTreeMap::from([("file".to_string(), [1.0, 1.0, 1.0])]) }.to_json();
        fs::write(stats.join(format!("{}.jsonl", day_name(t0.div_euclid(86400)))), format!("{line}\n")).unwrap();
        let (db, _) = Db::for_test_with_batch("stats-store", std::time::Duration::from_millis(10));
        for t in [t0, t0 + 3900, t0 + 7200] {
            db.send(Change::StatsMinute { t, json: Rollup { t, values: BTreeMap::from([("a".to_string(), [2.0, 1.0, 3.0])]) }.to_json().to_string() });
        }
        db.flush().unwrap();
        let wait = |h: &Arc<Mutex<History>>| {
            for _ in 0..200 {
                if !h.lock().unwrap().loading {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("still loading");
        };
        let h = Arc::new(Mutex::new(History::new(memory_minutes(&db))));
        let store = Store::start(stats.clone(), h.clone(), db.clone(), 30);
        wait(&h);
        db.flush().unwrap();
        assert!(h.lock().unwrap().keys() == ["a"], "the store's minutes, not the file's");
        assert!(stats.exists(), "files are left as they are");
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
        let _store = Store::start(stats.clone(), again.clone(), db.clone(), 30);
        wait(&again);
        db.flush().unwrap();
        assert_eq!(again.lock().unwrap().query(&["a".into()], t0, t0, 10)["a"]["v"], serde_json::json!([2.0]));
        assert_eq!(db.count("select count(*) from stats_minutes"), 5);
        assert_eq!(db.count("select count(*) from stats_hours"), 4);
    }
}
