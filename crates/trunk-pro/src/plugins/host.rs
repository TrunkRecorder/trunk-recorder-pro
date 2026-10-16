//! Running plugins: one process each, kept running (restarted with backoff
//! when one dies), fed the events it subscribed to.
//!
//! Nothing here blocks the engine: each event is serialized once and offered
//! to each subscriber's queue, and a plugin that falls behind loses events
//! (counted and reported) rather than holding up the recorder. Calls that
//! need M4A are encoded on worker threads first. What a plugin is to have
//! late (its delays) goes to the [`Hold`] instead, which gives it to the
//! plugin when it's due.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
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
    format, topic, AudioChunk, CallFiles, CallRecord, ConcludedCall, Hello, HostInfo, HostMessage, Level, Manifest, Metrics, Outcome, PluginMessage, Shutdown, State,
    SystemInfo, API_VERSION, EXIT_CONFIG,
};

use super::delay::About;
use super::hold::{Hold, Offer, Outlet};
use super::{describe, executable, Encoder};

/// What the plugins are up to, for the interface (or the terminal).
#[derive(Clone, Debug)]
pub enum Note {
    Log { plugin: String, level: Level, text: String },
    State { plugin: String, state: State, message: String },
    Result { plugin: String, path: String, outcome: Outcome, message: String, url: String },
    /// Its figures (queue, timing, the services it talks to).
    Metrics { plugin: String, metrics: Metrics },
}

/// A plugin's process as the host sees it.
#[derive(Clone, Debug, Default)]
pub struct ProcessStats {
    pub id: String,
    /// Events it was too far behind to be given.
    pub dropped: u64,
    /// Times it was started again after exiting.
    pub restarts: u64,
    /// Since it was last started, s (None: not running).
    pub uptime_s: Option<f64>,
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
    restarts: AtomicU64,
    started: Mutex<Option<Instant>>,
    /// When everything queued for it, `shutdown` last, was written to it.
    drained_at: Arc<Mutex<Option<Instant>>>,
    /// It may be given calls decrypted with our keys (`shareDecrypted`); whether it was said when one was withheld.
    share_decrypted: bool,
    withheld_said: AtomicBool,
    /// Whether it was said that it isn't streamed the live audio of calls it's delayed for.
    audio_held_said: AtomicBool,
}

struct Shared {
    plugins: Vec<Plugin>,
    stopping: AtomicBool,
    /// Plugins may start (false: wait for the ones they replace to exit).
    go: Arc<AtomicBool>,
    notes: Notes,
    /// Where what's delayed waits (None: nothing is).
    hold: Option<Arc<Hold>>,
}

/// How long `shutdown` waits for a plugin to read what was queued before
/// `shutdown` (its grace counts from then).
const DRAIN_MAX: Duration = Duration::from_secs(30);

struct EncodeJob {
    call: ConcludedCall,
    audio: CallAudio,
    about: About,
}

/// A concluded call's audio, for the plugins.
pub struct CallAudio {
    /// Its WAV, to encode from (and to write, should that fail).
    pub wav: Arc<Vec<u8>>,
    /// The WAV is at `files.wav` (otherwise only in `wav`).
    pub wav_written: bool,
    /// Where its files are: the .json, the .wav (written or not), the .m4a once made.
    pub files: CallFiles,
    /// Where an .m4a for the plugins goes.
    pub m4a_to: PathBuf,
}

impl CallAudio {
    /// A call already on disk at `base` (its path without the extension).
    pub fn on_disk(base: &Path) -> CallAudio {
        let wav = with_ext(base, "wav");
        let m4a = Some(with_ext(base, "m4a")).filter(|p| p.exists());
        CallAudio {
            wav: Arc::new(std::fs::read(&wav).unwrap_or_default()),
            wav_written: true,
            files: CallFiles { json: with_ext(base, "json"), wav, m4a },
            m4a_to: with_ext(base, "m4a"),
        }
    }

    /// Write the WAV, when it wasn't (the plugins get that instead of an .m4a).
    fn write_wav(&mut self) {
        if !self.wav_written {
            self.wav_written = std::fs::write(&self.files.wav, &*self.wav).is_ok();
        }
    }
}

pub struct PluginHost {
    shared: Arc<Shared>,
    encoder: Option<(Encoder, u32)>,
    encode_tx: Option<SyncSender<EncodeJob>>,
    encoders: Vec<JoinHandle<()>>,
    supervisors: Vec<JoinHandle<()>>,
    /// What the session should build for plugins.
    pub topics: PluginTopics,
    /// Some plugin wants live audio.
    pub audio: bool,
    /// Each system's short name, by its number (for audio chunks).
    names: BTreeMap<u16, String>,
    done: bool,
}

