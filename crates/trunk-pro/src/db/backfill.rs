//! Bringing the store up to date with the capture folder: one full walk the
//! first time the store sees a folder, then, on every start, a look at the
//! last two days' folders for calls recorded while the recorder was off. Runs
//! on its own thread and gives way to live writes.

use std::collections::BTreeSet;
use std::fs::DirEntry;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::OptionalExtension;
use trunk_recorder_plugin::CallRecord;

use super::row::call_row;
use super::{Db, Write};
use crate::config::Config;
use crate::runtime::Ctx;

/// Files between looks at the writer's queue.
const YIELD_EVERY: usize = 200;
/// Queued writes above which the backfill waits for the writer.
///
/// The writer's `MAX_BATCH` is derived from this: held writes count as
/// queued, so a batch cut above it could never be reached while the
/// backfill waits.
pub(super) const QUEUE_HIGH: usize = 2048;
/// Files between progress lines in the log.
const LOG_EVERY: usize = 10_000;

/// What a run did.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Call records (`.json` files) seen.
    pub walked: usize,
    /// Calls sent to the store.
    pub indexed: usize,
    /// Records left out: already indexed, unreadable or past retention.
    pub skipped: usize,
    /// Folders that could not be read.
    pub unreadable: usize,
    /// Recent day folders that are not there (a reconcile only).
    pub missing: usize,
    /// The capture folder itself could not be read, so nothing was recorded
    /// as done and the next start walks it again.
    pub aborted: bool,
}

/// The day folders of the last two days for every system's short name, every
/// name its sites were recorded under before, and every conventional system.
pub fn recent_folders(cfg: &Config, now_s: i64) -> Vec<PathBuf> {
    let dir = Path::new(&cfg.recording.capture_dir);
    let mut names: Vec<String> = cfg
        .systems
        .iter()
        .flat_map(|s| std::iter::once(s.short_name.clone()).chain(trunk_app::migrate::old_names(s)))
        .collect();
    names.extend(cfg.conventional.iter().map(|v| v.short_name.clone()));
    let mut folders = BTreeSet::new();
    for t in [now_s, now_s - 86_400] {
        let offset = crate::runtime::local_offset(t);
        for n in &names {
            folders.insert(dir.join(trunk_app::filename::default_path(n, "", t, offset)));
        }
    }
    folders.into_iter().collect()
}

/// Index the capture folder in the background. A walk already going is left
/// to finish: starting another would only read every file twice. Nothing
/// runs when the store is off.
pub fn start(ctx: Arc<Ctx>) {
    if ctx.db.is_disabled() {
        return;
    }
    if !ctx.db.try_begin_backfill() {
        log::debug!("Call store: a backfill is already running");
        return;
    }
    let (dir, recent, days) = {
        let c = ctx.config.lock().unwrap();
        let now_s = (crate::clock::unix_ms() / 1000) as i64;
        (
            PathBuf::from(&c.recording.capture_dir),
            recent_folders(&c, now_s),
            c.store.retention_days,
        )
    };
    let db = ctx.db.clone();
    let spawned = std::thread::Builder::new()
        .name("db-backfill".into())
        .spawn(move || {
            let _end = EndBackfill(ctx.db.clone());
            let now_ms = crate::clock::unix_ms() as i64;
            run(&ctx.db, &dir, &recent, days, now_ms);
        });
    if let Err(e) = spawned {
        db.end_backfill();
        log::warn!("Call store: could not start the backfill: {e}");
    }
}

/// Clears the single-flight flag when the backfill thread ends, however it ends.
struct EndBackfill(Db);

impl Drop for EndBackfill {
    fn drop(&mut self) {
        self.0.end_backfill();
    }
}

fn meta(db: &Db, key: &str) -> Option<String> {
    db.with_reader(|c| {
        c.query_row("select value from meta where key = ?", [key], |r| {
            r.get::<_, String>(0)
        })
        .optional()
    })
    .unwrap_or_else(|e| {
        log::warn!("Call store: reading {key}: {e}");
        None
    })
}

