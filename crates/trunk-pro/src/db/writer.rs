//! The one thread that writes the call store. Writes arrive on a channel and
//! are applied in batches, one transaction each, so a busy system costs about
//! one commit a second while a lone write still commits at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use trunk_app::protocol::ToClient;

use super::Write;
use super::row::CallRow;
use crate::runtime::{Hub, publish};

/// At most one commit per this long under load.
const COMMIT_EVERY: Duration = Duration::from_secs(1);

/// Start the `db-writer` thread. It runs until every sender of `rx` is gone.
/// `queued` counts the writes sent and not yet taken off the channel.
pub fn spawn(
    conn: Connection,
    hub: Hub,
    rx: Receiver<Write>,
    commits: Arc<AtomicU64>,
    queued: Arc<AtomicUsize>,
) -> Option<std::thread::JoinHandle<()>> {
    let spawned = std::thread::Builder::new()
        .name("db-writer".into())
        .spawn(move || {
            let mut w = Writer {
                conn,
                hub,
                commits,
                queued,
            };
            w.run(rx);
        });
    spawned
        .inspect_err(|e| log::error!("Call store: could not start the writer thread: {e}"))
        .ok()
}

struct Writer {
    conn: Connection,
    /// For announcing changes to browsers once rows are written.
    hub: Hub,
    commits: Arc<AtomicU64>,
    queued: Arc<AtomicUsize>,
}

impl Writer {
    fn run(&mut self, rx: Receiver<Write>) {
        while let Ok(first) = rx.recv() {
            let started = Instant::now();
            let mut flushes = Vec::new();
            let mut checkpoint = false;
            let mut retention: Option<i64> = None;
            let mut announce = Vec::new();
            let mut outcome = Ok(());
            match self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
            {
                Ok(tx) => {
                    apply(
                        &tx,
                        first,
                        &mut flushes,
                        &mut checkpoint,
                        &mut retention,
                        &mut announce,
                        &self.queued,
                    );
                    while let Ok(next) = rx.try_recv() {
                        apply(
                            &tx,
                            next,
                            &mut flushes,
                            &mut checkpoint,
                            &mut retention,
                            &mut announce,
                            &self.queued,
                        );
                    }
                    match tx.commit() {
                        Ok(()) => {
                            self.commits.fetch_add(1, Ordering::Relaxed);
                            for m in announce.drain(..) {
                                publish(&self.hub, m);
                            }
                        }
                        Err(e) => {
                            log::warn!("Call store: commit failed: {e}");
                            outcome = Err(format!("commit failed: {e}"));
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Call store: could not begin a transaction: {e}");
                    outcome = Err(format!("could not begin a transaction: {e}"));
                    self.queued.fetch_sub(1, Ordering::Relaxed);
                    while let Ok(next) = rx.try_recv() {
                        self.queued.fetch_sub(1, Ordering::Relaxed);
                        if let Write::Flush(done) = next {
                            flushes.push(done);
                        }
                    }
                    if let Write::Flush(done) = first {
                        flushes.push(done);
                    }
                }
            }
            if let Some(before_ms) = retention {
                self.retain(before_ms);
            }
            if checkpoint {
                self.checkpoint();
            }
            for done in flushes {
                let _ = done.send(outcome.clone());
            }
            if let Some(rest) = COMMIT_EVERY.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    }

    /// Delete the calls that started before `before_ms`, with their
    /// transmissions and frequencies (they cascade), oldest first, 1,000 to a
    /// transaction so readers and the next writes are never held up long.
    /// The audio and JSON files are not touched.
    fn retain(&mut self, before_ms: i64) {
        let mut total = 0usize;
        loop {
            let batch = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .and_then(|tx| {
                    let n = tx.execute(
                        "delete from calls where id in
                        (select id from calls where start_ms < ?1 order by start_ms limit 1000)",
                        [before_ms],
                    )?;
                    tx.commit()?;
                    Ok(n)
                });
            match batch {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    self.commits.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    log::warn!("Call store: retention failed: {e}");
                    break;
                }
            }
        }
        if total > 0 {
            log::info!("Call store: retention removed {total} call records");
        }
    }

    fn checkpoint(&self) {
        if let Err(e) = self
            .conn
            .query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))
        {
            log::warn!("Call store: checkpoint failed: {e}");
        }
    }
}

