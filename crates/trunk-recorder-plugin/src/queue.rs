//! A queue of calls worked off the event thread — what an uploader needs:
//! retries with backoff, a warning status while calls are waiting, results
//! and [`Metrics`] (queue depth, upload time, how the service is answering)
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
//!
//! A plugin that sends calls to more than one place (a system's settings can
//! name a server of its own) starts it with [`CallQueue::start_by`] and says
//! which place each call goes to: each place is a lane of its own, with its
//! own workers, retries, capacity and endpoint status, so one that's down
//! doesn't hold up the others.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::protocol::{ConcludedCall, Endpoint, EndpointState, Metrics, Outcome, State};
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
    /// Calls worked on at once (in each lane).
    pub threads: usize,
    /// Where to save calls still waiting at shutdown (None: they fail).
    pub save_to: Option<PathBuf>,
    /// Calls waiting beyond this (in a lane) are refused (reported as failed).
    pub capacity: usize,
    /// A name for the status line, e.g. "upload" → "3 uploads waiting to retry".
    pub noun: &'static str,
    /// The service the work goes to ("Broadcastify Calls"), for the
    /// dashboard's endpoint status ("Broadcastify Calls at <lane>" for a
    /// lane). None: the plugin's own name is shown (or the lane's).
    pub endpoint: Option<String>,
}

impl Default for QueueOptions {
    fn default() -> Self {
        QueueOptions { retry_after: [10, 60, 300, 900].map(Duration::from_secs).to_vec(), threads: 2, save_to: None, capacity: 10_000, noun: "call", endpoint: None }
    }
}

impl QueueOptions {
    /// The defaults, saving waiting calls to `queue.jsonl` in `data_dir`.
    pub fn saved_in(data_dir: &Path) -> Self {
        QueueOptions { save_to: Some(data_dir.join("queue.jsonl")), ..Default::default() }
    }

    /// The endpoint status's name for `lane`.
    fn endpoint_name(&self, lane: &str) -> Option<String> {
        match (&self.endpoint, lane) {
            (Some(e), "") => Some(e.clone()),
            (Some(e), l) => Some(format!("{e} at {l}")),
            (None, "") => None,
            (None, l) => Some(l.to_string()),
        }
    }
}

struct Item {
    call: ConcludedCall,
    tries: usize,
    due: Instant,
    last_error: String,
}

/// The calls for one place, and how it's answering.
#[derive(Default)]
struct Lane {
    ready: VecDeque<Item>,
    waiting: Vec<Item>,
    busy: usize,
    last_error: String,
    stats: Tally,
}

#[derive(Default)]
struct Inner {
    lanes: BTreeMap<String, Lane>,
    closing: Option<Instant>,
    /// The waiting count last reported in a status.
    reported: usize,
    /// The last error of any lane, for the status line.
    last_error: String,
    /// Something changed since metrics last went out.
    changed: bool,
    sent: Option<Instant>,
    /// Each lane's state when metrics last went out.
    sent_states: Vec<EndpointState>,
}

/// What a lane has done, for [`Metrics`].
#[derive(Default)]
struct Tally {
    retries: u64,
    bytes: u64,
    latency_ms: Option<f64>,
    last_ok: Option<f64>,
    last_error: Option<f64>,
    /// Failed tries in a row (0 after a success).
    in_a_row: u32,
    tried: bool,
    /// Unix s of the first try (the clock for "silent" before any success).
    first_try: Option<f64>,
}

/// Metrics go out at most this often, and at least this often while something changes.
const METRICS_MIN: Duration = Duration::from_secs(5);
const METRICS_MAX: Duration = Duration::from_secs(30);
/// The service counts as down after this many failed tries in a row, or
/// this long without a success while calls wait.
const DOWN_AFTER: u32 = 3;
const DOWN_SILENT: Duration = Duration::from_secs(300);

fn unix_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

