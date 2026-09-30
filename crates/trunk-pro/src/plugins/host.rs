//! Running plugins: one process each, kept running (restarted with backoff
//! when one dies), fed the events it subscribed to.
//!
//! Nothing here blocks the engine: each event is serialized once and offered
//! to each subscriber's queue, and a plugin that falls behind loses events
//! (counted and reported) rather than holding up the recorder. Calls that
//! need M4A are encoded on worker threads first.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;
use trunk_app::PluginTopics;
use trunk_recorder_plugin::{
    format, topic, AudioChunk, CallFiles, CallRecord, ConcludedCall, Hello, HostInfo, HostMessage, Level, Manifest, Outcome, PluginMessage, Shutdown, State,
    SystemInfo, API_VERSION, EXIT_CONFIG,
};

use super::{describe, executable, Encoder, PluginsFile};

/// What the plugins are up to, for the interface (or the terminal).
#[derive(Clone, Debug)]
pub enum Note {
    Log { plugin: String, level: Level, text: String },
    State { plugin: String, state: State, message: String },
    Result { plugin: String, path: String, outcome: Outcome, message: String, url: String },
}

pub type Notes = Arc<dyn Fn(Note) + Send + Sync>;

/// Messages waiting for a plugin; beyond this it's behind, and loses them.
const QUEUE: usize = 1024;
/// Calls waiting to be encoded.
const ENCODE_QUEUE: usize = 256;

struct Plugin {
    id: String,
    manifest: Manifest,
    exe: PathBuf,
    data_dir: PathBuf,
    hello: Arc<str>,
    tx: Mutex<Option<SyncSender<Arc<str>>>>,
    child: Mutex<Option<Child>>,
    dropped: AtomicU64,
    last_drop_note: Mutex<Option<Instant>>,
}

struct Shared {
    plugins: Vec<Plugin>,
    stopping: AtomicBool,
    notes: Notes,
}

struct EncodeJob {
    call: ConcludedCall,
}

pub struct PluginHost {
    shared: Arc<Shared>,
    capture_dir: PathBuf,
    encoder: Option<(Encoder, u32)>,
    encode_tx: Option<SyncSender<EncodeJob>>,
    encoders: Vec<JoinHandle<()>>,
    supervisors: Vec<JoinHandle<()>>,
    /// What the session should build for plugins.
    pub topics: PluginTopics,
    /// Some plugin wants live audio.
    pub audio: bool,
    done: bool,
}

/// A plugin to run: what `plugins.json` says, resolved.
pub struct Spec {
    pub id: String,
    pub exe: PathBuf,
    pub config: Value,
    pub systems: BTreeMap<String, Value>,
    /// Its data folder (None: the usual, in the config folder).
    pub data_dir: Option<PathBuf>,
}

impl Spec {
    /// The enabled plugins of `file`.
    pub fn enabled(file: &PluginsFile) -> Vec<Spec> {
        file.plugins
            .iter()
            .filter(|(_, e)| e.enabled)
            .map(|(id, e)| Spec { id: id.clone(), exe: executable(id, e), config: e.config.clone(), systems: e.systems.clone(), data_dir: None })
            .collect()
    }
}

