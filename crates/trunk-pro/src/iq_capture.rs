//! Recording what the radios receive to SigMF files, while recording goes on,
//! and reading those files back for `replay` (research/iq-corpus.md).
//!
//! A capture is a folder under the capture directory's `iq-captures/`: for
//! each radio a `source-N.sigmf-data` and `.sigmf-meta`, and a
//! `capture.sigmf-collection` naming them. Each meta file says all that's
//! needed to decode its capture again ("trp:config"): the systems as they were
//! set up, with nothing a user would call private (allow-lists: a field
//! nobody thought about stays out).
//!
//! Each source's thread offers its blocks to a [`Tap`]: one atomic load when
//! no capture is running. A capture's samples queue for a writer thread; a
//! writer that falls behind ends the capture, it never holds up decoding.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use num_complex::Complex32;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha512};
use trunk_app::config::{Config, NameLists, SampleFormat, SourceKind, SystemNames};
use trunk_app::protocol::{IqCapture, IqCaptureActive, IqWaterfall, ToClient};
use trunk_core::trunk::tape::{ControlTape, VoiceTape};
use trunk_core::trunk::{IdField, Status};

use crate::runtime::{Ctx, EngineCmd};
use crate::sdr::Block;

/// Captures' folder, in the capture directory.
const FOLDER: &str = "iq-captures";
const COLLECTION: &str = "capture.sigmf-collection";
pub(crate) const DIAGNOSTICS: &str = "diagnostics.json";
const MIN_S: u32 = 5;
const MAX_S: u32 = 300;
/// Blocks queued for a writer: seconds of samples, however a radio cuts them.
pub(crate) const QUEUE: usize = 512;
/// Every radio starts with its first block after this, so they start together.
const START_DELAY: Duration = Duration::from_millis(300);
/// A radio that sends nothing for this long has stopped.
const STALL: Duration = Duration::from_secs(10);
/// Free space a capture leaves on the disk, past its own size.
const DISK_MARGIN: u64 = 1 << 30;

/// What a source sends a capture's writer.
enum Chunk {
    /// When the first sample was received, Unix ms.
    Begin(f64),
    Bytes(Vec<u8>),
    Iq(Vec<Complex32>),
    /// Samples lost here.
    Gap(u64),
}

/// What's capturing from one source, while it does.
struct Job {
    tx: SyncSender<Chunk>,
    /// Samples still wanted.
    left: u64,
    not_before: Instant,
    started: bool,
    rate_hz: f64,
    center_hz: f64,
    /// An instant, and the same instant in Unix ms.
    clock: (Instant, f64),
    /// Why the capture was given up.
    failed: Arc<Mutex<Option<String>>>,
    activity: Arc<Activity>,
}

impl Job {
    /// To the writer, without waiting: a source's thread never does.
    fn send(&self, c: Chunk) -> Result<(), TrySendError<Chunk>> {
        self.tx.try_send(c)?;
        self.activity.queued();
        Ok(())
    }
}

/// How one source's capture is going, for `diagnostics.json`: lost samples beside what the disk was doing.
#[derive(Default)]
pub(crate) struct Activity {
    in_queue: AtomicUsize,
    queue_peak: AtomicUsize,
    lost: AtomicU64,
    slowest_write_us: AtomicU64,
    written: AtomicU64,
}

impl Activity {
    fn queued(&self) {
        let n = self.in_queue.fetch_add(1, Ordering::Relaxed) + 1;
        self.queue_peak.fetch_max(n, Ordering::Relaxed);
    }

    fn taken(&self) {
        self.in_queue.fetch_sub(1, Ordering::Relaxed);
    }

    fn wrote(&self, bytes: usize, took: Duration) {
        self.written.fetch_add(bytes as u64, Ordering::Relaxed);
        self.slowest_write_us.fetch_max(took.as_micros() as u64, Ordering::Relaxed);
    }

    /// What's happened since last asked: samples lost, the fullest the queue got, the slowest write (ms), bytes written.
    pub(crate) fn take(&self) -> (u64, usize, f64, u64) {
        let peak = self.queue_peak.swap(self.in_queue.load(Ordering::Relaxed), Ordering::Relaxed);
        (self.lost.swap(0, Ordering::Relaxed), peak, self.slowest_write_us.swap(0, Ordering::Relaxed) as f64 / 1000.0, self.written.swap(0, Ordering::Relaxed))
    }
}

/// A source's latest blocks, for a capture that looks back.
struct Ring {
    blocks: VecDeque<(Instant, u64, Chunk)>,
    samples: u64,
    /// The most kept.
    room: u64,
}

impl Ring {
    fn push(&mut self, at: Instant, dropped: u64, chunk: Chunk, n: u64) {
        self.blocks.push_back((at, dropped, chunk));
        self.samples += n;
        while self.samples > self.room {
            let Some((_, _, old)) = self.blocks.pop_front() else { break };
            self.samples -= old.samples();
        }
    }
}

#[derive(Default)]
struct TapState {
    job: Option<Job>,
    ring: Option<Ring>,
}

/// Where one source's samples can be taken from ([`Tap::samples`]).
#[derive(Default)]
pub struct Tap {
    armed: AtomicBool,
    job: Mutex<TapState>,
}

impl Chunk {
    fn samples(&self) -> u64 {
        match self {
            Chunk::Bytes(b) => b.len() as u64 / 2,
            Chunk::Iq(v) => v.len() as u64,
            _ => 0,
        }
    }

    fn of(block: &Block, keep: usize) -> Chunk {
        match block {
            Block::U8(b) => Chunk::Bytes(b[..keep * 2].to_vec()),
            Block::Iq(b) => Chunk::Iq(b[..keep].to_vec()),
            Block::Real16(_) => unreachable!("the inbox makes them I/Q"),
        }
    }
}

impl Tap {
    /// A block from the source, the samples lost just before it, and when it arrived.
    #[inline]
    pub fn samples(&self, block: &Block, dropped: u64, at: Instant) {
        if self.armed.load(Ordering::Relaxed) {
            self.take(block, dropped, at);
        }
    }

    fn take(&self, block: &Block, dropped: u64, at: Instant) {
        let mut state = self.job.lock().unwrap();
        let n = block.len() as u64;
        if let Some(r) = &mut state.ring {
            r.push(at, dropped, Chunk::of(block, n as usize), n);
        }
        let slot = &mut state.job;
        let Some(j) = slot.as_mut() else { return };
        if at < j.not_before {
            return;
        }
        let mut sent = Ok(());
        if !j.started {
            j.started = true;
            let ms = j.clock.1 + at.saturating_duration_since(j.clock.0).as_secs_f64() * 1000.0 - n as f64 / j.rate_hz * 1000.0;
            sent = j.send(Chunk::Begin(ms));
        } else if dropped > 0 {
            let lost = dropped.min(j.left);
            j.left -= lost;
            j.activity.lost.fetch_add(lost, Ordering::Relaxed);
            sent = j.send(Chunk::Gap(lost));
        }
        let keep = n.min(j.left) as usize;
        if sent.is_ok() && keep > 0 {
            j.left -= keep as u64;
            sent = j.send(Chunk::of(block, keep));
        }
        if let Err(e) = sent {
            if matches!(e, TrySendError::Full(_)) {
                *j.failed.lock().unwrap() = Some("The disk couldn't keep up with the radio, so the capture was given up.".into());
            }
            self.end(&mut state);
        } else if job_done(&state.job) {
            self.end(&mut state);
        }
    }