/// A plugin to run: what the config says of it (its `plugins` entry and each system's settings), resolved.
pub struct Spec {
    pub id: String,
    /// Which copy of it (0: the first; see [`super::copies`]).
    pub copy: usize,
    pub exe: PathBuf,
    pub config: Value,
    /// Its settings for each system, by short name; [`OVERRIDE`] in one
    /// overrides some of `config` for that system.
    pub systems: BTreeMap<String, Value>,
    /// Its data folder (None: the usual, in the config folder).
    pub data_dir: Option<PathBuf>,
    /// It may be given calls decrypted with our keys (`shareDecrypted`, when a system has keys).
    pub share_decrypted: bool,
}

impl Spec {
    /// The config's enabled plugins, each copy of each.
    pub fn enabled(cfg: &crate::config::Config) -> Vec<Spec> {
        cfg.plugins
            .iter()
            .filter(|(_, p)| p.enabled)
            .flat_map(|(id, p)| {
                (0..super::copies(cfg, id)).map(move |copy| Spec {
                    id: id.clone(),
                    copy,
                    exe: executable(id, p),
                    config: p.settings.clone(),
                    systems: super::system_settings(cfg, id, copy),
                    data_dir: None,
                    share_decrypted: p.share_decrypted && cfg.has_keys(),
                })
            })
            .collect()
    }
}

impl PluginHost {
    /// Start `specs`. `systems`: every system calls come from (their `config` is filled in here).
    /// A plugin that can't start is reported and left out.
    pub fn start(specs: Vec<Spec>, audio: &super::AudioSettings, systems: &[SystemInfo], capture_dir: &Path, notes: Notes) -> PluginHost {
        Self::start_when(specs, audio, systems, capture_dir, notes, Arc::new(AtomicBool::new(true)), None)
    }