/// What a walk comes across.
enum Found<'a> {
    /// A `.json` file.
    Record(&'a Path),
    /// A folder that could not be listed.
    Unreadable(&'a Path, std::io::Error),
}

/// Walk `roots` depth first, each folder's entries in name order: a folder's
/// `.json` files go to `f` before the folders under it are walked, so
/// indexing starts at once and only the folders still to walk are held.
fn walk(
    roots: &[&Path],
    f: &mut impl FnMut(Found<'_>) -> rusqlite::Result<()>,
) -> rusqlite::Result<()> {
    let mut stack: Vec<PathBuf> = roots.iter().rev().map(|p| p.to_path_buf()).collect();
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(e) => {
                f(Found::Unreadable(&d, e))?;
                continue;
            }
        };
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if is_dir(&e) {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "json") {
                files.push(p);
            }
        }
        files.sort();
        dirs.sort();
        for p in &files {
            f(Found::Record(p))?;
        }
        stack.extend(dirs.into_iter().rev());
    }
    Ok(())
}

/// Whether an entry is a folder: its type as the listing gives it, and for a
/// link, what the link points at.
fn is_dir(e: &DirEntry) -> bool {
    match e.file_type() {
        Ok(t) if !t.is_symlink() => t.is_dir(),
        _ => e.path().is_dir(),
    }
}

/// Why a full walk is not recorded as done, if it is not: one that found no
/// call records (a drive not mounted yet) or could not read a folder would
/// otherwise leave calls out for good.
fn not_done_why(stats: &Stats) -> Option<String> {
    if stats.walked == 0 {
        Some("no call records were found in it".into())
    } else if stats.unreadable > 0 {
        Some(format!(
            "{} folder(s) in it could not be read",
            stats.unreadable
        ))
    } else {
        None
    }
}

