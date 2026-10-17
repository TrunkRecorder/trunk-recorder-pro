//! The performance report: where the recorder's time goes, thread by thread
//! and stage by stage, with what each source runs — enough to tell from
//! someone else's machine which work to spread over more cores.
//!
//! Every 10 s a row: each thread's CPU (from the OS), each core's, the
//! [`trunk_core::prof`] stages of every thread by source, how far behind the
//! drivers the engine runs, and what each source runs (heads, channels by
//! kind). An hour of rows is kept. The report sums them over the last
//! minute, the busiest minute and the hour, adds the machine and the shape
//! of the config (no keys, passwords, plugin settings, serial numbers or
//! file paths), and is written to [`FILE`] in the data folder every minute;
//! the built-in interface serves it at `/perf-report.json` (Platform → Performance report).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use sysinfo::System;
use trunk_core::prof::{self, Stage};
use trunk_core::trunk::SourceLoad;

use crate::runtime::Ctx;

/// A row's length.
const EVERY: Duration = Duration::from_secs(10);
/// Rows kept: an hour.
const KEPT: usize = 360;
/// Rows in a "minute" window.
const MINUTE: usize = 6;
/// The report's file name, in the data folder.
pub const FILE: &str = "perf-report.json";
/// What the report's layout is: bumped when it changes.
const FORMAT: u32 = 1;

static START: OnceLock<Instant> = OnceLock::new();
/// What one reading of the clock costs here, ns (a span takes two).
static CLOCK_NS: OnceLock<f64> = OnceLock::new();

