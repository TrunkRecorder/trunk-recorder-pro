//! A queue of calls worked off the event thread — what an uploader needs:
//! retries with backoff, a warning status while calls are waiting, results
//! reported for you, and calls still waiting at shutdown saved to the data
//! folder and picked up at the next start.
//!
//! ```ignore
//! let queue = CallQueue::start(host.clone(), QueueOptions::saved_in(&setup.data_dir), move |call| {
//!     match upload(call) {
//!         Ok(url) => Attempt::Done { url },
//!         Err(e) if e.is_temporary() => Attempt::Retry(e.to_string()),
//!         Err(e) => Attempt::Fail(e.to_string()),
//!     }
//! });
//! // in call_concluded:   queue.push(call);
//! // in shutdown:         self.queue.shutdown(grace);
//! ```

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::protocol::{ConcludedCall, Outcome, State};
use crate::sdk::Host;

/// What became of one try at a call.
#[derive(Clone, Debug)]
pub enum Attempt {
    /// Done; `url`: where it can be found now (or empty).
    Done { url: String },
    /// Not for this plugin (reported as skipped).
    Skip(String),
    /// Failed for now (the service is down, the network's out): try again later.
    Retry(String),
    /// Failed for good (it was refused): report it and move on.
    Fail(String),
}

#[derive(Clone, Debug)]
pub struct QueueOptions {
    /// How long to wait before each retry; after the last, the call fails.
    pub retry_after: Vec<Duration>,
    /// Calls worked on at once.
    pub threads: usize,
    /// Where to save calls still waiting at shutdown (None: they fail).
    pub save_to: Option<PathBuf>,
    /// Calls waiting beyond this are refused (reported as failed).
    pub capacity: usize,
    /// A name for the status line, e.g. "upload" → "3 uploads waiting to retry".
    pub noun: &'static str,
}

impl Default for QueueOptions {
    fn default() -> Self {
        QueueOptions { retry_after: [10, 60, 300, 900].map(Duration::from_secs).to_vec(), threads: 2, save_to: None, capacity: 10_000, noun: "call" }
    }
}

impl QueueOptions {
    /// The defaults, saving waiting calls to `queue.jsonl` in `data_dir`.
    pub fn saved_in(data_dir: &Path) -> Self {
        QueueOptions { save_to: Some(data_dir.join("queue.jsonl")), ..Default::default() }
    }
}

struct Item {
    call: ConcludedCall,
    tries: usize,
    due: Instant,
    last_error: String,
}

#[derive(Default)]
struct Inner {
    ready: VecDeque<Item>,
    waiting: Vec<Item>,
    busy: usize,
    closing: Option<Instant>,
    /// The waiting count last reported in a status.
    reported: usize,
    last_error: String,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

pub struct CallQueue {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    host: Host,
    opts: QueueOptions,
}

impl CallQueue {
    /// Start `opts.threads` workers running `work` on each call pushed — and
    /// on calls saved by the last shutdown, first.
    pub fn start<F>(host: Host, opts: QueueOptions, work: F) -> CallQueue
    where
        F: Fn(&ConcludedCall) -> Attempt + Send + Sync + 'static,
    {
        let shared = Arc::new(Shared { inner: Mutex::new(Inner::default()), wake: Condvar::new() });
        if let Some(path) = &opts.save_to {
            let saved = load(path);
            if !saved.is_empty() {
                host.info(format!("{} {}(s) left from last time", saved.len(), opts.noun));
                let now = Instant::now();
                shared.inner.lock().unwrap().ready.extend(saved.into_iter().map(|call| Item { call, tries: 0, due: now, last_error: String::new() }));
            }
            let _ = std::fs::remove_file(path);
        }
        let work = Arc::new(work);
        let workers = (0..opts.threads.max(1))
            .map(|i| {
                let (sh, h, w, o) = (shared.clone(), host.clone(), work.clone(), opts.clone());
                std::thread::Builder::new().name(format!("queue-{i}")).spawn(move || worker(&sh, &h, &*w, &o)).expect("thread")
            })
            .collect();
        CallQueue { shared, workers, host, opts }
    }