impl Lane {
    fn endpoint_state(&self) -> EndpointState {
        let t = &self.stats;
        if !t.tried {
            return EndpointState::Unknown;
        }
        let since = t.last_ok.or(t.first_try).unwrap_or_else(unix_now);
        let silent = !self.waiting.is_empty() && unix_now() - since > DOWN_SILENT.as_secs_f64();
        if t.in_a_row >= DOWN_AFTER || silent {
            EndpointState::Down
        } else if t.in_a_row > 0 || !self.waiting.is_empty() {
            EndpointState::Degraded
        } else {
            EndpointState::Up
        }
    }
}

impl Inner {
    fn states(&self) -> Vec<EndpointState> {
        self.lanes.values().map(Lane::endpoint_state).collect()
    }

    fn waiting(&self) -> usize {
        self.lanes.values().map(|l| l.waiting.len()).sum()
    }

    /// The lanes' figures together, and each lane's endpoint.
    fn metrics(&self, o: &QueueOptions) -> Metrics {
        let mut m = Metrics { queued: Some(0), retrying: Some(0), in_flight: Some(0), retries: Some(0), bytes_sent: Some(0), ..Default::default() };
        let add = |a: &mut Option<u64>, b: usize| *a = Some(a.unwrap_or(0) + b as u64);
        let later = |a: &mut Option<f64>, b: Option<f64>| {
            if b > *a {
                *a = b;
            }
        };
        for (name, l) in &self.lanes {
            let t = &l.stats;
            add(&mut m.queued, l.ready.len() + l.waiting.len());
            add(&mut m.retrying, l.waiting.len());
            add(&mut m.in_flight, l.busy);
            add(&mut m.retries, t.retries as usize);
            add(&mut m.bytes_sent, t.bytes as usize);
            let latency = t.latency_ms.map(|l| (l * 10.0).round() / 10.0);
            // The slowest place is the one to know about.
            later(&mut m.latency_ms, latency);
            later(&mut m.last_ok, t.last_ok);
            if t.last_error > m.last_error {
                m.last_error = t.last_error;
                m.last_error_text = l.last_error.clone();
            }
            if let Some(n) = o.endpoint_name(name) {
                let state = l.endpoint_state();
                let last_error = if state == EndpointState::Up { String::new() } else { l.last_error.clone() };
                m.endpoints.push(Endpoint { name: n, state, latency_ms: t.latency_ms, last_ok: t.last_ok, last_error });
            }
        }
        m
    }

    /// Send metrics when something changed and it's been a while (or a
    /// service's state changed: at once).
    fn report(&mut self, host: &Host, o: &QueueOptions) {
        let states = self.states();
        let since = self.sent.map_or(Duration::MAX, |t| t.elapsed());
        let due = (self.changed && since >= METRICS_MIN) || since >= METRICS_MAX || states != self.sent_states;
        if due {
            host.metrics(&self.metrics(o));
            self.sent = Some(Instant::now());
            self.sent_states = states;
            self.changed = false;
        }
    }
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

type Work = dyn Fn(&ConcludedCall) -> Attempt + Send + Sync;
type LaneOf = dyn Fn(&ConcludedCall) -> String + Send + Sync;

pub struct CallQueue {
    shared: Arc<Shared>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    host: Host,
    opts: QueueOptions,
    work: Arc<Work>,
    lane_of: Box<LaneOf>,
}

impl CallQueue {
    /// Start `opts.threads` workers running `work` on each call pushed — and
    /// on calls saved by the last shutdown, first.
    pub fn start<F>(host: Host, opts: QueueOptions, work: F) -> CallQueue
    where
        F: Fn(&ConcludedCall) -> Attempt + Send + Sync + 'static,
    {
        Self::start_by(host, opts, |_| String::new(), work)
    }

