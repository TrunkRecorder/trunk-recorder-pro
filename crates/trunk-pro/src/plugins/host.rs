//! Running plugins: one process each, kept running (restarted with backoff
//! when one dies), fed the events it subscribed to.
//!
//! Nothing here blocks the engine: each event is serialized once and offered
//! to each subscriber's queue, and a plugin that falls behind loses events
//! (counted and reported) rather than holding up the recorder. A call reaches
//! the plugins once the finish threads have written its files (an .m4a made
//! for them too, [`super::plan`]). What a plugin is to have
//! late (its delays) goes to the [`Hold`] instead, which gives it to the
//! plugin when it's due.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;
use trunk_app::PluginTopics;
use trunk_recorder_plugin::{
    format, topic, Action, AudioChunk, CallFiles, CallRecord, CallsPage, CallsQuery, ConcludedCall, Hello, HostInfo, HostMessage, Level, LogLine, Manifest, Metrics, Outcome, PluginMessage, Shutdown, State,
    SiteInfo, Status, SystemInfo, TapChunk, TapSet, Totals, VoiceChunk, API_VERSION, EXIT_CONFIG,
};
use trunk_core::trunk::{LiveAudio, SiteId, SystemId};

use super::delay::About;
use super::hold::{Hold, Offer, Outlet};
use super::{describe, executable, Encoder, RunSystem};

/// What the plugins are up to, for the interface (or the terminal).
#[derive(Clone, Debug)]
pub enum Note {
    Log { plugin: String, level: Level, text: String },
    State { plugin: String, state: State, message: String },
    /// One of its notices (`key`), or taken away (no `state`); `key` none: all of them (it stopped).
    Notice { plugin: String, key: Option<String>, state: Option<State>, message: String },
    /// `took_ms`: the plugin's own time on it, when it said (SDK 0.6 and later).
    Result { plugin: String, path: String, outcome: Outcome, message: String, url: String, took_ms: Option<u64> },
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

/// Answers a plugin's `calls.query` (the recorder's [`crate::runtime::calls`]).
pub type Calls = Arc<dyn Fn(&CallsQuery) -> CallsPage + Send + Sync>;

/// Saves a plugin's `settings.save` (by its id: the settings it gives, each key replacing that
/// setting) to the config, as TRP's own edits are saved. Err: why it wasn't.
pub type Saves = Arc<dyn Fn(&str, serde_json::Map<String, Value>) -> Result<(), String> + Send + Sync>;

/// Messages waiting for a plugin; beyond this it's behind, and loses them.
const QUEUE: usize = 1024;

/// A call's start or end, by its id. A plugin keeps track of what's going by
/// these, so one isn't lost as other events are when the plugin falls behind;
/// nor is an answer to something it asked, which it waits for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Life {
    Start(u32),
    End(u32),
    Answer,
}

/// Starts, ends and answers a plugin's queue had no room for, in order.
#[derive(Default)]
struct Owed(VecDeque<(Life, Arc<str>)>);

impl Owed {
    /// Owe it `l`. A call that begins and ends before the plugin has heard
    /// of it needn't be told at all, so what's owed is at most the calls going.
    fn add(&mut self, life: Life, l: &Arc<str>) {
        if let Life::End(id) = life {
            if let Some(i) = self.0.iter().position(|(o, _)| *o == Life::Start(id)) {
                self.0.remove(i);
                return;
            }
        }
        self.0.push_back((life, l.clone()));
    }
}

/// What of the run a plugin is given: calls decrypted with our keys (`shareDecrypted`), and not
/// the systems and sites it's turned off for (`systemsOff`, `sitesOff`: nothing about those
/// systems, nor what's of those sites alone).
#[derive(Default)]
struct Scope {
    share_decrypted: bool,
    off: BTreeSet<SystemId>,
    sites_off: BTreeSet<SiteId>,
}

impl Scope {
    fn of(s: &Spec) -> Scope {
        Scope { share_decrypted: s.share_decrypted, off: s.off.clone(), sites_off: s.sites_off.clone() }
    }
}

impl Plugin {
    /// What of the run it's given now.
    fn scope(&self) -> RwLockReadGuard<'_, Scope> {
        self.scope.read().unwrap()
    }
}

struct Plugin {
    id: String,
    manifest: Manifest,
    exe: PathBuf,
    data_dir: PathBuf,
    /// What it starts with (changed by new settings it took live).
    hello: Mutex<Hello>,
    tx: Mutex<Option<SyncSender<Arc<str>>>>,
    child: Mutex<Option<Child>>,
    /// Given to it once its queue has emptied.
    owed: Arc<Mutex<Owed>>,
    dropped: AtomicU64,
    last_drop_note: Mutex<Option<Instant>>,
    restarts: AtomicU64,
    started: Mutex<Option<Instant>>,
    /// When everything queued for it, `shutdown` last, was written to it.
    drained_at: Arc<Mutex<Option<Instant>>>,
    /// What of the run it's given (new with its settings: [`PluginHost::renew`]).
    scope: RwLock<Scope>,
    /// Its settings changed: its process is to finish what's queued and start again ([`PluginHost::renew`]).
    renew: Arc<AtomicBool>,
    /// It was told to stop for that: its exit is no failure, and it starts again at once.
    renewing: Arc<AtomicBool>,
    /// Whether it was said when a decrypted call was withheld.
    withheld_said: AtomicBool,
    /// Whether it was said that it isn't streamed the live media of calls it's delayed for.
    audio_held_said: AtomicBool,
}

struct Shared {
    plugins: Vec<Plugin>,
    /// The run's systems: what a line's system number (or short name, a log line's) is.
    systems: Vec<RunSystem>,
    stopping: AtomicBool,
    /// Plugins may start (false: wait for the ones they replace to exit).
    go: Arc<AtomicBool>,
    notes: Notes,
    /// Where what's delayed waits (None: nothing is).
    hold: Option<Arc<Hold>>,
    /// Buttons pressed ([`PluginHost::action`]) waiting for the plugin's
    /// answer, by the request number it was given.
    answers: Mutex<HashMap<u64, mpsc::Sender<(bool, String)>>>,
    requests: AtomicU64,
    /// Answers `calls.query` (None: no recorder to ask, as in `plugin test`).
    calls: Option<Calls>,
    /// Saves `settings.save` (None: no config to save it to, as in `plugin test`).
    saves: Option<Saves>,
}

/// How long `shutdown` waits for a plugin to read what was queued before
/// `shutdown` (its grace counts from then).
const DRAIN_MAX: Duration = Duration::from_secs(30);

/// How long a plugin restarting for new settings ([`PluginHost::renew`]) has to finish and exit,
/// as a reload gives the plugins it replaces.
const RENEW_GRACE: Duration = Duration::from_secs(10);

pub struct PluginHost {
    shared: Arc<Shared>,
    /// The M4A encoder this host's plugins are told of (the finish threads encode).
    encoder: Option<(Encoder, u32)>,
    supervisors: Vec<JoinHandle<()>>,
    /// What the session should build for plugins.
    pub topics: PluginTopics,
    /// Some plugin wants calls' live media ([`topic::is_live`]: audio, voice).
    pub live: bool,
    /// The log's tap that hands its lines to the plugins taking `log` ([`crate::logging::tap`]).
    log_tap: Option<u64>,
    done: bool,
}

/// A plugin to run: what the config says of it (its `plugins` entry and each system's settings), resolved.
pub struct Spec {
    pub id: String,
    /// Which copy of it (0: the first; see [`super::copies`]).
    pub copy: usize,
    pub exe: PathBuf,
    pub config: Value,
    /// Its settings for each system, by id; [`OVERRIDE`] in one
    /// overrides some of `config` for that system.
    pub systems: BTreeMap<SystemId, Value>,
    /// Its data folder (None: the usual, in the config folder).
    pub data_dir: Option<PathBuf>,
    /// It may be given calls decrypted with our keys (`shareDecrypted`, when a system has keys).
    pub share_decrypted: bool,
    /// The systems it's turned off for (`systemsOff`).
    pub off: BTreeSet<SystemId>,
    /// The sites it's turned off for (`sitesOff`).
    pub sites_off: BTreeSet<SiteId>,
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
                    off: p.systems_off.iter().copied().collect(),
                    sites_off: p.sites_off.iter().copied().collect(),
                })
            })
            .collect()
    }
}

impl PluginHost {
    /// Start `specs`. `systems`: every system calls come from (their `config` is filled in here).
    /// A plugin that can't start is reported and left out.
    pub fn start(specs: Vec<Spec>, audio: &super::AudioSettings, systems: &[RunSystem], capture_dir: &Path, notes: Notes) -> PluginHost {
        Self::start_when(specs, audio, systems, capture_dir, notes, Arc::new(AtomicBool::new(true)), None, None, None)
    }