/// Bring the store up to date with `capture_dir`: all of it when the store
/// has not indexed this folder before, else only `recent_folders`. Records
/// that started more than `retention_days` (when not 0) before now are left
/// out, where now is `now_ms` but at most a day past the newest call in the
/// store ([`Db::now_ms`]).
pub fn run(
    db: &Db,
    capture_dir: &Path,
    recent_folders: &[PathBuf],
    retention_days: u32,
    now_ms: i64,
) -> Stats {
    let folder = capture_dir.display().to_string();
    let full = meta(db, "backfill_done_at").is_none()
        || meta(db, "capture_dir").as_deref() != Some(folder.as_str());
    let roots: Vec<&Path> = if full {
        vec![capture_dir]
    } else {
        recent_folders.iter().map(PathBuf::as_path).collect()
    };
    let floor_ms = (retention_days > 0).then(|| db.retention_cutoff(retention_days, now_ms));
    let mut stats = Stats::default();
    let walked = db.with_reader(|c| {
        let mut known = c.prepare("select 1 from calls where path = ?")?;
        walk(&roots, &mut |found| {
            let p = match found {
                Found::Record(p) => p,
                Found::Unreadable(d, e) => {
                    if full && d == capture_dir {
                        stats.aborted = true;
                    } else if !full && e.kind() == std::io::ErrorKind::NotFound {
                        // A recent day's folder that is not there yet is ordinary.
                        if roots.contains(&d) {
                            stats.missing += 1;
                        }
                    } else {
                        stats.unreadable += 1;
                        log::warn!("Call store: could not read {}: {e}", d.display());
                    }
                    return Ok(());
                }
            };
            stats.walked += 1;
            if stats.walked % YIELD_EVERY == 0 {
                while db.queued() > QUEUE_HIGH {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            if stats.walked % LOG_EVERY == 0 {
                log::info!("Call store: walked {} call records", stats.walked);
            }
            let Ok(rel) = p.strip_prefix(capture_dir) else {
                stats.skipped += 1;
                return Ok(());
            };
            let rel = rel.with_extension("").to_string_lossy().replace('\\', "/");
            if known.exists([&rel])? {
                stats.skipped += 1;
                return Ok(());
            }
            let text = match std::fs::read_to_string(p) {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("Call store: could not read {}: {e}", p.display());
                    stats.skipped += 1;
                    return Ok(());
                }
            };
            let record: CallRecord = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    log::debug!("Call store: {} is not a call record: {e}", p.display());
                    stats.skipped += 1;
                    return Ok(());
                }
            };
            if floor_ms.is_some_and(|f| record.start_time_ms < f) {
                stats.skipped += 1;
                return Ok(());
            }
            let audio = ["wav", "m4a"].iter().any(|x| p.with_extension(x).exists());
            let row = call_row(&rel, &record, &text, audio, true, db.recorder(), now_ms);
            db.send(Write::Call { row, live: false });
            stats.indexed += 1;
            Ok(())
        })
    });
    if stats.aborted {
        log::warn!("Call store: {folder} cannot be read; it will be indexed on the next start");
        return stats;
    }
    if let Err(e) = walked {
        log::warn!("Call store: the backfill stopped early: {e}");
    } else if full {
        match not_done_why(&stats) {
            Some(why) => log::warn!(
                "Call store: {folder} is not marked as indexed: {why}; it is walked again on the next start"
            ),
            None => {
                db.send(Write::Meta {
                    key: "backfill_done_at".into(),
                    value: now_ms.to_string(),
                });
                db.send(Write::Meta {
                    key: "capture_dir".into(),
                    value: folder,
                });
                if let Err(e) = db.flush() {
                    log::warn!("Call store: {e}");
                }
            }
        }
    } else if !roots.is_empty() && stats.missing == roots.len() {
        log::info!(
            "Call store: none of the last two days' {} folders under {folder} are there",
            roots.len()
        );
    }
    log::info!(
        "Call store: indexed {} of {} call records ({} skipped)",
        stats.indexed,
        stats.walked,
        stats.skipped
    );
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TestDir;
    use crate::db::row::tests::record;

    fn capture(name: &str, calls: &[(&str, u32, i64)]) -> TestDir {
        let dir = TestDir::new(&format!("backfill-{name}"));
        for (rel, tg, start) in calls {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let r = record(*tg, *start);
            std::fs::write(p.with_extension("json"), r.to_json()).unwrap();
            std::fs::write(p.with_extension("wav"), b"RIFF").unwrap();
        }
        dir
    }

    fn count(db: &Db, sql: &str) -> u32 {
        db.with_reader(|c| c.query_row(sql, [], |r| r.get(0)))
            .unwrap()
    }

    #[test]
    fn a_first_run_indexes_the_folder_once() {
        let dir = capture(
            "first",
            &[
                ("s/2026/10/25/1-1_1", 1, 1000),
                ("s/2026/10/26/2-2_1", 2, 2000),
            ],
        );
        let (db, _) = Db::for_test("backfill-first");
        let s = run(&db, &dir, &[], 0, 10_000);
        db.flush().unwrap();
        assert_eq!((s.walked, s.indexed), (2, 2));
        assert_eq!(count(&db, "select count(*) from calls where audio = 1"), 2);
        let s = run(&db, &dir, &[], 0, 10_000);
        assert_eq!(
            s.walked, 0,
            "the second run only reconciles the recent folders, and none were given"
        );
    }

    #[test]
    fn a_changed_capture_folder_is_walked_again() {
        let a = capture("change-a", &[("s/2026/10/25/1-1_1", 1, 1000)]);
        let b = capture("change-b", &[("s/2026/10/25/3-3_1", 3, 3000)]);
        let (db, _) = Db::for_test("backfill-change");
        run(&db, &a, &[], 0, 10_000);
        let s = run(&db, &b, &[], 0, 10_000);
        db.flush().unwrap();
        assert_eq!(s.indexed, 1);
        assert_eq!(
            count(&db, "select count(*) from calls"),
            2,
            "the old rows stay"
        );
    }

    #[test]
    fn retention_bounds_the_backfill() {
        let now = 100 * 86_400_000;
        let dir = capture(
            "retention",
            &[
                ("s/old", 1, now - 200 * 86_400_000),
                ("s/new", 1, now - 10 * 86_400_000),
            ],
        );
        let (db, _) = Db::for_test("backfill-retention");
        let s = run(&db, &dir, &[], 120, now);
        assert_eq!((s.indexed, s.skipped), (1, 1));
    }

    #[test]
    fn a_clock_ahead_does_not_leave_the_archive_out() {
        let day = 86_400_000;
        let newest = 1000 * day;
        let dir = capture("clock-ahead", &[("s/a", 1, newest - 10 * day)]);
        let (db, _) = Db::for_test("backfill-clock-ahead");
        let r = record(1, newest);
        db.send(Write::Call {
            row: call_row("live/1", &r, &r.to_json(), true, true, None, 0),
            live: true,
        });
        db.flush().unwrap();
        // The clock says 500 days past the newest call in the store.
        let s = run(&db, &dir, &[], 120, newest + 500 * day);
        assert_eq!((s.indexed, s.skipped), (1, 0));
    }

    #[test]
    fn an_unreadable_record_is_skipped() {
        let dir = capture("bad", &[("s/good", 1, 1000)]);
        std::fs::write(dir.join("s/bad.json"), b"{ not json").unwrap();
        let (db, _) = Db::for_test("backfill-bad");
        let s = run(&db, &dir, &[], 0, 10_000);
        assert_eq!((s.indexed, s.skipped), (1, 1));
    }

    #[test]
    fn reconcile_indexes_only_the_recent_folders() {
        let dir = capture(
            "recent",
            &[
                ("s/2026/10/20/1-1_1", 1, 1000),
                ("s/2026/10/26/2-2_1", 2, 2000),
            ],
        );
        let (db, _) = Db::for_test("backfill-recent");
        run(&db, &dir, &[], 0, 10_000); // full walk
        std::fs::write(
            dir.join("s/2026/10/26/3-3_1.json"),
            record(3, 3000).to_json(),
        )
        .unwrap();
        std::fs::write(
            dir.join("s/2026/10/20/4-4_1.json"),
            record(4, 4000).to_json(),
        )
        .unwrap();
        let s = run(&db, &dir, &[dir.join("s/2026/10/26")], 0, 10_000);
        db.flush().unwrap();
        assert_eq!(s.indexed, 1, "only the recent folder's new call");
    }

    #[test]
    fn a_reconcile_whose_recent_folders_are_all_missing_counts_them() {
        let dir = capture("all-missing", &[("s/2026/10/20/1-1_1", 1, 1000)]);
        let (db, _) = Db::for_test("backfill-all-missing");
        run(&db, &dir, &[], 0, 10_000); // full walk
        let recent = [dir.join("s/2026/10/26"), dir.join("s/2026/10/25")];
        let s = run(&db, &dir, &recent, 0, 10_000);
        assert_eq!((s.missing, s.unreadable, s.walked), (2, 0, 0));
        std::fs::create_dir_all(&recent[0]).unwrap();
        let s = run(&db, &dir, &recent, 0, 10_000);
        assert_eq!(s.missing, 1, "one is there now");
    }

    #[test]
    fn a_full_walk_that_finds_no_records_is_not_marked_done() {
        let dir = TestDir::new("backfill-empty");
        let (db, _) = Db::for_test("backfill-empty");
        let s = run(&db, &dir, &[], 0, 10_000);
        db.flush().unwrap();
        assert_eq!((s.aborted, s.walked), (false, 0));
        assert_eq!(meta(&db, "backfill_done_at"), None, "a late mount re-walks");
        assert_eq!(meta(&db, "capture_dir"), None);
        std::fs::create_dir_all(dir.join("s")).unwrap();
        std::fs::write(dir.join("s/1.json"), record(1, 1000).to_json()).unwrap();
        let s = run(&db, &dir, &[], 0, 10_000);
        assert_eq!(s.indexed, 1);
        assert!(meta(&db, "backfill_done_at").is_some());
    }

    #[test]
    fn a_walk_that_could_not_read_a_folder_is_not_marked_done() {
        let stats = |walked, unreadable| Stats {
            walked,
            unreadable,
            ..Default::default()
        };
        assert!(not_done_why(&stats(10, 1)).is_some_and(|w| w.contains("could not be read")));
        assert!(not_done_why(&stats(0, 0)).is_some_and(|w| w.contains("no call records")));
        assert_eq!(not_done_why(&stats(10, 0)), None);
    }

    #[test]
    fn the_walk_hands_each_folders_records_over_before_going_deeper() {
        let dir = capture(
            "order",
            &[("b/2", 1, 1), ("a/z/3", 1, 1), ("a/1", 1, 1), ("0", 1, 1)],
        );
        let mut seen = Vec::new();
        walk(&[&*dir], &mut |f| {
            if let Found::Record(p) = f {
                let rel = p.strip_prefix(&*dir).unwrap().with_extension("");
                seen.push(rel.to_string_lossy().replace('\\', "/"));
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, ["0", "a/1", "a/z/3", "b/2"]);
    }

    #[test]
    fn recent_folders_cover_two_days_and_every_name() {
        let dir = TestDir::new("backfill-names");
        let ctx = crate::runtime::test_ctx(&dir);
        let cfg = ctx.config.lock().unwrap().clone();
        let folders = recent_folders(&cfg, 1_700_000_000);
        let want = |n: &str, t: i64| {
            Path::new(&cfg.recording.capture_dir).join(trunk_app::filename::default_path(
                n,
                "",
                t,
                crate::runtime::local_offset(t),
            ))
        };
        for n in ["ohmarcs", "ohmarcs-butler"] {
            for t in [1_700_000_000, 1_700_000_000 - 86_400] {
                assert!(folders.contains(&want(n, t)), "{n} {t}");
            }
        }
        let unique: BTreeSet<_> = folders.iter().collect();
        assert_eq!(unique.len(), folders.len());
    }

    #[test]
    fn a_long_batch_interval_does_not_throttle_the_backfill() {
        let calls: Vec<(String, u32, i64)> = (0..6000).map(|i| (format!("s/{i}"), 1, i)).collect();
        let refs: Vec<(&str, u32, i64)> =
            calls.iter().map(|(p, t, s)| (p.as_str(), *t, *s)).collect();
        let dir = capture("slow-batch", &refs);
        let (db, _) = Db::for_test_with_batch("backfill-slow-batch", Duration::from_secs(60));
        let t = std::time::Instant::now();
        let s = run(&db, &dir, &[], 0, 10_000);
        assert_eq!(s.indexed, 6000);
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "a 60 s batch interval held the backfill: {:?}",
            t.elapsed()
        );
        assert!(db.commits() >= 3, "{} commits", db.commits());
        db.flush().unwrap();
        assert_eq!(count(&db, "select count(*) from calls"), 6000);
    }

    #[test]
    fn backfill_yields_to_live_writes() {
        // More files than the queue holds, so the walk is still going when
        // the live write is sent; that is asserted, not assumed.
        let calls: Vec<(String, u32, i64)> = (0..6000).map(|i| (format!("s/{i}"), 1, i)).collect();
        let refs: Vec<(&str, u32, i64)> =
            calls.iter().map(|(p, t, s)| (p.as_str(), *t, *s)).collect();
        let dir = capture("yield", &refs);
        let (db, _) = Db::for_test("backfill-yield");
        let t = std::thread::spawn({
            let db = db.clone();
            let dir = dir.to_path_buf();
            move || run(&db, &dir, &[], 0, 10_000)
        });
        while db.queued() == 0 && !t.is_finished() {
            std::thread::sleep(Duration::from_millis(1));
        }
        let started = std::time::Instant::now();
        let r = record(9, 99);
        db.send(Write::Call {
            row: call_row("live/1", &r, &r.to_json(), true, true, None, 0),
            live: true,
        });
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a live write is never queued behind the backfill"
        );
        assert!(
            !t.is_finished(),
            "the walk was still running when the live write went in"
        );
        t.join().unwrap();
        db.flush().unwrap();
        assert!(
            db.with_reader(|c| crate::db::read::one(c, "live/1"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_capture_folder_that_cannot_be_read_is_not_marked_done() {
        let base = TestDir::new("backfill-missing");
        let dir = base.join("capture");
        let (db, _) = Db::for_test("backfill-missing");
        let s = run(&db, &dir, &[], 0, 10_000);
        db.flush().unwrap();
        assert!(s.aborted);
        assert_eq!(meta(&db, "backfill_done_at"), None);
        assert_eq!(meta(&db, "capture_dir"), None);
        // Once the folder is there, the next run walks it.
        std::fs::create_dir_all(dir.join("s")).unwrap();
        std::fs::write(dir.join("s/1.json"), record(1, 1000).to_json()).unwrap();
        let s = run(&db, &dir, &[], 0, 10_000);
        assert_eq!((s.aborted, s.indexed), (false, 1));
        assert!(meta(&db, "backfill_done_at").is_some());
    }

    #[test]
    fn only_one_backfill_runs_at_a_time() {
        let (db, _) = Db::for_test("backfill-single");
        assert!(db.try_begin_backfill());
        assert!(!db.try_begin_backfill(), "a second walk while one is going");
        // `start` leaves without a thread while the flag is held.
        let base = TestDir::new("backfill-single");
        let dir = base.join("capture");
        let ctx = crate::runtime::test_ctx(&dir);
        assert!(ctx.db.try_begin_backfill());
        start(ctx.clone());
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !dir.exists() || meta(&ctx.db, "backfill_done_at").is_none(),
            "no walk ran"
        );
        ctx.db.end_backfill();
        start(ctx.clone());
        for _ in 0..200 {
            if ctx.db.try_begin_backfill() {
                break; // the walk finished and gave the flag back
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        db.end_backfill();
        assert!(db.try_begin_backfill());
    }

    #[test]
    fn a_store_that_is_off_starts_no_backfill() {
        let base = TestDir::new("backfill-off");
        let store = base.join("trunk-pro.sqlite");
        crate::db::seed_newer(&store);
        let ctx = crate::runtime::test_ctx_with_store(&base.join("capture"), &store);
        assert!(ctx.db.is_disabled());
        start(ctx.clone());
        assert!(!ctx.db.try_begin_backfill(), "off: nothing to claim");
    }
}