    /// `start`, with the processes held back until `go` is set (events queue
    /// meanwhile): the plugins they replace share their data folders. What
    /// a plugin is to have late goes to `hold` (None: everything goes now).
    pub fn start_when(
        specs: Vec<Spec>,
        audio: &super::AudioSettings,
        systems: &[SystemInfo],
        capture_dir: &Path,
        notes: Notes,
        go: Arc<AtomicBool>,
        hold: Option<Arc<Hold>>,
    ) -> PluginHost {
        let mut ready = Vec::new();
        for mut s in specs {
            // (It runs in its data folder: a relative path would be from there.)
            s.exe = std::fs::canonicalize(&s.exe).unwrap_or(s.exe);
            match describe(&s.exe) {
                Ok(m) => {
                    if m.id != s.id && s.copy == 0 {
                        notes(Note::Log { plugin: s.id.clone(), level: Level::Warn, text: format!("{} says it is \"{}\"", s.exe.display(), m.id) });
                    }
                    // From here on a copy goes by its label: its notes, its data folder.
                    s.id = super::copy_label(&s.id, s.copy);
                    // Said every time it starts.
                    if s.share_decrypted {
                        notes(Note::Log { plugin: s.id.clone(), level: Level::Warn, text: "is given calls decrypted with your keys (shareDecrypted)".into() });
                    }
                    // Each system's settings, and the plugin's as they are for it.
                    let mut settings = BTreeMap::new();
                    // Required settings left empty, and for which systems ("": for none).
                    let mut unset: Vec<(String, Vec<String>)> = Vec::new();
                    let mut note_unset = |field: String, system: &str| match unset.iter_mut().find(|(f, _)| *f == field) {
                        Some((_, on)) => on.push(system.to_string()),
                        None => unset.push((field, vec![system.to_string()])),
                    };
                    let mut systems = BTreeMap::new();
                    for (name, entry) in std::mem::take(&mut s.systems) {
                        let (own, merged, unknown) = resolve(m.config.as_ref(), &s.config, entry);
                        for k in unknown {
                            notes(Note::Log { plugin: s.id.clone(), level: Level::Warn, text: format!("{name}: \"{k}\" isn't one of its settings, so it can't be overridden; ignored") });
                        }
                        for f in unset_required(m.config.as_ref(), merged.as_ref().unwrap_or(&s.config)) {
                            note_unset(f, &name);
                        }
                        if let Some(v) = merged {
                            settings.insert(name.clone(), v);
                        }
                        systems.insert(name, own);
                    }
                    s.systems = systems;
                    // (Used by no system: its own settings decide.)
                    if s.systems.is_empty() {
                        unset_required(m.config.as_ref(), &s.config).into_iter().for_each(|f| note_unset(f, ""));
                    }
                    // Not set up yet (the interface says so too): it would only fail.
                    if !unset.is_empty() {
                        let what: Vec<String> = unset
                            .iter()
                            .map(|(f, on)| if on.iter().all(|x| x.is_empty()) { f.clone() } else { format!("{f} (for {})", on.join(", ")) })
                            .collect();
                        let text = format!("not started: {} {} set", what.join(", "), if unset.len() == 1 { "isn't" } else { "aren't" });
                        notes(Note::State { plugin: s.id.clone(), state: State::Warning, message: text.clone() });
                        notes(Note::Log { plugin: s.id, level: Level::Warn, text });
                        continue;
                    }
                    ready.push((s, m, settings));
                }
                Err(e) => {
                    notes(Note::State { plugin: s.id.clone(), state: State::Error, message: e.clone() });
                    notes(Note::Log { plugin: s.id, level: Level::Error, text: e });
                }
            }
        }
        let wants_m4a = ready.iter().any(|(_, m, _)| m.subscribes(topic::CALL_CONCLUDED) && m.wants_format(format::M4A));
        let encoder = if wants_m4a { Encoder::find(&audio.encoder) } else { None };
        if wants_m4a && encoder.is_none() {
            let who: Vec<&str> = ready.iter().filter(|(_, m, _)| m.wants_format(format::M4A)).map(|(s, _, _)| s.id.as_str()).collect();
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
        for (s, m, settings) in ready {
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
                systems: systems
                    .iter()
                    .map(|y| SystemInfo {
                        config: s.systems.get(&y.short_name).cloned().unwrap_or(Value::Null),
                        settings: settings.get(&y.short_name).cloned().unwrap_or(Value::Null),
                        ..y.clone()
                    })
                    .collect(),
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
                restarts: AtomicU64::new(0),
                started: Mutex::new(None),
                drained_at: Arc::new(Mutex::new(None)),
                share_decrypted: s.share_decrypted,
                withheld_said: AtomicBool::new(false),
                audio_held_said: AtomicBool::new(false),
            });
        }
        let shared = Arc::new(Shared { plugins, stopping: AtomicBool::new(false), go, notes, hold });
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
        let names = systems.iter().map(|y| (y.index, y.short_name.clone())).collect();
        PluginHost { shared, encoder, encode_tx, encoders, supervisors, topics, audio: live_audio, names, done: false }
    }

    /// Each plugin's process: drops, restarts, uptime.
    pub fn process_stats(&self) -> Vec<ProcessStats> {
        self.shared
            .plugins
            .iter()
            .map(|p| ProcessStats {
                id: p.id.clone(),
                dropped: p.dropped.load(Ordering::Relaxed),
                restarts: p.restarts.load(Ordering::Relaxed),
                uptime_s: p.started.lock().unwrap().map(|t| t.elapsed().as_secs_f64()),
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.shared.plugins.is_empty()
    }

    /// Its plugins, for the hold to give what's due to.
    pub fn outlet(&self) -> Arc<dyn Outlet> {
        self.shared.clone()
    }

    /// The M4A encoder in use (its name).
    pub fn encoder(&self) -> Option<&'static str> {
        self.encoder.as_ref().map(|(e, _)| e.name())
    }

    /// An event from the session.
    pub fn event(&self, m: &HostMessage) {
        let on = |system: &str, talkgroup: Option<u32>| About { system: system.to_string(), talkgroup, decrypted: false };
        let (t, about) = match m {
            HostMessage::CallStart(c) => (topic::CALL_START, on(&c.short_name, Some(c.talkgroup))),
            HostMessage::CallEnd(c) => (topic::CALL_END, on(&c.short_name, Some(c.talkgroup))),
            HostMessage::Unit(u) => (topic::UNIT, on(&u.short_name, u.talkgroup)),
            // (No one system's: never delayed.)
            HostMessage::Status(_) => (topic::STATUS, About::default()),
            // (Audio and concluded calls may be decrypted ones: they go only by `audio_frame` and
            // `concluded`, which know.)
            _ => return,
        };
        if self.shared.plugins.iter().any(|p| p.manifest.subscribes(t)) {
            self.shared.dispatch(t, &line(m), &about, None);
        }
    }

    /// A live audio frame (the session's `[2][u16 system][u32 call][u32 tg][i16…]`).
    /// `decrypted`: of a call decrypted with our keys (only plugins with `shareDecrypted` are given it).
    pub fn audio_frame(&self, frame: &[u8], decrypted: bool) {
        if !self.audio || frame.len() < 11 {
            return;
        }
        let system = u16::from_le_bytes([frame[1], frame[2]]);
        let call_id = u32::from_le_bytes([frame[3], frame[4], frame[5], frame[6]]);
        let talkgroup = u32::from_le_bytes([frame[7], frame[8], frame[9], frame[10]]);
        let pcm = trunk_recorder_plugin::base64::encode(&frame[11..]);
        let short_name = self.names.get(&system).cloned().unwrap_or_default();
        let about = About { system: short_name.clone(), talkgroup: Some(talkgroup), decrypted };
        let chunk = AudioChunk { call_id, system, short_name, talkgroup, sample_rate: 8000, pcm };
        self.shared.dispatch(topic::AUDIO, &line(&HostMessage::Audio(chunk)), &about, None);
    }

    /// The plugins that take concluded calls (each reports on every one);
    /// not one that gave up (a config error).
    /// (`decrypted`: a call decrypted with our keys, which only plugins with `shareDecrypted` take.)
    pub fn call_takers(&self, decrypted: bool) -> usize {
        self.shared.plugins.iter().filter(|p| p.manifest.subscribes(topic::CALL_CONCLUDED) && p.tx.lock().unwrap().is_some() && (!decrypted || p.share_decrypted)).count()
    }

    /// Some plugin takes the WAV file: one that doesn't ask for M4A, does
    /// with no encoder to make it, or asks for the WAV too. (Otherwise the
    /// WAV needn't be written.)
    pub fn needs_wav(&self) -> bool {
        self.shared.plugins.iter().any(|p| p.manifest.subscribes(topic::CALL_CONCLUDED) && (self.encoder.is_none() || takes_wav(&p.manifest)))
    }

    /// A concluded call (`rel`: relative to the capture folder, no extension;
    /// `json`: its call JSON) whose JSON is written, and its audio as
    /// `audio` says. An .m4a is made here when there isn't one and a plugin
    /// wants it.
    /// `decrypted`: some of it was decrypted with our keys.
    pub fn concluded(&self, system: u16, rel: &str, json: &str, mut audio: CallAudio, decrypted: bool) {
        let record: CallRecord = serde_json::from_str(json).unwrap_or_default();
        if self.call_takers(decrypted) == 0 {
            if decrypted {
                self.shared.say_all_withheld(topic::CALL_CONCLUDED);
            }
            return;
        }
        let call = |files: &CallFiles| ConcludedCall { path: rel.to_string(), system, call: record.clone(), files: files.clone() };
        let short_name = self.names.get(&system).cloned().unwrap_or_else(|| record.short_name.clone());
        let about = About { system: short_name, talkgroup: Some(record.talkgroup), decrypted };
        match &self.encode_tx {
            Some(tx) if audio.files.m4a.is_none() => {
                if let Err(TrySendError::Full(mut job) | TrySendError::Disconnected(mut job)) = tx.try_send(EncodeJob { call: call(&audio.files), audio, about }) {
                    self.shared.note_log("", Level::Warn, "M4A encoding is behind: a call goes out as WAV only");
                    job.audio.write_wav();
                    self.shared.dispatch(topic::CALL_CONCLUDED, &line(&HostMessage::CallConcluded(job.call)), &job.about, Some(rel));
                }
            }
            _ => {
                if audio.files.m4a.is_none() {
                    audio.write_wav();
                }
                self.shared.dispatch(topic::CALL_CONCLUDED, &line(&HostMessage::CallConcluded(call(&audio.files))), &about, Some(rel));
            }
        }
    }

    /// Finish encoding, tell every plugin to stop, and give each `grace` to
    /// exit from when it has read `shutdown` (behind what was queued, for up
    /// to `DRAIN_MAX`).
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
        let asked = Instant::now();
        let mut killed = vec![false; sh.plugins.len()];
        while self.supervisors.iter().any(|t| !t.is_finished()) {
            let now = Instant::now();
            for (i, p) in sh.plugins.iter().enumerate() {
                let drained = *p.drained_at.lock().unwrap();
                let deadline = drained.map_or(asked + DRAIN_MAX + grace, |t| t + grace);
                if killed[i] || self.supervisors[i].is_finished() || now < deadline {
                    continue;
                }
                killed[i] = true;
                if let Some(c) = p.child.lock().unwrap().as_mut() {
                    if c.try_wait().ok().flatten().is_none() {
                        sh.note_log(&p.id, Level::Warn, "didn't stop in time: killed");
                        let _ = c.kill();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
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

/// The fields `schema` (a plugin's settings) requires — its `required`
/// list, or `"x-required": true` — that `values` leaves empty, by title, as
/// the interface's "Needs setting up" finds them. Per-system settings don't
/// count: a plugin skips a system that isn't set up.
/// The key in a system's settings for a plugin that holds the plugin's own
/// settings it overrides: `{ "apiKey": "…", "override": { "server": "…" } }`.
pub const OVERRIDE: &str = "override";

/// A system's settings for a plugin (`entry`): its own (without
/// [`OVERRIDE`]); the plugin's (`config`) with what it overrides (None: it
/// overrides none); and the keys it overrides that the plugin's `schema`
/// has no setting for (left out).
fn resolve(schema: Option<&Value>, config: &Value, mut entry: Value) -> (Value, Option<Value>, Vec<String>) {
    let over = match entry.as_object_mut().and_then(|o| o.remove(OVERRIDE)) {
        Some(Value::Object(o)) if !o.is_empty() => o,
        _ => return (entry, None, Vec::new()),
    };
    let props = schema.and_then(|s| s.get("properties")).and_then(Value::as_object);
    let mut merged = config.as_object().cloned().unwrap_or_default();
    let mut unknown = Vec::new();
    for (k, v) in over {
        if !props.is_some_and(|p| p.contains_key(&k)) {
            unknown.push(k);
        } else if !v.is_null() {
            merged.insert(k, v);
        }
    }
    (entry, Some(Value::Object(merged)), unknown)
}

fn unset_required(schema: Option<&Value>, values: &Value) -> Vec<String> {
    let Some(schema) = schema else { return Vec::new() };
    let props = schema.get("properties").and_then(Value::as_object);
    let mut keys: Vec<&str> = schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    for (k, f) in props.into_iter().flatten() {
        if f.get("x-required") == Some(&Value::Bool(true)) && !keys.contains(&k.as_str()) {
            keys.push(k);
        }
    }
    let empty = |v: Option<&Value>| match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    };
    keys.into_iter()
        .filter(|k| empty(values.get(*k)))
        .map(|k| props.and_then(|p| p.get(k)?.get("title")?.as_str()).unwrap_or(k).to_string())
        .collect()
}

/// A plugin uses the WAV file even with an .m4a made: it doesn't ask for
/// M4A, or asks for the WAV too. upload-script hands the WAV to the script
/// first, as Trunk Recorder does, but its manifest (0.1.1) predates
/// [`format::WAV`].
fn takes_wav(m: &Manifest) -> bool {
    !m.wants_format(format::M4A) || m.wants_format(format::WAV) || m.id == "upload-script"
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

    /// Say, once a run, that `p` is not given decrypted calls, and how to allow it.
    fn say_withheld(&self, p: &Plugin) {
        if !p.share_decrypted && !p.withheld_said.swap(true, Ordering::Relaxed) {
            self.note_log(&p.id, Level::Info, "is not given calls decrypted with your keys (Share decrypted calls, on its card in Setup → Plugins, allows it)");
        }
    }

    /// A decrypted call came and no plugin that takes calls may have it: say so for each (they are
    /// otherwise passed over without a word, as nothing is sent to them at all).
    fn say_all_withheld(&self, t: &str) {
        for p in self.plugins.iter().filter(|p| p.manifest.subscribes(t)) {
            self.say_withheld(p);
        }
    }

    /// Send `l` (of topic `t`) to each plugin that subscribed to it, now or,
    /// when its delays say so, later (by the hold). `about`: what it's about —
    /// a call decrypted with our keys (or its audio) goes only to plugins with
    /// `shareDecrypted`; `call`: it's that finished call.
    fn dispatch(&self, t: &str, l: &Arc<str>, about: &About, call: Option<&str>) {
        for p in self.plugins.iter().filter(|p| p.manifest.subscribes(t)) {
            if about.decrypted && !p.share_decrypted {
                self.say_withheld(p);
                continue;
            }
            if let Some(h) = self.hold.as_ref().filter(|h| !h.delay(&p.id, about).is_zero()) {
                // Live audio late isn't live: a delayed call's isn't streamed at all.
                if t == topic::AUDIO {
                    if !p.audio_held_said.swap(true, Ordering::Relaxed) {
                        self.note_log(&p.id, Level::Info, "isn't given the live audio of calls it's delayed for");
                    }
                } else if !h.push(&p.id, about, l, call) {
                    self.dropped(p);
                }
                continue;
            }
            if self.give(p, l) == Offer::Full {
                self.dropped(p);
            }
        }
    }

    /// Put `l` in `p`'s queue.
    fn give(&self, p: &Plugin, l: &Arc<str>) -> Offer {
        let Some(tx) = p.tx.lock().unwrap().clone() else {
            return Offer::Gone;
        };
        match tx.try_send(l.clone()) {
            Ok(()) => Offer::Sent,
            Err(TrySendError::Full(_)) => Offer::Full,
            Err(TrySendError::Disconnected(_)) => Offer::Gone,
        }
    }

    /// `p` lost an event: it's too far behind.
    fn dropped(&self, p: &Plugin) {
        let n = p.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        let mut last = p.last_drop_note.lock().unwrap();
        if last.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            *last = Some(Instant::now());
            self.note_log(&p.id, Level::Warn, format!("is falling behind: {n} event(s) dropped so far"));
        }
    }
}

impl Outlet for Shared {
    fn offer(&self, label: &str, line: &Arc<str>, decrypted: bool) -> Offer {
        match self.plugins.iter().find(|p| p.id == label) {
            Some(p) if !decrypted || p.share_decrypted => self.give(p, line),
            _ => Offer::Gone,
        }
    }
}

fn encode_worker(rx: &Mutex<Receiver<EncodeJob>>, sh: &Shared, e: &Encoder, kbps: u32) {
    let mut last_error: Option<Instant> = None;
    loop {
        let job = rx.lock().unwrap().recv();
        let Ok(EncodeJob { mut call, mut audio, about }) = job else {
            return;
        };
        match e.m4a(&audio.wav, &audio.m4a_to, kbps) {
            Ok(()) => call.files.m4a = Some(audio.m4a_to),
            Err(err) => {
                audio.write_wav();
                if last_error.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
                    last_error = Some(Instant::now());
                    sh.note_log("", Level::Warn, format!("M4A encoding failed ({err}); plugins get WAV"));
                }
            }
        }
        let rel = call.path.clone();
        sh.dispatch(topic::CALL_CONCLUDED, &line(&HostMessage::CallConcluded(call)), &about, Some(&rel));
    }
}

/// Keep plugin `i` running until the host stops, feeding it from `rx`.
fn supervise(sh: &Shared, i: usize, mut rx: Receiver<Arc<str>>) {
    let p = &sh.plugins[i];
    while !sh.go.load(Ordering::Relaxed) {
        if sh.stopping.load(Ordering::Relaxed) {
            // Replaced again before it started.
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        let (exit, back) = run_once(sh, p, rx);
        rx = back;
        *p.started.lock().unwrap() = None;
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
        p.restarts.fetch_add(1, Ordering::Relaxed);
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// A pipe's lines until it closes; bytes that aren't UTF-8 become U+FFFD
/// (a stray one mustn't stop the reading, leaving the plugin blocked).
fn lossy_lines(r: impl Read) -> impl Iterator<Item = String> {
    let mut r = BufReader::new(r);
    std::iter::from_fn(move || {
        let mut buf = Vec::new();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                while buf.last().is_some_and(|&b| b == b'\n' || b == b'\r') {
                    buf.pop();
                }
                Some(String::from_utf8_lossy(&buf).into_owned())
            }
        }
    })
}

enum Ended {
    /// The process exited (its code; None: killed).
    Exited(Option<i32>),
    /// The queue was closed and emptied: the host is stopping.
    Drained,
}

/// One run of a plugin's process. Gives the queue back.
fn run_once(sh: &Shared, p: &Plugin, rx: Receiver<Arc<str>>) -> (Result<Ended, String>, Receiver<Arc<str>>) {
    let mut cmd = Command::new(&p.exe);
    cmd.current_dir(&p.data_dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Its own process group: a Ctrl-C in the terminal stops the recorder,
    // which then stops it (saving its queue), not both at once.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let spawned = cmd.spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return (Err(format!("couldn't start {}: {e}", p.exe.display())), rx),
    };
    let (mut stdin, stdout, stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
    *p.child.lock().unwrap() = Some(child);
    *p.started.lock().unwrap() = Some(Instant::now());
    let dead = Arc::new(AtomicBool::new(false));
    let hello = p.hello.clone();
    let dead2 = dead.clone();
    let drained_at = p.drained_at.clone();
    let writer = std::thread::spawn(move || {
        let mut ok = stdin.write_all(hello.as_bytes()).and_then(|_| stdin.flush()).is_ok();
        let mut drained = false;
        while ok && !dead2.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(l) => ok = stdin.write_all(l.as_bytes()).and_then(|_| stdin.flush()).is_ok(),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    *drained_at.lock().unwrap() = Some(Instant::now());
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
        for l in lossy_lines(stderr) {
            if !l.trim().is_empty() {
                notes(Note::Log { plugin: id.clone(), level: Level::Info, text: l });
            }
        }
    });
    for l in lossy_lines(stdout) {
        if l.trim().is_empty() {
            continue;
        }
        let note = match serde_json::from_str::<PluginMessage>(&l) {
            Ok(PluginMessage::Ready) => Note::State { plugin: p.id.clone(), state: State::Ok, message: "running".into() },
            Ok(PluginMessage::Log { level, message }) => Note::Log { plugin: p.id.clone(), level, text: message },
            Ok(PluginMessage::Status { state, message }) => Note::State { plugin: p.id.clone(), state, message },
            Ok(PluginMessage::CallResult { path, outcome, message, url }) => Note::Result { plugin: p.id.clone(), path, outcome, message, url },
            Ok(PluginMessage::Metrics(metrics)) => Note::Metrics { plugin: p.id.clone(), metrics },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_takes_the_wav_with_an_m4a_made() {
        let m = |id: &str, formats: &[&str]| Manifest { id: id.into(), audio_formats: formats.iter().map(|f| f.to_string()).collect(), ..Default::default() };
        assert!(takes_wav(&m("rdioscanner", &[])));
        assert!(!takes_wav(&m("openmhz", &[format::M4A])));
        assert!(takes_wav(&m("mine", &[format::M4A, format::WAV])));
        // Its published manifest asks for M4A alone, but it passes the WAV.
        assert!(takes_wav(&m("upload-script", &[format::M4A])));
    }

    #[test]
    fn a_required_setting_left_empty_is_named() {
        let schema = serde_json::json!({ "required": ["apiKey"],
            "properties": { "apiKey": { "type": "string", "title": "API key" }, "url": { "type": "string", "x-required": true }, "n": { "type": "integer" } } });
        assert_eq!(unset_required(Some(&schema), &serde_json::json!({ "apiKey": "", "n": 3 })), ["API key", "url"]);
        assert!(unset_required(Some(&schema), &serde_json::json!({ "apiKey": "k", "url": "http://x" })).is_empty());
        assert!(unset_required(None, &Value::Null).is_empty());
    }

    fn test_plugin(id: &str, share_decrypted: bool) -> (Plugin, Receiver<Arc<str>>) {
        let (tx, rx) = mpsc::sync_channel::<Arc<str>>(8);
        let manifest = Manifest { id: id.into(), subscribe: vec![topic::CALL_CONCLUDED.into(), topic::AUDIO.into()], ..Default::default() };
        let p = Plugin {
            id: id.into(),
            manifest,
            exe: PathBuf::new(),
            data_dir: PathBuf::new(),
            hello: "".into(),
            tx: Mutex::new(Some(tx)),
            child: Mutex::new(None),
            dropped: AtomicU64::new(0),
            last_drop_note: Mutex::new(None),
            restarts: AtomicU64::new(0),
            started: Mutex::new(None),
            drained_at: Arc::new(Mutex::new(None)),
            share_decrypted,
            withheld_said: AtomicBool::new(false),
            audio_held_said: AtomicBool::new(false),
        };
        (p, rx)
    }

    /// A decrypted call (or its live audio) reaches only the plugins allowed it; a clear one reaches all.
    #[test]
    fn a_decrypted_call_goes_only_to_the_plugins_allowed_it() {
        let (allowed, local_rx) = test_plugin("rdioscanner", true);
        let (public, public_rx) = test_plugin("openmhz", false);
        let said = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = said.clone();
        let sh = Shared {
            plugins: vec![allowed, public],
            stopping: AtomicBool::new(false),
            go: Arc::new(AtomicBool::new(true)),
            notes: Arc::new(move |n| {
                if let Note::Log { plugin, text, .. } = n {
                    log.lock().unwrap().push(format!("{plugin}: {text}"));
                }
            }),
            hold: None,
        };
        let msg: Arc<str> = "x\n".into();
        let (clear, decrypted) = (About { system: "dcfd".into(), ..Default::default() }, About { system: "dcfd".into(), decrypted: true, ..Default::default() });
        sh.dispatch(topic::CALL_CONCLUDED, &msg, &clear, None);
        assert!(local_rx.try_recv().is_ok() && public_rx.try_recv().is_ok(), "a clear call goes to both");
        sh.dispatch(topic::CALL_CONCLUDED, &msg, &decrypted, None);
        sh.dispatch(topic::AUDIO, &msg, &decrypted, None);
        sh.say_all_withheld(topic::CALL_CONCLUDED);
        assert!(local_rx.try_recv().is_ok() && local_rx.try_recv().is_ok(), "the plugin that was allowed gets the call and its audio");
        assert!(public_rx.try_recv().is_err(), "the public one gets neither");
        // It is told once, with how to allow it, never the key.
        let said = said.lock().unwrap();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].starts_with("openmhz: is not given calls decrypted"), "{said:?}");
    }

    /// What a system overrides of a plugin's settings, kept apart from its own.
    #[test]
    fn a_system_overrides_the_plugins_settings() {
        use serde_json::json;
        let schema = json!({ "properties": { "server": { "type": "string" }, "retries": { "type": "integer" } } });
        let config = json!({ "server": "https://a", "retries": 3 });
        let (own, merged, unknown) = resolve(Some(&schema), &config, json!({ "apiKey": "k", "override": { "server": "https://b", "colour": "red" } }));
        assert_eq!(own, json!({ "apiKey": "k" }));
        assert_eq!(merged, Some(json!({ "server": "https://b", "retries": 3 })));
        assert_eq!(unknown, ["colour"]);
        assert_eq!(resolve(Some(&schema), &config, json!({ "apiKey": "k" })), (json!({ "apiKey": "k" }), None, vec![]));
        assert_eq!(resolve(Some(&schema), &config, json!({ "apiKey": "k", "override": {} })).1, None);
        // Nothing for the whole recorder: the override is all there is.
        assert_eq!(resolve(Some(&schema), &Value::Null, json!({ "override": { "server": "https://b" } })).1, Some(json!({ "server": "https://b" })));
    }

    /// A plugin (a shell script) that keeps the hello it's sent.
    #[cfg(unix)]
    fn hello_keeper(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let exe = dir.join("keeper");
        let manifest = r#"{"id":"keeper","name":"Keeper","version":"1.0.0","api":1,"subscribe":["call.concluded"],
            "config":{"type":"object","properties":{"server":{"type":"string","title":"Server","x-required":true}}}}"#.replace('\n', " ");
        std::fs::write(&exe, format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\nread line\necho \"$line\" > hello.json\necho '{{\"type\":\"ready\"}}'\ncat > /dev/null\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    /// A plugin delayed for a talkgroup: that talkgroup's calls wait in the
    /// hold (their live audio isn't sent), others go at once; once the delay
    /// is lifted, what waited goes too.
    #[cfg(unix)]
    #[test]
    fn a_delayed_talkgroup_waits_in_the_hold() {
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("trunk-pro-host-delay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("taker");
        let manifest = r#"{"id":"taker","name":"Taker","version":"1.0.0","api":1,"subscribe":["call.concluded","audio"]}"#;
        std::fs::write(&exe, format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\necho '{{\"type\":\"ready\"}}'\ncat >> got.jsonl\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hold = Hold::new(Arc::default());
        let cfg = json!({ "systems": [{ "shortName": "dcfd", "pluginDelays": [{ "talkgroups": ["5"], "delayS": 3600 }] }] });
        hold.set_delays(super::super::delay::Delays::of(&serde_json::from_value(cfg).unwrap()));
        let systems = vec![SystemInfo { index: 0, short_name: "dcfd".into(), kind: "p25".into(), ..Default::default() }];
        let spec = Spec { id: "taker".into(), copy: 0, exe, config: Value::Null, systems: BTreeMap::new(), data_dir: Some(dir.join("data")), share_decrypted: false };
        let notes: Notes = Arc::new(|_| {});
        let mut host = PluginHost::start_when(vec![spec], &super::super::AudioSettings::default(), &systems, &dir, notes, Arc::new(AtomicBool::new(true)), Some(hold.clone()));
        hold.attach(Some(host.outlet()));
        let call = |rel: &str, tg: u32| {
            let audio = CallAudio { wav: Arc::new(vec![]), wav_written: true, files: CallFiles::default(), m4a_to: dir.join("x.m4a") };
            host.concluded(0, rel, &json!({ "talkgroup": tg, "short_name": "dcfd" }).to_string(), audio, false);
        };
        call("dcfd/held", 5);
        call("dcfd/now", 6);
        let mut frame = vec![2u8, 0, 0, 1, 0, 0, 0];
        frame.extend(5u32.to_le_bytes());
        frame.extend([0u8; 320]);
        host.audio_frame(&frame, false);
        let got = dir.join("data/got.jsonl");
        let lines = || std::fs::read_to_string(&got).unwrap_or_default();
        let t0 = Instant::now();
        while !lines().contains("dcfd/now") && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(lines().contains("dcfd/now") && !lines().contains("dcfd/held"), "{}", lines());
        assert!(!lines().contains("\"audio\""), "a delayed call's live audio isn't sent");
        assert_eq!(hold.calls().get("taker"), Some(&1));
        hold.set_delays(Default::default());
        while !lines().contains("dcfd/held") && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        host.shutdown(Duration::from_secs(1));
        assert!(lines().contains("dcfd/held"), "it goes once the delay is lifted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each system gets the plugin's settings with its overrides; a required
    /// one may come from the systems instead of the plugin's own settings.
    #[cfg(unix)]
    #[test]
    fn systems_get_the_settings_they_override() {
        use serde_json::json;
        let dir = std::env::temp_dir().join(format!("trunk-pro-host-override-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = hello_keeper(&dir);
        let notes_seen = Arc::new(Mutex::new(Vec::new()));
        let seen = notes_seen.clone();
        let notes: Notes = Arc::new(move |n| seen.lock().unwrap().push(n));
        let systems: Vec<SystemInfo> = ["a", "b"].iter().enumerate().map(|(i, n)| SystemInfo { index: i as u16, short_name: n.to_string(), kind: "p25".into(), ..Default::default() }).collect();
        let spec = |config: Value, a: Value, b: Value, data: &str| Spec {
            id: "keeper".into(),
            copy: 0,
            exe: exe.clone(),
            config,
            systems: BTreeMap::from([("a".to_string(), a), ("b".to_string(), b)]),
            data_dir: Some(dir.join(data)),
            share_decrypted: false,
        };
        let audio = super::super::AudioSettings::default();

        // No server for the recorder, but each system names one.
        let s = spec(Value::Null, json!({ "k": 1, "override": { "server": "https://a" } }), json!({ "override": { "server": "https://b" } }), "both");
        let mut host = PluginHost::start(vec![s], &audio, &systems, &dir, notes.clone());
        assert!(!host.is_empty(), "{:?}", notes_seen.lock().unwrap());
        let hello = dir.join("both/hello.json");
        let t0 = Instant::now();
        while !hello.exists() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        host.shutdown(Duration::from_secs(1));
        let h: Value = serde_json::from_str(&std::fs::read_to_string(&hello).unwrap()).unwrap();
        assert_eq!(h["systems"][0]["config"], json!({ "k": 1 }), "the override isn't the system's own setting");
        assert_eq!(h["systems"][0]["settings"], json!({ "server": "https://a" }));
        assert_eq!(h["systems"][1]["settings"], json!({ "server": "https://b" }));

        // One system without: not started, and it's named.
        let s = spec(Value::Null, json!({ "override": { "server": "https://a" } }), json!({ "k": 2 }), "one");
        let host = PluginHost::start(vec![s], &audio, &systems, &dir, notes.clone());
        assert!(host.is_empty());
        let said = notes_seen.lock().unwrap().iter().rev().find_map(|n| if let Note::State { message, .. } = n { Some(message.clone()) } else { None });
        assert_eq!(said.as_deref(), Some("not started: Server (for b) isn't set"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