fn clock() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Turn the stage timing on (before any thread that's timed starts).
pub fn enable() {
    START.get_or_init(Instant::now);
    CLOCK_NS.get_or_init(|| {
        let t = Instant::now();
        for _ in 0..100_000 {
            std::hint::black_box(clock());
        }
        t.elapsed().as_nanos() as f64 / 100_000.0
    });
    prof::set_clock(clock);
}

pub fn path() -> std::path::PathBuf {
    crate::paths::data_dir().join(FILE)
}

// ---------------------------------------------------------------- the engine's flow

/// One source's samples into the engine, since the recording started.
#[derive(Clone, Default, Debug)]
struct Flow {
    msgs: u64,
    samples: u64,
    dropped: u64,
    /// Summed over messages: how long each waited between the driver handing it over and the engine taking it, ms.
    lag_sum_ms: f64,
    /// The longest wait since the sampler last read it.
    lag_max_ms: f64,
}

#[derive(Default)]
struct EngineShared {
    /// Changes with each recording (its counts start again).
    run: u64,
    flows: Vec<Flow>,
    load: Vec<SourceLoad>,
    labels: Vec<String>,
    /// Calls handed to the finish thread, and written on the engine thread because its queue was full.
    finish_queued: u64,
    finish_inline: u64,
    recording: bool,
}

static ENGINE: Mutex<EngineShared> = Mutex::new(EngineShared { run: 0, flows: Vec::new(), load: Vec::new(), labels: Vec::new(), finish_queued: 0, finish_inline: 0, recording: false });

/// A recording starts, with these sources: their counts start again.
pub fn recording_started(labels: Vec<String>) {
    let mut e = ENGINE.lock().unwrap();
    e.run += 1;
    e.flows = vec![Flow::default(); labels.len()];
    e.load = Vec::new();
    e.labels = labels;
    e.finish_queued = 0;
    e.finish_inline = 0;
    e.recording = true;
}

pub fn recording_stopped() {
    let mut e = ENGINE.lock().unwrap();
    e.recording = false;
    e.load.clear();
}

/// A source's message taken by its decoding thread: `samples` of them,
/// `dropped` before them, handed over by the driver `at`.
pub fn received(source: usize, samples: u64, dropped: u64, at: Instant) {
    let lag = at.elapsed().as_secs_f64() * 1000.0;
    let mut e = ENGINE.lock().unwrap();
    let Some(f) = e.flows.get_mut(source) else { return };
    f.msgs += 1;
    f.samples += samples;
    f.dropped += dropped;
    f.lag_sum_ms += lag;
    f.lag_max_ms = f.lag_max_ms.max(lag);
}

/// The engine thread's part, once a second: what the sources run, and the finish thread's counts.
pub struct Ticker {
    told: Instant,
}

impl Ticker {
    pub fn new() -> Ticker {
        Ticker { told: Instant::now() }
    }

    pub fn tick(&mut self, load: impl FnOnce() -> Vec<SourceLoad>, finish_queued: u64, finish_inline: u64) {
        if self.told.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.told = Instant::now();
        let load = load();
        let mut e = ENGINE.lock().unwrap();
        e.load = load;
        e.finish_queued = finish_queued;
        e.finish_inline = finish_inline;
    }
}

// ---------------------------------------------------------------- threads' CPU, from the OS

/// A thread's CPU time since it started.
#[derive(Clone, Debug)]
struct ThreadCpu {
    id: u64,
    name: String,
    ns: u64,
}

#[cfg(target_os = "linux")]
fn threads() -> Vec<ThreadCpu> {
    let tick = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else { return Vec::new() };
    let mut out = Vec::new();
    for e in dir.flatten() {
        let Some(id) = e.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
        let Some(ticks) = stat_ticks(&stat) else { continue };
        let name = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default().trim().to_string();
        out.push(ThreadCpu { id, name, ns: ticks * 1_000_000_000 / tick });
    }
    out
}

/// A `/proc/<pid>/task/<tid>/stat` line's CPU time (utime + stime), in clock ticks.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn stat_ticks(stat: &str) -> Option<u64> {
    // After the name (which may hold spaces and parentheses): the state, then utime and stime are the 12th and 13th.
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut f = rest.split_whitespace().skip(11);
    Some(f.next()?.parse::<u64>().ok()? + f.next()?.parse::<u64>().ok()?)
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn threads() -> Vec<ThreadCpu> {
    unsafe extern "C" {
        fn mach_port_deallocate(task: libc::mach_port_t, name: libc::mach_port_t) -> libc::kern_return_t;
    }
    let mut out = Vec::new();
    unsafe {
        let task = libc::mach_task_self();
        let mut list: libc::thread_act_array_t = std::ptr::null_mut();
        let mut n: libc::mach_msg_type_number_t = 0;
        if libc::task_threads(task, &mut list, &mut n) != 0 {
            return out;
        }
        for i in 0..n as usize {
            let th = *list.add(i);
            let mut info: libc::thread_extended_info = std::mem::zeroed();
            let mut count = libc::THREAD_EXTENDED_INFO_COUNT;
            if libc::thread_info(th, libc::THREAD_EXTENDED_INFO as _, &mut info as *mut _ as libc::thread_info_t, &mut count) == 0 {
                let name = std::ffi::CStr::from_ptr(info.pth_name.as_ptr()).to_string_lossy().into_owned();
                out.push(ThreadCpu { id: th as u64, name, ns: info.pth_user_time + info.pth_system_time });
            }
            mach_port_deallocate(task, th);
        }
        libc::vm_deallocate(task, list as libc::vm_address_t, n as libc::vm_size_t * std::mem::size_of::<libc::thread_act_t>() as libc::vm_size_t);
    }
    out
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn threads() -> Vec<ThreadCpu> {
    Vec::new()
}

// ---------------------------------------------------------------- rows

/// One (thread, stage, source)'s work in a row.
#[derive(Clone, Debug)]
struct StageRow {
    thread: String,
    stage: Stage,
    source: Option<u16>,
    ns: u64,
    count: u64,
    max_ns: u64,
}

#[derive(Clone, Debug, Default)]
struct FlowRow {
    msgs: u64,
    samples: u64,
    dropped: u64,
    lag_sum_ms: f64,
    lag_max_ms: f64,
}

/// [`EVERY`] of the recorder.
#[derive(Clone, Debug)]
struct Row {
    /// When it ended, Unix s.
    t: f64,
    dt: f64,
    /// (thread, id, CPU % of one core).
    threads: Vec<(String, u64, f64)>,
    cores: Vec<f32>,
    stages: Vec<StageRow>,
    flows: Vec<FlowRow>,
    load: Vec<SourceLoad>,
    labels: Vec<String>,
    finish_queued: u64,
    finish_inline: u64,
    recording: bool,
}

struct State {
    rows: VecDeque<Row>,
    started: f64,
    /// The last readings, to take the next row's from.
    last_at: Instant,
    last_threads: BTreeMap<u64, u64>,
    /// By (thread id, stage, source).
    last_prof: BTreeMap<(usize, Stage, Option<u16>), (u64, u64)>,
    last_flow: (u64, Vec<Flow>, u64, u64),
    sys: System,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

impl State {
    fn new() -> State {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        let mut s = State { rows: VecDeque::new(), started: unix_now(), last_at: Instant::now(), last_threads: BTreeMap::new(), last_prof: BTreeMap::new(), last_flow: (0, Vec::new(), 0, 0), sys };
        // (The first row counts from here, not from each thread's start.)
        s.last_threads = threads().into_iter().map(|t| (t.id, t.ns)).collect();
        for t in prof::take() {
            for (stage, src, x) in t.totals.iter() {
                s.last_prof.insert((t.id, stage, src), (x.ns, x.count));
            }
        }
        s
    }

    fn sample(&mut self) {
        let dt = self.last_at.elapsed().as_secs_f64().max(1e-3);
        self.last_at = Instant::now();
        let mut threads_row = Vec::new();
        let mut seen = BTreeMap::new();
        for t in threads() {
            let was = self.last_threads.get(&t.id).copied().unwrap_or(0);
            let pct = t.ns.saturating_sub(was) as f64 / 1e9 / dt * 100.0;
            seen.insert(t.id, t.ns);
            threads_row.push((t.name, t.id, pct));
        }
        self.last_threads = seen;
        self.sys.refresh_cpu_usage();
        let cores = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect();
        let mut stages = Vec::new();
        for t in prof::take() {
            for (stage, source, x) in t.totals.iter() {
                let key = (t.id, stage, source);
                let (ns0, n0) = self.last_prof.get(&key).copied().unwrap_or((0, 0));
                let (ns, count) = (x.ns.saturating_sub(ns0), x.count.saturating_sub(n0));
                self.last_prof.insert(key, (x.ns, x.count));
                if ns > 0 || count > 0 {
                    stages.push(StageRow { thread: t.thread.clone(), stage, source, ns, count, max_ns: x.max_ns });
                }
            }
        }
        let (flows, load, labels, fq, fi, recording) = {
            let mut e = ENGINE.lock().unwrap();
            if e.run != self.last_flow.0 {
                self.last_flow = (e.run, vec![Flow::default(); e.flows.len()], 0, 0);
            }
            let flows: Vec<FlowRow> = e
                .flows
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let w = self.last_flow.1.get(i).cloned().unwrap_or_default();
                    FlowRow {
                        msgs: f.msgs.saturating_sub(w.msgs),
                        samples: f.samples.saturating_sub(w.samples),
                        dropped: f.dropped.saturating_sub(w.dropped),
                        lag_sum_ms: (f.lag_sum_ms - w.lag_sum_ms).max(0.0),
                        lag_max_ms: f.lag_max_ms,
                    }
                })
                .collect();
            let (fq, fi) = (e.finish_queued.saturating_sub(self.last_flow.2), e.finish_inline.saturating_sub(self.last_flow.3));
            self.last_flow = (e.run, e.flows.clone(), e.finish_queued, e.finish_inline);
            for f in e.flows.iter_mut() {
                f.lag_max_ms = 0.0;
            }
            (flows, e.load.clone(), e.labels.clone(), fq, fi, e.recording)
        };
        self.rows.push_back(Row { t: unix_now(), dt, threads: threads_row, cores, stages, flows, load, labels, finish_queued: fq, finish_inline: fi, recording });
        while self.rows.len() > KEPT {
            self.rows.pop_front();
        }
    }
}

/// Start the sampler: a row every 10 s, the report written every minute.
pub fn start(ctx: Arc<Ctx>) {
    if !prof::enabled() {
        return;
    }
    log::info!("Performance report: {} (or Platform → Performance report)", path().display());
    let _ = std::thread::Builder::new().name("perf".into()).spawn(move || {
        *STATE.lock().unwrap() = Some(State::new());
        let mut n = 0u32;
        loop {
            std::thread::sleep(EVERY);
            if let Some(s) = STATE.lock().unwrap().as_mut() {
                s.sample();
            }
            n += 1;
            if n % MINUTE as u32 == 0 {
                let r = report(&ctx);
                let _ = std::fs::create_dir_all(crate::paths::data_dir());
                if let Err(e) = trunk_app::config::write_atomic(&path(), serde_json::to_string_pretty(&r).unwrap_or_default()) {
                    log::warn!("Couldn't write {}: {e}", path().display());
                }
            }
        }
    });
}

// ---------------------------------------------------------------- the report

// (+ 0.0: an empty sum is −0.0.)
fn r1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0 + 0.0
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0 + 0.0
}

