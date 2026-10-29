//! The one thread that writes the call store, whatever its backend. Writes
//! arrive on a channel and are applied in batches, one transaction each:
//! under load one commit per `storage.commitSeconds`, while a lone write
//! after a quiet spell commits at once. Each commit writes whole pages (on
//! SQLite twice: the WAL, then the checkpoint), so fewer commits are gentler
//! on flash (research/storage.md). It persists only: the live list and the
//! interface are told by whoever sends the write. Retention runs here too, a
//! chunk at a time between batches.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use super::store::{CallStore, Change};
use super::{DAY_MS, Write};

/// Most writes one transaction takes: it bounds how long one runs (a
/// backfill sends this many, then flushes, so live writes never queue
/// behind it).
pub(super) const MAX_BATCH: usize = 1000;
/// Calls retention deletes in one transaction.
const RETAIN_CHUNK: usize = 1000;
/// How often retention looks for calls past `retentionDays`.
const RETAIN_EVERY: Duration = Duration::from_secs(3600);

/// Start the `db-writer` thread over `store`; it runs until every sender of
/// `rx` is gone. `batch`: the least time between two commits under load.
/// `retention_days`: calls kept (0: all).
pub fn spawn(store: Arc<dyn CallStore>, rx: Receiver<Write>, commits: Arc<AtomicU64>, batch: Duration, retention_days: u32) -> Option<std::thread::JoinHandle<()>> {
    let spawned = std::thread::Builder::new().name("db-writer".into()).spawn(move || {
        let mut w = Writer { store, commits, batch, retention_days, retiring: None, retain_at: Instant::now() };
        w.run(rx);
    });
    spawned.inspect_err(|e| log::error!("Call store: could not start the writer thread: {e}")).ok()
}

struct Writer {
    store: Arc<dyn CallStore>,
    commits: Arc<AtomicU64>,
    batch: Duration,
    retention_days: u32,
    /// Retention under way: calls that started before this go, a chunk per pass.
    retiring: Option<i64>,
    /// When retention next looks.
    retain_at: Instant,
}