    /// The radio changed: the capture goes on through a pause, not to another frequency.
    pub fn tuned(&self, center_hz: f64, gap: u64) {
        if !self.armed.load(Ordering::Relaxed) {
            return;
        }
        let mut state = self.job.lock().unwrap();
        let Some(j) = state.job.as_mut() else { return };
        if (center_hz - j.center_hz).abs() > 1.0 {
            *j.failed.lock().unwrap() = Some("The radio was retuned during the capture.".into());
            self.end(&mut state);
        } else if j.started && gap > 0 {
            let lost = gap.min(j.left);
            j.left -= lost;
            let _ = j.send(Chunk::Gap(lost));
            if job_done(&state.job) {
                self.end(&mut state);
            }
        }
    }

    fn arm(&self, job: Job) {
        self.job.lock().unwrap().job = Some(job);
        self.armed.store(true, Ordering::Relaxed);
    }

    /// Hand the writer the samples kept so far, then go on from them: `job` hasn't begun.
    fn arm_after_lookback(&self, mut job: Job) {
        // (The writer's queue may not take it all at once: this thread can wait, the source's can't.)
        loop {
            let blocks = self.job.lock().unwrap().ring.as_mut().map(|r| { r.samples = 0; std::mem::take(&mut r.blocks) }).unwrap_or_default();
            let few = blocks.len() < 16;
            for (at, dropped, chunk) in blocks {
                if !job.started {
                    job.started = true;
                    let ms = job.clock.1 - job.clock.0.saturating_duration_since(at).as_secs_f64() * 1000.0 - chunk.samples() as f64 / job.rate_hz * 1000.0;
                    job.activity.queued();
                    let _ = job.tx.send(Chunk::Begin(ms));
                } else if dropped > 0 {
                    job.activity.lost.fetch_add(dropped, Ordering::Relaxed);
                    job.activity.queued();
                    let _ = job.tx.send(Chunk::Gap(dropped));
                }
                job.activity.queued();
                let _ = job.tx.send(chunk);
            }
            if few {
                break;
            }
        }
        self.arm(job);
    }

    /// Keep this many of the source's latest samples, for a capture to look back on.
    fn keep_lookback(&self, room: u64) {
        self.job.lock().unwrap().ring = Some(Ring { blocks: VecDeque::new(), samples: 0, room });
        self.armed.store(true, Ordering::Relaxed);
    }

    /// No more samples: the writer sees its queue end.
    fn end(&self, state: &mut TapState) {
        state.job = None;
        if state.ring.is_none() {
            self.armed.store(false, Ordering::Relaxed);
        }
    }

    fn cancel(&self, why: &str) {
        let mut state = self.job.lock().unwrap();
        if let Some(j) = state.job.as_ref() {
            *j.failed.lock().unwrap() = Some(why.to_string());
        }
        state.ring = None;
        self.end(&mut state);
    }
}

fn job_done(slot: &Option<Job>) -> bool {
    slot.as_ref().is_some_and(|j| j.left == 0)
}

struct Active {
    name: String,
    seconds: u32,
    started: Instant,
}

/// The recorder's captures: the running session's taps, the one being made, and how the last went.
#[derive(Default)]
pub struct Captures {
    taps: Mutex<Vec<Arc<Tap>>>,
    active: Mutex<Option<Active>>,
    last_error: Mutex<Option<String>>,
    rules: Mutex<Option<SyncSender<crate::iq_rules::Fact>>>,
    /// A capture is being sent ([`crate::iq_send`]).
    pub sending: Mutex<bool>,
}

impl Captures {
    /// A session began: a tap for each source, keeping what a capture rule may look back on.
    pub fn session_began(&self, cfg: &Config) -> Vec<Arc<Tap>> {
        let taps: Vec<Arc<Tap>> = cfg.sources.iter().map(|_| Arc::default()).collect();
        let look = cfg.diagnostics.wideband_lookback(|d| crate::iq_rules::seconds(d).unwrap_or(0.0));
        if look > 0.0 {
            // Held as a block of each radio's samples, in memory: the budget decides how far back.
            let per_s: f64 = cfg.sources.iter().map(|s| s.rate_hz * memory_per_sample(&s.kind) as f64).sum();
            let room_s = look.min(cfg.diagnostics.budget.ram_mb as f64 * 1e6 / per_s);
            if room_s < look {
                log::warn!("Capture rules look back {look:.0} s, but their memory budget holds {room_s:.1} s of these radios");
            }
            for (t, s) in taps.iter().zip(&cfg.sources) {
                t.keep_lookback((s.rate_hz * room_s) as u64);
            }
        }
        *self.taps.lock().unwrap() = taps.clone();
        taps
    }

    /// Where the session's capture rules take facts.
    pub fn set_rules(&self, tx: Option<SyncSender<crate::iq_rules::Fact>>) {
        *self.rules.lock().unwrap() = tx;
    }

    pub fn has_rules(&self) -> bool {
        self.rules.lock().unwrap().is_some()
    }

    /// Tell the rules what happened; they're never waited for.
    pub fn fact(&self, f: crate::iq_rules::Fact) {
        if let Some(tx) = &*self.rules.lock().unwrap() {
            let _ = tx.try_send(f);
        }
    }

    /// The session ended: a capture still running is given up.
    pub fn session_ended(&self) {
        *self.rules.lock().unwrap() = None;
        for t in self.taps.lock().unwrap().drain(..) {
            t.cancel("Recording stopped during the capture.");
        }
    }
}

/// What the running session knows that a capture keeps.
pub struct Live {
    pub status: Status,
    /// Each site's band plan, in site order.
    pub bandplans: Vec<String>,
    pub sources: Vec<trunk_app::session::SourceCounts>,
}

fn live(ctx: &Ctx) -> Option<Live> {
    let (done, answer) = mpsc::channel();
    ctx.tell_engine(EngineCmd::IqLive { done });
    answer.recv_timeout(Duration::from_secs(5)).ok()
}

/// A file a radio's samples take, per sample.
fn bytes_per_sample(kind: &SourceKind) -> u64 {
    match kind {
        SourceKind::Rtlsdr { .. } | SourceKind::Hackrf { .. } => 2,
        SourceKind::File { format, path, .. } if SampleFormat::of(*format, path) == SampleFormat::Cu8 => 2,
        _ => 4,
    }
}

/// What a block of a source's samples takes in memory, per sample.
fn memory_per_sample(kind: &SourceKind) -> u64 {
    if bytes_per_sample(kind) == 2 { 2 } else { 8 }
}

/// About how many bytes capturing every radio for `seconds` takes.
pub fn estimate(cfg: &Config, seconds: f64) -> u64 {
    cfg.sources.iter().map(|s| (s.rate_hz * seconds) as u64 * bytes_per_sample(&s.kind)).sum()
}

/// A capture of every radio.
pub struct Request {
    /// Seconds from now, and already received.
    pub after_s: f64,
    pub before_s: f64,
    pub note: String,
    /// `healthy` or `rule`.
    pub purpose: &'static str,
    pub rule: Option<Value>,
}