impl PluginHost {
    /// Start `specs`. `systems`: every system calls come from (their `config` is filled in here).
    /// A plugin that can't start is reported and left out.
    pub fn start(specs: Vec<Spec>, audio: &super::AudioSettings, systems: &[SystemInfo], capture_dir: &Path, notes: Notes) -> PluginHost {
        let mut ready = Vec::new();
        for mut s in specs {
            // (It runs in its data folder: a relative path would be from there.)
            s.exe = std::fs::canonicalize(&s.exe).unwrap_or(s.exe);
            match describe(&s.exe) {
                Ok(m) => {
                    if m.id != s.id {
                        notes(Note::Log { plugin: s.id.clone(), level: Level::Warn, text: format!("{} says it is \"{}\"", s.exe.display(), m.id) });
                    }
                    ready.push((s, m));
                }
                Err(e) => {
                    notes(Note::State { plugin: s.id.clone(), state: State::Error, message: e.clone() });
                    notes(Note::Log { plugin: s.id, level: Level::Error, text: e });
                }
            }
        }
        let wants_m4a = ready.iter().any(|(_, m)| m.subscribes(topic::CALL_CONCLUDED) && m.wants_format(format::M4A));
        let encoder = if wants_m4a { Encoder::find(&audio.encoder) } else { None };
        if wants_m4a && encoder.is_none() {
            let who: Vec<&str> = ready.iter().filter(|(_, m)| m.wants_format(format::M4A)).map(|(s, _)| s.id.as_str()).collect();
            let text = if audio.encoder == "none" {
                format!("M4A encoding is off: {} get WAV only", who.join(", "))
            } else {
                format!("No M4A encoder found (install ffmpeg): {} get WAV only", who.join(", "))
            };
            notes(Note::Log { plugin: String::new(), level: Level::Warn, text });
        }
        let mut topics = PluginTopics::default();
        let mut live_audio = false;
        let mut plugins = Vec::new();
        let mut queues = Vec::new();
        for (s, m) in ready {
            topics.calls |= m.subscribes(topic::CALL_START) || m.subscribes(topic::CALL_END);
            topics.units |= m.subscribes(topic::UNIT);
            topics.status |= m.subscribes(topic::STATUS);
            live_audio |= m.subscribes(topic::AUDIO);
            let data_dir = s.data_dir.clone().unwrap_or_else(|| super::data_dir(&s.id));
            let _ = std::fs::create_dir_all(&data_dir);
            let mut formats = vec!["wav".to_string()];
            if encoder.is_some() && m.wants_format(format::M4A) {
                formats.push(format::M4A.into());
            }
            let hello = Hello {
                api: API_VERSION,
                host: HostInfo { name: "trunk-pro".into(), version: env!("CARGO_PKG_VERSION").into() },
                config: s.config,
                systems: systems.iter().map(|y| SystemInfo { config: s.systems.get(&y.short_name).cloned().unwrap_or(Value::Null), ..y.clone() }).collect(),
                capture_dir: capture_dir.to_path_buf(),
                data_dir: data_dir.clone(),
                audio_formats: formats,
            };
            // The queue outlives the process: events wait through a (re)start.
            let (tx, rx) = mpsc::sync_channel::<Arc<str>>(QUEUE);
            queues.push(rx);
            plugins.push(Plugin {
                id: s.id,
                manifest: m,
                exe: s.exe,
                data_dir,
                hello: line(&HostMessage::Hello(hello)),
                tx: Mutex::new(Some(tx)),
                child: Mutex::new(None),
                dropped: AtomicU64::new(0),
                last_drop_note: Mutex::new(None),
            });
        }
        let shared = Arc::new(Shared { plugins, stopping: AtomicBool::new(false), notes });
        let supervisors = queues
            .into_iter()
            .enumerate()
            .map(|(i, rx)| {
                let sh = shared.clone();
                std::thread::Builder::new().name(format!("plugin-{}", sh.plugins[i].id)).spawn(move || supervise(&sh, i, rx)).expect("thread")
            })
            .collect();
        let encoder = encoder.map(|e| (e, audio.bitrate_kbps.clamp(8, 320)));
        let (encode_tx, encoders) = match &encoder {
            Some((e, kbps)) => {
                let (tx, rx) = mpsc::sync_channel::<EncodeJob>(ENCODE_QUEUE);
                let rx = Arc::new(Mutex::new(rx));
                let n = std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 3);
                let workers = (0..n)
                    .map(|i| {
                        let (rx, sh, e, kbps) = (rx.clone(), shared.clone(), e.clone(), *kbps);
                        std::thread::Builder::new().name(format!("encode-{i}")).spawn(move || encode_worker(&rx, &sh, &e, kbps)).expect("thread")
                    })
                    .collect();
                (Some(tx), workers)
            }
            None => (None, Vec::new()),
        };
        PluginHost { shared, capture_dir: capture_dir.to_path_buf(), encoder, encode_tx, encoders, supervisors, topics, audio: live_audio, done: false }
    }

    pub fn is_empty(&self) -> bool {
        self.shared.plugins.is_empty()
    }

    /// The M4A encoder in use (its name).
    pub fn encoder(&self) -> Option<&'static str> {
        self.encoder.as_ref().map(|(e, _)| e.name())
    }

    /// An event from the session.
    pub fn event(&self, m: &HostMessage) {
        let t = match m {
            HostMessage::CallStart(_) => topic::CALL_START,
            HostMessage::CallEnd(_) => topic::CALL_END,
            HostMessage::Unit(_) => topic::UNIT,
            HostMessage::Status(_) => topic::STATUS,
            HostMessage::Audio(_) => topic::AUDIO,
            HostMessage::CallConcluded(_) => topic::CALL_CONCLUDED,
            _ => return,
        };
        if self.shared.plugins.iter().any(|p| p.manifest.subscribes(t)) {
            self.shared.dispatch(t, &line(m));
        }
    }

    /// A live audio frame (the session's `[2][u16 system][u32 call][u32 tg][i16…]`).
    pub fn audio_frame(&self, frame: &[u8]) {
        if !self.audio || frame.len() < 11 {
            return;
        }
        let system = u16::from_le_bytes([frame[1], frame[2]]);
        let call_id = u32::from_le_bytes([frame[3], frame[4], frame[5], frame[6]]);
        let talkgroup = u32::from_le_bytes([frame[7], frame[8], frame[9], frame[10]]);
        let pcm = trunk_recorder_plugin::base64::encode(&frame[11..]);
        let chunk = AudioChunk { call_id, system, talkgroup, sample_rate: 8000, pcm };
        self.shared.dispatch(topic::AUDIO, &line(&HostMessage::Audio(chunk)));
    }

    /// A call whose files are written (`rel`: relative to the capture folder,
    /// no extension; `json`: its call JSON).
    pub fn concluded(&self, system: u16, rel: &str, json: &str) {
        if !self.shared.plugins.iter().any(|p| p.manifest.subscribes(topic::CALL_CONCLUDED)) {
            return;
        }
        let base = self.capture_dir.join(rel);
        let record: CallRecord = serde_json::from_str(json).unwrap_or_default();
        let call = ConcludedCall {
            path: rel.to_string(),
            system,
            call: record,
            files: CallFiles { json: with_ext(&base, "json"), wav: with_ext(&base, "wav"), m4a: None },
        };
        match &self.encode_tx {
            Some(tx) => {
                if let Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) = tx.try_send(EncodeJob { call }) {
                    self.shared.note_log("", Level::Warn, "M4A encoding is behind: a call goes out as WAV only");
                    self.shared.dispatch(topic::CALL_CONCLUDED, &line(&HostMessage::CallConcluded(job.call)));
                }
            }
            None => self.shared.dispatch(topic::CALL_CONCLUDED, &line(&HostMessage::CallConcluded(call))),
        }
    }

    /// Finish encoding, tell every plugin to stop, and give them `grace` to exit.
    pub fn shutdown(&mut self, grace: Duration) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        drop(self.encode_tx.take());
        for w in self.encoders.drain(..) {
            let _ = w.join();
        }
        let sh = &self.shared;
        sh.stopping.store(true, Ordering::Relaxed);
        let bye = line(&HostMessage::Shutdown(Shutdown { grace_s: grace.as_secs_f64() }));
        for p in &sh.plugins {
            // Queued behind whatever's still waiting; then stdin closes (which
            // means the same, if the queue is full).
            if let Some(tx) = p.tx.lock().unwrap().take() {
                let _ = tx.try_send(bye.clone());
            }
        }
        let deadline = Instant::now() + grace;
        while self.supervisors.iter().any(|t| !t.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        for p in &sh.plugins {
            if let Some(c) = p.child.lock().unwrap().as_mut() {
                if c.try_wait().ok().flatten().is_none() {
                    sh.note_log(&p.id, Level::Warn, "didn't stop in time: killed");
                    let _ = c.kill();
                }
            }
        }
        for t in self.supervisors.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.shutdown(Duration::from_secs(2));
    }
}