    /// [`start`](Self::start), with each call put in the lane `lane` names
    /// (the server it goes to, say): each lane has `opts.threads` workers of
    /// its own, and its own retries and endpoint status. Calls saved by the
    /// last shutdown are put in the lane `lane` names for them now.
    pub fn start_by<L, F>(host: Host, opts: QueueOptions, lane: L, work: F) -> CallQueue
    where
        L: Fn(&ConcludedCall) -> String + Send + Sync + 'static,
        F: Fn(&ConcludedCall) -> Attempt + Send + Sync + 'static,
    {
        let shared = Arc::new(Shared { inner: Mutex::new(Inner::default()), wake: Condvar::new() });
        let q = CallQueue { shared, workers: Mutex::new(Vec::new()), host, opts, work: Arc::new(work), lane_of: Box::new(lane) };
        if let Some(path) = &q.opts.save_to {
            let saved = load(path);
            if !saved.is_empty() {
                q.host.info(format!("{} {}(s) left from last time", saved.len(), q.opts.noun));
                let now = Instant::now();
                let mut g = q.shared.inner.lock().unwrap();
                for call in saved {
                    let key = (q.lane_of)(&call);
                    q.lane(&mut g, &key).ready.push_back(Item { call, tries: 0, due: now, last_error: String::new() });
                }
            }
            let _ = std::fs::remove_file(path);
        }
        q
    }