/// Capture every radio of the running session for `seconds`, in the background: the capture button.
pub fn start(ctx: &Arc<Ctx>, seconds: u32, note: &str) -> Result<(), String> {
    if !(MIN_S..=MAX_S).contains(&seconds) {
        return Err(format!("A capture is {MIN_S} to {MAX_S} seconds long."));
    }
    begin_capture(ctx, &Request { after_s: seconds as f64, before_s: 0.0, note: note.to_string(), purpose: "healthy", rule: None })
}

/// Capture every radio of the running session, in the background.
pub fn begin_capture(ctx: &Arc<Ctx>, req: &Request) -> Result<(), String> {
    let seconds = (req.after_s + req.before_s).ceil() as u32;
    let taps = ctx.iq.taps.lock().unwrap().clone();
    let cfg = ctx.run.lock().unwrap().clone().filter(|c| c.sources.len() == taps.len() && !taps.is_empty()).ok_or("Start recording first: a capture is of what the radios are receiving now.")?;
    let name = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    {
        let mut active = ctx.iq.active.lock().unwrap();
        if active.is_some() {
            return Err("A capture is already running.".into());
        }
        *active = Some(Active { name: name.clone(), seconds, started: Instant::now() });
    }
    *ctx.iq.last_error.lock().unwrap() = None;
    let begun = begin(ctx, &cfg, taps, &name, req);
    if begun.is_err() {
        *ctx.iq.active.lock().unwrap() = None;
    }
    begun
}

fn begin(ctx: &Arc<Ctx>, cfg: &Config, taps: Vec<Arc<Tap>>, name: &str, req: &Request) -> Result<(), String> {
    let seconds = req.after_s + req.before_s;
    // (Where captures go can change while recording: it's not one of the run's settings.)
    let root = root(&ctx.config.lock().unwrap());
    fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let need = estimate(cfg, seconds);
    if let Some((_, free)) = crate::spool::statvfs(&root).filter(|d| d.1 < need + DISK_MARGIN) {
        return Err(format!("This capture takes about {} MB and leaves 1 GB free, but only {} MB is free on that disk.", need / 1_000_000, free / 1_000_000));
    }
    let dir = root.join(name);
    fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let before = live(ctx).ok_or("The recording isn't answering, so nothing was captured.")?;
    let centers = cfg.resolved_centers();
    let failed = Arc::new(Mutex::new(None));
    let not_before = Instant::now() + START_DELAY;
    let clock = (Instant::now(), crate::clock::unix_ms() as f64);
    let mut writers = Vec::new();
    let activity: Vec<Arc<Activity>> = taps.iter().map(|_| Arc::default()).collect();
    for (i, tap) in taps.iter().enumerate() {
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let path = dir.join(format!("source-{i}.sigmf-data"));
        let doing = activity[i].clone();
        writers.push(std::thread::Builder::new().name(format!("iq-{i}")).spawn(move || write_samples(rx, &path, &doing)).map_err(|e| e.to_string())?);
        let rate_hz = cfg.sources[i].rate_hz;
        let job = Job { tx, left: (rate_hz * req.after_s) as u64, not_before, started: false, rate_hz, center_hz: centers[i], clock, failed: failed.clone(), activity: activity[i].clone() };
        if req.before_s > 0.0 {
            tap.arm_after_lookback(Job { not_before: Instant::now(), ..job });
        } else {
            tap.arm(job);
        }
    }
    let stop_watching = Arc::new(AtomicBool::new(false));
    let watching = {
        let (stop, dir, recordings) = (stop_watching.clone(), dir.clone(), PathBuf::from(&cfg.recording.capture_dir));
        std::thread::Builder::new().name("iq-watch".into()).spawn(move || crate::diagnostics::watch(&activity, &dir, &recordings, &stop)).map_err(|e| e.to_string())?
    };
    let (ctx, cfg, dir, name, what) = (ctx.clone(), cfg.clone(), dir, name.to_string(), (req.note.clone(), req.purpose, req.rule.clone()));
    let finished = move || {
        let results: Vec<_> = writers.into_iter().map(|w| w.join().unwrap_or_else(|_| Err("A capture writer failed.".into()))).collect();
        stop_watching.store(true, Ordering::Relaxed);
        let watched = watching.join().unwrap_or(Value::Null);
        let saved = failed.lock().unwrap().take().map_or_else(|| save(&ctx, &cfg, &dir, &what, &before, results, &watched), Err);
        match &saved {
            Ok(()) => log::info!("IQ capture saved: {}", dir.display()),
            Err(e) => {
                log::warn!("IQ capture {name} not saved: {e}");
                let _ = fs::remove_dir_all(&dir);
            }
        }
        *ctx.iq.last_error.lock().unwrap() = saved.err();
        *ctx.iq.active.lock().unwrap() = None;
    };
    std::thread::Builder::new().name("iq-capture".into()).spawn(finished).map_err(|e| e.to_string())?;
    Ok(())
}

/// What a writer put in its file.
#[derive(Default)]
struct Written {
    /// Unix ms of the first sample.
    start_ms: f64,
    format: Option<SampleFormat>,
    samples: u64,
    /// (samples in the file before it, samples lost there).
    gaps: Vec<(u64, u64)>,
    sha512: String,
}

#[cfg(test)]
impl Written {
    fn lost(&self) -> u64 {
        self.gaps.iter().map(|g| g.1).sum()
    }
}

fn write_samples(rx: mpsc::Receiver<Chunk>, path: &Path, activity: &Activity) -> Result<Written, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut file = BufWriter::with_capacity(1 << 20, File::create(path).map_err(fail)?);
    let mut hash = Sha512::new();
    let mut w = Written::default();
    let mut out = Vec::new();
    loop {
        let next = rx.recv_timeout(STALL);
        if next.is_ok() {
            activity.taken();
        }
        let bytes: &[u8] = match next {
            Ok(Chunk::Begin(ms)) => {
                w.start_ms = ms;
                continue;
            }
            Ok(Chunk::Gap(n)) => {
                w.gaps.push((w.samples, n));
                continue;
            }
            Ok(Chunk::Bytes(b)) => {
                w.format = Some(SampleFormat::Cu8);
                w.samples += b.len() as u64 / 2;
                out = b;
                &out
            }
            Ok(Chunk::Iq(iq)) => {
                w.format = Some(SampleFormat::Cs16);
                w.samples += iq.len() as u64;
                out.clear();
                out.reserve(iq.len() * 4);
                for v in &iq {
                    out.extend_from_slice(&full_scale(v.re).to_le_bytes());
                    out.extend_from_slice(&full_scale(v.im).to_le_bytes());
                }
                &out
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => return Err("The radio stopped sending samples during the capture.".into()),
        };
        hash.update(bytes);
        let started = Instant::now();
        file.write_all(bytes).map_err(fail)?;
        activity.wrote(bytes.len(), started.elapsed());
    }
    file.flush().map_err(fail)?;
    w.sha512 = crate::toolkit::hex(&hash.finalize());
    Ok(w)
}