/// Apply one write inside the batch's transaction. Failures are logged; the
/// batch carries on.
fn apply(
    conn: &Connection,
    w: Write,
    flushes: &mut Vec<Sender<Result<(), String>>>,
    checkpoint: &mut bool,
    retention: &mut Option<i64>,
    announce: &mut Vec<ToClient>,
    queued: &AtomicUsize,
) {
    queued.fetch_sub(1, Ordering::Relaxed);
    match w {
        Write::Call { row, live } => {
            if let Err(e) = write_call(conn, &row, live) {
                log::warn!("Call store: call write {} failed: {e}", row.path);
            }
        }
        Write::CallFiles { path, audio, json } => match set_files(conn, &path, audio, json) {
            Ok(true) => announce.push(ToClient::CallFiles { path, audio, json }),
            Ok(false) => {}
            Err(e) => log::warn!("Call store: files write {path} failed: {e}"),
        },
        // Runs after the batch commits, in transactions of its own.
        Write::Retention { before_ms } => {
            *retention = Some(retention.map_or(before_ms, |b| b.max(before_ms)));
        }
        Write::Meta { key, value } => {
            let r = conn.execute(
                "insert into meta (key, value) values (?1, ?2)
                 on conflict(key) do update set value = excluded.value",
                params![key, value],
            );
            if let Err(e) = r {
                log::warn!("Call store: meta write {key} failed: {e}");
            }
        }
        Write::Checkpoint => *checkpoint = true,
        Write::Flush(done) => flushes.push(done),
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
    use crate::runtime::Out;

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

    #[test]
    fn call_files_updates_the_flags_and_says_so_once() {
        let (hub, mut rx) = tokio::sync::broadcast::channel(16);
        let (db, _) = Db::for_test_on("files", hub);
        let r = record(1, 1000);
        db.send(Write::Call {
            row: call_row("ohmarcs/b", &r, &r.to_json(), true, true, None, 1),
            live: true,
        });
        // Same batch: the first update changes nothing, the second does.
        db.send(Write::CallFiles {
            path: "ohmarcs/b".into(),
            audio: true,
            json: true,
        });
        db.send(Write::CallFiles {
            path: "ohmarcs/b".into(),
            audio: false,
            json: true,
        });
        db.flush().unwrap();
        let m = rx.try_recv().expect("one callFiles");
        let Out::Text(s) = &*m else {
            panic!("callFiles goes to every connection")
        };
        assert!(
            s.contains("\"callFiles\"") && s.contains("\"audio\":false"),
            "{s}"
        );
        assert!(
            rx.try_recv().is_err(),
            "no message for the unchanged update"
        );
    }

    #[test]
    fn retention_deletes_old_rows_in_batches_and_cascades() {
        let (db, _) = Db::for_test("retention");
        for i in 0..2500u32 {
            let r = record(1, i as i64);
            db.send(Write::Call {
                row: call_row(&format!("s/{i}"), &r, &r.to_json(), true, true, None, 0),
                live: true,
            });
        }
        db.flush().unwrap();
        let before = db.commits();
        db.send(Write::Retention { before_ms: 2200 });
        db.flush().unwrap();
        let (n, tx): (u32, u32) = db
            .with_reader(|c| {
                Ok((
                    c.query_row("select count(*) from calls", [], |r| r.get(0))?,
                    c.query_row("select count(*) from call_transmissions", [], |r| r.get(0))?,
                ))
            })
            .unwrap();
        assert_eq!((n, tx), (300, 600));
        assert!(
            db.commits() - before >= 3,
            "2200 rows in batches of 1000: at least three commits"
        );
        db.send(Write::Retention { before_ms: 0 });
        db.flush().unwrap();
        let n: u32 = db
            .with_reader(|c| c.query_row("select count(*) from calls", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 300, "nothing before 0");
    }
}