    /// Lane `key`, started (its workers) if it's new.
    fn lane<'a>(&self, g: &'a mut Inner, key: &str) -> &'a mut Lane {
        if !g.lanes.contains_key(key) {
            g.lanes.insert(key.to_string(), Lane::default());
            let mut workers = self.workers.lock().unwrap();
            for i in 0..self.opts.threads.max(1) {
                let (sh, h, w, o, k) = (self.shared.clone(), self.host.clone(), self.work.clone(), self.opts.clone(), key.to_string());
                let name = if key.is_empty() { format!("queue-{i}") } else { format!("queue-{key}-{i}") };
                workers.push(std::thread::Builder::new().name(name).spawn(move || worker(&sh, &k, &h, &*w, &o)).expect("thread"));
            }
        }
        g.lanes.get_mut(key).unwrap()
    }

    pub fn push(&self, call: ConcludedCall) {
        let key = (self.lane_of)(&call);
        let mut g = self.shared.inner.lock().unwrap();
        if g.closing.is_some() {
            drop(g);
            self.host.call_result(&call.path, Outcome::Failed, "recorder stopping", "");
            return;
        }
        let lane = self.lane(&mut g, &key);
        if lane.ready.len() + lane.waiting.len() >= self.opts.capacity {
            drop(g);
            self.host.call_result(&call.path, Outcome::Failed, format!("too many {}s waiting", self.opts.noun), "");
            return;
        }
        lane.ready.push_back(Item { call, tries: 0, due: Instant::now(), last_error: String::new() });
        g.changed = true;
        drop(g);
        self.shared.wake.notify_all();
    }

    /// Calls queued or being worked on.
    pub fn len(&self) -> usize {
        let g = self.shared.inner.lock().unwrap();
        g.lanes.values().map(|l| l.ready.len() + l.waiting.len() + l.busy).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Work what's ready until `grace` is nearly up; save the rest (or fail it).
    /// Call it from [`crate::Plugin::shutdown`]; calls pushed after it are refused.
    pub fn shutdown(&mut self, grace: Duration) {
        // Leave a little of the grace period for saving.
        let deadline = Instant::now() + grace.mul_f64(0.8);
        {
            let mut g = self.shared.inner.lock().unwrap();
            if g.closing.is_some() {
                return;
            }
            g.closing = Some(deadline);
        }
        self.shared.wake.notify_all();
        for w in std::mem::take(&mut *self.workers.lock().unwrap()) {
            let _ = w.join();
        }
        let mut g = self.shared.inner.lock().unwrap();
        let left: Vec<Item> = g.lanes.values_mut().flat_map(|l| l.ready.drain(..).chain(l.waiting.drain(..)).collect::<Vec<_>>()).collect();
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

/// Work lane `key`'s calls.
fn worker(sh: &Shared, key: &str, host: &Host, work: &Work, o: &QueueOptions) {
    let mut g = sh.inner.lock().unwrap();
    loop {
        let now = Instant::now();
        let closing = g.closing;
        let lane = g.lanes.get_mut(key).expect("lane");
        // Retries that are due go to the front: they've waited longest.
        let mut i = 0;
        while i < lane.waiting.len() {
            if lane.waiting[i].due <= now {
                let it = lane.waiting.swap_remove(i);
                lane.ready.push_front(it);
            } else {
                i += 1;
            }
        }
        if let Some(deadline) = closing {
            // Stopping: work what's ready (retries not yet due are saved) until the deadline.
            if now >= deadline || lane.ready.is_empty() {
                return;
            }
        }
        let Some(mut it) = lane.ready.pop_front() else {
            let next = lane.waiting.iter().map(|w| w.due).min();
            g.report(host, o);
            let wait = next.map_or(Duration::from_secs(1), |t| t.saturating_duration_since(now)).min(Duration::from_secs(1));
            g = sh.wake.wait_timeout(g, wait).unwrap().0;
            continue;
        };
        lane.busy += 1;
        drop(g);
        let started = Instant::now();
        let r = work(&it.call);
        let took_ms = started.elapsed().as_secs_f64() * 1000.0;
        g = sh.inner.lock().unwrap();
        g.changed = true;
        let lane = g.lanes.get_mut(key).expect("lane");
        lane.busy -= 1;
        {
            let t = &mut lane.stats;
            t.first_try.get_or_insert_with(unix_now);
            match &r {
                Attempt::Done { .. } => {
                    t.tried = true;
                    t.in_a_row = 0;
                    t.last_ok = Some(unix_now());
                    t.latency_ms = Some(t.latency_ms.map_or(took_ms, |l| l + 0.2 * (took_ms - l)));
                    let f = &it.call.files;
                    t.bytes += f.m4a.as_ref().and_then(|m| std::fs::metadata(m).ok()).or_else(|| std::fs::metadata(&f.wav).ok()).map_or(0, |m| m.len());
                }
                Attempt::Retry(_) | Attempt::Fail(_) => {
                    t.tried = true;
                    t.in_a_row += 1;
                    t.last_error = Some(unix_now());
                    t.retries += matches!(r, Attempt::Retry(_)) as u64;
                }
                Attempt::Skip(_) => {}
            }
        }
        // What went wrong, and where (when there's more than one place).
        let said = |m: &str| if key.is_empty() { m.to_string() } else { format!("{key}: {m}") };
        let mut error = None;
        match r {
            Attempt::Done { url } => host.call_result(&it.call.path, Outcome::Ok, "", url),
            Attempt::Skip(m) => host.call_result(&it.call.path, Outcome::Skipped, m, ""),
            Attempt::Fail(m) => {
                lane.last_error = m.clone();
                error = Some(said(&m));
                host.call_result(&it.call.path, Outcome::Failed, m, "");
            }
            Attempt::Retry(m) => match o.retry_after.get(it.tries) {
                Some(d) => {
                    it.tries += 1;
                    it.due = Instant::now() + *d;
                    it.last_error = m.clone();
                    lane.last_error = m.clone();
                    error = Some(said(&m));
                    lane.waiting.push(it);
                }
                None => host.call_result(&it.call.path, Outcome::Failed, format!("gave up after {} tries: {m}", it.tries + 1), ""),
            },
        }
        if let Some(e) = error {
            g.last_error = e;
        }
        // Say when calls start or stop waiting on retries (not on every change).
        let n = g.waiting();
        if (n == 0) != (g.reported == 0) || n >= g.reported * 2 && n >= 10 {
            g.reported = n;
            if n == 0 {
                host.status(State::Ok, "");
            } else {
                let noun = if n == 1 { o.noun.to_string() } else { format!("{}s", o.noun) };
                host.status(State::Warning, format!("{n} {noun} waiting to retry: {}", g.last_error));
            }
        }
        g.report(host, o);
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
    fn metrics_follow_the_service() {
        let dir = testing::temp_dir("queue");
        let (host, out) = testing::capture();
        let tries = Arc::new(AtomicUsize::new(0));
        let t = tries.clone();
        let opts = QueueOptions { endpoint: Some("Example Calls".into()), ..quick(&dir) };
        let mut q = CallQueue::start(host, opts, move |_| {
            if t.fetch_add(1, Ordering::SeqCst) == 0 {
                Attempt::Retry("503".into())
            } else {
                Attempt::Done { url: String::new() }
            }
        });
        q.push(testing::call(&dir, "sys1", 5));
        let t0 = Instant::now();
        while !q.is_empty() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        q.shutdown(Duration::from_secs(1));
        let m = out.output().metrics();
        // The service went degraded on the retry (reported at once), then up.
        let states: Vec<EndpointState> = m.iter().filter_map(|x| x.endpoints.first().map(|e| e.state)).collect();
        assert!(states.contains(&EndpointState::Degraded), "{states:?}");
        assert_eq!(states.last(), Some(&EndpointState::Up), "{states:?}");
        let last = m.last().unwrap();
        assert_eq!(last.retries, Some(1));
        assert_eq!(last.queued, Some(0));
        assert!(last.latency_ms.is_some() && last.last_ok.is_some());
        assert_eq!(last.endpoints[0].name, "Example Calls");
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

    /// Two places: one that hangs and is down doesn't hold up the other,
    /// and each has its own endpoint status.
    #[test]
    fn a_lane_that_is_down_holds_up_no_other() {
        let dir = testing::temp_dir("queue");
        let (host, out) = testing::capture();
        let opts = QueueOptions { endpoint: Some("Example".into()), retry_after: vec![Duration::from_secs(3600)], ..quick(&dir) };
        let mut q = CallQueue::start_by(
            host,
            opts,
            |c| if c.call.short_name == "down" { "down.example".into() } else { "up.example".into() },
            |c| {
                if c.call.short_name == "down" {
                    std::thread::sleep(Duration::from_millis(300));
                    Attempt::Retry("timed out".into())
                } else {
                    Attempt::Done { url: String::new() }
                }
            },
        );
        q.push(testing::call(&dir, "down", 1));
        std::thread::sleep(Duration::from_millis(20));
        let up = testing::call(&dir, "up", 2);
        q.push(up.clone());
        let t0 = Instant::now();
        while out.output().results().is_empty() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(t0.elapsed() < Duration::from_millis(250), "the up lane waited on the down one");
        assert_eq!(out.output().results(), vec![(up.path, Outcome::Ok, String::new(), String::new())]);
        // The down lane's try ends; it waits to retry, and says where.
        while q.len() > 1 || out.output().status().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(out.output().status(), Some((State::Warning, "1 call waiting to retry: down.example: timed out".into())));
        let m = q.shared.inner.lock().unwrap().metrics(&q.opts);
        let ends: Vec<(&str, EndpointState)> = m.endpoints.iter().map(|e| (e.name.as_str(), e.state)).collect();
        assert_eq!(ends, [("Example at down.example", EndpointState::Degraded), ("Example at up.example", EndpointState::Up)]);
        assert_eq!((m.queued, m.retrying), (Some(1), Some(1)));
        q.shutdown(Duration::from_secs(1));
        // Saved, and put back in its lane next time.
        let (host, out) = testing::capture();
        let mut q = CallQueue::start_by(host, quick(&dir), |c| c.call.short_name.clone(), |_| Attempt::Done { url: String::new() });
        assert_eq!(q.shared.inner.lock().unwrap().lanes.keys().collect::<Vec<_>>(), ["down"]);
        let t0 = Instant::now();
        while !q.is_empty() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        q.shutdown(Duration::from_secs(1));
        assert_eq!(out.output().results()[0].1, Outcome::Ok);
    }
}