/// Rows summed into one window.
fn window(rows: &[&Row]) -> Value {
    if rows.is_empty() {
        return Value::Null;
    }
    let secs: f64 = rows.iter().map(|r| r.dt).sum();
    let wall_ns = secs * 1e9;
    let pct = |ns: f64| r2(ns / wall_ns * 100.0);
    // Threads, by OS id (the same name can be several threads: tokio's workers).
    let mut threads: BTreeMap<u64, (String, f64)> = BTreeMap::new();
    for r in rows {
        for (name, id, p) in &r.threads {
            let e = threads.entry(*id).or_insert_with(|| (name.clone(), 0.0));
            e.1 += p * r.dt;
        }
    }
    let mut threads: Vec<(String, u64, f64)> = threads.into_iter().map(|(id, (n, s))| (n, id, s / secs)).filter(|t| t.2 >= 0.05).collect();
    threads.sort_by(|a, b| b.2.total_cmp(&a.2));
    let process: f64 = threads.iter().map(|t| t.2).sum();
    let ncores = rows.iter().map(|r| r.cores.len()).max().unwrap_or(0);
    let cores: Vec<f64> = (0..ncores).map(|i| r1(rows.iter().map(|r| r.cores.get(i).copied().unwrap_or(0.0) as f64 * r.dt).sum::<f64>() / secs)).collect();
    // Stages: by (thread, stage) over sources, and by source.
    #[derive(Default)]
    struct Acc {
        ns: u64,
        count: u64,
        max_ns: u64,
    }
    let mut by_stage: BTreeMap<(String, &'static str), Acc> = BTreeMap::new();
    let mut by_source: BTreeMap<u16, BTreeMap<&'static str, u64>> = BTreeMap::new();
    let mut busy: BTreeMap<String, u64> = BTreeMap::new();
    for r in rows {
        for s in &r.stages {
            let a = by_stage.entry((s.thread.clone(), s.stage.name())).or_default();
            a.ns += s.ns;
            a.count += s.count;
            a.max_ns = a.max_ns.max(s.max_ns);
            *busy.entry(s.thread.clone()).or_default() += s.ns;
            if let Some(src) = s.source {
                *by_source.entry(src).or_default().entry(s.stage.name()).or_default() += s.ns;
            }
        }
    }
    let mut stages: Vec<(&(String, &str), &Acc)> = by_stage.iter().collect();
    stages.sort_by(|a, b| b.1.ns.cmp(&a.1.ns));
    let stages: Vec<Value> = stages
        .into_iter()
        .map(|((thread, stage), a)| {
            json!({
                "thread": thread,
                "stage": stage,
                "pctOfCore": pct(a.ns as f64),
                "perSecond": r1(a.count as f64 / secs),
                "avgUs": r2(if a.count > 0 { a.ns as f64 / a.count as f64 / 1e3 } else { 0.0 }),
                "maxUs": r1(a.max_ns as f64 / 1e3),
            })
        })
        .collect();
    let busy: Map<String, Value> = busy.into_iter().map(|(t, ns)| (t, json!(pct(ns as f64)))).collect();
    let spans: u64 = by_stage.values().map(|a| a.count).sum();
    let clock_ns = CLOCK_NS.get().copied().unwrap_or(0.0);
    // Per source: its flow, what it ran (averaged over the rows), and its stages.
    let last = rows.last().unwrap();
    let nsrc = rows.iter().map(|r| r.flows.len().max(r.load.len())).max().unwrap_or(0);
    let mut sources = Vec::new();
    for i in 0..nsrc {
        let mut f = FlowRow::default();
        let mut lag_max: f64 = 0.0;
        for r in rows {
            if let Some(x) = r.flows.get(i) {
                f.msgs += x.msgs;
                f.samples += x.samples;
                f.dropped += x.dropped;
                f.lag_sum_ms += x.lag_sum_ms;
                lag_max = lag_max.max(x.lag_max_ms);
            }
        }
        let loads: Vec<&SourceLoad> = rows.iter().filter_map(|r| r.load.get(i)).collect();
        let avg = |g: &dyn Fn(&SourceLoad) -> usize| if loads.is_empty() { 0.0 } else { r1(loads.iter().map(|l| g(l) as f64).sum::<f64>() / loads.len() as f64) };
        let maxed = |g: &dyn Fn(&SourceLoad) -> usize| loads.iter().map(|l| g(l)).max().unwrap_or(0);
        let mut kinds: Vec<&'static str> = loads.iter().flat_map(|l| l.voice.keys().copied()).collect();
        kinds.sort();
        kinds.dedup();
        let voice: Map<String, Value> = kinds.iter().map(|k| (k.to_string(), json!({ "avg": avg(&|l| l.voice.get(k).copied().unwrap_or(0)), "max": maxed(&|l| l.voice.get(k).copied().unwrap_or(0)) }))).collect();
        let st = by_source.get(&(i as u16)).cloned().unwrap_or_default();
        let total: u64 = st.values().sum();
        let mut st: Vec<(&str, u64)> = st.into_iter().collect();
        st.sort_by(|a, b| b.1.cmp(&a.1));
        let l0 = loads.last();
        sources.push(json!({
            "source": i,
            "label": last.labels.get(i),
            "rateHz": l0.map(|l| l.rate_hz),
            "fftSize": l0.map(|l| l.fft_size),
            "decimation": l0.map(|l| l.decim),
            "heads": { "avg": avg(&|l| l.heads), "max": maxed(&|l| l.heads) },
            "controlCarriers": l0.map(|l| l.control),
            "voiceChannels": voice,
            "conventional": { "watched": l0.map(|l| l.conventional), "openAvg": avg(&|l| l.conventional_open), "openMax": maxed(&|l| l.conventional_open) },
            "pctOfCore": pct(total as f64),
            "stagesPctOfCore": st.into_iter().map(|(k, ns)| (k.to_string(), json!(pct(ns as f64)))).collect::<Map<String, Value>>(),
            "messagesPerSecond": r1(f.msgs as f64 / secs),
            "samplesPerSecond": (f.samples as f64 / secs).round(),
            "droppedSamples": f.dropped,
            "queueWaitAvgMs": r2(if f.msgs > 0 { f.lag_sum_ms / f.msgs as f64 } else { 0.0 }),
            "queueWaitMaxMs": r1(lag_max),
        }));
    }
    json!({
        "fromUnix": (rows[0].t - rows[0].dt).round(),
        "seconds": secs.round(),
        "recording": rows.iter().all(|r| r.recording),
        "processPctOfCore": r1(process),
        "threads": threads.iter().map(|(n, id, p)| json!({ "name": n, "id": id, "pctOfCore": r1(*p) })).collect::<Vec<_>>(),
        "timedBusyPctOfCore": busy,
        "coresPct": cores,
        "stages": stages,
        "sources": sources,
        "timing": { "spansPerSecond": r1(spans as f64 / secs), "clockNs": r1(clock_ns), "overheadPctOfCore": r2(spans as f64 * 2.0 * clock_ns / wall_ns * 100.0) },
        "finish": { "calls": rows.iter().map(|r| r.finish_queued).sum::<u64>(), "writtenOnEngineThread": rows.iter().map(|r| r.finish_inline).sum::<u64>() },
    })
}

/// The busiest decoding thread in a row (a source's `dsp-N`, or the
/// engine's): its name and CPU, % of one core. (Not one-off work such as the
/// history scan at start, which would pick the first minute every time.)
fn busiest(r: &Row) -> (&str, f64) {
    r.threads.iter().filter(|t| t.0.starts_with("dsp-") || t.0 == "engine").map(|t| (t.0.as_str(), t.2)).fold(("", 0.0), |b, t| if t.1 > b.1 { t } else { b })
}

/// A compact line per row.
fn timeline(rows: &[&Row]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            let stages: Map<String, Value> = {
                let mut by: BTreeMap<&str, u64> = BTreeMap::new();
                for s in &r.stages {
                    *by.entry(s.stage.name()).or_default() += s.ns;
                }
                by.into_iter().map(|(k, ns)| (k.to_string(), json!(r1(ns as f64 / (r.dt * 1e9) * 100.0)))).collect()
            };
            let (top, top_pct) = busiest(r);
            let total = |g: &dyn Fn(&SourceLoad) -> usize| r.load.iter().map(g).sum::<usize>();
            json!({
                "t": r.t.round(),
                "recording": r.recording,
                "processPct": r1(r.threads.iter().map(|t| t.2).sum::<f64>()),
                "busiestThread": top,
                "busiestThreadPct": r1(top_pct),
                "maxCorePct": r1(r.cores.iter().copied().fold(0.0f32, f32::max) as f64),
                "stagesPct": stages,
                "queueWaitMaxMs": r1(r.flows.iter().map(|f| f.lag_max_ms).fold(0.0, f64::max)),
                "droppedSamples": r.flows.iter().map(|f| f.dropped).sum::<u64>(),
                "heads": total(&|l| l.heads),
                "voiceChannels": total(&|l| l.voice.values().sum()),
                "conventionalOpen": total(&|l| l.conventional_open),
            })
        })
        .collect()
}