/// A float sample as `cs16` ([`trunk_app::samples::to_iq`] reads it back).
fn full_scale(v: f32) -> i16 {
    (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

fn datatype(f: SampleFormat) -> &'static str {
    match f {
        SampleFormat::Cu8 => "cu8",
        SampleFormat::Cs16 => "ci16_le",
        SampleFormat::Cf32 => "cf32_le",
    }
}

fn iso(ms: f64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map_or_else(String::new, |d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Only `keys` of an object.
pub(crate) fn keep(v: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().filter_map(|k| v.get(*k).map(|x| (k.to_string(), x.clone()))).collect::<Map<_, _>>())
}

pub(crate) const RECORDING_KEYS: &[&str] = &["callTimeoutS", "conversationMode", "recordUnknown", "recordEncrypted", "recordUnitToUnit", "keepSilentCalls", "minCallS", "maxCallS", "minTransmissionS", "prerollS", "maxRecorders", "vocoder", "normalizeAudio", "digitalLevelDb", "analogLevelDb", "dropDuplicateCalls"];
const SYSTEM_KEYS: &[&str] = &["shortName", "type", "enabled", "defaultMode", "nxdnType", "preferredSite", "receiveOn", "sites", "recording"];
const SITE_KEYS: &[&str] = &["name", "enabled", "controlChannelsHz", "modulation", "expect", "voiceChannelsHz", "bandplan", "bandplanBaseHz", "bandplanSpacingHz", "bandplanOffset", "bandplanHighHz", "lcnTableHz", "dmrChannelsHz", "colorCode", "nxdnRate", "nxdnChannelsHz", "ran", "esk"];
const CONVENTIONAL_KEYS: &[&str] = &["shortName", "enabled", "squelchDb", "demod", "channels", "recording"];
const CHANNEL_KEYS: &[&str] = &["freqHz", "mode", "talkgroup", "code", "slot", "matchTalkgroup", "squelchDb", "bandwidthKhz", "enabled", "receiveOn", "autoTune"];
pub(crate) const RADIO_KEYS: &[&str] = &["type", "rateHz", "ppm", "autoTune", "guardHz", "gainDb", "agc", "antenna", "wireFormat", "gainMode", "gainStep", "lnaStep", "mixerStep", "vgaStep", "lnaDb", "vgaDb", "amp", "biasTee", "gains", "bufLen", "buffers", "packing"];

/// A list of source labels, as this capture calls them.
fn relabel(v: &mut Value, labels: &BTreeMap<String, String>) {
    if let Some(a) = v.get_mut("receiveOn").and_then(Value::as_array_mut) {
        for l in a.iter_mut() {
            *l = json!(l.as_str().and_then(|s| labels.get(s)).map_or("", String::as_str));
        }
    }
}

/// The config's systems and how they're recorded, as a config for this
/// capture's files: what decoding them takes, and nothing else.
fn decode_config(cfg: &Config, written: &[Written]) -> Value {
    let cfg_json = serde_json::to_value(cfg).unwrap_or_default();
    let centers = cfg.resolved_centers();
    let labels: BTreeMap<String, String> = cfg.sources.iter().enumerate().map(|(i, s)| (s.label(), format!("file source-{i}.sigmf-data"))).collect();
    let sources: Vec<Value> = cfg
        .sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let format = match written[i].format.unwrap_or(SampleFormat::Cu8) {
                SampleFormat::Cu8 => "cu8",
                SampleFormat::Cs16 => "cs16",
                SampleFormat::Cf32 => "cf32",
            };
            json!({ "type": "file", "path": format!("source-{i}.sigmf-data"), "format": format, "realtime": false, "centerHz": centers[i], "rateHz": s.rate_hz, "autoTune": s.auto_tune, "guardHz": s.guard_hz, "name": format!("radio-{i}") })
        })
        .collect();
    let clean = |row: &Value, keys: &[&str]| {
        let mut v = keep(row, keys);
        if let Some(r) = v.get_mut("recording") {
            *r = keep(r, RECORDING_KEYS);
        }
        relabel(&mut v, &labels);
        v
    };
    let enabled = |v: &&Value| v["enabled"] != json!(false);
    let list = |k: &str| cfg_json[k].as_array().cloned().unwrap_or_default();
    let systems: Vec<Value> = list("systems")
        .iter()
        .filter(enabled)
        .map(|s| {
            let mut v = clean(s, SYSTEM_KEYS);
            v["sites"] = Value::Array(s["sites"].as_array().into_iter().flatten().map(|x| keep(x, SITE_KEYS)).collect());
            v
        })
        .collect();
    let conventional: Vec<Value> = list("conventional")
        .iter()
        .filter(enabled)
        .map(|c| {
            let mut v = clean(c, CONVENTIONAL_KEYS);
            v["channels"] = Value::Array(c["channels"].as_array().into_iter().flatten().map(|x| clean(x, CHANNEL_KEYS)).collect());
            v
        })
        .collect();
    json!({ "sources": sources, "systems": systems, "conventional": conventional, "recording": keep(&cfg_json["recording"], RECORDING_KEYS) })
}

/// Of each system's talkgroups, what changes how calls are decoded or kept: no names.
fn decode_talkgroups(names: &NameLists) -> Value {
    let rows = names.iter().filter_map(|(system, n)| {
        let kept: Vec<Value> = n.talkgroups.iter().filter(|r| !matches!(r.tg.mode.as_str(), "" | "D") || r.tg.ignore).map(|r| json!({ "number": r.tg.number, "mode": r.tg.mode, "ignore": r.tg.ignore })).collect();
        (!kept.is_empty()).then(|| (system.clone(), Value::Array(kept)))
    });
    Value::Object(rows.collect())
}

/// How decoding went while the capture was made: what a replay of it should come close to.
fn live_summary(before: &Status, after: &Status) -> Value {
    let sites: Vec<Value> = after
        .sites
        .iter()
        .map(|a| {
            let b = before.sites.iter().find(|b| b.site == a.site);
            json!({
                "system": a.short_name,
                "site": a.site_name,
                "modulation": a.modulation,
                "control_channel_hz": a.control_channel_hz,
                "nac": a.identity.get(IdField::Nac),
                "wacn": a.identity.wacn(),
                "sysid": a.identity.sys_id(),
                "rfss": a.identity.rfss(),
                "site_id": a.identity.site(),
                "messages_good": a.good - b.map_or(0, |b| b.good),
                "messages_bad": a.bad - b.map_or(0, |b| b.bad),
                "calls": a.calls_concluded - b.map_or(0, |b| b.calls_concluded),
            })
        })
        .collect();
    json!({ "sites": sites, "calls": after.calls_concluded - before.calls_concluded })
}

