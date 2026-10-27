//! The one thread that writes the call store. Writes arrive on a channel and
//! are applied in batches, one transaction each: under load one commit per
//! `storage.commitSeconds`, while a lone write after a quiet spell commits at
//! once. Each commit writes whole pages, twice (the WAL, then the checkpoint),
//! so fewer commits are gentler on flash (research/storage.md). It persists
//! only: the live list and the interface are told by whoever sends the write.
//! Retention runs here too, a chunk at a time between batches.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::row::CallRow;
use super::{DAY_MS, Write};

/// Most writes one transaction takes: it bounds how long one runs (a
/// backfill sends this many, then flushes, so live writes never queue
/// behind it).
pub(super) const MAX_BATCH: usize = 1000;
/// Calls retention deletes in one transaction.
const RETAIN_CHUNK: i64 = 1000;
/// How often retention looks for calls past `retentionDays`.
const RETAIN_EVERY: Duration = Duration::from_secs(3600);

/// Start the `db-writer` thread; it runs until every sender of `rx` is gone.
/// `batch`: the least time between two commits under load. `retention_days`:
/// calls kept (0: all).
pub fn spawn(conn: Connection, rx: Receiver<Write>, commits: Arc<AtomicU64>, batch: Duration, retention_days: u32) -> Option<std::thread::JoinHandle<()>> {
    let spawned = std::thread::Builder::new().name("db-writer".into()).spawn(move || {
        let mut w = Writer { conn, commits, batch, retention_days, retiring: None, retain_at: Instant::now() };
        w.run(rx);
    });
    spawned.inspect_err(|e| log::error!("Call store: could not start the writer thread: {e}")).ok()
}

struct Writer {
    conn: Connection,
    commits: Arc<AtomicU64>,
    batch: Duration,
    retention_days: u32,
    /// Retention under way: calls that started before this go, a chunk per pass.
    retiring: Option<i64>,
    /// When retention next looks.
    retain_at: Instant,
}

/// What one transaction collects while its writes are applied.
#[derive(Default)]
struct Batch {
    /// Waiting `flush` callers, answered once the batch is done.
    flushes: Vec<Sender<Result<(), String>>>,
    checkpoint: bool,
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
                let mut flushing = matches!(held[0], Write::Flush(_));
                while !flushing && held.len() < MAX_BATCH {
                    let Some(rest) = self.batch.checked_sub(at.elapsed()) else { break };
                    match rx.recv_timeout(rest) {
                        Ok(w) => {
                            flushing = matches!(w, Write::Flush(_));
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
            let mut batch = Batch::default();
            // (What else is waiting joins the batch, up to its size.)
            while held.len() < MAX_BATCH {
                let Ok(next) = rx.try_recv() else { break };
                held.push(next);
            }
            let outcome = self.commit(held, &mut batch);
            if batch.checkpoint {
                self.checkpoint();
            }
            for done in batch.flushes {
                let _ = done.send(outcome.clone());
            }
            if disconnected {
                break;
            }
        }
        // (The planner's statistics, kept current as SQLite advises, on the way out.)
        if let Err(e) = self.conn.execute_batch("pragma optimize") {
            log::debug!("Call store: optimize: {e}");
        }
    }

    /// Apply `held` in one transaction. A write that fails is logged and the
    /// rest go on; a transaction that can't begin or commit loses the batch
    /// (the live list still has its calls, and the next start's reconcile
    /// indexes them from their files).
    fn commit(&mut self, held: Vec<Write>, batch: &mut Batch) -> Result<(), String> {
        let mut tx = match self.conn.transaction_with_behavior(TransactionBehavior::Immediate) {
            Ok(tx) => tx,
            Err(e) => {
                log::warn!("Call store: could not begin a transaction: {e}");
                batch.flushes.extend(held.into_iter().filter_map(|w| if let Write::Flush(done) = w { Some(done) } else { None }));
                return Err(format!("could not begin a transaction: {e}"));
            }
        };
        // (Set only by tests' Write::Retention: the writer's own retention runs between batches.)
        #[allow(unused_mut)]
        let mut retire = None;
        for w in held {
            match w {
                // (A call with its transmissions and frequencies, or none of it.)
                Write::Call { row, live } => {
                    let written = tx.savepoint().and_then(|sp| {
                        write_call(&sp, &row, live)?;
                        sp.commit()
                    });
                    if let Err(e) = written {
                        log::warn!("Call store: call write {} failed: {e}", row.path);
                    }
                }
                Write::CallFiles { path, audio, json } => {
                    if let Err(e) = set_files(&tx, &path, audio, json) {
                        log::warn!("Call store: files write {path} failed: {e}");
                    }
                }
                Write::Meta { key, value } => {
                    if let Err(e) = tx.execute("insert into meta (key, value) values (?1, ?2) on conflict(key) do update set value = excluded.value", params![key, value]) {
                        log::warn!("Call store: meta write {key} failed: {e}");
                    }
                }
                #[cfg(test)]
                Write::Retention { before_ms } => retire = Some(retire.map_or(before_ms, |b: i64| b.max(before_ms))),
                Write::Checkpoint => batch.checkpoint = true,
                Write::Flush(done) => batch.flushes.push(done),
            }
        }
        match tx.commit() {
            Ok(()) => {
                self.commits.fetch_add(1, Ordering::Relaxed);
                if let Some(b) = retire {
                    self.retiring = Some(self.retiring.map_or(b, |r| r.max(b)));
                }
                Ok(())
            }
            Err(e) => {
                log::warn!("Call store: commit failed: {e}");
                Err(format!("commit failed: {e}"))
            }
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
            let newest: Option<i64> = self.conn.query_row("select max(start_ms) from calls", [], |r| r.get(0)).unwrap_or(None);
            let now = crate::clock::unix_ms() as i64;
            let now = newest.map_or(now, |n| now.min(n.saturating_add(DAY_MS)));
            self.retiring = Some(now - i64::from(self.retention_days) * DAY_MS);
        }
        let Some(before_ms) = self.retiring else { return };
        let gone = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).and_then(|tx| {
            let n = tx.execute("delete from calls where id in (select id from calls where start_ms < ?1 order by start_ms limit ?2)", params![before_ms, RETAIN_CHUNK])?;
            tx.commit()?;
            Ok(n)
        });
        match gone {
            Ok(n) => {
                if n > 0 {
                    self.commits.fetch_add(1, Ordering::Relaxed);
                    log::debug!("Call store: retention removed {n} call records");
                }
                if (n as i64) < RETAIN_CHUNK {
                    self.retiring = None;
                }
            }
            Err(e) => {
                log::warn!("Call store: retention failed: {e}");
                self.retiring = None;
            }
        }
    }