/// The config's shape: what decides the work, nothing secret.
fn config_shape(ctx: &Ctx) -> Value {
    let c = ctx.config.lock().unwrap().clone();
    let sources: Vec<Value> = c
        .sources
        .iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                for k in ["serial", "args", "path", "settings"] {
                    o.remove(k);
                }
            }
            v
        })
        .collect();
    let systems: Vec<Value> = c
        .systems
        .iter()
        .map(|s| {
            json!({
                "shortName": s.short_name,
                "kind": s.kind,
                "enabled": s.enabled,
                "sites": s.sites.iter().map(|x| json!({ "modulation": x.modulation, "controlChannels": x.control_channels_hz.len(), "voiceChannels": x.voice_channels_hz.len() })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let conventional: Vec<Value> = c
        .conventional
        .iter()
        .map(|s| {
            let mut modes: BTreeMap<String, usize> = BTreeMap::new();
            for ch in &s.channels {
                *modes.entry(serde_json::to_value(ch.mode).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| format!("{:?}", ch.mode))).or_default() += 1;
            }
            json!({ "shortName": s.short_name, "enabled": s.enabled, "channels": s.channels.len(), "modes": modes })
        })
        .collect();
    let r = &c.recording;
    json!({
        "sources": sources,
        "systems": systems,
        "conventional": conventional,
        "recording": {
            "maxRecorders": r.max_recorders,
            "prerollS": r.preroll_s,
            "vocoder": r.vocoder,
            "captureFrames": r.capture_frames,
            "normalizeAudio": r.normalize_audio,
            "compressWav": r.compress_wav,
            "m4aEncoder": r.m4a.encoder,
            "ramSpool": serde_json::to_value(&r.ram_spool).unwrap_or(Value::Null),
        },
        "plugins": c.plugins.iter().filter(|(_, p)| p.enabled).map(|(n, _)| n.clone()).collect::<Vec<_>>(),
    })
}

fn machine(ctx: &Ctx) -> Value {
    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let cpus = sys.cpus();
    let la = System::load_average();
    let plat = ctx.host_last.lock().unwrap().as_ref().map(|h| h["platform"].clone()).unwrap_or(Value::Null);
    json!({
        "os": System::long_os_version(),
        "kernel": System::kernel_version(),
        "arch": System::cpu_arch(),
        "cpu": cpus.first().map(|c| c.brand().trim().to_string()),
        "logicalCores": cpus.len(),
        "physicalCores": System::physical_core_count(),
        "mhz": cpus.first().map(|c| c.frequency()),
        "memoryBytes": sys.total_memory(),
        "loadAverage": [r2(la.one), r2(la.five), r2(la.fifteen)],
        "container": plat.get("container"),
        "cpuLimitCores": plat.get("cpuLimitCores"),
        "uptimeS": plat.get("uptimeS"),
    })
}

/// Everything since the process started, from the stage timers.
fn since_start() -> Value {
    let mut out = Vec::new();
    for t in prof::snapshot() {
        for (stage, src, x) in t.totals.iter() {
            out.push(json!({ "thread": t.thread, "stage": stage.name(), "source": src, "ms": r1(x.ns as f64 / 1e6), "count": x.count }));
        }
    }
    json!(out)
}

const ABOUT: &str = "Trunk Recorder Pro performance report. pctOfCore: 100 = one CPU core busy the whole window. \
threads: CPU per thread from the OS. stages: where the timed threads spend it, in wall time (exclusive: a stage inside another isn't counted twice; \
timedBusyPctOfCore adds them up per thread; above the thread's OS CPU means it was descheduled mid-stage; timing estimates its own cost). chz.* is the channelizer (fft: one forward FFT per wideband block; head: one per channel per block), \
voice.* the trunked voice receivers, vocoder.* the IMBE/AMBE synthesis, control.* the control channels, conv.* conventional channels, \
trunk.calls the call logic, session.*/deliver/housekeeping the engine loop's own work. Each source decodes on its own thread (dsp-N); \
the engine thread polls and delivers. queueWait: how long samples waited between the driver and their decoding thread (rising = it can't \
keep up). Rows are 10 s; windows: the last minute, the busiest minute (by the busiest decoding thread's CPU) and the last hour. Holds no keys, passwords, plugin settings, serial numbers or file paths.";

/// The report as it stands now.
pub fn report(ctx: &Ctx) -> Value {
    let (rows, started): (Vec<Row>, f64) = {
        let g = STATE.lock().unwrap();
        match g.as_ref() {
            Some(s) => (s.rows.iter().cloned().collect(), s.started),
            None => (Vec::new(), unix_now()),
        }
    };
    let all: Vec<&Row> = rows.iter().collect();
    let last: Vec<&Row> = all.iter().rev().take(MINUTE).rev().copied().collect();
    let busiest: Vec<&Row> = if all.len() <= MINUTE {
        all.clone()
    } else {
        let score = |w: &[&Row]| w.iter().map(|r| busiest(r).1 + 1e-3 * r.threads.iter().map(|t| t.2).sum::<f64>()).sum::<f64>();
        let best = (0..=all.len() - MINUTE).max_by(|&a, &b| score(&all[a..a + MINUTE]).total_cmp(&score(&all[b..b + MINUTE]))).unwrap_or(0);
        all[best..best + MINUTE].to_vec()
    };
    let phase = ctx.phase.lock().unwrap().phase;
    let topics: Vec<String> = ctx.topics.lock().unwrap().keys().cloned().collect();
    json!({
        "about": ABOUT,
        "format": FORMAT,
        "generatedUnix": unix_now().round(),
        "generated": chrono::Local::now().to_rfc3339(),
        "samplingSinceUnix": started.round(),
        "version": env!("CARGO_PKG_VERSION"),
        "build": { "profile": if cfg!(debug_assertions) { "debug" } else { "release" }, "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "machine": machine(ctx),
        "state": { "phase": phase, "browsers": ctx.hub.receiver_count(), "topicsWatched": topics },
        "config": config_shape(ctx),
        "windows": { "lastMinute": window(&last), "busiestMinute": window(&busiest), "lastHour": window(&all) },
        "timeline": timeline(&all),
        "sinceStart": since_start(),
    })
}

/// A file name for the downloaded report.
pub fn download_name() -> String {
    format!("trunk-pro-perf-{}.json", chrono::Local::now().format("%Y%m%d-%H%M"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_cpu_time() {
        // (A Raspberry Pi's engine thread; a name with spaces and a parenthesis.)
        let line = "2817 (engine) R 2790 2790 2790 0 -1 4194368 1301 0 0 0 51234 812 0 0 20 0 18 0 4410 2104131584 25813 18446744073709551615";
        assert_eq!(stat_ticks(line), Some(51234 + 812));
        assert_eq!(stat_ticks("9 (a b) c)) S 1 1 1 0 -1 0 0 0 0 0 7 3 0 0"), Some(10));
        assert_eq!(stat_ticks("9 (x) S 1 2"), None);
    }

    #[test]
    fn this_platform_lists_its_threads() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let t = std::thread::Builder::new().name("perf-probe".into()).spawn(|| std::thread::sleep(Duration::from_millis(300))).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            assert!(threads().iter().any(|t| t.name == "perf-probe"));
            t.join().unwrap();
        }
    }
}