fn save(ctx: &Ctx, cfg: &Config, dir: &Path, what: &(String, &'static str, Option<Value>), before: &Live, results: Vec<Result<Written, String>>, watched: &Value) -> Result<(), String> {
    let written = results.into_iter().collect::<Result<Vec<_>, _>>()?;
    let (note, purpose, rule) = what;
    let after = live(ctx).ok_or("The recording ended during the capture.")?;
    let names = ctx.names.lock().unwrap().clone();
    let centers = cfg.resolved_centers();
    let context = json!({
        "trp:config": decode_config(cfg, &written),
        "trp:talkgroups": decode_talkgroups(&names),
        "trp:bandplans": after.bandplans,
        "trp:live": live_summary(&before.status, &after.status),
        "trp:purpose": purpose,
        "trp:rule": rule,
    });
    let mut streams = Vec::new();
    for (i, w) in written.iter().enumerate() {
        let source = keep(&serde_json::to_value(&cfg.sources[i]).unwrap_or_default(), RADIO_KEYS);
        let meta = recording_meta(w, centers[i], cfg.sources[i].rate_hz, source, note, &context);
        let text = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
        let name = format!("source-{i}");
        let path = dir.join(format!("{name}.sigmf-meta"));
        fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        streams.push(json!({ "name": name, "hash": crate::toolkit::hex(&Sha512::digest(text.as_bytes())) }));
    }
    let collection = json!({ "collection": { "core:version": "1.2.0", "core:streams": streams } });
    let text = serde_json::to_string_pretty(&collection).map_err(|e| e.to_string())?;
    let path = dir.join(COLLECTION);
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut diagnostics = crate::diagnostics::build(cfg, &names, &after, &ctx.shared.recent_events());
    diagnostics["capture"] = watched.clone();
    let path = dir.join(DIAGNOSTICS);
    fs::write(&path, serde_json::to_string_pretty(&diagnostics).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", path.display()))
}

fn recording_meta(w: &Written, center_hz: f64, rate_hz: f64, source: Value, note: &str, context: &Value) -> Value {
    let mut captures = vec![json!({ "core:sample_start": 0, "core:global_index": 0, "core:frequency": center_hz, "core:datetime": iso(w.start_ms) })];
    let mut lost = 0;
    for (at, n) in &w.gaps {
        lost += n;
        captures.push(json!({ "core:sample_start": at, "core:global_index": at + lost, "core:frequency": center_hz }));
    }
    let hw = source["type"].as_str().unwrap_or("radio").to_string();
    let mut global = json!({
        "core:datatype": datatype(w.format.unwrap_or(SampleFormat::Cu8)),
        "core:sample_rate": rate_hz,
        "core:version": "1.2.0",
        "core:recorder": format!("Trunk Recorder Pro {}", env!("CARGO_PKG_VERSION")),
        "core:hw": hw,
        "core:sha512": w.sha512,
        "core:collection": "capture",
        "core:description": note,
        "core:extensions": [{ "name": "trp", "version": "0.1.0", "optional": true }],
        "trp:tier": "wideband",
        "trp:app": { "version": env!("CARGO_PKG_VERSION") },
        "trp:source": source,
        "trp:start_unix_ms": w.start_ms,
    });
    if let (Some(g), Some(c)) = (global.as_object_mut(), context.as_object()) {
        g.extend(c.clone());
    }
    json!({ "global": global, "captures": captures, "annotations": [] })
}

pub fn root(cfg: &Config) -> PathBuf {
    match cfg.diagnostics.capture_dir.trim() {
        "" => PathBuf::from(&cfg.recording.capture_dir).join(FOLDER),
        own => PathBuf::from(own),
    }
}

/// What's in a capture's folder, for the list.
fn describe(dir: &Path) -> Option<IqCapture> {
    let name = dir.file_name()?.to_str()?.to_string();
    let text = fs::read_to_string(dir.join(COLLECTION)).ok()?;
    let streams: Value = serde_json::from_str(&text).ok()?;
    let streams = streams["collection"]["core:streams"].as_array()?.clone();
    let meta: Value = serde_json::from_str(&fs::read_to_string(dir.join(format!("{}.sigmf-meta", streams.first()?["name"].as_str()?))).ok()?).ok()?;
    let g = &meta["global"];
    let (mut bytes, mut lost, mut samples) = (0, 0, 0.0);
    for s in &streams {
        let stem = s["name"].as_str()?;
        let m: Value = serde_json::from_str(&fs::read_to_string(dir.join(format!("{stem}.sigmf-meta"))).ok()?).ok()?;
        let size = fs::metadata(dir.join(format!("{stem}.sigmf-data"))).ok()?.len();
        bytes += size;
        let last = m["captures"].as_array()?.last()?.clone();
        lost += last["core:global_index"].as_u64()? - last["core:sample_start"].as_u64()?;
        let per = match m["global"]["core:datatype"].as_str() {
            Some("cu8") => 2,
            Some("cf32_le") => 8,
            _ => 4,
        };
        samples = (size / per) as f64 / m["global"]["core:sample_rate"].as_f64()?;
    }
    let sum = |k: &str| g["trp:live"]["sites"].as_array().map_or(0, |a| a.iter().filter_map(|s| s[k].as_u64()).sum());
    Some(IqCapture {
        name,
        path: dir.display().to_string(),
        tier: g["trp:tier"].as_str().unwrap_or("wideband").to_string(),
        purpose: g["trp:purpose"].as_str().unwrap_or("").to_string(),
        rule: g["trp:rule"]["id"].as_str().unwrap_or("").to_string(),
        seconds: samples.round() as u32,
        radios: streams.len() as u32,
        bytes,
        note: g["core:description"].as_str().unwrap_or("").to_string(),
        lost_samples: lost,
        messages_good: sum("messages_good"),
        messages_bad: sum("messages_bad"),
        calls: g["trp:live"]["calls"].as_u64().unwrap_or(0),
        send: crate::iq_send::status(dir),
    })
}

/// A capture's folder, by the name the list gave it.
fn capture_dir(ctx: &Ctx, name: &str) -> Result<PathBuf, String> {
    let mut parts = Path::new(name).components();
    if !matches!((parts.next(), parts.next()), (Some(std::path::Component::Normal(_)), None)) {
        return Err("No such capture.".into());
    }
    let dir = root(&ctx.config.lock().unwrap()).join(name);
    if !dir.join(COLLECTION).is_file() {
        return Err("No such capture.".into());
    }
    Ok(dir)
}

/// What a capture would publish, for its owner to look over.
pub fn review(ctx: &Ctx, id: u64, name: &str) -> ToClient {
    let reviewed = (|| -> Result<ToClient, String> {
        let dir = capture_dir(ctx, name)?;
        let opened = open(dir.to_str().ok_or("Its path isn't text")?)?;
        let images = opened.recordings.iter().map(crate::waterfall::of_recording).collect::<Result<Vec<_>, _>>()?;
        let read = |f: &str| fs::read_to_string(dir.join(f)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let waterfalls = images.into_iter().map(|i| IqWaterfall { width: i.width, height: i.height, levels: crate::toolkit::hex(&i.pixels) }).collect();
        Ok(ToClient::IqCaptureReview { id, name: name.to_string(), metadata: read("source-0.sigmf-meta").map_or(Value::Null, |m| m["global"].clone()), diagnostics: read(DIAGNOSTICS).unwrap_or(Value::Null), waterfalls, error: None })
    })();
    reviewed.unwrap_or_else(|error| ToClient::IqCaptureReview { id, name: name.to_string(), metadata: Value::Null, diagnostics: Value::Null, waterfalls: vec![], error: Some(error) })
}

/// Send a capture its owner has reviewed.
pub fn send(ctx: &Arc<Ctx>, name: &str) -> Result<(), String> {
    if ctx.iq.active.lock().unwrap().as_ref().is_some_and(|a| a.name == name) {
        return Err("That capture is still being made.".into());
    }
    crate::iq_send::start(ctx, capture_dir(ctx, name)?)
}

/// Withdraw a capture that was sent.
pub fn withdraw(ctx: &Ctx, name: &str) -> Result<(), String> {
    crate::iq_send::withdraw(&capture_dir(ctx, name)?)
}

/// The captures on disk and the one being made: the answer to every capture command.
pub fn listing(ctx: &Ctx, id: u64, error: Option<String>) -> ToClient {
    let cfg = ctx.config.lock().unwrap().clone();
    let mut captures: Vec<IqCapture> = fs::read_dir(root(&cfg)).into_iter().flatten().flatten().filter_map(|e| describe(&e.path())).collect();
    captures.sort_by(|a, b| b.name.cmp(&a.name));
    let active = ctx.iq.active.lock().unwrap().as_ref().map(|a| IqCaptureActive { name: a.name.clone(), seconds: a.seconds, elapsed_s: a.started.elapsed().as_secs_f64() });
    let error = error.or_else(|| ctx.iq.last_error.lock().unwrap().clone());
    ToClient::IqCaptures { id, active, captures, root: root(&cfg).display().to_string(), recording: !ctx.iq.taps.lock().unwrap().is_empty(), presets: crate::iq_rules::presets(), can_send: crate::iq_send::available(), error }
}

/// A folder for a channel tape a rule keeps: the capture directory's, named for what it is.
fn tape_dir(ctx: &Ctx, kind: &str, freq: u64) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f");
    root(&ctx.config.lock().unwrap()).join(format!("{stamp}-{kind}-{freq}"))
}

fn add_diagnostics(ctx: &Ctx, dir: &Path, live: &Live) -> Result<(), String> {
    let run = ctx.run.lock().unwrap().clone().ok_or("Recording has stopped")?;
    let names = ctx.names.lock().unwrap().clone();
    let d = crate::diagnostics::build(&run, &names, live, &ctx.shared.recent_events());
    let path = dir.join(DIAGNOSTICS);
    fs::write(&path, serde_json::to_string_pretty(&d).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", path.display()))
}

/// Save a voice channel's tape, for rule `rule`.
pub fn save_voice(ctx: &Ctx, t: &VoiceTape, rule: Value) -> Result<(), String> {
    let dir = tape_dir(ctx, "voice", t.freq_hz as u64);
    crate::tape_file::write_voice(&dir, t, json!({ "trp:purpose": "rule", "trp:rule": rule }))?;
    let live = live(ctx).ok_or("The recording ended")?;
    add_diagnostics(ctx, &dir, &live)?;
    log::info!("Voice channel capture saved: {}", dir.display());
    Ok(())
}

/// Save a control channel's tape, for rule `rule`.
pub fn save_control(ctx: &Ctx, cfg: &Config, t: &ControlTape, live: &Live, rule: Value) -> Result<(), String> {
    let freq = live.status.sites.get(t.site as usize).and_then(|s| s.control_channel_hz).unwrap_or(0);
    let dir = tape_dir(ctx, "control", freq);
    let config = decode_config(cfg, &vec_of_written(cfg.sources.len()));
    let bandplan = live.bandplans.get(t.site as usize).map_or("", String::as_str);
    crate::tape_file::write_control(&dir, t, freq as f64, &config, bandplan, json!({ "trp:purpose": "rule", "trp:rule": rule }))?;
    add_diagnostics(ctx, &dir, live)?;
    log::info!("Control channel capture saved: {}", dir.display());
    Ok(())
}

fn vec_of_written(n: usize) -> Vec<Written> {
    (0..n).map(|_| Written::default()).collect()
}

/// Delete a finished capture.
pub fn delete(ctx: &Ctx, name: &str) -> Result<(), String> {
    if ctx.iq.active.lock().unwrap().as_ref().is_some_and(|a| a.name == name) {
        return Err("That capture is still being made.".into());
    }
    let mut parts = Path::new(name).components();
    if !matches!((parts.next(), parts.next()), (Some(std::path::Component::Normal(_)), None)) {
        return Err("No such capture.".into());
    }
    let dir = root(&ctx.config.lock().unwrap()).join(name);
    if !dir.join(COLLECTION).is_file() {
        return Err("No such capture.".into());
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// One radio's file of a capture being replayed.
pub struct Recording {
    pub data: PathBuf,
    pub format: SampleFormat,
    pub center_hz: f64,
    pub rate_hz: f64,
    pub auto_tune: bool,
    pub guard_hz: f64,
    /// Samples to leave out at the start, so every radio starts at the same moment.
    pub skip: u64,
    /// (samples in the file before it, samples lost there).
    pub gaps: Vec<(u64, u64)>,
}

/// A capture opened for replay.
pub struct Opened {
    pub recordings: Vec<Recording>,
    /// The systems it was made with, if the file says.
    pub config: Option<Config>,
    /// The same, as written.
    pub config_json: Option<Value>,
    pub names: NameLists,
    pub bandplans: Vec<String>,
}

/// Whether `path` names a SigMF capture: a meta or collection file, or a capture's folder.
pub fn is_sigmf(path: &str) -> bool {
    path.ends_with(".sigmf-meta") || path.ends_with(".sigmf-collection") || Path::new(path).join(COLLECTION).is_file()
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_recording(meta_path: &Path, meta: &Value) -> Result<(Recording, f64), String> {
    let bad = |what: &str| format!("{}: {what}", meta_path.display());
    let g = &meta["global"];
    let format = match g["core:datatype"].as_str() {
        Some("cu8") => SampleFormat::Cu8,
        Some("ci16_le") => SampleFormat::Cs16,
        Some("cf32_le") => SampleFormat::Cf32,
        other => return Err(bad(&format!("sample type {} can't be replayed (cu8, ci16_le, cf32_le)", other.unwrap_or("(none)")))),
    };
    let captures = g.get("core:sample_rate").and(meta["captures"].as_array()).filter(|c| !c.is_empty()).ok_or_else(|| bad("no sample rate or captures"))?;
    let center_hz = captures[0]["core:frequency"].as_f64().ok_or_else(|| bad("no core:frequency"))?;
    if captures.iter().any(|c| c["core:frequency"].as_f64().is_some_and(|f| f != center_hz)) {
        return Err(bad("it changes frequency, which replay can't follow"));
    }
    let mut gaps = Vec::new();
    let mut shift = 0;
    for c in captures {
        let start = c["core:sample_start"].as_u64().ok_or_else(|| bad("a capture without core:sample_start"))?;
        let index = c["core:global_index"].as_u64().unwrap_or(start);
        if index.saturating_sub(start) > shift {
            gaps.push((start, index - start - shift));
            shift = index - start;
        }
    }
    let data = meta_path.with_file_name(g["core:dataset"].as_str().map_or_else(|| format!("{}.sigmf-data", meta_path.file_stem().unwrap_or_default().to_string_lossy()), str::to_string));
    let source = &g["trp:source"];
    let rec = Recording {
        data,
        format,
        center_hz,
        rate_hz: g["core:sample_rate"].as_f64().ok_or_else(|| bad("no core:sample_rate"))?,
        auto_tune: source["autoTune"].as_bool().unwrap_or(false),
        guard_hz: source["guardHz"].as_f64().unwrap_or(trunk_core::trunk::DEFAULT_GUARD_HZ),
        skip: 0,
        gaps,
    };
    Ok((rec, g["trp:start_unix_ms"].as_f64().unwrap_or(0.0)))
}

/// Open a capture by its folder, collection or meta file.
pub fn open(path: &str) -> Result<Opened, String> {
    let p = Path::new(path);
    let collection = if p.is_dir() { Some(p.join(COLLECTION)) } else { path.ends_with(".sigmf-collection").then(|| p.to_path_buf()) };
    let metas: Vec<PathBuf> = match &collection {
        Some(c) => {
            let v = read_json(c)?;
            let streams = v["collection"]["core:streams"].as_array().filter(|s| !s.is_empty()).ok_or_else(|| format!("{}: no core:streams", c.display()))?;
            let mut metas = Vec::new();
            for s in streams {
                let name = s["name"].as_str().ok_or_else(|| format!("{}: a stream without a name", c.display()))?;
                let meta = c.with_file_name(format!("{name}.sigmf-meta"));
                let text = fs::read(&meta).map_err(|e| format!("{}: {e}", meta.display()))?;
                if s["hash"].as_str().is_some_and(|h| !h.eq_ignore_ascii_case(&crate::toolkit::hex(&Sha512::digest(&text)))) {
                    return Err(format!("{}: doesn't match the hash in its collection", meta.display()));
                }
                metas.push(meta);
            }
            metas
        }
        None => vec![p.to_path_buf()],
    };
    let mut read = Vec::new();
    let mut first = Value::Null;
    for m in &metas {
        let v = read_json(m)?;
        read.push(read_recording(m, &v)?);
        if first.is_null() {
            first = v;
        }
    }
    // Every radio from the moment the last of them started.
    let latest = read.iter().map(|r| r.1).fold(0.0, f64::max);
    let recordings: Vec<Recording> = read
        .into_iter()
        .map(|(mut r, start)| {
            if start > 0.0 {
                r.skip = ((latest - start) / 1000.0 * r.rate_hz).round() as u64;
                r.gaps.retain(|g| g.0 >= r.skip);
            }
            r
        })
        .collect();
    let g = &first["global"];
    let config = match g.get("trp:config") {
        Some(c) => {
            let cfg = Config::from_value(c.clone()).map_err(|e| format!("{path}: its trp:config: {e}"))?;
            if cfg.sources.len() != recordings.len() {
                return Err(format!("{path}: it was made with {} radios: replay the capture's folder", cfg.sources.len()));
            }
            Some(cfg)
        }
        None => None,
    };
    let mut names = NameLists::new();
    for (system, rows) in g["trp:talkgroups"].as_object().into_iter().flatten() {
        let talkgroups = serde_json::from_value(rows.clone()).map_err(|e| format!("{path}: its trp:talkgroups: {e}"))?;
        names.insert(system.clone(), SystemNames { talkgroups, ..Default::default() });
    }
    let bandplans = g["trp:bandplans"].as_array().into_iter().flatten().map(|b| b.as_str().unwrap_or("").to_string()).collect();
    Ok(Opened { recordings, config, config_json: g.get("trp:config").cloned(), names, bandplans })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret_config() -> Config {
        Config::from_value(json!({
            "sources": [{ "type": "usrp", "args": "addr=SECRET-ADDRESS,serial=SECRET-SERIAL", "gainDb": 30, "centerHz": 858000000, "rateHz": 8000000, "name": "SECRET-RADIO-NAME" }],
            "recording": { "captureDir": "/SECRET-PATH", "recordEncrypted": true, "vocoder": "mbelib", "normalizeAudio": false },
            "keys": { "a": { "file": "/SECRET-KEY-FILE" } },
            "systems": [{
                "shortName": "dcfd", "name": "SECRET-SYSTEM-TITLE", "type": "p25", "keys": "a", "receiveOn": ["USRP addr=SECRET-ADDRESS,serial=SECRET-SERIAL"],
                "plugins": { "openmhz": { "settings": { "apiKey": "SECRET-API-KEY" } } },
                "sites": [{ "controlChannelsHz": [857987500.0], "directoryId": "SECRET-DIRECTORY" }]
            }],
            "conventional": [{ "shortName": "fire", "channelFile": "/SECRET-CHANNELS.csv", "channels": [{ "freqHz": 155000000.0, "mode": "fm", "name": "SECRET-CHANNEL-NAME", "tag": "SECRET-TAG", "receiveOn": ["USRP addr=SECRET-ADDRESS,serial=SECRET-SERIAL"] }] }],
            "storage": { "backend": "postgres", "url": "postgres://SECRET-DB" }
        }))
        .unwrap()
    }

    fn written(format: SampleFormat) -> Written {
        Written { start_ms: 1_000_000.0, format: Some(format), samples: 100, gaps: vec![], sha512: "ab".into() }
    }

    #[test]
    fn nothing_private_reaches_the_metadata() {
        let cfg = secret_config();
        let c = decode_config(&cfg, &[written(SampleFormat::Cs16)]);
        let source = keep(&serde_json::to_value(&cfg.sources[0]).unwrap(), RADIO_KEYS);
        let text = format!("{c}{source}");
        assert!(!text.contains("SECRET"), "{text}");
        assert_eq!(c["systems"][0]["receiveOn"][0], "file source-0.sigmf-data", "pinned to its radio, by what the capture calls it");
        assert_eq!(source["gainDb"], 30.0, "how the radio was set is kept");
    }

    #[test]
    fn its_config_decodes_as_the_original_does() {
        let cfg = secret_config();
        let c = decode_config(&cfg, &[written(SampleFormat::Cs16)]);
        let back = Config::from_value(c).unwrap();
        assert!(matches!(&back.sources[0].kind, SourceKind::File { realtime: false, format: Some(SampleFormat::Cs16), .. }));
        let (a, b) = (cfg.engine_config(0.0, &Default::default()), back.engine_config(0.0, &Default::default()));
        assert_eq!(a.systems.len(), b.systems.len());
        assert_eq!(a.systems[0].sites[0].control_channels, b.systems[0].sites[0].control_channels);
        assert!(b.systems[0].calls.record_encrypted);
        assert_eq!(b.vocoder, trunk_core::mbe::Profile::Mbelib, "the vocoder decides the audio");
        assert!(!b.systems[0].save.normalize);
        assert!(b.systems[0].pins.allows(0), "still received on its radio");
        assert_eq!(a.sources[0].center_hz, b.sources[0].center_hz);
        assert_eq!(b.conventional.len(), 1);
    }

    #[test]
    fn talkgroups_keep_how_they_decode_and_no_names() {
        let mut names = NameLists::new();
        let row = |number, mode: &str, alpha: &str| trunk_core::trunk::TalkgroupRow { tg: trunk_core::trunk::Talkgroup { number, mode: mode.into(), alpha_tag: alpha.into(), ..Default::default() }, ..Default::default() };
        names.insert("x".into(), SystemNames { talkgroups: vec![row(1, "D", "SECRET-A"), row(2, "A", "SECRET-B")], ..Default::default() });
        let v = decode_talkgroups(&names);
        assert!(!v.to_string().contains("SECRET"));
        assert_eq!(v["x"].as_array().unwrap().len(), 1, "digital is the default: only the exception is kept");
        assert_eq!(v["x"][0]["number"], 2);
    }

    /// A folder of its own under the system's temp directory.
    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trp-iq-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_tap_stops_at_its_length_and_says_what_was_lost() {
        let (pool, tap) = (crate::sdr::Pool::<u8>::default(), Tap::default());
        let (tx, rx) = mpsc::sync_channel(16);
        let dir = temp("tap");
        let path = dir.join("x");
        let at = Instant::now();
        let failed = Arc::new(Mutex::new(None));
        tap.arm(Job { tx, left: 10, not_before: at, started: false, rate_hz: 1000.0, center_hz: 1e6, clock: (at, 5000.0), failed: failed.clone(), activity: Arc::default() });
        let block = |n: usize| Block::U8(pool.copy(&vec![7u8; n * 2]));
        tap.samples(&block(4), 0, at);
        tap.samples(&block(2), 3, at);
        assert!(tap.armed.load(Ordering::Relaxed));
        tap.samples(&block(4), 0, at);
        assert!(!tap.armed.load(Ordering::Relaxed), "ten samples' worth: done");
        tap.samples(&block(4), 0, at);
        let w = write_samples(rx, &path, &Activity::default()).unwrap();
        assert_eq!((w.samples, w.lost(), w.gaps.clone()), (7, 3, vec![(4, 3)]));
        assert_eq!(w.start_ms, 5000.0 - 4.0, "its first block began before it arrived");
        assert_eq!(fs::metadata(&path).unwrap().len(), 14);
        assert!(failed.lock().unwrap().is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn captures_go_where_the_config_says() {
        let mut cfg = Config::default();
        cfg.recording.capture_dir = "/recordings".into();
        assert_eq!(root(&cfg), Path::new("/recordings/iq-captures"));
        cfg.diagnostics.capture_dir = "  /fast/iq ".into();
        assert_eq!(root(&cfg), Path::new("/fast/iq"));
    }

    #[test]
    fn activity_says_what_happened_since_last_asked() {
        let a = Activity::default();
        a.queued();
        a.queued();
        a.taken();
        a.lost.fetch_add(7, Ordering::Relaxed);
        a.wrote(100, Duration::from_millis(5));
        a.wrote(50, Duration::from_millis(2));
        assert_eq!(a.take(), (7, 2, 5.0, 150));
        assert_eq!(a.take(), (0, 1, 0.0, 0), "still one in the queue");
    }

    #[test]
    fn a_full_queue_gives_up_without_waiting() {
        let (pool, tap) = (crate::sdr::Pool::<u8>::default(), Tap::default());
        let (tx, _rx) = mpsc::sync_channel(1);
        let at = Instant::now();
        let failed = Arc::new(Mutex::new(None));
        tap.arm(Job { tx, left: 100, not_before: at, started: false, rate_hz: 1000.0, center_hz: 1e6, clock: (at, 0.0), failed: failed.clone(), activity: Arc::default() });
        tap.samples(&Block::U8(pool.copy(&[1, 2])), 0, at);
        assert!(!tap.armed.load(Ordering::Relaxed));
        assert!(failed.lock().unwrap().as_ref().unwrap().contains("keep up"));
    }

    #[test]
    fn retuning_ends_a_capture_and_a_pause_is_a_gap() {
        let (tx, rx) = mpsc::sync_channel(4);
        let (pool, tap, at) = (crate::sdr::Pool::<u8>::default(), Tap::default(), Instant::now());
        let failed = Arc::new(Mutex::new(None));
        tap.arm(Job { tx, left: 100, not_before: at, started: false, rate_hz: 1000.0, center_hz: 1e6, clock: (at, 0.0), failed: failed.clone(), activity: Arc::default() });
        tap.samples(&Block::U8(pool.copy(&[1, 2])), 0, at);
        tap.tuned(1e6, 5);
        assert!(failed.lock().unwrap().is_none());
        tap.tuned(2e6, 0);
        assert!(failed.lock().unwrap().as_ref().unwrap().contains("retuned"));
        assert!(!tap.armed.load(Ordering::Relaxed));
        let w = write_samples(rx, &temp("retune").join("x"), &Activity::default()).unwrap();
        assert_eq!(w.lost(), 5);
    }

    #[test]
    fn a_recording_reads_back_with_its_gaps_and_alignment() {
        let dir = temp("read");
        let mut a = written(SampleFormat::Cu8);
        a.gaps = vec![(40, 5), (1500, 2)];
        let mut b = written(SampleFormat::Cs16);
        b.start_ms += 1000.0;
        let c = json!({ "trp:bandplans": ["plan"] });
        let source = json!({ "type": "rtlsdr", "autoTune": true, "guardHz": 50000.0 });
        for (name, w, hz) in [("source-0", &a, 100e6), ("source-1", &b, 200e6)] {
            let m = recording_meta(w, hz, 1000.0, source.clone(), "a note", &c);
            fs::write(dir.join(format!("{name}.sigmf-meta")), m.to_string()).unwrap();
        }
        let text = fs::read(dir.join("source-0.sigmf-meta")).unwrap();
        fs::write(dir.join(COLLECTION), json!({ "collection": { "core:streams": [{ "name": "source-0", "hash": crate::toolkit::hex(&Sha512::digest(&text)) }, { "name": "source-1" }] } }).to_string()).unwrap();
        let o = open(dir.to_str().unwrap()).unwrap();
        let (x, y) = (&o.recordings[0], &o.recordings[1]);
        assert_eq!((x.center_hz, y.center_hz, x.rate_hz), (100e6, 200e6, 1000.0));
        assert!(x.format == SampleFormat::Cu8 && y.format == SampleFormat::Cs16);
        assert_eq!((x.skip, y.skip), (1000, 0), "started a second before the other");
        assert!(x.auto_tune && x.guard_hz == 50000.0);
        assert_eq!(o.bandplans, ["plan"]);
        assert!(o.config.is_none());
        assert_eq!(x.gaps, vec![(1500, 2)], "a gap in what's left out doesn't count");

        fs::write(dir.join("source-0.sigmf-meta"), "{}").unwrap();
        assert!(open(dir.to_str().unwrap()).err().unwrap().contains("hash"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_sidecar_can_name_an_existing_file() {
        let dir = temp("ncd");
        let meta = dir.join("x.sigmf-meta");
        fs::write(&meta, json!({ "global": { "core:datatype": "cu8", "core:sample_rate": 2400000.0, "core:dataset": "p25.cu8" }, "captures": [{ "core:sample_start": 0, "core:frequency": 858300000.0 }] }).to_string()).unwrap();
        let o = open(meta.to_str().unwrap()).unwrap();
        assert_eq!(o.recordings[0].data, dir.join("p25.cu8"));
        assert_eq!(o.recordings[0].guard_hz, trunk_core::trunk::DEFAULT_GUARD_HZ);
        fs::remove_dir_all(&dir).unwrap();
    }
}