impl Writer {
    fn run(&mut self, rx: Receiver<Write>) {
        let mut last_commit: Option<Instant> = None;
        loop {
            // Idle: a write, or retention's next chunk (at once) or next look.
            let idle = if self.retiring.is_some() { Duration::ZERO } else { self.retain_at.saturating_duration_since(Instant::now()) };
            let first = match rx.recv_timeout(idle) {
                Ok(w) => w,
                Err(RecvTimeoutError::Timeout) => {
                    self.retain_step();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut held = vec![first];
            let mut disconnected = false;
            // Wait out the interval since the last commit, taking writes as they come;
            // a flush, a full batch or the last handle going ends the wait at once.
            if let Some(at) = last_commit {
                let mut flushing = matches!(held[0], Write::Flush(_) | Write::Talkgroups { .. });
                while !flushing && held.len() < MAX_BATCH {
                    let Some(rest) = self.batch.checked_sub(at.elapsed()) else { break };
                    match rx.recv_timeout(rest) {
                        Ok(w) => {
                            flushing = matches!(w, Write::Flush(_) | Write::Talkgroups { .. });
                            held.push(w);
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
            }
            last_commit = Some(Instant::now());
            // (What else is waiting joins the batch, up to its size.)
            while held.len() < MAX_BATCH {
                let Ok(next) = rx.try_recv() else { break };
                held.push(next);
            }
            self.commit(held);
            if disconnected {
                break;
            }
        }
        self.store.close();
    }

    /// Apply `held` as one batch, then what came with it: a checkpoint, a
    /// retention cut (tests), the flushes waiting on it. A batch that can't
    /// be committed is lost to the store (the live list still has its calls,
    /// and the next start's reconcile indexes them from their files).
    fn commit(&mut self, held: Vec<Write>) {
        let (mut changes, mut flushes, mut checkpoint): (Vec<Change>, Vec<Sender<Result<(), String>>>, bool) = (Vec::new(), Vec::new(), false);
        // (Set only by tests' Write::Retention: the writer's own retention runs between batches.)
        #[allow(unused_mut)]
        let mut retire: Option<i64> = None;
        for w in held {
            match w {
                Write::Call { row, live } => changes.push(Change::Call { row: Box::new(row), live }),
                Write::CallFiles { path, audio, json } => changes.push(Change::CallFiles { path, audio, json }),
                Write::Meta { key, value } => changes.push(Change::Meta { key, value }),
                Write::Kept { kind, key, value } => changes.push(Change::Kept { kind, key, value }),
                // (Turned into JSON here, off the engine's thread: a whole registry can be megabytes.)
                Write::Registry { system, changes: c } => changes.extend(serde_json::to_string(&c).ok().map(|json| Change::RegistryChanges { system, json })),
                Write::StatsMinute { t, json } => changes.push(Change::StatsMinute { t, json }),
                Write::StatsHours(rows) => changes.push(Change::StatsHours(rows)),
                Write::RetireStats { minutes_before, hours_before } => changes.push(Change::RetireStats { minutes_before, hours_before }),
                Write::Talkgroups { system, old, new } => {
                    let (lines, len) = super::store::talkgroup_lines(&old, &new);
                    changes.push(Change::Talkgroups { system, lines, len });
                }
                Write::RegistryWhole { system, radio } => changes.extend(serde_json::to_string(&radio).ok().map(|json| Change::RegistryWhole { system, json })),
                #[cfg(test)]
                Write::Retention { before_ms } => retire = Some(retire.map_or(before_ms, |b| b.max(before_ms))),
                Write::Checkpoint => checkpoint = true,
                Write::Flush(done) => flushes.push(done),
            }
        }
        let outcome = if changes.is_empty() {
            Ok(())
        } else {
            let applied = self.store.apply(changes);
            match &applied {
                Ok(()) => {
                    self.commits.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => log::warn!("Call store: {e}"),
            }
            applied
        };
        if let Some(b) = retire.filter(|_| outcome.is_ok()) {
            self.retiring = Some(self.retiring.map_or(b, |r| r.max(b)));
        }
        if checkpoint {
            self.store.checkpoint();
        }
        for done in flushes {
            let _ = done.send(outcome.clone());
        }
    }

    /// Retention's next step: when it's time to look, where `retentionDays`
    /// cuts (counted back from now, but at most a day past the newest call,
    /// so a clock jumping ahead can't empty the store); then one chunk of the
    /// oldest calls past the cut, with their transmissions and frequencies.
    /// The files on disk aren't touched.
    fn retain_step(&mut self) {
        if self.retiring.is_none() {
            self.retain_at = Instant::now() + RETAIN_EVERY;
            if self.retention_days == 0 {
                return;
            }
            let now = crate::clock::unix_ms() as i64;
            let now = self.store.newest_start().ok().flatten().map_or(now, |n| now.min(n.saturating_add(DAY_MS)));
            self.retiring = Some(now - i64::from(self.retention_days) * DAY_MS);
        }
        let Some(before_ms) = self.retiring else { return };
        match self.store.retire(before_ms, RETAIN_CHUNK) {
            Ok(n) => {
                if n > 0 {
                    self.commits.fetch_add(1, Ordering::Relaxed);
                    log::debug!("Call store: retention removed {n} call records");
                }
                if n < RETAIN_CHUNK {
                    self.retiring = None;
                }
            }
            Err(e) => {
                log::warn!("Call store: retention failed: {e}");
                self.retiring = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::row::{call_row, tests::record};
    use super::super::{Db, Write};
    use std::time::Duration;

    #[test]
    fn a_stream_commits_once_per_interval() {
        for db in Db::for_each_backend("interval", Duration::from_secs(2)) {
            let t = std::time::Instant::now();
            while t.elapsed() < Duration::from_millis(4500) {
                db.send(Write::Meta { key: format!("k{}", t.elapsed().as_millis()), value: "v".into() });
                std::thread::sleep(Duration::from_millis(50));
            }
            db.flush().unwrap();
            assert!(db.commits() <= 4, "{} commits in 4.5 s at a 2 s interval ({})", db.commits(), db.kind());
        }
        for db in Db::for_each_backend("lone", Duration::from_secs(60)) {
            let t = std::time::Instant::now();
            db.send(Write::Meta { key: "a".into(), value: "1".into() });
            db.flush().unwrap();
            assert!(t.elapsed() < Duration::from_millis(1500), "the first write after a quiet spell commits at once ({})", db.kind());
        }
    }

    #[test]
    fn one_transaction_takes_at_most_a_batch() {
        for db in Db::for_each_backend("drain-cap", Duration::from_secs(60)) {
            // A producer faster than the writer must not make one transaction
            // swallow everything, whatever the disk speed.
            for i in 0..2500 {
                db.send(Write::Meta { key: format!("k{i}"), value: "v".into() });
            }
            db.flush().unwrap();
            assert!(db.commits() >= 3, "{} commits for 2,500 writes", db.commits());
            assert_eq!(db.count("select count(*) from meta where key like 'k%'"), 2500);
        }
    }

    #[test]
    fn dropping_the_last_handle_does_not_wait_out_the_interval() {
        let (db, path) = Db::for_test_with_batch("drop-fast", Duration::from_secs(60));
        db.send(Write::Meta { key: "a".into(), value: "1".into() });
        db.flush().unwrap();
        // The next write is held back for the interval.
        db.send(Write::Meta { key: "b".into(), value: "2".into() });
        let t = std::time::Instant::now();
        drop(db);
        assert!(t.elapsed() < Duration::from_millis(1500), "the writer leaves at once: {:?}", t.elapsed());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn a_live_row_wins_over_a_backfilled_one() {
        for db in Db::for_each_backend("upsert", Duration::from_secs(1)) {
            let r = record(9179, 1_700_000_000_000);
            let json = r.to_json();
            db.send(Write::Call { row: call_row("ohmarcs/a", &r, &json, true, true, None, 1), live: true });
            db.send(Write::Call { row: call_row("ohmarcs/a", &r, &json, false, false, None, 2), live: false });
            db.flush().unwrap();
            assert_eq!(db.count("select count(*) from calls where path = 'ohmarcs/a'"), 1);
            assert_eq!(db.one("ohmarcs/a").unwrap().unwrap()["audio"], true, "the backfilled copy is ignored ({})", db.kind());
            db.send(Write::Call { row: call_row("ohmarcs/a", &r, &json, false, true, None, 3), live: true });
            db.flush().unwrap();
            assert_eq!(db.one("ohmarcs/a").unwrap().unwrap()["audio"], false, "a live copy replaces");
            assert_eq!(db.count("select count(*) from call_transmissions"), 2, "children replaced, not duplicated");
        }
    }

    /// A call whose transmissions can't be written leaves nothing behind
    /// (no row without its children), and the batch's other calls land.
    #[test]
    fn a_call_is_written_whole_or_not_at_all() {
        for db in Db::for_each_backend("whole", Duration::from_secs(1)) {
            // A trigger that refuses the second call's transmissions.
            db.refuse_unit_for_test(666);
            let ok = record(1, 1000);
            let mut bad = record(2, 2000);
            bad.src_list[1].src = 666;
            db.send(Write::Call { row: call_row("s/ok", &ok, &ok.to_json(), true, true, None, 0), live: true });
            db.send(Write::Call { row: call_row("s/bad", &bad, &bad.to_json(), true, true, None, 0), live: true });
            db.flush().unwrap();
            let got = (db.count("select count(*) from calls"), db.count("select count(*) from call_transmissions"));
            assert_eq!(got, (1, 2), "s/ok and its two transmissions; nothing of s/bad ({})", db.kind());
        }
    }

    #[test]
    fn call_files_updates_the_flags() {
        for db in Db::for_each_backend("files", Duration::from_secs(1)) {
            let r = record(1, 1000);
            db.send(Write::Call { row: call_row("ohmarcs/b", &r, &r.to_json(), true, true, None, 1), live: true });
            db.send(Write::CallFiles { path: "ohmarcs/b".into(), audio: false, json: true });
            db.flush().unwrap();
            let e = db.one("ohmarcs/b").unwrap().unwrap();
            assert_eq!((e["audio"].as_bool(), e["json"].as_bool()), (Some(false), Some(true)), "{}", db.kind());
        }
    }

    /// Calls (and transmissions) once `calls` says `want`, or two seconds.
    fn wait_for(db: &Db, want: i64) -> (i64, i64) {
        let t = std::time::Instant::now();
        loop {
            let n = (db.count("select count(*) from calls"), db.count("select count(*) from call_transmissions"));
            if n.0 == want || t.elapsed() > Duration::from_secs(2) {
                return n;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Retention deletes a chunk at a time between batches, with the calls'
    /// transmissions, and a live write isn't held up meanwhile.
    #[test]
    fn retention_deletes_old_rows_a_chunk_at_a_time_and_cascades() {
        for db in Db::for_each_backend("retention", Duration::from_secs(1)) {
            for i in 0..2500u32 {
                let r = record(1, i as i64);
                db.send(Write::Call { row: call_row(&format!("s/{i}"), &r, &r.to_json(), true, true, None, 0), live: true });
            }
            db.flush().unwrap();
            let before = db.commits();
            db.send(Write::Retention { before_ms: 2200 });
            db.flush().unwrap();
            let r = record(1, 9999);
            db.send(Write::Call { row: call_row("live", &r, &r.to_json(), true, true, None, 0), live: true });
            db.flush().unwrap();
            assert_eq!(wait_for(&db, 301), (301, 602), "{}", db.kind());
            assert!(db.commits() - before >= 4, "2200 rows in chunks of 1000, and the live write's own");
            db.send(Write::Retention { before_ms: 0 });
            db.flush().unwrap();
            assert_eq!(wait_for(&db, 301).0, 301, "nothing before 0");
        }
    }
}
