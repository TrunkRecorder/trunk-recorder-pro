//! The dashboard's history on disk: each minute's [`Rollup`]s as JSON lines
//! in `<data>/stats/YYYY-MM-DD.jsonl` (UTC days; a minute may have several
//! lines — the engine's, the platform's, the plugins' — merged when read).
//! Appended, never rewritten (an SD card's friend); a finished day is
//! gzipped, and days past [`KEEP_DAYS`] deleted. At startup the files are
//! read into the in-memory [`History`] in the background.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use trunk_app::stats::{History, Rollup};

/// Days of files kept (a week, and the day being written).
pub const KEEP_DAYS: i64 = 8;

/// Hands rollups to the writer thread.
#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Rollup>,
}

impl Store {
    /// Start the writer (and the reader of what's on disk) for `dir`, filling `history`.
    pub fn start(dir: PathBuf, history: Arc<Mutex<History>>) -> Store {
        let (tx, rx) = mpsc::channel::<Rollup>();
        history.lock().unwrap().loading = true;
        let (d2, h2) = (dir.clone(), history.clone());
        let _ = std::thread::Builder::new().name("stats-load".into()).spawn(move || {
            load(&d2, &h2);
            h2.lock().unwrap().loading = false;
        });
        let _ = std::thread::Builder::new().name("stats-store".into()).spawn(move || write(&dir, &history, rx));
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

fn load(dir: &Path, history: &Mutex<History>) {
    let today = chrono::Utc::now().timestamp().div_euclid(86400);
    for (day, path) in files(dir) {
        if day < today - KEEP_DAYS {
            continue;
        }
        let Ok(f) = fs::File::open(&path) else { continue };
        let reader: Box<dyn Read> = if path.extension().is_some_and(|e| e == "gz") { Box::new(flate2::read::GzDecoder::new(f)) } else { Box::new(f) };
        let mut batch = Vec::new();
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if let Some(r) = serde_json::from_str(&line).ok().as_ref().and_then(Rollup::from_json) {
                batch.push(r);
            }
            // A few hundred at a time: the live writer shares the lock.
            if batch.len() >= 500 {
                let mut h = history.lock().unwrap();
                batch.drain(..).for_each(|r| h.insert(&r));
            }
        }
        let mut h = history.lock().unwrap();
        batch.drain(..).for_each(|r| h.insert(&r));
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
        let store = Store::start(dir.clone(), h.clone());
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
}