    pub fn push(&self, call: ConcludedCall) {
        let mut g = self.shared.inner.lock().unwrap();
        if g.closing.is_some() {
            drop(g);
            self.host.call_result(&call.path, Outcome::Failed, "recorder stopping", "");
            return;
        }
        if g.ready.len() + g.waiting.len() >= self.opts.capacity {
            drop(g);
            self.host.call_result(&call.path, Outcome::Failed, format!("too many {}s waiting", self.opts.noun), "");
            return;
        }
        g.ready.push_back(Item { call, tries: 0, due: Instant::now(), last_error: String::new() });
        drop(g);
        self.shared.wake.notify_one();
    }

    /// Calls queued or being worked on.
    pub fn len(&self) -> usize {
        let g = self.shared.inner.lock().unwrap();
        g.ready.len() + g.waiting.len() + g.busy
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Work what's ready until `grace` is nearly up; save the rest (or fail it).
    /// Call it from [`crate::Plugin::shutdown`]; calls pushed after it are refused.
    pub fn shutdown(&mut self, grace: Duration) {
        if self.workers.is_empty() {
            return;
        }
        // Leave a little of the grace period for saving.
        let deadline = Instant::now() + grace.mul_f64(0.8);
        self.shared.inner.lock().unwrap().closing = Some(deadline);
        self.shared.wake.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
        let mut g = self.shared.inner.lock().unwrap();
        let g = &mut *g;
        let left: Vec<Item> = g.ready.drain(..).chain(g.waiting.drain(..)).collect();
        if left.is_empty() {
            return;
        }
        match &self.opts.save_to {
            Some(path) => match save(path, &left) {
                Ok(()) => self.host.info(format!("{} {}(s) saved for next time", left.len(), self.opts.noun)),
                Err(e) => {
                    self.host.error(format!("couldn't save {} waiting {}(s): {e}", left.len(), self.opts.noun));
                    left.iter().for_each(|i| self.host.call_result(&i.call.path, Outcome::Failed, "recorder stopped", ""));
                }
            },
            None => left.iter().for_each(|i| self.host.call_result(&i.call.path, Outcome::Failed, "recorder stopped", "")),
        }
    }
}

fn worker(sh: &Shared, host: &Host, work: &(dyn Fn(&ConcludedCall) -> Attempt + Send + Sync), o: &QueueOptions) {
    let mut g = sh.inner.lock().unwrap();
    loop {
        let now = Instant::now();
        // Retries that are due go to the front: they've waited longest.
        let mut i = 0;
        while i < g.waiting.len() {
            if g.waiting[i].due <= now {
                let it = g.waiting.swap_remove(i);
                g.ready.push_front(it);
            } else {
                i += 1;
            }
        }
        if let Some(deadline) = g.closing {
            // Stopping: work what's ready (retries not yet due are saved) until the deadline.
            if now >= deadline || g.ready.is_empty() {
                return;
            }
        }
        let Some(mut it) = g.ready.pop_front() else {
            let next = g.waiting.iter().map(|w| w.due).min();
            let wait = next.map_or(Duration::from_secs(1), |t| t.saturating_duration_since(now)).min(Duration::from_secs(1));
            g = sh.wake.wait_timeout(g, wait).unwrap().0;
            continue;
        };
        g.busy += 1;
        drop(g);
        let r = work(&it.call);
        g = sh.inner.lock().unwrap();
        g.busy -= 1;
        match r {
            Attempt::Done { url } => host.call_result(&it.call.path, Outcome::Ok, "", url),
            Attempt::Skip(m) => host.call_result(&it.call.path, Outcome::Skipped, m, ""),
            Attempt::Fail(m) => host.call_result(&it.call.path, Outcome::Failed, m, ""),
            Attempt::Retry(m) => match o.retry_after.get(it.tries) {
                Some(d) => {
                    it.tries += 1;
                    it.due = Instant::now() + *d;
                    it.last_error = m.clone();
                    g.last_error = m;
                    g.waiting.push(it);
                }
                None => host.call_result(&it.call.path, Outcome::Failed, format!("gave up after {} tries: {m}", it.tries + 1), ""),
            },
        }
        // Say when calls start or stop waiting on retries (not on every change).
        let n = g.waiting.len();
        if (n == 0) != (g.reported == 0) || n >= g.reported * 2 && n >= 10 {
            g.reported = n;
            if n == 0 {
                host.status(State::Ok, "");
            } else {
                let noun = if n == 1 { o.noun.to_string() } else { format!("{}s", o.noun) };
                host.status(State::Warning, format!("{n} {noun} waiting to retry: {}", g.last_error));
            }
        }
    }
}

fn load(path: &Path) -> Vec<ConcludedCall> {
    let Ok(f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<ConcludedCall>(&l).ok())
        // (The user may have deleted the call meanwhile.)
        .filter(|c| c.files.wav.exists() || c.files.m4a.as_ref().is_some_and(|m| m.exists()))
        .collect()
}

fn save(path: &Path, items: &[Item]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for it in items {
        writeln!(f, "{}", serde_json::to_string(&it.call).unwrap_or_default())?;
    }
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn quick(dir: &Path) -> QueueOptions {
        QueueOptions { retry_after: vec![Duration::from_millis(20), Duration::from_millis(20)], threads: 1, ..QueueOptions::saved_in(dir) }
    }

    #[test]
    fn retries_then_succeeds_and_reports() {
        let dir = testing::temp_dir("queue");
        let (host, out) = testing::capture();
        let tries = Arc::new(AtomicUsize::new(0));
        let t = tries.clone();
        let mut q = CallQueue::start(host, quick(&dir), move |_| {
            if t.fetch_add(1, Ordering::SeqCst) == 0 {
                Attempt::Retry("down".into())
            } else {
                Attempt::Done { url: "u".into() }
            }
        });
        let call = testing::call(&dir, "sys1", 5);
        q.push(call.clone());
        let t0 = Instant::now();
        while !q.is_empty() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        q.shutdown(Duration::from_secs(1));
        let out = out.output();
        assert_eq!(tries.load(Ordering::SeqCst), 2);
        assert_eq!(out.results(), vec![(call.path, Outcome::Ok, String::new(), "u".into())]);
        assert!(matches!(out.status(), Some((State::Ok, _))));
    }

    #[test]
    fn gives_up_after_the_last_retry() {
        let dir = testing::temp_dir("queue");
        let (host, out) = testing::capture();
        let mut q = CallQueue::start(host, quick(&dir), |_| Attempt::Retry("down".into()));
        q.push(testing::call(&dir, "sys1", 5));
        let t0 = Instant::now();
        while !q.is_empty() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        q.shutdown(Duration::from_secs(1));
        let r = out.output().results();
        assert_eq!(r[0].1, Outcome::Failed);
        assert!(r[0].2.contains("3 tries"), "{}", r[0].2);
    }

    #[test]
    fn saves_what_waits_at_shutdown_and_resumes() {
        let dir = testing::temp_dir("queue");
        let (host, _) = testing::capture();
        let opts = QueueOptions { retry_after: vec![Duration::from_secs(3600)], threads: 1, ..QueueOptions::saved_in(&dir) };
        let mut q = CallQueue::start(host, opts.clone(), |_| Attempt::Retry("down".into()));
        q.push(testing::call(&dir, "sys1", 5));
        std::thread::sleep(Duration::from_millis(50));
        q.shutdown(Duration::from_secs(1));
        assert!(dir.join("queue.jsonl").exists());
        // Next start: it's worked first.
        let (host, out) = testing::capture();
        let mut q = CallQueue::start(host, opts, |_| Attempt::Done { url: String::new() });
        let t0 = Instant::now();
        while !q.is_empty() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        q.shutdown(Duration::from_secs(1));
        assert_eq!(out.output().results()[0].1, Outcome::Ok);
        assert!(!dir.join("queue.jsonl").exists());
    }
}