    fn checkpoint(&self) {
        if let Err(e) = self.conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(())) {
            log::warn!("Call store: checkpoint failed: {e}");
        }
    }
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
    use super::super::row::{call_row, tests::record};
    use super::super::{Db, Write};

    #[test]
    fn a_stream_commits_once_per_interval() {
        let (db, _) = Db::for_test_with_batch("interval", std::time::Duration::from_secs(2));
        let t = std::time::Instant::now();
        while t.elapsed() < std::time::Duration::from_millis(4500) {
            db.send(Write::Meta {
                key: format!("k{}", t.elapsed().as_millis()),
                value: "v".into(),
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        db.flush().unwrap();
        assert!(
            db.commits() <= 4,
            "{} commits in 4.5 s at a 2 s interval",
            db.commits()
        );
        let (db2, _) = Db::for_test_with_batch("lone", std::time::Duration::from_secs(60));
        let t = std::time::Instant::now();
        db2.send(Write::Meta {
            key: "a".into(),
            value: "1".into(),
        });
        db2.flush().unwrap();
        assert!(
            t.elapsed() < std::time::Duration::from_millis(1500),
            "the first write after a quiet spell commits at once"
        );
    }

    #[test]
    fn one_transaction_takes_at_most_a_batch() {
        let (db, _) = Db::for_test_with_batch("drain-cap", std::time::Duration::from_secs(60));
        // A producer faster than the writer must not make one transaction
        // swallow everything, whatever the disk speed.
        for i in 0..2500 {
            db.send(Write::Meta {
                key: format!("k{i}"),
                value: "v".into(),
            });
        }
        db.flush().unwrap();
        assert!(
            db.commits() >= 3,
            "{} commits for 2,500 writes",
            db.commits()
        );
        let n: u32 = db
            .with_reader(|c| {
                c.query_row("select count(*) from meta where key like 'k%'", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        assert_eq!(n, 2500);
    }

    #[test]
    fn dropping_the_last_handle_does_not_wait_out_the_interval() {
        let (db, path) = Db::for_test_with_batch("drop-fast", std::time::Duration::from_secs(60));
        db.send(Write::Meta {
            key: "a".into(),
            value: "1".into(),
        });
        db.flush().unwrap();
        // The next write is held back for the interval.
        db.send(Write::Meta {
            key: "b".into(),
            value: "2".into(),
        });
        let t = std::time::Instant::now();
        drop(db);
        assert!(
            t.elapsed() < std::time::Duration::from_millis(1500),
            "the writer leaves at once: {:?}",
            t.elapsed()
        );
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn a_live_row_wins_over_a_backfilled_one() {
        let (db, _) = Db::for_test("upsert");
        let r = record(9179, 1_700_000_000_000);
        let json = r.to_json();
        db.send(Write::Call {
            row: call_row("ohmarcs/a", &r, &json, true, true, None, 1),
            live: true,
        });
        db.send(Write::Call {
            row: call_row("ohmarcs/a", &r, &json, false, false, None, 2),
            live: false,
        });
        db.flush().unwrap();
        let (n, audio): (u32, bool) = db
            .with_reader(|c| {
                c.query_row(
                    "select count(*), max(audio) from calls where path='ohmarcs/a'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!((n, audio), (1, true), "the backfilled copy is ignored");
        db.send(Write::Call {
            row: call_row("ohmarcs/a", &r, &json, false, true, None, 3),
            live: true,
        });
        db.flush().unwrap();
        let audio: bool = db
            .with_reader(|c| {
                c.query_row("select audio from calls where path='ohmarcs/a'", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        assert!(!audio, "a live copy replaces");
        let tx: u32 = db
            .with_reader(|c| {
                c.query_row("select count(*) from call_transmissions", [], |r| r.get(0))
            })
            .unwrap();
        assert_eq!(tx, 2, "children replaced, not duplicated");
    }

    /// A call whose transmissions can't be written leaves nothing behind
    /// (no row without its children), and the batch's other calls land.
    #[test]
    fn a_call_is_written_whole_or_not_at_all() {
        let (db, path) = Db::for_test("whole");
        // A trigger that fails the second call's transmissions.
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("create trigger refuse before insert on call_transmissions when new.unit = 666 begin select raise(abort, 'refused'); end;").unwrap();
        drop(c);
        let ok = record(1, 1000);
        let mut bad = record(2, 2000);
        bad.src_list[1].src = 666;
        db.send(Write::Call { row: call_row("s/ok", &ok, &ok.to_json(), true, true, None, 0), live: true });
        db.send(Write::Call { row: call_row("s/bad", &bad, &bad.to_json(), true, true, None, 0), live: true });
        db.flush().unwrap();
        let (calls, tx): (u32, u32) = db.with_reader(|c| Ok((c.query_row("select count(*) from calls", [], |r| r.get(0))?, c.query_row("select count(*) from call_transmissions", [], |r| r.get(0))?))).unwrap();
        assert_eq!((calls, tx), (1, 2), "s/ok and its two transmissions; nothing of s/bad");
    }

    #[test]
    fn call_files_updates_the_flags() {
        let (db, _) = Db::for_test("files");
        let r = record(1, 1000);
        db.send(Write::Call { row: call_row("ohmarcs/b", &r, &r.to_json(), true, true, None, 1), live: true });
        db.send(Write::CallFiles { path: "ohmarcs/b".into(), audio: false, json: true });
        db.flush().unwrap();
        let (audio, json): (bool, bool) = db.with_reader(|c| c.query_row("select audio, json from calls where path = 'ohmarcs/b'", [], |r| Ok((r.get(0)?, r.get(1)?)))).unwrap();
        assert_eq!((audio, json), (false, true));
    }

    /// Calls until `count` says `want`, or two seconds.
    fn wait_for(db: &Db, want: u32) -> (u32, u32) {
        let t = std::time::Instant::now();
        loop {
            let n: (u32, u32) = db.with_reader(|c| Ok((c.query_row("select count(*) from calls", [], |r| r.get(0))?, c.query_row("select count(*) from call_transmissions", [], |r| r.get(0))?))).unwrap();
            if n.0 == want || t.elapsed() > std::time::Duration::from_secs(2) {
                return n;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Retention deletes a chunk at a time between batches, with the calls'
    /// transmissions, and a live write isn't held up meanwhile.
    #[test]
    fn retention_deletes_old_rows_a_chunk_at_a_time_and_cascades() {
        let (db, _) = Db::for_test("retention");
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
        assert_eq!(wait_for(&db, 301), (301, 602));
        assert!(db.commits() - before >= 4, "2200 rows in chunks of 1000, and the live write's own");
        db.send(Write::Retention { before_ms: 0 });
        db.flush().unwrap();
        assert_eq!(wait_for(&db, 301).0, 301, "nothing before 0");
    }
}