    /// `start`, with the processes held back until `go` is set (events queue
    /// meanwhile): the plugins they replace share their data folders. What
    /// a plugin is to have late goes to `hold` (None: everything goes now).
    /// `calls` answers the plugins' `calls.query`, `saves` their `settings.save`.
    #[allow(clippy::too_many_arguments)]
    pub fn start_when(
        specs: Vec<Spec>,
        audio: &super::AudioSettings,
        systems: &[RunSystem],
        capture_dir: &Path,
        notes: Notes,
        go: Arc<AtomicBool>,
        hold: Option<Arc<Hold>>,
        calls: Option<Calls>,
        saves: Option<Saves>,
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
                    let site_props = m.system_config.as_ref().and_then(|c| c.get("properties")).and_then(Value::as_object);
                    for k in m.site_overrides.iter().filter(|k| !site_props.is_some_and(|p| p.contains_key(*k))) {
                        notes(Note::Log { plugin: s.id.clone(), level: Level::Warn, text: format!("says a site can set \"{k}\" again, but it has no such setting for a system") });
                    }
                    let props = m.config.as_ref().and_then(|c| c.get("properties")).and_then(Value::as_object);
                    for k in m.system_overrides.iter().filter(|k| !props.is_some_and(|p| p.contains_key(*k))) {
                        notes(Note::Log { plugin: s.id.clone(), level: Level::Warn, text: format!("says a system can set \"{k}\" again, but it has no such setting") });
                    }
                    match systems_for(&s, &m, systems, &notes) {
                        Ok(told) => ready.push((s, m, told)),
                        // Not set up yet (the interface says so too): it would only fail.
                        Err(text) => {
                            notes(Note::State { plugin: s.id.clone(), state: State::Warning, message: text.clone() });
                            notes(Note::Log { plugin: s.id, level: Level::Warn, text });
                        }
                    }
                }
                Err(e) => {
                    notes(Note::State { plugin: s.id.clone(), state: State::Error, message: e.clone() });
                    notes(Note::Log { plugin: s.id, level: Level::Error, text: e });
                }
            }
        }
        let wants_m4a = ready.iter().any(|(_, m, _)| m.subscribes(topic::CALL_CONCLUDED) && m.wants_format(format::M4A));
        let encoder = if wants_m4a { Encoder::find(audio.encoder) } else { None };
        if wants_m4a && encoder.is_none() {
            let who: Vec<&str> = ready.iter().filter(|(_, m, _)| m.wants_format(format::M4A)).map(|(s, _, _)| s.id.as_str()).collect();
            let text = if audio.encoder == crate::config::M4aEncoder::None {
                format!("M4A encoding is off: {} get WAV only", who.join(", "))
            } else {
                format!("No M4A encoder found (install ffmpeg): {} get WAV only", who.join(", "))
            };
            notes(Note::Log { plugin: String::new(), level: Level::Warn, text });
        }
        let mut topics = PluginTopics::default();
        let mut live = false;
        let mut plugins = Vec::new();
        let mut queues = Vec::new();
        for (s, m, told) in ready {
            topics.calls |= m.subscribes(topic::CALL_START) || m.subscribes(topic::CALL_END);
            topics.call_updates |= m.subscribes(topic::CALL_UPDATE);
            topics.units |= m.subscribes(topic::UNIT);
            topics.status |= m.subscribes(topic::STATUS);
            topics.totals |= m.subscribes(topic::TOTALS);
            topics.tone_patterns |= m.subscribes(topic::TONE_PATTERN);
            topics.voice |= m.subscribes(topic::VOICE);
            topics.taps = topics.taps.and(TapSet::of_topics(m.subscribe.iter().map(String::as_str)));
            live |= topic::ALL.iter().any(|&t| topic::is_live(t) && m.subscribes(t));
            let data_dir = s.data_dir.clone().unwrap_or_else(|| super::data_dir(&s.id));
            if let Err(p) = trunk_app::files::make_dir(&data_dir) {
                log::warn!("Plugin {}: its data folder can't be made, so it may not keep its state. {p}", s.id);
            }
            let mut formats = vec!["wav".to_string()];
            if encoder.is_some() && m.wants_format(format::M4A) {
                formats.push(format::M4A.into());
            }
            let hello = Hello {
                api: API_VERSION,
                host: HostInfo { name: "trunk-pro".into(), version: env!("CARGO_PKG_VERSION").into() },
                config: s.config.clone(),
                systems: told,
                capture_dir: capture_dir.to_path_buf(),
                data_dir: data_dir.clone(),
                audio_formats: formats,
            };
            // The queue outlives the process: events wait through a (re)start.
            let (tx, rx) = mpsc::sync_channel::<Arc<str>>(QUEUE);
            queues.push(rx);
            let scope = RwLock::new(Scope::of(&s));
            plugins.push(Plugin {
                id: s.id,
                manifest: m,
                exe: s.exe,
                data_dir,
                hello: Mutex::new(hello),
                tx: Mutex::new(Some(tx)),
                child: Mutex::new(None),
                owed: Default::default(),
                dropped: AtomicU64::new(0),
                last_drop_note: Mutex::new(None),
                restarts: AtomicU64::new(0),
                started: Mutex::new(None),
                drained_at: Arc::new(Mutex::new(None)),
                scope,
                renew: Default::default(),
                renewing: Default::default(),
                withheld_said: AtomicBool::new(false),
                audio_held_said: AtomicBool::new(false),
            });
        }
        let shared = Arc::new(Shared { plugins, systems: systems.to_vec(), stopping: AtomicBool::new(false), go, notes, hold, answers: Default::default(), requests: AtomicU64::new(0), calls, saves });
        let supervisors = queues
            .into_iter()
            .enumerate()
            .map(|(i, rx)| {
                let sh = shared.clone();
                std::thread::Builder::new().name(format!("plugin-{}", sh.plugins[i].id)).spawn(move || supervise(&sh, i, rx)).expect("thread")
            })
            .collect();
        let encoder = encoder.map(|e| (e, audio.bitrate_kbps.clamp(8, 320)));
        // (These plugins' from now on: the ones they replace stop taking the lines.)
        let log_tap = shared.plugins.iter().any(|p| p.manifest.subscribes(topic::LOG)).then(|| {
            let sh = Arc::downgrade(&shared);
            crate::logging::tap(Arc::new(move |t: &crate::logging::Tapped| {
                if let Some(sh) = sh.upgrade() {
                    sh.log_line(t);
                }
            }))
        });
        PluginHost { shared, encoder, supervisors, topics, live, log_tap, done: false }
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
        let sh = &self.shared;
        let on = |system: SystemId, talkgroup: Option<u32>| About { system, talkgroup, ..Default::default() };
        let (t, about) = match m {
            HostMessage::CallStart(c) => (topic::CALL_START, on(sh.id_at(c.system), Some(c.talkgroup))),
            HostMessage::CallUpdate(c) => (topic::CALL_UPDATE, on(sh.id_at(c.system), Some(c.talkgroup))),
            HostMessage::CallEnd(c) => (topic::CALL_END, on(sh.id_at(c.system), Some(c.talkgroup))),
            HostMessage::Unit(u) => (topic::UNIT, About { sites: vec![SiteId(u.site_id)], ..on(sh.id_at(u.system), u.talkgroup) }),
            // (Its tones may have been heard in decrypted voice: only plugins allowed that get it.)
            HostMessage::TonePattern(t) => (topic::TONE_PATTERN, About { decrypted: t.decrypted, ..on(sh.id_named(&t.short_name), Some(t.talkgroup)) }),
            // (Every site's: each plugin is sent those it's on for. Never delayed.)
            HostMessage::Status(st) => {
                let all = line(m);
                for p in sh.plugins.iter().filter(|p| p.manifest.subscribes(topic::STATUS)) {
                    let sc = p.scope();
                    let on = |x: &&trunk_recorder_plugin::SystemStatus| !sc.off.contains(&sh.id_at(x.scope.index)) && !sc.sites_off.contains(&SiteId(x.scope.site_id));
                    let l = if st.systems.iter().all(|x| on(&x)) {
                        all.clone()
                    } else {
                        let systems = st.systems.iter().filter(on).cloned().collect();
                        line(&HostMessage::Status(Status { systems, ..st.clone() }))
                    };
                    sh.offer(p, topic::STATUS, &l, &About::default(), None, None);
                }
                return;
            }
            // (Every system's figures: each plugin is sent those of the systems and sites it's on for.)
            HostMessage::Totals(totals) => {
                if !sh.wants(topic::TOTALS) {
                    return;
                }
                let all = line(m);
                for p in sh.plugins.iter().filter(|p| p.manifest.subscribes(topic::TOTALS)) {
                    let sc = p.scope();
                    let l = match totals_for(totals, |index| sc.off.contains(&sh.id_at(index)), |site| sc.sites_off.contains(&SiteId(site))) {
                        None => all.clone(),
                        Some(t) => line(&HostMessage::Totals(t)),
                    };
                    sh.offer(p, topic::TOTALS, &l, &About::default(), None, None);
                }
                return;
            }
            // (Live media and concluded calls may be decrypted ones: they go only by `live` and
            // `concluded`, which know.)
            _ => return,
        };
        let life = match m {
            HostMessage::CallStart(c) => Some(Life::Start(c.id)),
            HostMessage::CallEnd(c) => Some(Life::End(c.id)),
            _ => None,
        };
        if self.shared.wants(t) {
            self.shared.dispatch(t, &line(m), &about, None, life);
        }
    }

    /// Plugin `specs`' settings changed (its copies, as the config has them now): each copy is
    /// told to stop behind what's already queued for it, and its process starts again at once with
    /// them, while the other plugins carry on. What comes meanwhile waits in its queue. False when
    /// that won't do (a copy more or fewer, one not running, or one that can't start as set):
    /// restart them all.
    pub fn renew(&self, specs: Vec<Spec>) -> bool {
        let Some(id) = specs.first().map(|s| s.id.clone()) else { return false };
        let running = self.shared.plugins.iter().filter(|p| super::parse_label(&p.id).0 == id).count();
        if running != specs.len() {
            return false;
        }
        let mut ready = Vec::new();
        for s in specs {
            let s = Spec { id: super::copy_label(&s.id, s.copy), ..s };
            let Some(i) = self.shared.plugins.iter().position(|p| p.id == s.id) else { return false };
            let p = &self.shared.plugins[i];
            // (Given up on: its supervisor has gone.)
            if p.tx.lock().unwrap().is_none() {
                return false;
            }
            let Ok(told) = systems_for(&s, &p.manifest, &self.shared.systems, &self.shared.notes) else { return false };
            ready.push((i, s, told));
        }
        for (i, s, told) in ready {
            let p = &self.shared.plugins[i];
            {
                let mut h = p.hello.lock().unwrap();
                h.config = s.config.clone();
                h.systems = told;
            }
            *p.scope.write().unwrap() = Scope::of(&s);
            p.renew.store(true, Ordering::Relaxed);
            let sh = self.shared.clone();
            std::thread::Builder::new().name(format!("renew-{}", p.id)).spawn(move || sh.renew_watch(i)).expect("thread");
        }
        true
    }

    /// Plugin `id` takes `config` as its settings while it runs, if it can
    /// ([`Shared::live`]). False: it has to be restarted for them, as one
    /// that stopped over its settings (exit 78) waits to be.
    pub fn settle(&self, id: &str, config: &Value) -> bool {
        let Ok(p) = self.shared.live(id, config) else { return false };
        let mut hello = p.hello.lock().unwrap();
        let h = Hello { config: config.clone(), ..hello.clone() };
        if self.shared.give(p, &line(&HostMessage::Settings(h.clone()))) != Offer::Sent {
            return false;
        }
        // (Restarted after a crash, it starts with them.)
        *hello = h;
        true
    }

    /// Someone pressed one of plugin `id`'s buttons (its first copy's):
    /// hand it `action` on `value`, and wait (`wait` at most) for its
    /// answer, ok or not and what it said. Err: there's none to give.
    pub fn action(&self, id: &str, action: String, value: Value, wait: Duration) -> Result<(bool, String), String> {
        let sh = &self.shared;
        let p = sh.plugins.iter().find(|p| p.id == id).ok_or("It isn't running: plugins run while recording")?;
        let request = sh.requests.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel();
        sh.answers.lock().unwrap().insert(request, tx);
        let answer = match sh.give(p, &line(&HostMessage::Action(Action { request, action, value }))) {
            Offer::Sent => rx.recv_timeout(wait).map_err(|_| "No answer from the plugin".to_string()),
            Offer::Full => Err("It's too far behind to take that now".into()),
            Offer::Gone => Err("It isn't running".into()),
        };
        sh.answers.lock().unwrap().remove(&request);
        answer
    }

    /// A call's live audio, to the plugins that take it: its PCM as an `audio` chunk, its vocoder frames
    /// (`voice`, built as [`PluginTopics::voice`] asks) as a `voice` one. One decrypted with our keys
    /// goes only to plugins with `shareDecrypted`.
    pub fn live(&self, short_name: &str, a: &LiveAudio, voice: Option<&VoiceChunk>) {
        if !self.live {
            return;
        }
        let about = About { system: self.shared.id_at(a.system), talkgroup: Some(a.talkgroup), decrypted: a.decrypted, ..Default::default() };
        if self.shared.wants(topic::AUDIO) {
            let pcm = trunk_recorder_plugin::base64::encode(&trunk_core::wav::pcm16(&a.samples));
            let chunk = AudioChunk { call_id: a.call_id, system: a.system, system_id: self.shared.id_at(a.system).0, short_name: short_name.to_string(), talkgroup: a.talkgroup, sample_rate: 8000, pcm };
            self.shared.dispatch(topic::AUDIO, &line(&HostMessage::Audio(chunk)), &about, None, None);
        }
        if let Some(chunk) = voice.filter(|_| self.topics.voice) {
            self.shared.dispatch(topic::VOICE, &line(&HostMessage::Voice(chunk.clone())), &about, None, None);
        }
    }

    /// A piece of a signal tap's stream, to the plugins subscribed to its topic (`tap.<id>`). Never
    /// late: a plugin delayed for its system or talkgroup isn't sent it, as with a call's live
    /// audio. (A stream of voice we decrypted, [`TapChunk::decrypted`], goes only to plugins with
    /// `shareDecrypted`, as that call's audio does; the rest is of the air, or of how it decoded.)
    pub fn tap(&self, chunk: &TapChunk) {
        let t = chunk.tap.topic();
        if !self.shared.wants(&t) {
            return;
        }
        let about = About { system: self.shared.id_at(chunk.system), talkgroup: chunk.talkgroup, decrypted: chunk.decrypted, ..Default::default() };
        self.shared.dispatch(&t, &line(&HostMessage::Tap(chunk.clone())), &about, None, None);
    }

    /// The plugins that take finished call `record` of `system` (its index;
    /// each reports on every one it's given); not one that gave up (a config
    /// error), nor one off for the system, or for every site that heard it.
    /// (`decrypted`: a call decrypted with our keys, which only plugins with
    /// `shareDecrypted` take.)
    pub fn call_takers(&self, system: u16, record: &CallRecord, decrypted: bool) -> usize {
        self.takers(system, record, decrypted).count()
    }

    /// [`Self::call_takers`]: those of the plugins that take concluded calls that may have it.
    fn takers(&self, system: u16, record: &CallRecord, decrypted: bool) -> impl Iterator<Item = &Plugin> {
        let sh = &self.shared;
        let about = About { decrypted, ..about_record(sh.id_at(system), record) };
        sh.plugins.iter().filter(move |p| p.manifest.subscribes(topic::CALL_CONCLUDED) && p.tx.lock().unwrap().is_some() && sh.may_take(p, &about))
    }

    /// What the plugins take of finished call `record` of `system` (its index;
    /// `decrypted`: one decrypted with our keys, which only plugins with
    /// `shareDecrypted` take): how many, and whether one takes the WAV, one
    /// wants an .m4a ([`super::archive::plan`] says what's written for them).
    pub fn plugin_audio(&self, system: u16, record: &CallRecord, decrypted: bool) -> super::archive::PluginAudio {
        let takers: Vec<&Plugin> = self.takers(system, record, decrypted).collect();
        super::archive::PluginAudio { takers: takers.len(), wav: takers.iter().any(|p| takes_wav(&p.manifest)), m4a: takers.iter().any(|p| p.manifest.wants_format(format::M4A)) }
    }

    /// The encoder and bitrate this host's plugins get .m4a files from, if any.
    pub fn encoder_settings(&self) -> Option<&(Encoder, u32)> {
        self.encoder.as_ref()
    }

    /// A concluded call (`rel`: relative to the capture folder, no
    /// extension), its record and its files as they are on disk now (the
    /// finish thread wrote what [`super::archive::plan`] said): to the
    /// plugins that take it. `decrypted`: some of it was decrypted with our keys.
    pub fn concluded(&self, system: u16, rel: &str, record: CallRecord, files: CallFiles, decrypted: bool) {
        if self.call_takers(system, &record, decrypted) == 0 {
            if decrypted {
                self.shared.say_all_withheld(topic::CALL_CONCLUDED);
            }
            return;
        }
        let about = About { decrypted, ..about_record(self.shared.id_at(system), &record) };
        let call = ConcludedCall { path: rel.to_string(), system, call: record, files };
        self.shared.dispatch(topic::CALL_CONCLUDED, &line(&HostMessage::CallConcluded(call)), &about, Some(rel), None);
    }

    /// Tell every plugin to stop, and give each `grace` to
    /// exit from when it has read `shutdown` (behind what was queued, for up
    /// to `DRAIN_MAX`).
    pub fn shutdown(&mut self, grace: Duration) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        if let Some(id) = self.log_tap.take() {
            crate::logging::untap(id);
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

/// The key in a system's settings for a plugin that holds each of its sites'
/// own (by site id): `{ "apiKey": "…", "perSite": { "3": { "systemId": 77 } } }`.
pub const PER_SITE: &str = "perSite";

/// `t` without the systems (by [`SystemInfo::index`]) and sites a plugin is off for: their sites
/// and channels, and every series under their keys. A radio's stay, shared as it may be. (A
/// system's own series, of all its sites, stay with a site off.) None: nothing is left out.
fn totals_for(t: &Totals, system_off: impl Fn(u16) -> bool, site_off: impl Fn(u32) -> bool) -> Option<Totals> {
    // A system's key: its site's or channel's, up to the site or frequency.
    let system_key = |key: &str| key.split_once("/site/").or_else(|| key.split_once("/ch/")).map_or(key, |(k, _)| k).to_string();
    let mut gone: BTreeSet<String> = BTreeSet::new();
    for s in &t.sites {
        if system_off(s.index) {
            gone.insert(system_key(&s.key));
        } else if site_off(s.site_id) {
            gone.insert(s.key.clone());
        }
    }
    gone.extend(t.channels.iter().filter(|c| system_off(c.index)).map(|c| system_key(&c.key)));
    gone.retain(|k| !k.is_empty());
    if gone.is_empty() {
        return None;
    }
    let under = |key: &str| gone.iter().any(|g| key.strip_prefix(g.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('/')));
    Some(Totals {
        series: t.series.iter().filter(|(k, _)| !under(k)).map(|(k, v)| (k.clone(), v.clone())).collect(),
        sites: t.sites.iter().filter(|s| !under(&s.key)).cloned().collect(),
        channels: t.channels.iter().filter(|c| !under(&c.key)).cloned().collect(),
        ..t.clone()
    })
}

/// What recorded call `r` of `system` is about: its talkgroup, and the sites
/// that heard it (none named: its own, if any).
fn about_record(system: SystemId, r: &CallRecord) -> About {
    let mut sites: Vec<SiteId> = r.sites.iter().map(|s| SiteId(s.id)).filter(|s| s.0 != 0).collect();
    if sites.is_empty() && r.site_id != 0 {
        sites.push(SiteId(r.site_id));
    }
    About { system, talkgroup: Some(r.talkgroup), decrypted: r.decrypted, sites }
}

/// Each site's settings for a plugin, from a system's own (`own`, which
/// loses [`PER_SITE`]): by site id, the system's with what the site sets
/// again of those the plugin lets it (`overridable`, [`Manifest::site_overrides`],
/// each a setting of its `schema`, `system_config`). A site that sets none
/// isn't there. And the keys sites set that they may not (left out).
fn resolve_sites(schema: Option<&Value>, overridable: &[String], own: &mut Value) -> (BTreeMap<SiteId, Value>, Vec<String>) {
    let per_site = match own.as_object_mut().and_then(|o| o.remove(PER_SITE)) {
        Some(Value::Object(o)) => o,
        _ => return (BTreeMap::new(), Vec::new()),
    };
    let props = schema.and_then(|s| s.get("properties")).and_then(Value::as_object);
    let mut sites = BTreeMap::new();
    let mut ignored = Vec::new();
    for (id, set) in per_site {
        let Ok(id) = id.parse::<u32>() else { continue };
        let mut merged = own.as_object().cloned().unwrap_or_default();
        let mut any = false;
        for (k, v) in set.as_object().into_iter().flatten().filter(|(_, v)| !v.is_null()) {
            if overridable.contains(k) && props.is_some_and(|p| p.contains_key(k)) {
                merged.insert(k.clone(), v.clone());
                any = true;
            } else if !ignored.contains(k) {
                ignored.push(k.clone());
            }
        }
        if any {
            sites.insert(SiteId(id), Value::Object(merged));
        }
    }
    (sites, ignored)
}

/// A system's settings for a plugin (`entry`): its own (without
/// [`OVERRIDE`]); the plugin's (`config`) with what it sets again of those
/// the plugin lets it (`overridable`, [`Manifest::system_overrides`], each a
/// setting in its `schema`; None: it sets none again); and the keys it sets
/// that it may not (left out).
fn resolve(schema: Option<&Value>, overridable: &[String], config: &Value, mut entry: Value) -> (Value, Option<Value>, Vec<String>) {
    let over = match entry.as_object_mut().and_then(|o| o.remove(OVERRIDE)) {
        Some(Value::Object(o)) if !o.is_empty() => o,
        _ => return (entry, None, Vec::new()),
    };
    let props = schema.and_then(|s| s.get("properties")).and_then(Value::as_object);
    let mut merged = config.as_object().cloned().unwrap_or_default();
    let mut any = false;
    let mut ignored = Vec::new();
    for (k, v) in over.into_iter().filter(|(_, v)| !v.is_null()) {
        if overridable.contains(&k) && props.is_some_and(|p| p.contains_key(&k)) {
            merged.insert(k, v);
            any = true;
        } else {
            ignored.push(k);
        }
    }
    (entry, any.then_some(Value::Object(merged)), ignored)
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

/// The files of a call already on disk at `base` (its path without the extension).
pub fn files_on_disk(base: &Path) -> CallFiles {
    let m4a = Some(with_ext(base, "m4a")).filter(|p| p.exists());
    CallFiles { json: with_ext(base, "json"), wav: with_ext(base, "wav"), m4a }
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
    /// Plugin `i` was renewed ([`PluginHost::renew`]): once it's told to stop, it has
    /// [`RENEW_GRACE`] to exit, or is stopped.
    fn renew_watch(&self, i: usize) {
        let p = &self.plugins[i];
        while p.renew.load(Ordering::Relaxed) && !self.stopping.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
        // (Not running when asked: it starts with them, nothing to wait for.)
        if !p.renewing.load(Ordering::Relaxed) {
            return;
        }
        let was = *p.started.lock().unwrap();
        let until = Instant::now() + RENEW_GRACE;
        let same = || *p.started.lock().unwrap() == was;
        while Instant::now() < until && same() && !self.stopping.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if same() && !self.stopping.load(Ordering::Relaxed) && let Some(c) = p.child.lock().unwrap().as_mut() {
            self.note_log(&p.id, Level::Warn, format!("didn't stop within {} s for its new settings; stopped", RENEW_GRACE.as_secs()));
            let _ = c.kill();
        }
    }

    fn note_log(&self, plugin: &str, level: Level, text: impl Into<String>) {
        (self.notes)(Note::Log { plugin: plugin.to_string(), level, text: text.into() });
    }

    /// Say, once a run, that `p` is not given decrypted calls, and how to allow it.
    fn say_withheld(&self, p: &Plugin) {
        if !p.scope().share_decrypted && !p.withheld_said.swap(true, Ordering::Relaxed) {
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

    /// The id of the run's system `index` (a line's `system`; none: no one system's).
    fn id_at(&self, index: u16) -> SystemId {
        self.systems.iter().find(|y| y.info.index == index).map_or_else(SystemId::default, |y| y.id)
    }

    /// The id of the run's system named `short_name` (none: no one system's).
    fn id_named(&self, short_name: &str) -> SystemId {
        self.systems.iter().find(|y| y.info.short_name == short_name).map_or_else(SystemId::default, |y| y.id)
    }

    /// Some plugin subscribes to topic `t`.
    fn wants(&self, t: &str) -> bool {
        self.plugins.iter().any(|p| p.manifest.subscribes(t))
    }

    /// Send `l` (of topic `t`) to each plugin that subscribed to it, now or,
    /// when its delays say so, later (by the hold). `about`: what it's about —
    /// a call decrypted with our keys (or its live media) goes only to plugins with
    /// `shareDecrypted`; `call`: it's that finished call; `life`: it's a
    /// call's start or end, which a plugin is never left without.
    fn dispatch(&self, t: &str, l: &Arc<str>, about: &About, call: Option<&str>, life: Option<Life>) {
        for p in self.plugins.iter().filter(|p| p.manifest.subscribes(t)) {
            self.offer(p, t, l, about, call, life);
        }
    }

    /// [`Self::dispatch`]'s rules for one plugin: the one way anything about a
    /// system reaches a plugin.
    fn offer(&self, p: &Plugin, t: &str, l: &Arc<str>, about: &About, call: Option<&str>, life: Option<Life>) {
        if !self.may_take(p, about) {
            let withheld = about.decrypted && {
                let sc = p.scope();
                !sc.share_decrypted && !sc.off.contains(&about.system)
            };
            if withheld {
                self.say_withheld(p);
            }
            return;
        }
        if let Some(h) = self.hold.as_ref().filter(|h| !h.delay(&p.id, about).is_zero()) {
            // Live media late isn't live: a delayed call's isn't streamed at all.
            if topic::is_live(t) {
                if !p.audio_held_said.swap(true, Ordering::Relaxed) {
                    self.note_log(&p.id, Level::Info, "isn't given the live audio or voice of calls it's delayed for");
                }
            } else if life.is_some() {
                h.keep(&p.id, about, l, call);
            } else if !h.push(&p.id, about, l, call) {
                self.dropped(p);
            }
            return;
        }
        self.send(p, t, l, life);
    }

    /// Give `l` to `p` now. What's owed comes after what's queued, so a
    /// call's end never passes its start; until it's caught up, it's spared
    /// the live audio, the bulk of what fills its queue.
    fn send(&self, p: &Plugin, t: &str, l: &Arc<str>, life: Option<Life>) {
        let mut owed = p.owed.lock().unwrap();
        match life {
            Some(life) if !owed.0.is_empty() || self.give(p, l) == Offer::Full => owed.add(life, l),
            Some(_) => {}
            None if topic::is_live(t) && !owed.0.is_empty() => {
                drop(owed);
                self.dropped(p);
            }
            None => {
                drop(owed);
                if self.give(p, l) == Offer::Full {
                    self.dropped(p);
                }
            }
        }
    }

    /// Whether plugin `id` can take `config` as its settings while it runs, rather than be
    /// restarted for them: it takes settings live (`live_settings`), runs as one copy, its
    /// systems don't override them, and every setting it needs is set. Err: why not. The one
    /// rule for [`PluginHost::settle`] and a plugin's own [`Self::save`].
    fn live(&self, id: &str, config: &Value) -> Result<&Plugin, String> {
        let copies: Vec<&Plugin> = self.plugins.iter().filter(|p| super::parse_label(&p.id).0 == id).collect();
        let [p] = copies[..] else { return Err("it runs as several copies, which share its settings".into()) };
        if !p.manifest.live_settings {
            return Err("it doesn't take settings as it runs (live_settings)".into());
        }
        let unset = unset_required(p.manifest.config.as_ref(), config);
        if !unset.is_empty() {
            return Err(format!("{} would be unset", unset.join(", ")));
        }
        if p.hello.lock().unwrap().systems.iter().any(|s| !s.settings.is_null() || !s.config.is_null()) {
            return Err("its systems have settings of their own".into());
        }
        Ok(p)
    }

    /// `p`'s `settings.save`: only what it can take as it runs ([`Self::live`], on its settings
    /// as the save leaves them), so a plugin is never restarted by its own save.
    fn save(&self, p: &Plugin, settings: serde_json::Map<String, Value>) -> Result<(), String> {
        let (id, _) = super::parse_label(&p.id);
        let mut merged = p.hello.lock().unwrap().config.clone();
        super::merge_settings(&mut merged, settings.clone());
        self.live(id, &merged).map_err(|e| format!("saving would restart it: {e}"))?;
        let saves = self.saves.as_ref().ok_or("there's no config to save them to here")?;
        saves(id, settings)
    }

    /// A line TRP logged: to each plugin taking `log`, but not its own notes.
    /// One about a system, or a call of it, is delayed as that system's (that
    /// talkgroup's) calls are for the plugin.
    fn log_line(&self, t: &crate::logging::Tapped) {
        let m = HostMessage::Log(LogLine { time: t.time, level: t.level.as_str().into(), system: t.system.unwrap_or_default().into(), text: t.text.into(), line: t.line.into() });
        let l = line(&m);
        let about = About { system: t.system.map_or_else(SystemId::default, |s| self.id_named(s)), talkgroup: t.talkgroup, ..Default::default() };
        for p in self.plugins.iter().filter(|p| p.manifest.subscribes(topic::LOG)) {
            if t.plugin == Some(p.id.as_str()) {
                continue;
            }
            self.offer(p, topic::LOG, &l, &about, None, None);
        }
    }

    /// Answer something `p` asked (`calls.query`, `settings.save`): never
    /// lost, and never waited for (the answer is given from the thread that
    /// reads what it says).
    fn answer(&self, p: &Plugin, l: &Arc<str>) {
        self.send(p, "", l, Some(Life::Answer));
    }

    /// Whether `p` may be given what's `about`, ever: not of a system it's
    /// off for, and a decrypted call (or its media) only with `shareDecrypted`.
    /// The one rule for what's sent ([`Self::offer`]), held ([`Outlet::offer`]),
    /// queried ([`Self::may_have`]) and waited on ([`PluginHost::call_takers`]).
    fn may_take(&self, p: &Plugin, about: &About) -> bool {
        let sc = p.scope();
        (!about.decrypted || sc.share_decrypted) && !sc.off.contains(&about.system) && (about.sites.is_empty() || about.sites.iter().any(|s| !sc.sites_off.contains(s)))
    }

    /// Whether `p` may be given stored call `c` now, as it would have been
    /// sent it ([`Self::may_take`]), a delayed one only once its delay since
    /// the call ended has passed.
    fn may_have(&self, p: &Plugin, c: &trunk_recorder_plugin::StoredCall, now_s: i64) -> bool {
        let r = &c.record;
        let system = if r.system_id != 0 { SystemId(r.system_id) } else { self.id_named(&r.short_name) };
        let about = About { decrypted: r.decrypted, ..about_record(system, r) };
        if !self.may_take(p, &about) {
            return false;
        }
        let delay = self.hold.as_ref().map_or(Duration::ZERO, |h| h.delay(&p.id, &about));
        delay.is_zero() || r.stop_time + delay.as_secs() as i64 <= now_s
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
    fn offer(&self, label: &str, line: &Arc<str>, about: &About) -> Offer {
        match self.plugins.iter().find(|p| p.id == label) {
            Some(p) if self.may_take(p, about) => self.give(p, line),
            _ => Offer::Gone,
        }
    }
}

/// Keep plugin `i` running until the host stops, feeding it from `rx`.
/// What plugin `s` is told of the run's systems: each it's on for, with its settings for it (its
/// own, and the plugin's as they are for that system) and each site's it's on for. Settings it
/// can't take are said in its notes. Err: required settings left empty, as said when it isn't started.
fn systems_for(s: &Spec, m: &Manifest, systems: &[RunSystem], notes: &Notes) -> Result<Vec<SystemInfo>, String> {
    let mut settings = BTreeMap::new();
    // Required settings left empty, and for which systems ("": for none).
    let mut unset: Vec<(String, Vec<String>)> = Vec::new();
    let mut note_unset = |field: String, system: &str| match unset.iter_mut().find(|(f, _)| *f == field) {
        Some((_, on)) => on.push(system.to_string()),
        None => unset.push((field, vec![system.to_string()])),
    };
    let mut own_settings = BTreeMap::new();
    let mut site_settings: BTreeMap<SiteId, Value> = BTreeMap::new();
    for (id, entry) in s.systems.clone().into_iter().filter(|(id, _)| !s.off.contains(id)) {
        let name = systems.iter().find(|y| y.id == id).map_or_else(|| format!("system {id}"), |y| y.info.short_name.clone());
        let (mut own, merged, ignored) = resolve(m.config.as_ref(), &m.system_overrides, &s.config, entry);
        for k in ignored {
            notes(Note::Log { plugin: s.id.clone(), level: Level::Warn, text: format!("{name}: \"{k}\" can't be set for one system (the plugin doesn't offer it); ignored") });
        }
        let (sites, ignored) = resolve_sites(m.system_config.as_ref(), &m.site_overrides, &mut own);
        for k in ignored {
            notes(Note::Log { plugin: s.id.clone(), level: Level::Warn, text: format!("{name}: \"{k}\" can't be set for one site (the plugin doesn't offer it); ignored") });
        }
        site_settings.extend(sites);
        for f in unset_required(m.config.as_ref(), merged.as_ref().unwrap_or(&s.config)) {
            note_unset(f, &name);
        }
        if let Some(v) = merged {
            settings.insert(id, v);
        }
        own_settings.insert(id, own);
    }
    // (Used by no system: its own settings decide.)
    if own_settings.is_empty() {
        unset_required(m.config.as_ref(), &s.config).into_iter().for_each(|f| note_unset(f, ""));
    }
    if !unset.is_empty() {
        let what: Vec<String> = unset.iter().map(|(f, on)| if on.iter().all(|x| x.is_empty()) { f.clone() } else { format!("{f} (for {})", on.join(", ")) }).collect();
        return Err(format!("not started: {} {} set", what.join(", "), if unset.len() == 1 { "isn't" } else { "aren't" }));
    }
    Ok(systems
        .iter()
        .filter(|y| !s.off.contains(&y.id))
        .map(|y| SystemInfo {
            config: own_settings.get(&y.id).cloned().unwrap_or(Value::Null),
            settings: settings.get(&y.id).cloned().unwrap_or(Value::Null),
            // (Not those it's off for.)
            sites: y
                .info
                .sites
                .iter()
                .filter(|t| !s.sites_off.contains(&SiteId(t.id)))
                .map(|t| SiteInfo { config: site_settings.get(&SiteId(t.id)).cloned().unwrap_or(Value::Null), ..t.clone() })
                .collect(),
            ..y.info.clone()
        })
        .collect())
}

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
        let renewed = p.renewing.swap(false, Ordering::Relaxed);
        // What it said stays true goes with it (a host replacing this one starts afresh).
        if !stopping {
            (sh.notes)(Note::Notice { plugin: p.id.clone(), key: None, state: None, message: String::new() });
        }
        match exit {
            _ if stopping => return,
            // Everything's been sent and the host is stopping.
            Ok(Ended::Drained) => return,
            // It stopped for new settings: it starts again with them, at once.
            _ if renewed => {
                sh.note_log(&p.id, Level::Info, "restarting with its new settings");
                backoff = Duration::from_secs(1);
                continue;
            }
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
    // (Starting now, it starts with the settings renewed meanwhile: no need to stop it for them.)
    p.renew.store(false, Ordering::Relaxed);
    let hello = line(&HostMessage::Hello(p.hello.lock().unwrap().clone()));
    let bye = line(&HostMessage::Shutdown(Shutdown { grace_s: RENEW_GRACE.as_secs_f64() }));
    let (renew, renewing) = (p.renew.clone(), p.renewing.clone());
    let dead2 = dead.clone();
    let drained_at = p.drained_at.clone();
    let owed = p.owed.clone();
    let writer = std::thread::spawn(move || {
        let mut ok = stdin.write_all(hello.as_bytes()).and_then(|_| stdin.flush()).is_ok();
        let mut drained = false;
        while ok && !dead2.load(Ordering::Relaxed) {
            // New settings: it stops behind what it was given, and what's queued waits for the next one.
            if renew.swap(false, Ordering::Relaxed) {
                renewing.store(true, Ordering::Relaxed);
                let _ = stdin.write_all(bye.as_bytes()).and_then(|_| stdin.flush());
                break;
            }
            let wait = if owed.lock().unwrap().0.is_empty() { Duration::from_millis(100) } else { Duration::ZERO };
            match rx.recv_timeout(wait) {
                Ok(l) => ok = stdin.write_all(l.as_bytes()).and_then(|_| stdin.flush()).is_ok(),
                // (Its queue is empty: what it's owed comes after all of that.)
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    while ok {
                        let Some((_, l)) = owed.lock().unwrap().0.pop_front() else { break };
                        ok = stdin.write_all(l.as_bytes()).and_then(|_| stdin.flush()).is_ok();
                    }
                }
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
            Ok(PluginMessage::Notice { key, state, message }) => Note::Notice { plugin: p.id.clone(), key: Some(key), state, message },
            Ok(PluginMessage::CallResult { path, outcome, message, url, took_ms }) => Note::Result { plugin: p.id.clone(), path, outcome, message, url, took_ms },
            Ok(PluginMessage::Metrics(metrics)) => Note::Metrics { plugin: p.id.clone(), metrics },
            // (To the button that was pressed, waiting in `action`.)
            Ok(PluginMessage::ActionResult { request, ok, message }) => {
                if let Some(tx) = sh.answers.lock().unwrap().remove(&request) {
                    let _ = tx.send((ok, message));
                }
                continue;
            }
            // (Answered on its own queue, behind what's already waiting for it: only the calls it would
            // have been sent by now.)
            Ok(PluginMessage::CallsQuery { request, query }) => {
                let mut page = match &sh.calls {
                    Some(answer) => answer(&query),
                    None => CallsPage { error: Some("no recorder to ask".into()), ..Default::default() },
                };
                let now_s = crate::clock::unix_s() as i64;
                page.entries.retain(|c| sh.may_have(p, c, now_s));
                sh.answer(p, &line(&HostMessage::Calls { request, page }));
                continue;
            }
            // (Answered on its own queue, behind the settings the save sends it.)
            Ok(PluginMessage::SettingsSave { request, settings }) => {
                let saved = sh.save(p, settings);
                if let Err(e) = &saved {
                    sh.note_log(&p.id, Level::Warn, format!("couldn't save its settings: {e}"));
                }
                let (ok, message) = match saved {
                    Ok(()) => (true, String::new()),
                    Err(e) => (false, e),
                };
                sh.answer(p, &line(&HostMessage::SettingsSaved { request, ok, message }));
                continue;
            }
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
            hello: Mutex::new(Hello::default()),
            tx: Mutex::new(Some(tx)),
            child: Mutex::new(None),
            owed: Default::default(),
            dropped: AtomicU64::new(0),
            last_drop_note: Mutex::new(None),
            restarts: AtomicU64::new(0),
            started: Mutex::new(None),
            drained_at: Arc::new(Mutex::new(None)),
            scope: RwLock::new(Scope { share_decrypted, ..Default::default() }),
            renew: Default::default(),
            renewing: Default::default(),
            withheld_said: AtomicBool::new(false),
            audio_held_said: AtomicBool::new(false),
        };
        (p, rx)
    }

    /// A plugin too far behind loses events, but not a call's start or end.
    #[test]
    fn a_full_queue_owes_a_plugin_its_calls_starts_and_ends() {
        let mut o = Owed::default();
        let l: Arc<str> = "x\n".into();
        o.add(Life::Start(1), &l);
        o.add(Life::End(2), &l);
        o.add(Life::Start(3), &l);
        o.add(Life::End(1), &l);
        assert_eq!(o.0.iter().map(|x| x.0).collect::<Vec<_>>(), [Life::End(2), Life::Start(3)], "begun and ended unheard: nothing to tell");
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
            systems: Vec::new(),
            stopping: AtomicBool::new(false),
            go: Arc::new(AtomicBool::new(true)),
            notes: Arc::new(move |n| {
                if let Note::Log { plugin, text, .. } = n {
                    log.lock().unwrap().push(format!("{plugin}: {text}"));
                }
            }),
            hold: None,
            answers: Default::default(),
            requests: AtomicU64::new(0),
            calls: None,
            saves: None,
        };
        let msg: Arc<str> = "x\n".into();
        let (clear, decrypted) = (About { system: SystemId(1), ..Default::default() }, About { system: SystemId(1), decrypted: true, ..Default::default() });
        sh.dispatch(topic::CALL_CONCLUDED, &msg, &clear, None, None);
        assert!(local_rx.try_recv().is_ok() && public_rx.try_recv().is_ok(), "a clear call goes to both");
        sh.dispatch(topic::CALL_CONCLUDED, &msg, &decrypted, None, None);
        sh.dispatch(topic::AUDIO, &msg, &decrypted, None, None);
        sh.say_all_withheld(topic::CALL_CONCLUDED);
        assert!(local_rx.try_recv().is_ok() && local_rx.try_recv().is_ok(), "the plugin that was allowed gets the call and its audio");
        assert!(public_rx.try_recv().is_err(), "the public one gets neither");
        // It is told once, with how to allow it, never the key.
        let said = said.lock().unwrap();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].starts_with("openmhz: is not given calls decrypted"), "{said:?}");
    }

    /// A plugin off for a system is given nothing about it: not its calls or
    /// their media, nor its entry in the status; nor waited on for its calls.
    #[test]
    fn a_plugin_off_for_a_system_hears_nothing_of_it() {
        let (mut all, all_rx) = test_plugin("mqtt", false);
        let (mut some, some_rx) = test_plugin("notify", false);
        for p in [&mut all, &mut some] {
            p.manifest.subscribe.extend([topic::STATUS.to_string(), topic::CALL_START.to_string(), topic::UNIT.to_string(), topic::TOTALS.to_string()]);
        }
        some.scope.get_mut().unwrap().off.insert(SystemId(2));
        some.scope.get_mut().unwrap().sites_off.insert(SiteId(5));
        let systems: Vec<RunSystem> = ["a", "b"].iter().enumerate().map(|(i, n)| RunSystem { id: SystemId(i as u32 + 1), info: SystemInfo { index: i as u16, short_name: n.to_string(), ..Default::default() } }).collect();
        let sh = Shared {
            plugins: vec![all, some],
            systems,
            stopping: AtomicBool::new(false),
            go: Arc::new(AtomicBool::new(true)),
            notes: Arc::new(|_| {}),
            hold: None,
            answers: Default::default(),
            requests: AtomicU64::new(0),
            calls: None,
            saves: None,
        };
        let host = PluginHost { shared: Arc::new(sh), encoder: None, supervisors: Vec::new(), topics: Default::default(), live: true, log_tap: None, done: true };
        let got = |rx: &Receiver<Arc<str>>| -> Vec<serde_json::Value> { rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect() };
        for system in [0, 1] {
            host.event(&HostMessage::CallStart(trunk_recorder_plugin::CallInfo { system, talkgroup: 5, ..Default::default() }));
        }
        let scope = |index: u16| trunk_recorder_plugin::SiteScope { index, ..Default::default() };
        host.event(&HostMessage::Status(Status { time: 1.0, systems: vec![trunk_recorder_plugin::SystemStatus { scope: scope(0), ..Default::default() }, trunk_recorder_plugin::SystemStatus { scope: scope(1), ..Default::default() }] }));
        let (to_all, to_some) = (got(&all_rx), got(&some_rx));
        assert_eq!(to_all.iter().map(|m| m["type"].as_str().unwrap()).collect::<Vec<_>>(), ["call.start", "call.start", "status"]);
        assert_eq!(to_all[2]["systems"].as_array().unwrap().len(), 2);
        assert_eq!(to_some.len(), 2, "{to_some:?}");
        assert_eq!((&to_some[0]["system"], &to_some[1]["type"]), (&serde_json::json!(0), &serde_json::json!("status")));
        assert_eq!(to_some[1]["systems"].as_array().unwrap().iter().map(|x| x["index"].as_u64().unwrap()).collect::<Vec<_>>(), [0], "only the systems it's on for");
        // Its calls aren't waited on, or encoded, for it.
        let rec = CallRecord::default();
        assert_eq!((host.call_takers(0, &rec, false), host.call_takers(1, &rec, false)), (2, 1));
        assert_eq!(host.plugin_audio(1, &rec, false).takers, 1);
        // Off for a site: a call only it heard isn't for it, one another site heard too is.
        let heard = |ids: &[u32]| CallRecord { sites: ids.iter().map(|&id| trunk_recorder_plugin::SiteRef { id, name: String::new() }).collect(), ..Default::default() };
        assert_eq!((host.call_takers(0, &heard(&[5]), false), host.call_takers(0, &heard(&[5, 6]), false)), (1, 2));
        assert_eq!(host.call_takers(0, &CallRecord { site_id: 5, ..Default::default() }, false), 1, "the kept copy's site, when none are listed");
        // Nor its unit events.
        host.event(&HostMessage::Unit(trunk_recorder_plugin::UnitEvent { system: 0, site_id: 5, ..Default::default() }));
        host.event(&HostMessage::Unit(trunk_recorder_plugin::UnitEvent { system: 0, site_id: 6, ..Default::default() }));
        assert_eq!((got(&all_rx).len(), got(&some_rx).len()), (2, 1));
        // Of the totals, the figures of the systems and sites it's on for, and every radio's.
        let site = |index: u16, site_id: u32, key: &str| trunk_recorder_plugin::SiteScope { index, site_id, key: key.into(), ..Default::default() };
        let keys = ["sys/a/calls/started", "sys/a/site/north/cc/good", "sys/a/site/south/cc/good", "sys/b/cc/good", "sys/b/ch/1/open", "sys/bb/cc/good", "src/800/samples"];
        let totals = Totals {
            series: keys.iter().map(|k| (k.to_string(), Default::default())).collect(),
            sites: vec![site(0, 5, "sys/a/site/north"), site(0, 6, "sys/a/site/south"), site(1, 9, "sys/b")],
            channels: vec![trunk_recorder_plugin::ChannelScope { key: "sys/b/ch/1".into(), index: 1, ..Default::default() }],
            ..Default::default()
        };
        host.event(&HostMessage::Totals(totals));
        let (to_all, to_some) = (got(&all_rx), got(&some_rx));
        let series = |m: &serde_json::Value| m["series"].as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(series(&to_all[0]).len(), keys.len());
        assert_eq!(series(&to_some[0]), ["src/800/samples", "sys/a/calls/started", "sys/a/site/south/cc/good", "sys/bb/cc/good"]);
        assert_eq!(to_some[0]["sites"].as_array().unwrap().iter().map(|x| x["key"].as_str().unwrap()).collect::<Vec<_>>(), ["sys/a/site/south"]);
        assert!(to_some[0]["channels"].as_array().unwrap().is_empty());
        // Held, then off for its system: not given.
        let off = About { system: SystemId(2), ..Default::default() };
        assert_eq!(Outlet::offer(&*host.shared, "notify", &"x\n".into(), &off), Offer::Gone);
        assert_eq!(Outlet::offer(&*host.shared, "mqtt", &"x\n".into(), &off), Offer::Sent);
    }

    /// What a system sets again of a plugin's settings, kept apart from its
    /// own: only those the plugin offers (`system_overrides`); none by default.
    #[test]
    fn a_system_overrides_the_plugins_settings() {
        use serde_json::json;
        let schema = json!({ "properties": { "server": { "type": "string" }, "retries": { "type": "integer" } } });
        let config = json!({ "server": "https://a", "retries": 3 });
        let offered = ["server".to_string(), "colour".to_string()];
        let (own, merged, mut ignored) = resolve(Some(&schema), &offered, &config, json!({ "apiKey": "k", "override": { "server": "https://b", "retries": 9, "colour": "red" } }));
        ignored.sort();
        assert_eq!(own, json!({ "apiKey": "k" }));
        assert_eq!(merged, Some(json!({ "server": "https://b", "retries": 3 })));
        assert_eq!(ignored, ["colour", "retries"], "not offered, or offered but no setting");
        assert_eq!(resolve(Some(&schema), &offered, &config, json!({ "apiKey": "k" })), (json!({ "apiKey": "k" }), None, vec![]));
        assert_eq!(resolve(Some(&schema), &offered, &config, json!({ "apiKey": "k", "override": {} })).1, None);
        // Nothing for the whole recorder: the override is all there is.
        assert_eq!(resolve(Some(&schema), &offered, &Value::Null, json!({ "override": { "server": "https://b" } })).1, Some(json!({ "server": "https://b" })));
        // A plugin that offers none: every system has its settings.
        let (own, merged, ignored) = resolve(Some(&schema), &[], &config, json!({ "apiKey": "k", "override": { "server": "https://b" } }));
        assert_eq!((own, merged, ignored), (json!({ "apiKey": "k" }), None, vec!["server".to_string()]));
    }

    /// A plugin (a shell script) that keeps the hello it's sent.
    #[cfg(unix)]
    fn hello_keeper(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let exe = dir.join("keeper");
        let manifest = r#"{"id":"keeper","name":"Keeper","version":"1.0.0","api":1,"subscribe":["call.concluded"],"system_overrides":["server"],"site_overrides":["k"],
            "system_config":{"type":"object","properties":{"k":{"type":"integer"},"name":{"type":"string"}}},
            "config":{"type":"object","properties":{"server":{"type":"string","title":"Server","x-required":true}}}}"#.replace('\n', " ");
        std::fs::write(&exe, format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\nread line\necho \"$line\" > hello.json\necho '{{\"type\":\"ready\"}}'\ncat > /dev/null\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    /// A plugin's `calls.query` is answered on its own queue, for its
    /// request number, by what the host was given to answer with.
    #[cfg(unix)]
    #[test]
    fn a_plugins_calls_query_is_answered() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("trunk-pro-host-calls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = r#"{"id":"asker","name":"Asker","version":"1.0.0","api":1,"subscribe":["call.concluded"]}"#;
        let exe = dir.join("asker");
        std::fs::write(&exe, format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\nread line\necho '{{\"type\":\"ready\"}}'\necho '{{\"type\":\"calls.query\",\"request\":7,\"talkgroup\":101}}'\nwhile read l; do case \"$l\" in *'\"type\":\"calls\"'*) echo \"$l\" > answer.json;; esac; done\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let spec = Spec { id: "asker".into(), copy: 0, exe, config: Value::Null, systems: BTreeMap::new(), data_dir: Some(dir.join("data")), share_decrypted: false, off: Default::default(), sites_off: Default::default() };
        let calls: Calls = Arc::new(|q| CallsPage { next: (q.talkgroup == Some(101)).then(|| "42:x".into()), ..Default::default() });
        let mut host = PluginHost::start_when(vec![spec], &super::super::AudioSettings::default(), &[], &dir, Arc::new(|_| {}), Arc::new(AtomicBool::new(true)), None, Some(calls), None);
        let answer = dir.join("data/answer.json");
        let t0 = Instant::now();
        while !answer.exists() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        host.shutdown(Duration::from_secs(1));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&answer).expect("answered")).unwrap();
        assert_eq!((v["request"].as_u64(), v["next"].as_str()), (Some(7), Some("42:x")), "{v}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// New settings for one plugin restart it alone: it's told to stop behind what it was given,
    /// starts again at once with them (no failure, no restart counted), and a call that came
    /// meanwhile reaches the new process. The other plugin's process carries on.
    #[cfg(unix)]
    #[test]
    fn renewed_settings_restart_that_plugin_alone() {
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("trunk-pro-host-renew-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = |id: &str| {
            let exe = dir.join(id);
            let manifest = format!(r#"{{"id":"{id}","name":"{id}","version":"1.0.0","api":1,"subscribe":["call.concluded"]}}"#);
            let script = format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\nread line\necho \"$line\" >> hellos.jsonl\necho '{{\"type\":\"ready\"}}'\nwhile read l; do case \"$l\" in *'\"type\":\"shutdown\"'*) exit 0;; *) echo \"$l\" >> got.jsonl;; esac; done\n");
            std::fs::write(&exe, script).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            exe
        };
        let spec = |id: &str, server: &str| Spec { id: id.into(), copy: 0, exe: exe(id), config: json!({ "server": server }), systems: BTreeMap::new(), data_dir: Some(dir.join(format!("{id}-data"))), share_decrypted: false, off: Default::default(), sites_off: Default::default() };
        let systems = vec![RunSystem { id: SystemId(1), info: SystemInfo { index: 0, short_name: "dcfd".into(), kind: "p25".into(), ..Default::default() } }];
        let mut host = PluginHost::start_when(vec![spec("a", "1"), spec("b", "1")], &super::super::AudioSettings::default(), &systems, &dir, Arc::new(|_| {}), Arc::new(AtomicBool::new(true)), None, None, None);
        let read = |id: &str, f: &str| std::fs::read_to_string(dir.join(format!("{id}-data/{f}"))).unwrap_or_default();
        let until = |what: &dyn Fn() -> bool| {
            let t0 = Instant::now();
            while !what() && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        until(&|| !read("a", "hellos.jsonl").is_empty() && !read("b", "hellos.jsonl").is_empty());
        assert!(host.renew(vec![spec("a", "2")]));
        host.concluded(0, "dcfd/after", CallRecord { talkgroup: 5, short_name: "dcfd".into(), ..Default::default() }, CallFiles::default(), false);
        until(&|| read("a", "hellos.jsonl").lines().count() == 2 && read("a", "got.jsonl").contains("dcfd/after"));
        let hellos: Vec<Value> = read("a", "hellos.jsonl").lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(hellos.iter().map(|h| h["config"]["server"].as_str().unwrap()).collect::<Vec<_>>(), ["1", "2"], "started again with its new settings");
        assert!(read("a", "got.jsonl").contains("dcfd/after"), "the call that came meanwhile reached the new process");
        assert_eq!(read("b", "hellos.jsonl").lines().count(), 1, "the other plugin carried on");
        assert!(host.process_stats().iter().all(|p| p.restarts == 0), "no failure, no restart counted");
        // One that isn't running can't be renewed: they all restart.
        assert!(!host.renew(vec![spec("c", "1")]));
        host.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&dir);
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
        hold.set_delays(super::super::delay::Delays::of(&trunk_app::Config::from_value(cfg).unwrap()));
        let systems = vec![RunSystem { id: SystemId(1), info: SystemInfo { index: 0, short_name: "dcfd".into(), kind: "p25".into(), ..Default::default() } }];
        let spec = Spec { id: "taker".into(), copy: 0, exe, config: Value::Null, systems: BTreeMap::new(), data_dir: Some(dir.join("data")), share_decrypted: false, off: Default::default(), sites_off: Default::default() };
        let notes: Notes = Arc::new(|_| {});
        let mut host = PluginHost::start_when(vec![spec], &super::super::AudioSettings::default(), &systems, &dir, notes, Arc::new(AtomicBool::new(true)), Some(hold.clone()), None, None);
        hold.attach(Some(host.outlet()));
        let call = |rel: &str, tg: u32| {
            host.concluded(0, rel, CallRecord { talkgroup: tg, short_name: "dcfd".into(), ..Default::default() }, CallFiles::default(), false);
        };
        call("dcfd/held", 5);
        call("dcfd/now", 6);
        let frame = trunk_core::trunk::frames::VoiceFrame { codec: trunk_recorder_plugin::Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind: trunk_recorder_plugin::FrameKind::Voice, tone: None };
        let audio = LiveAudio { call_id: 1, system: 0, talkgroup: 5, unit: 0, samples: vec![0.0; 160], frames: vec![frame], decrypted: false };
        host.live("dcfd", &audio, trunk_app::session::voice_chunk(1, "dcfd", "", &audio).as_ref());
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
        let site = |id: u32, name: &str| SiteInfo { id, name: name.into(), config: Value::Null };
        let systems: Vec<RunSystem> = ["a", "b"]
            .iter()
            .enumerate()
            .map(|(i, n)| RunSystem { id: SystemId(i as u32 + 1), info: SystemInfo { index: i as u16, short_name: n.to_string(), kind: "p25".into(), sites: vec![site(7 + 2 * i as u32, "north"), site(8 + 2 * i as u32, "south")], ..Default::default() } })
            .collect();
        let spec = |config: Value, a: Value, b: Value, data: &str| Spec {
            id: "keeper".into(),
            copy: 0,
            exe: exe.clone(),
            config,
            systems: BTreeMap::from([(SystemId(1), a), (SystemId(2), b)]),
            data_dir: Some(dir.join(data)),
            share_decrypted: false,
            off: Default::default(),
            sites_off: Default::default(),
        };
        let audio = super::super::AudioSettings::default();

        // No server for the recorder, but each system names one.
        let s = spec(Value::Null, json!({ "k": 1, "name": "A", "override": { "server": "https://a" }, "perSite": { "8": { "k": 5, "name": "not offered" } } }), json!({ "override": { "server": "https://b" } }), "both");
        let mut host = PluginHost::start(vec![s], &audio, &systems, &dir, notes.clone());
        assert!(!host.is_empty(), "{:?}", notes_seen.lock().unwrap());
        let hello = dir.join("both/hello.json");
        let t0 = Instant::now();
        while !hello.exists() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        host.shutdown(Duration::from_secs(1));
        let h: Value = serde_json::from_str(&std::fs::read_to_string(&hello).unwrap()).unwrap();
        assert_eq!(h["systems"][0]["config"], json!({ "k": 1, "name": "A" }), "the override and the sites' aren't the system's own settings");
        // Each site's: the system's with what it sets again of those the plugin offers (south's k, not its name).
        assert_eq!((&h["systems"][0]["sites"][0]["id"], h["systems"][0]["sites"][0].get("config")), (&json!(7), None), "north sets none");
        assert_eq!(h["systems"][0]["sites"][1]["config"], json!({ "k": 5, "name": "A" }));
        assert_eq!(h["systems"][0]["settings"], json!({ "server": "https://a" }));
        assert_eq!(h["systems"][1]["settings"], json!({ "server": "https://b" }));

        // Off for b: it isn't told of it, nor asked to be set up for it.
        let mut s = spec(Value::Null, json!({ "override": { "server": "https://a" } }), json!({ "k": 2 }), "off");
        s.off.insert(SystemId(2));
        let mut host = PluginHost::start(vec![s], &audio, &systems, &dir, notes.clone());
        assert!(!host.is_empty(), "{:?}", notes_seen.lock().unwrap());
        let hello = dir.join("off/hello.json");
        let t0 = Instant::now();
        while !hello.exists() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        host.shutdown(Duration::from_secs(1));
        let h: Value = serde_json::from_str(&std::fs::read_to_string(&hello).unwrap()).unwrap();
        assert_eq!(h["systems"].as_array().unwrap().iter().map(|x| x["short_name"].as_str().unwrap()).collect::<Vec<_>>(), ["a"]);

        // One system without: not started, and it's named.
        let s = spec(Value::Null, json!({ "override": { "server": "https://a" } }), json!({ "k": 2 }), "one");
        let host = PluginHost::start(vec![s], &audio, &systems, &dir, notes.clone());
        assert!(host.is_empty());
        let said = notes_seen.lock().unwrap().iter().rev().find_map(|n| if let Note::State { message, .. } = n { Some(message.clone()) } else { None });
        assert_eq!(said.as_deref(), Some("not started: Server (for b) isn't set"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plugin that takes new settings as it runs is sent them (and starts
    /// with them after a crash); settings leaving one it needs empty, or a
    /// plugin that stopped over its settings, take a restart instead.
    #[cfg(unix)]
    #[test]
    fn live_settings_reach_a_running_plugin() {
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("trunk-pro-host-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // It keeps every line after its hello; with "stop" for a server, it stops over its settings.
        let exe = dir.join("live");
        let manifest = r#"{"id":"live","name":"Live","version":"1.0.0","api":1,"subscribe":["tone_pattern"],"live_settings":true,
            "config":{"type":"object","properties":{"server":{"type":"string","title":"Server","x-required":true}}}}"#.replace('\n', " ");
        std::fs::write(
            &exe,
            format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\nread line\ncase \"$line\" in *stop*) exit 78;; esac\necho '{{\"type\":\"ready\"}}'\nwhile read l; do echo \"$l\" >> lines.jsonl; done\n"),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let notes: Notes = Arc::new(|_| {});
        let audio = super::super::AudioSettings::default();
        let spec = |server: &str, data: &str| Spec {
            id: "live".into(),
            copy: 0,
            exe: exe.clone(),
            config: json!({ "server": server }),
            systems: BTreeMap::new(),
            data_dir: Some(dir.join(data)),
            share_decrypted: false,
            off: Default::default(),
            sites_off: Default::default(),
        };
        let wait = |cond: &dyn Fn() -> bool| {
            let t0 = Instant::now();
            while !cond() && t0.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        let mut host = PluginHost::start(vec![spec("https://a", "on")], &audio, &[], &dir, notes.clone());
        let lines = dir.join("on/lines.jsonl");
        assert!(host.settle("live", &json!({ "server": "https://b" })));
        wait(&|| std::fs::read_to_string(&lines).is_ok_and(|t| t.contains("https://b")));
        let sent: Value = serde_json::from_str(std::fs::read_to_string(&lines).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!((sent["type"].as_str(), sent["config"]["server"].as_str()), (Some("settings"), Some("https://b")));
        assert!(!host.settle("live", &json!({ "server": "" })), "a setting it needs left empty: restarted, to say so");
        host.shutdown(Duration::from_secs(1));

        // Stopped over its settings: only a restart starts it again.
        let host = PluginHost::start(vec![spec("stop", "stopped")], &audio, &[], &dir, notes);
        wait(&|| host.shared.plugins[0].tx.lock().unwrap().is_none());
        assert!(!host.settle("live", &json!({ "server": "https://c" })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plugin taking `log` is sent TRP's lines, not its own.
    #[cfg(unix)]
    #[test]
    fn the_log_reaches_plugins_that_take_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("trunk-pro-host-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("logger");
        let manifest = r#"{"id":"logger","name":"L","version":"1.0.0","api":1,"subscribe":["log"]}"#;
        std::fs::write(&exe, format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\nread line\necho '{{\"type\":\"ready\"}}'\nwhile read l; do echo \"$l\" >> lines.jsonl; done\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let spec = Spec { id: "logger".into(), copy: 0, exe, config: Value::Null, systems: BTreeMap::new(), data_dir: Some(dir.join("data")), share_decrypted: false, off: Default::default(), sites_off: Default::default() };
        let hold = Hold::new(Arc::default());
        let cfg = serde_json::json!({ "systems": [{ "shortName": "held", "pluginDelays": [{ "delayS": 3600 }] }] });
        hold.set_delays(super::super::delay::Delays::of(&trunk_app::Config::from_value(cfg).unwrap()));
        let held = [RunSystem { id: SystemId(1), info: SystemInfo { index: 0, short_name: "held".into(), kind: "p25".into(), ..Default::default() } }];
        let mut host = PluginHost::start_when(vec![spec], &super::super::AudioSettings::default(), &held, &dir, super::super::notes_to_log(), Arc::new(AtomicBool::new(true)), Some(hold.clone()), None, None);
        hold.attach(Some(host.outlet()));
        crate::logging::text(trunk_app::log::Level::Warning, "info", "log-topic-test: TRP said");
        // One about a system the plugin is delayed for waits as its calls do.
        crate::logging::record(&trunk_app::log::Record::text(trunk_app::log::Level::Warning, Some("held"), "log-topic-test: of a delayed system"));
        crate::logging::plugin_note("logger", trunk_app::log::Level::Info, "log-topic-test: its own");
        let t0 = Instant::now();
        let lines = || std::fs::read_to_string(dir.join("data/lines.jsonl")).unwrap_or_default();
        while !lines().contains("TRP said") && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        host.shutdown(Duration::from_secs(1));
        let got: Vec<Value> = lines().lines().filter(|l| l.contains("log-topic-test")).filter_map(|l| serde_json::from_str(l).ok()).collect();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!((got[0]["type"].as_str(), got[0]["level"].as_str(), got[0]["text"].as_str()), (Some("log"), Some("warning"), Some("log-topic-test: TRP said")));
        assert!(got[0]["line"].as_str().unwrap().contains("(warning)   log-topic-test"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plugin saves some of its settings: they're handed to what saves them, and it's told how
    /// that went (by the request number it gave). One the save would restart is refused: one
    /// without live settings, or a save that unsets a setting it needs.
    #[cfg(unix)]
    #[test]
    fn a_plugin_saves_its_settings() {
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("trunk-pro-host-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = |name: &str, live: bool, save: &str| {
            let exe = dir.join(name);
            let schema = r#"{"type":"object","properties":{"token":{"type":"string"}},"required":["token"]}"#;
            let manifest = format!(r#"{{"id":"{name}","name":"N","version":"1.0.0","api":1,"subscribe":["tone_pattern"],"live_settings":{live},"config":{schema}}}"#);
            let save = format!(r#"{{"type":"settings.save","request":7,"settings":{save}}}"#);
            std::fs::write(&exe, format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\nread line\necho '{{\"type\":\"ready\"}}'\necho '{save}'\nwhile read l; do echo \"$l\" >> lines.jsonl; done\n")).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            Spec { id: name.into(), copy: 0, exe, config: json!({ "token": "t" }), systems: BTreeMap::new(), data_dir: Some(dir.join(name).with_extension("d")), share_decrypted: false, off: Default::default(), sites_off: Default::default() }
        };
        let saved: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let s2 = saved.clone();
        let saves: Saves = Arc::new(move |id, m| {
            s2.lock().unwrap().push((id.to_string(), Value::Object(m)));
            Ok(())
        });
        let audio = super::super::AudioSettings::default();
        let named = r#"{"serverName":"LolWut","old":null}"#;
        let specs = vec![script("namer", true, named), script("fixed", false, named), script("needy", true, r#"{"token":null}"#)];
        let mut host = PluginHost::start_when(specs, &audio, &[], &dir, Arc::new(|_| {}), Arc::new(AtomicBool::new(true)), None, None, Some(saves));
        let answer = |name: &str| -> Option<Value> {
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(5) {
                let lines = std::fs::read_to_string(dir.join(name).with_extension("d").join("lines.jsonl")).unwrap_or_default();
                if let Some(l) = lines.lines().find(|l| l.contains("settings.saved")) {
                    return serde_json::from_str(l).ok();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        };
        assert_eq!(answer("namer"), Some(json!({ "type": "settings.saved", "request": 7, "ok": true })));
        let refused = answer("fixed").expect("answered");
        assert_eq!((refused["request"].as_u64(), refused["ok"].as_bool()), (Some(7), Some(false)));
        assert!(refused["message"].as_str().unwrap().contains("live_settings"), "{refused}");
        let needy = answer("needy").expect("answered");
        assert_eq!(needy["ok"].as_bool(), Some(false));
        assert!(needy["message"].as_str().unwrap().contains("token would be unset"), "{needy}");
        assert_eq!(*saved.lock().unwrap(), [("namer".to_string(), json!({ "serverName": "LolWut", "old": null }))], "only the plugin that may");
        host.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A button pressed: the plugin is handed it and its answer comes back to
    /// whoever pressed it; one that isn't running, or doesn't answer, says so.
    #[cfg(unix)]
    #[test]
    fn a_button_pressed_waits_for_the_plugins_answer() {
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("trunk-pro-host-action-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // It answers "test" with what it was given, and nothing else at all.
        let exe = dir.join("act");
        let manifest = r#"{"id":"act","name":"Act","version":"1.0.0","api":1,"subscribe":["tone_pattern"]}"#;
        let answer = r#"r=$(echo "$l" | sed 's/.*"request":\([0-9]*\).*/\1/'); echo "{\"type\":\"action.result\",\"request\":$r,\"ok\":true,\"message\":\"sent to $(echo "$l" | sed 's/.*"to":"\([^"]*\)".*/\1/')\"}""#;
        std::fs::write(&exe, format!("#!/bin/sh\nif [ \"$1\" = --describe ]; then echo '{manifest}'; exit 0; fi\nread line\necho '{{\"type\":\"ready\"}}'\nwhile read l; do case \"$l\" in *'\"action\":\"test\"'*) {answer};; esac; done\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let spec = Spec { id: "act".into(), copy: 0, exe, config: Value::Null, systems: BTreeMap::new(), data_dir: Some(dir.join("data")), share_decrypted: false, off: Default::default(), sites_off: Default::default() };
        let mut host = PluginHost::start(vec![spec], &super::super::AudioSettings::default(), &[], &dir, Arc::new(|_| {}));
        let wait = Duration::from_secs(5);
        assert_eq!(host.action("act", "test".into(), json!({ "to": "discord" }), wait), Ok((true, "sent to discord".into())));
        assert_eq!(host.action("act", "other".into(), json!({}), Duration::from_millis(200)), Err("No answer from the plugin".into()));
        assert!(host.action("nope", "test".into(), json!({}), wait).is_err());
        assert!(host.shared.answers.lock().unwrap().is_empty(), "nothing left waiting");
        host.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