fn with_ext(base: &Path, ext: &str) -> PathBuf {
    PathBuf::from(format!("{}.{ext}", base.display()))
}

fn line(m: &HostMessage) -> Arc<str> {
    let mut s = serde_json::to_string(m).unwrap_or_default();
    s.push('\n');
    s.into()
}

impl Shared {
    fn note_log(&self, plugin: &str, level: Level, text: impl Into<String>) {
        (self.notes)(Note::Log { plugin: plugin.to_string(), level, text: text.into() });
    }

    fn dispatch(&self, t: &str, l: &Arc<str>) {
        for p in self.plugins.iter().filter(|p| p.manifest.subscribes(t)) {
            let Some(tx) = p.tx.lock().unwrap().clone() else {
                continue;
            };
            if let Err(TrySendError::Full(_)) = tx.try_send(l.clone()) {
                let n = p.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                let mut last = p.last_drop_note.lock().unwrap();
                if last.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
                    *last = Some(Instant::now());
                    self.note_log(&p.id, Level::Warn, format!("is falling behind: {n} event(s) dropped so far"));
                }
            }
        }
    }
}

fn encode_worker(rx: &Mutex<Receiver<EncodeJob>>, sh: &Shared, e: &Encoder, kbps: u32) {
    let mut last_error: Option<Instant> = None;
    loop {
        let job = rx.lock().unwrap().recv();
        let Ok(EncodeJob { mut call }) = job else {
            return;
        };
        let out = call.files.wav.with_extension("m4a");
        match e.m4a(&call.files.wav, &out, kbps) {
            Ok(()) => call.files.m4a = Some(out),
            Err(err) => {
                if last_error.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
                    last_error = Some(Instant::now());
                    sh.note_log("", Level::Warn, format!("M4A encoding failed ({err}); plugins get WAV"));
                }
            }
        }
        sh.dispatch(topic::CALL_CONCLUDED, &line(&HostMessage::CallConcluded(call)));
    }
}

