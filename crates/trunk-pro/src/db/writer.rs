//! The one thread that writes the call store. Writes arrive on a channel and
//! are applied in batches, one transaction each, so a busy system costs about
//! one commit a second while a lone write still commits at once.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use super::Write;
use crate::runtime::Hub;

/// At most one commit per this long under load.
const COMMIT_EVERY: Duration = Duration::from_secs(1);
/// Past this, the WAL is checkpointed after a commit.
const WAL_LIMIT: u64 = 64 * 1024 * 1024;

/// Start the `db-writer` thread. It runs until every sender of `rx` is gone.
/// `queued` counts the writes sent and not yet taken off the channel.
pub fn spawn(
    conn: Connection,
    hub: Hub,
    rx: Receiver<Write>,
    commits: Arc<AtomicU64>,
    queued: Arc<AtomicUsize>,
) {
    let wal = conn.path().map(|p| PathBuf::from(format!("{p}-wal")));
    let spawned = std::thread::Builder::new()
        .name("db-writer".into())
        .spawn(move || {
            let mut w = Writer {
                conn,
                _hub: hub,
                wal,
                commits,
                queued,
            };
            w.run(rx);
        });
    if let Err(e) = spawned {
        log::error!("db: could not start the writer thread: {e}");
    }
}

struct Writer {
    conn: Connection,
    /// For announcing changes to browsers once rows are written.
    _hub: Hub,
    wal: Option<PathBuf>,
    commits: Arc<AtomicU64>,
    queued: Arc<AtomicUsize>,
}

impl Writer {
    fn run(&mut self, rx: Receiver<Write>) {
        while let Ok(first) = rx.recv() {
            let started = Instant::now();
            let mut flushes = Vec::new();
            let mut checkpoint = false;
            match self.conn.transaction() {
                Ok(tx) => {
                    apply(&tx, first, &mut flushes, &mut checkpoint, &self.queued);
                    while let Ok(next) = rx.try_recv() {
                        apply(&tx, next, &mut flushes, &mut checkpoint, &self.queued);
                    }
                    match tx.commit() {
                        Ok(()) => {
                            self.commits.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => log::warn!("db: commit failed: {e}"),
                    }
                }
                Err(e) => {
                    log::warn!("db: could not begin a transaction: {e}");
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
            if checkpoint {
                self.checkpoint("truncate");
            } else if self.wal_size() > WAL_LIMIT {
                self.checkpoint("passive");
            }
            for done in flushes {
                let _ = done.send(());
            }
            if let Some(rest) = COMMIT_EVERY.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    }

    fn wal_size(&self) -> u64 {
        self.wal
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len())
    }

    fn checkpoint(&self, mode: &str) {
        if let Err(e) = self
            .conn
            .query_row(&format!("pragma wal_checkpoint({mode})"), [], |_| Ok(()))
        {
            log::warn!("db: checkpoint failed: {e}");
        }
    }
}

/// Apply one write inside the batch's transaction. Failures are logged; the
/// batch carries on.
fn apply(
    conn: &Connection,
    w: Write,
    flushes: &mut Vec<Sender<()>>,
    checkpoint: &mut bool,
    queued: &AtomicUsize,
) {
    queued.fetch_sub(1, Ordering::Relaxed);
    match w {
        Write::Call { live, .. } => log::debug!("db: call write (live: {live}) not applied yet"),
        Write::CallFiles { path, .. } => log::debug!("db: files write for {path} not applied yet"),
        Write::Retention { before_ms } => {
            log::debug!("db: retention before {before_ms} not applied yet")
        }
        Write::Meta { key, value } => {
            let r = conn.execute(
                "insert into meta (key, value) values (?1, ?2)
                 on conflict(key) do update set value = excluded.value",
                params![key, value],
            );
            if let Err(e) = r {
                log::warn!("db: meta write {key} failed: {e}");
            }
        }
        Write::Checkpoint => *checkpoint = true,
        Write::Flush(done) => flushes.push(done),
    }
}