/// Keep plugin `i` running until the host stops, feeding it from `rx`.
fn supervise(sh: &Shared, i: usize, mut rx: Receiver<Arc<str>>) {
    let p = &sh.plugins[i];
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        let (exit, back) = run_once(sh, p, rx);
        rx = back;
        let stopping = sh.stopping.load(Ordering::Relaxed);
        match exit {
            _ if stopping => return,
            // Everything's been sent and the host is stopping.
            Ok(Ended::Drained) => return,
            Err(e) => sh.note_log(&p.id, Level::Error, e),
            Ok(Ended::Exited(Some(EXIT_CONFIG))) => {
                // It said why (in its status); retrying won't help until the next run.
                drop(p.tx.lock().unwrap().take());
                return;
            }
            Ok(Ended::Exited(code)) => {
                let how = code.map_or("was killed".to_string(), |c| format!("exited ({c})"));
                sh.note_log(&p.id, Level::Error, format!("{how}; restarting in {} s", backoff.as_secs()));
                (sh.notes)(Note::State { plugin: p.id.clone(), state: State::Error, message: format!("{how}, restarting") });
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        let until = Instant::now() + backoff;
        while Instant::now() < until && !sh.stopping.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
        if sh.stopping.load(Ordering::Relaxed) {
            return;
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

enum Ended {
    /// The process exited (its code; None: killed).
    Exited(Option<i32>),
    /// The queue was closed and emptied: the host is stopping.
    Drained,
}

/// One run of a plugin's process. Gives the queue back.
fn run_once(sh: &Shared, p: &Plugin, rx: Receiver<Arc<str>>) -> (Result<Ended, String>, Receiver<Arc<str>>) {
    let spawned = Command::new(&p.exe).current_dir(&p.data_dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return (Err(format!("couldn't start {}: {e}", p.exe.display())), rx),
    };
    let (mut stdin, stdout, stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
    *p.child.lock().unwrap() = Some(child);
    let dead = Arc::new(AtomicBool::new(false));
    let hello = p.hello.clone();
    let dead2 = dead.clone();
    let writer = std::thread::spawn(move || {
        let mut ok = stdin.write_all(hello.as_bytes()).and_then(|_| stdin.flush()).is_ok();
        let mut drained = false;
        while ok && !dead2.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(l) => ok = stdin.write_all(l.as_bytes()).and_then(|_| stdin.flush()).is_ok(),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    drained = true;
                    break;
                }
            }
        }
        // (stdin closes here.)
        (rx, drained)
    });
    let (notes, id) = (sh.notes.clone(), p.id.clone());
    let err_reader = std::thread::spawn(move || {
        for l in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !l.trim().is_empty() {
                notes(Note::Log { plugin: id.clone(), level: Level::Info, text: l });
            }
        }
    });
    for l in BufReader::new(stdout).lines().map_while(Result::ok) {
        if l.trim().is_empty() {
            continue;
        }
        let note = match serde_json::from_str::<PluginMessage>(&l) {
            Ok(PluginMessage::Ready) => Note::State { plugin: p.id.clone(), state: State::Ok, message: "running".into() },
            Ok(PluginMessage::Log { level, message }) => Note::Log { plugin: p.id.clone(), level, text: message },
            Ok(PluginMessage::Status { state, message }) => Note::State { plugin: p.id.clone(), state, message },
            Ok(PluginMessage::CallResult { path, outcome, message, url }) => Note::Result { plugin: p.id.clone(), path, outcome, message, url },
            Ok(PluginMessage::Unknown) => continue,
            // Not the protocol (a stray print): keep it as a log line.
            Err(_) => Note::Log { plugin: p.id.clone(), level: Level::Info, text: l },
        };
        (sh.notes)(note);
    }
    // Its stdout closed: it's exiting (or has).
    dead.store(true, Ordering::Relaxed);
    let status = loop {
        let mut c = p.child.lock().unwrap();
        match c.as_mut().map(|c| c.try_wait()) {
            Some(Ok(Some(s))) => {
                *c = None;
                break s.code();
            }
            Some(Ok(None)) => {}
            _ => {
                *c = None;
                break None;
            }
        }
        drop(c);
        std::thread::sleep(Duration::from_millis(20));
    };
    let (rx, drained) = writer.join().expect("writer");
    let _ = err_reader.join();
    (Ok(if drained && status == Some(0) { Ended::Drained } else { Ended::Exited(status) }), rx)
}
