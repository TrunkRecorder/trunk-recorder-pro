//! One recording session, independent of platform: the engine plus what the
//! interface needs — status, spectrum, log lines, concluded calls (with their
//! files) and live audio — as [`Output`]s. The platform supplies samples, the
//! wall clock (`now_ms`), the local date for folder names, and does the I/O.

use serde_json::{json, Value};
use trunk_core::metrics;
use trunk_core::trunk::{heard_code, Call, ChannelKind, Engine, Event, Identity, LiveAudio, MessageType, NoteLevel, ProtocolStatus, SystemId, TapSet};

use crate::filename;
use sdr_core::clock::{ClockConfig, ClockEvent, SampleClock};
use crate::log::{Body, CallState, CallTag, Level, Record};
use crate::heard::HeardCodes;
use crate::saving::Saved;
use trunk_core::Complex32;

use crate::config::{Config, SourceKind};
use crate::stats::{NameBook, Rollup, SampleMeter, Shared, Stats, Topics};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use trunk_recorder_plugin::{CallInfo, HostMessage, SystemStatus as PluginSystemStatus, TalkgroupName, UnitEvent};

use crate::protocol::{DropStage, DroppedCall, ToClient};
use trunk_core::trunk::calls::Reason;

pub enum Output {
    /// A message for the interface.
    Message(ToClient),
    /// Live audio of a call of `audio.system` — that system's number for this
    /// run (from 0, trunked then conventional: `status.systemNames`) — whose
    /// short name, its identity, is `short_name`. (A call heard on several
    /// sites of a system plays from one of them at a time.) `voice`: its
    /// vocoder frames ([`voice_chunk`]) while plugins take topic `voice` or
    /// interfaces listen with it (`voice_listened`); none for analog.
    Audio { short_name: String, audio: LiveAudio, voice: Option<trunk_recorder_plugin::VoiceChunk>, voice_listened: bool },
    /// A concluded call to store at `<rel>.wav` / `<rel>.json` (relative to
    /// the recordings folder); `record` is its call JSON, and [`call_entry`]
    /// its history entry, which the platform sends as a `concluded` message
    /// once the files are stored.
    /// `decrypted`: some of it was decrypted with our keys.
    File { rel: String, system: u16, wav: Vec<u8>, record: trunk_recorder_plugin::CallRecord, decrypted: bool },
    /// An event for plugins (only those [`Session::plugin_topics`] asks for).
    Plugin(HostMessage),
    /// A piece of a signal tap's stream, for the plugins subscribed to its topic. `save`: its
    /// stream is saved ([`crate::config::TapRoute`]), to files at this path (relative to the
    /// recordings folder, without an extension: the same for each chunk of the stream).
    Tap { chunk: trunk_recorder_plugin::TapChunk, save: Option<String> },
    /// A line for the log (the platform's logger formats and routes it).
    Log(Record),
    /// A message only for interfaces watching `topic` ([`crate::stats::Topics`]).
    Topic { topic: String, msg: ToClient },
    /// A minute of the dashboard's series, for the history (and its files).
    Rollup(Rollup),
    /// Something capture rules can be on (`on`), and what's known of it.
    Fact { on: &'static str, facts: Value },
    /// A frequency of system `short_name` is identified by this FCC call
    /// sign (Motorola's base station ID, on its control channel or the
    /// voice channel itself): once a run for each frequency and call sign.
    /// The platform puts it in the config's [`crate::config::FccLicenses`]
    /// ([`Config::learn_call_sign`]), saved.
    CallSign { short_name: String, freq_hz: f64, call_sign: String },
    /// System `short_name`'s share for the community directory is due
    /// ([`crate::share`]): the platform posts `observation` to the directory,
    /// links the answer into the config (`site_names`: the share's sites, in
    /// order: [`Config::link_shared`]) and says how it went ([`Session::share_done`]).
    Share { short_name: String, observation: Value, site_names: Vec<String> },
}

/// Trunk Recorder's status summary comes this often (engine time), s.
const STATUS_EVERY_S: f64 = 200.0;
/// The control channel decode rate is checked over this long, s.
const RATE_EVERY_S: f64 = 10.0;
/// Every series' running totals go to plugins this often, ms.
const TOTALS_EVERY_MS: f64 = 10_000.0;
/// A running source that sends nothing for this long is fed silence in its
/// place ([`Engine::push_gap`]), so time goes on: calls on it end, and its
/// clock stays in step with the others'. ms of wall clock.
const QUIET_MS: f64 = 1000.0;
/// A source's delivered rate is measured over this long, from one buffer's
/// arrival to another's: drivers hand samples over in chunks (an RTL-SDR's
/// are 131,072, 55 ms at 2.4 MS/s), and a window timed by the status tick
/// would count a whole one in or out of it. ms.
const RATE_WINDOW_MS: f64 = 5000.0;
/// A source's last error is shown this long after it, s.
const ERROR_SHOWN_S: f64 = 300.0;
/// A move of the wall clock this big or more is logged, ms
/// ([`Session::correct_wall_clock`]): a step, not drift.
const STEP_TOLD_MS: f64 = 500.0;
/// The most silence fed for one gap, s: a driver's count of samples lost.
const MAX_GAP_S: f64 = 10.0;

/// What plugins subscribe to, so nothing else is built.
#[derive(Clone, Copy, Debug, Default)]
pub struct PluginTopics {
    pub calls: bool,
    pub call_updates: bool,
    pub units: bool,
    pub status: bool,
    /// Every series' running totals ([`trunk_recorder_plugin::Totals`]).
    pub totals: bool,
    pub tone_patterns: bool,
    pub voice: bool,
    /// The signal taps subscribed to (topic `tap.<id>`): turned on for every system.
    pub taps: TapSet,
}

/// Live audio as the browsers take it, a binary frame: `[2][u16 system][u32 call][u32 tg][i16…]`.
pub fn audio_frame(a: &LiveAudio) -> Vec<u8> {
    let mut frame = Vec::with_capacity(11 + a.samples.len() * 2);
    frame.push(2u8);
    frame.extend_from_slice(&a.system.to_le_bytes());
    frame.extend_from_slice(&a.call_id.to_le_bytes());
    frame.extend_from_slice(&a.talkgroup.to_le_bytes());
    frame.extend(trunk_core::wav::pcm16(&a.samples));
    frame
}

/// Live voice as the vocoder frames its audio was made from (`unit_name`: the
/// talker's, "": none): what the `voice` plugin topic and interfaces listening
/// with `voice` are sent ([`Output::Audio`]). None: analog, no frames.
pub fn voice_chunk(system_id: u32, short_name: &str, unit_name: &str, a: &LiveAudio) -> Option<trunk_recorder_plugin::VoiceChunk> {
    (!a.frames.is_empty()).then(|| trunk_recorder_plugin::VoiceChunk {
        call_id: a.call_id,
        system: a.system,
        system_id,
        short_name: short_name.to_string(),
        talkgroup: a.talkgroup,
        unit: a.unit,
        unit_name: unit_name.to_string(),
        decrypted: a.decrypted,
        frames: a.frames.clone(),
    })
}

/// Local time's offset from UTC at a Unix time, s (east positive) — Trunk
/// Recorder's folders are in local time, which only the platform knows.
pub type LocalOffset = fn(i64) -> i32;

/// What sees each of the engine's events as a session handles it ([`Session::watch`]).
pub type EventWatch = Box<dyn FnMut(&Event, &Engine) + Send>;

/// A source's counts this run ([`Session::source_counts`]).
#[derive(Clone, Debug, Default)]
pub struct SourceCounts {
    pub samples: u64,
    pub dropped: u64,
    pub errors: u64,
    pub rate_measured: f64,
    /// The gain its radio is set to, when that isn't the gain asked for ([`Session::source_gain`]).
    pub gain_set: Option<String>,
}

#[derive(Default, Clone)]
struct SourceStats {
    samples: u64,
    dropped: u64,
    errors: u64,
    last_error: Option<String>,
    /// When it came, Unix s (the error is shown for [`ERROR_SHOWN_S`]).
    last_error_s: f64,
    rate_measured: f64,
    gain_set: Option<String>,
    /// The newest buffer's arrival (ms) and the samples to its end; and the
    /// same where the rate's window began ([`RATE_WINDOW_MS`]).
    arrived: Option<(f64, u64)>,
    rate_mark: Option<(f64, u64)>,
    ended: bool,
    /// Pushed to since the last poll.
    fed: bool,
    /// Wall clock (ms) when it was last fed or filled in.
    fed_ms: Option<f64>,
    /// Its sample clock against the wall clock (live sources only).
    clock: ClockFit,
    /// A [`Feeder`] feeds it from a thread of its own (and fills in its quiet spells there).
    threaded: bool,
    /// Samples found lost that the driver didn't report ([`ClockFit`]), and how many of them were told.
    unreported: u64,
    unreported_told: u64,
}

/// A source's sample clock against the wall clock. Sample time runs slow or
/// fast by the radio's crystal error, starts late by the time the radio took
/// to open, and jumps when samples are lost or silence is filled in for
/// more than was missing. Each buffer is measured when the driver handed it
/// over (`push`'s `at_ms`), so time spent queued for a busy engine doesn't
/// count, by sdr-core's [`SampleClock`]: the least-delayed handover each
/// second (NTP's clock filter), a phase-and-rate loop between, and a jump
/// held for 3 s beyond 5 ms re-anchored at once — positive, samples lost that
/// nobody reported (an RTL-SDR's FIFO overflowing on a busy USB bus: it can't
/// say so). The offset that makes sample time wall time is set on the engine
/// ([`Engine::set_clock_offset`]).
#[derive(Default, Clone)]
struct ClockFit {
    /// Whether the wall clock means anything for it (not a capture played
    /// as fast as it can be).
    live: bool,
    /// Its samples come over a link that loses none and delivers them in bursts, as they come to
    /// its server (a stream's): fitted loosely ([`BURSTY`]), and a step is the link's delay, not
    /// samples lost.
    bursty: bool,
    /// Samples fed, silence filled in for gaps included.
    samples: u64,
    /// From the first samples.
    clock: Option<SampleClock>,
    /// The offset last set, and whether the clock was re-anchored since.
    applied: Option<f64>,
    stepped: bool,
    /// Samples lost unreported, not yet taken.
    unreported: u64,
}

/// A bursty link's clock fit ([`ClockFit::bursty`]): a stream's server sends what it has as it has
/// it, a second's buffering either way being nothing out of the way.
const BURSTY: ClockConfig = ClockConfig { window_s: 5.0, tau_min_s: 30.0, tau_max_s: 600.0, step_threshold_s: 1.0, step_windows: 3 };

impl ClockFit {
    /// The samples so far ended at `at_ms` on the wall clock.
    fn observe(&mut self, at_ms: f64, epoch_ms: f64, rate: f64) {
        if !self.live {
            return;
        }
        let bursty = self.bursty;
        let clock = self.clock.get_or_insert_with(|| if bursty { SampleClock::with_config(rate, BURSTY) } else { SampleClock::new(rate) });
        if let Some(ClockEvent::Step { offset_s, .. }) = clock.observe(self.samples, (at_ms - epoch_ms) / 1000.0) {
            self.stepped = true;
            if offset_s > 0.0 && !bursty {
                self.unreported += (offset_s * rate).round() as u64;
            }
        }
    }

    /// The offset to set (wall time less sample time, s), when it moved, and
    /// whether the clock was re-anchored (a step, not drift).
    fn steer(&mut self, rate: f64) -> Option<(f64, bool)> {
        let clock = self.clock.as_ref()?;
        let next = clock.time_of(self.samples) - self.samples as f64 / rate;
        if self.applied == Some(next) {
            return None;
        }
        self.applied = Some(next);
        Some((next, std::mem::take(&mut self.stepped)))
    }

    /// The radio's measured crystal error, ppm (positive: fast), once it has run a while.
    fn ppm(&self) -> Option<f64> {
        self.clock.as_ref().filter(|c| c.tau() > 8.0).map(SampleClock::ppm)
    }
}

/// Feeds one source from a thread of its own ([`Session::feeder`]): its
/// samples to the engine (decoded under its source's own lock), its counts,
/// clock and sample meter to the session.
pub struct Feeder {
    source: usize,
    engine: Engine,
    stats: Arc<Mutex<SourceStats>>,
    meter: Arc<Mutex<SampleMeter>>,
    rate_hz: f64,
    epoch_ms: f64,
    busy_us: Arc<AtomicU64>,
}

impl Feeder {
    /// The source it feeds.
    pub fn source(&self) -> usize {
        self.source
    }

    /// Raw u8 IQ; as [`Session::push`].
    pub fn push(&self, bytes: &[u8], dropped: u64, at_ms: f64) {
        let t = Instant::now();
        self.gap(dropped);
        self.engine.push_u8(self.source, bytes);
        self.meter.lock().unwrap().u8(bytes);
        self.fed(bytes.len() as u64 / 2, at_ms);
        self.busy_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
    }

    /// Float IQ; as [`Session::push_iq`].
    pub fn push_iq(&self, iq: &[Complex32], dropped: u64, at_ms: f64) {
        let t = Instant::now();
        self.gap(dropped);
        self.engine.push_iq(self.source, iq);
        self.meter.lock().unwrap().iq(iq);
        self.fed(iq.len() as u64, at_ms);
        self.busy_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
    }

    /// A stream's audio ([`Source::audio`]), `channels` interleaved; as [`Feeder::push_iq`]. (A
    /// stream has no sample meter: that is of a radio's IQ.)
    pub fn push_audio(&self, audio: &[f32], channels: usize, dropped: u64, at_ms: f64) {
        let t = Instant::now();
        self.gap(dropped);
        self.engine.push_audio(self.source, audio, channels);
        self.fed((audio.len() / channels.max(1)) as u64, at_ms);
        self.busy_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
    }

    fn fed(&self, n: u64, at_ms: f64) {
        let mut s = self.stats.lock().unwrap();
        s.samples += n;
        s.arrived = Some((at_ms, s.samples));
        s.clock.samples += n;
        s.clock.observe(at_ms, self.epoch_ms, self.rate_hz);
        s.fed = true;
        // (Lost all the same: counted with what the driver reported, and told once a poll.)
        let lost = std::mem::take(&mut s.clock.unreported);
        if lost > 0 {
            s.unreported += lost;
            s.dropped += lost;
            drop(s);
            self.meter.lock().unwrap().dropped += lost;
        }
    }

    /// `n` samples the radio didn't make, on purpose (a change it halted
    /// for: [`trunk_core`]'s callers say so with the change): fed as
    /// silence, so time goes on, but not counted as lost.
    pub fn quiet(&self, n: u64) {
        let most = n.min((MAX_GAP_S * self.rate_hz) as u64);
        if most == 0 {
            return;
        }
        self.stats.lock().unwrap().clock.samples += most;
        self.engine.push_gap(self.source, most);
    }

    /// `dropped` samples lost: counted, and fed as silence.
    fn gap(&self, dropped: u64) {
        if dropped == 0 {
            return;
        }
        let most = dropped.min((MAX_GAP_S * self.rate_hz) as u64);
        {
            let mut s = self.stats.lock().unwrap();
            s.dropped += dropped;
            s.clock.samples += most;
        }
        self.meter.lock().unwrap().dropped += dropped;
        self.engine.push_gap(self.source, most);
    }

    /// Nothing came for a while (`now_ms`: on `poll`'s clock): once it's been
    /// quiet long enough while running (unplugged, a driver reopening),
    /// silence for the time it sent nothing.
    pub fn idle(&self, now_ms: f64) {
        let n = {
            let mut s = self.stats.lock().unwrap();
            if std::mem::take(&mut s.fed) || s.fed_ms.is_none() {
                // (Not before its first samples: a radio still opening isn't a gap.)
                if s.samples > 0 {
                    s.fed_ms = Some(now_ms);
                }
                return;
            }
            let since = s.fed_ms.unwrap_or(now_ms);
            if s.ended || now_ms - since < QUIET_MS {
                return;
            }
            s.fed_ms = Some(now_ms);
            let n = (((now_ms - since) / 1000.0).min(MAX_GAP_S) * self.rate_hz) as u64;
            s.clock.samples += n;
            n
        };
        self.engine.push_gap(self.source, n);
    }
}

/// A radio system's own clock against the one calls are stamped by
/// ([`Session::radio_clocks`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RadioClock {
    /// The site that said it.
    pub site: String,
    /// The system's time less the recording's, ms: positive when the
    /// recording's clock is behind the system's.
    pub offset_ms: f64,
    /// When it was heard, Unix s.
    pub at_s: f64,
    /// The system vouches for its time (P25's IST clear).
    pub valid: bool,
}

pub struct Session {
    cfg: Config,
    engine: Engine,
    /// How many of the frequencies the config lists each source is the
    /// first to reach ([`Config::placement`]): 0, and no voice channel on
    /// it this run, and it isn't needed.
    listed: Vec<usize>,
    /// (Each behind a lock: a source's [`Feeder`] updates it from its thread.)
    stats: Vec<Arc<Mutex<SourceStats>>>,
    /// Time each source's feeder spent decoding, µs (the load figure: the busiest's).
    busy: Vec<Arc<AtomicU64>>,
    /// The dashboard's measurements ([`crate::stats`]).
    meas: Stats,
    log: Vec<Value>,
    /// Log records not yet handed out.
    records: Vec<Record>,
    /// What the config set to receive on a source that isn't one, left out of this run, and why ([`Session::left_out`]).
    left_out: crate::config::Unreceivable,
    local_offset: LocalOffset,
    busy_ms: f64,
    start_ms: Option<f64>,
    last_status_ms: f64,
    last_spec_ms: f64,
    /// Build audio frames (skip when nobody is listening).
    pub want_audio: bool,
    /// Capture rules are on: tell them what happens ([`Output::Fact`]).
    pub want_facts: bool,
    /// Each site, as calls number them: its system's number and its name.
    sites: Vec<(u16, String, trunk_core::trunk::SiteId)>,
    /// Each system's recording rules, as calls number them ([`Config::recordings`]).
    recordings: Vec<crate::config::Recording>,
    pub plugin_topics: PluginTopics,
    /// Each system's signal taps, as the engine was last told ([`Session::set_taps`]).
    taps: Vec<TapSet>,
    /// The tap streams being saved, each with its files' path ([`Output::Tap`]).
    tap_files: std::collections::HashMap<u64, String>,
    /// How the computer's clock is kept ([`trunk_recorder_plugin::ClockState::sync`]), as the
    /// app last saw; none: the operating system doesn't say.
    pub clock_sync: Option<String>,
    /// Wall clock (Unix ms) at the engine's time 0, as at Start: the clock
    /// sources are measured against. What times are given out by is the
    /// engine's ([`Session::correct_wall_clock`]).
    epoch_ms: f64,
    /// The wall clock's correction last applied, ms ([`Session::correct_wall_clock`]).
    wall_correction_ms: f64,
    last_plugin_status_ms: f64,
    plugin_good: Vec<u64>,
    last_plugin_totals_ms: f64,
    /// The codes conventional frequencies carried ([`crate::heard`]).
    heard: HeardCodes,
    /// AutoTune scores by source label, as loaded and as last given to save.
    tune: serde_json::Map<String, Value>,
    tune_saved: String,
    /// When what's kept between runs is next due ([`Session::due`]).
    saving: crate::saving::Clock,
    /// Each system's control messages counted at (its time, the count), for the decode rate.
    rate_at: Vec<(f64, u64)>,
    next_status_s: f64,
    /// The call sign each frequency of each system (by number) was last heard to say.
    call_signs: std::collections::BTreeMap<(u16, u64), String>,
    /// What the systems shared with the directory have been heard doing, and when to share them.
    share: crate::share::Ledger,
    /// This install's anonymous ID for the directory (the platform keeps it); empty: nothing is shared.
    recorder_id: String,
    /// Given each of the engine's events before the session handles it (`replay` prints them).
    pub watch: Option<EventWatch>,
}

impl Session {
    /// `bandplan`: the band plan saved for a site, by its key
    /// ([`Session::bandplans`], [`bandplan_key`]).
    /// `names`: each system's talkgroups and radio names ([`crate::config::NameLists`]).
    pub fn new(mut cfg: Config, names: &crate::config::NameLists, epoch_ms: f64, bandplan: &dyn Fn(&str) -> Option<String>, local_offset: LocalOffset) -> Result<Session, String> {
        // What's set to receive on a source that isn't one is left out, and said; the rest records (#216).
        if let Some(p) = cfg.problem() {
            return Err(p);
        }
        let aside = cfg.set_aside_unreceivable();
        // (A config read is numbered; one built in code may not be.)
        cfg.assign_system_ids();
        let engine = Engine::new(cfg.engine_config(epoch_ms, names))?;
        let systems = engine.systems();
        let mut sites = Vec::new();
        for (i, (system, s)) in trunk_core::trunk::sites(&systems).enumerate() {
            if let Some(b) = bandplan(&bandplan_key(&systems[system as usize], s)).or_else(|| bandplan(crate::migrate::old_bandplan(s))) {
                engine.load_bandplan(i, &b);
            }
            sites.push((system, s.name.clone(), s.id));
        }
        let n = cfg.sources.len();
        // (A capture played as fast as it can be has no wall clock to keep to.)
        let stats = cfg
            .sources
            .iter()
            .map(|s| Arc::new(Mutex::new(SourceStats { clock: ClockFit { live: !matches!(s.kind, SourceKind::File { realtime: false, .. }), bursty: s.audio(), ..Default::default() }, ..Default::default() })))
            .collect();
        let recordings = cfg.recordings();
        let meas = Stats::new(&cfg, Shared::new(), local_offset);
        {
            let mut book = meas.shared().names.lock().unwrap();
            book.set_systems(&systems);
            book.set_rows(&cfg);
        }
        meas.shared().radio.lock().unwrap().set_caps(crate::stats::radio::Caps::of(&cfg.storage));
        let mut session = Session {
            meas,
            listed: cfg.placement().iter().map(|p| p.listed).collect(),
            cfg,
            engine,
            stats,
            busy: (0..n).map(|_| Default::default()).collect(),
            log: Vec::new(),
            records: Vec::new(),
            left_out: Default::default(),
            local_offset,
            busy_ms: 0.0,
            start_ms: None,
            last_status_ms: 0.0,
            last_spec_ms: 0.0,
            want_audio: true,
            want_facts: false,
            sites,
            recordings,
            plugin_topics: PluginTopics::default(),
            taps: Vec::new(),
            tap_files: Default::default(),
            clock_sync: None,
            epoch_ms,
            wall_correction_ms: 0.0,
            last_plugin_status_ms: 0.0,
            plugin_good: Vec::new(),
            last_plugin_totals_ms: 0.0,
            heard: HeardCodes::default(),
            tune: Default::default(),
            tune_saved: String::new(),
            saving: Default::default(),
            rate_at: Vec::new(),
            next_status_s: STATUS_EVERY_S,
            call_signs: Default::default(),
            share: Default::default(),
            recorder_id: String::new(),
            watch: None,
        };
        session.share = crate::share::Ledger::new(session.cfg.systems.iter());
        if let Some(why) = Some(&aside).filter(|a| !a.is_empty()) {
            session.records.push(Record::text(Level::Error, None, format!("Not started: {why}")));
        }
        session.left_out = aside;
        Ok(session)
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Use `shared` (the platform's, outliving sessions) for the radio
    /// registry and the event monitor, instead of this session's own.
    pub fn attach(&mut self, shared: Arc<Shared>) {
        self.meas = Stats::new(&self.cfg, shared, self.local_offset);
        let mut book = self.meas.shared().names.lock().unwrap();
        book.set_systems(&self.engine.systems());
        book.set_rows(&self.cfg);
    }

    /// What was left out of this run (set to receive on a source that isn't one), as said while it
    /// records: "Not started: …"; none when nothing was.
    pub fn left_out(&self) -> Option<String> {
        (!self.left_out.is_empty()).then(|| format!("Not started: {}", self.left_out))
    }

    /// The registry and monitor this session feeds.
    pub fn shared(&self) -> Arc<Shared> {
        self.meas.shared().clone()
    }

    /// What the connected interfaces watch: costly outputs are made for these only.
    pub fn set_topics(&mut self, topics: Topics) {
        self.meas.topics = topics;
    }

    /// A system's talkgroups are now `rows` (a list loaded): calls from now on go by them.
    pub fn set_talkgroups(&mut self, system: SystemId, rows: &[trunk_core::trunk::TalkgroupRow]) -> bool {
        let Some(i) = self.index_of(system) else { return false };
        let set = self.engine.set_talkgroups(i, crate::config::talkgroups_of(rows));
        self.meas.shared().names.lock().unwrap().set_systems(&self.engine.systems());
        set
    }

    /// One talkgroup of system `system`, as it is now (none: not one of its talkgroups).
    pub fn set_talkgroup(&mut self, system: SystemId, number: u32, row: Option<trunk_core::trunk::Talkgroup>) -> bool {
        let Some(i) = self.index_of(system) else { return false };
        self.meas.shared().names.lock().unwrap().set_talkgroup(system, number, row.clone());
        self.engine.set_talkgroup(i, number, row)
    }

    /// System `system`'s own names for its radios are now `rows`: calls from now on are named by them.
    /// The sites running, each with the source its control channel is on.
    pub fn sites(&self) -> Vec<trunk_core::trunk::SiteStatus> {
        self.engine.status().sites
    }

    pub fn set_unit_names(&mut self, system: SystemId, rows: &[trunk_core::trunk::UnitNameRow]) -> bool {
        let Some(i) = self.index_of(system) else { return false };
        let set = self.engine.set_unit_names(i, rows);
        self.meas.shared().names.lock().unwrap().set_systems(&self.engine.systems());
        set
    }

    /// Each system's signal taps — those saved to files (the config's) and those plugins
    /// subscribe to — told to the engine when they change: channels opened from then on give them.
    fn set_taps(&mut self) {
        let asked = self.plugin_topics.taps;
        let now: Vec<TapSet> = self.cfg.call_systems().iter().map(|s| self.cfg.recording.saved_taps(s.id).and(asked)).collect();
        if now != self.taps {
            for (i, &set) in now.iter().enumerate() {
                self.engine.set_taps(i as u16, set);
            }
            self.taps = now;
        }
    }

    /// The encryption keys of system `system` (trunked or
    /// conventional): voice channels opened from now on decrypt with them.
    pub fn set_keys(&mut self, system: SystemId, keys: Option<std::sync::Arc<trunk_core::p25::crypt::Keyring>>) -> bool {
        self.index_of(system).is_some_and(|i| self.engine.set_keys(i, keys))
    }

    /// A radio's talker alias on system `system`.
    pub fn unit_alias(&self, system: SystemId, unit: u32) -> Option<String> {
        self.engine.unit_alias(self.index_of(system)?, unit)
    }


    /// Raw u8 IQ from `source`; `dropped`: samples the driver knows it lost
    /// before these (fed as silence, so time keeps up); `at_ms`: when the
    /// driver handed them over, on `poll`'s clock.
    pub fn push(&mut self, source: usize, bytes: &[u8], dropped: u64, at_ms: f64) {
        let f = self.feed_of(source);
        f.gap(dropped);
        f.engine.push_u8(source, bytes);
        f.meter.lock().unwrap().u8(bytes);
        f.fed(bytes.len() as u64 / 2, at_ms);
    }

    /// Float IQ from `source` (USRP, Airspy, float captures); as [`push`](Self::push).
    pub fn push_iq(&mut self, source: usize, iq: &[Complex32], dropped: u64, at_ms: f64) {
        let f = self.feed_of(source);
        f.gap(dropped);
        f.engine.push_iq(source, iq);
        f.meter.lock().unwrap().iq(iq);
        f.fed(iq.len() as u64, at_ms);
    }

    fn feed_of(&self, source: usize) -> Feeder {
        Feeder {
            source,
            engine: self.engine.clone(),
            stats: self.stats[source].clone(),
            meter: self.meas.meter_handle(source),
            rate_hz: self.engine.sources()[source].rate_hz,
            epoch_ms: self.epoch_ms,
            busy_us: self.busy[source].clone(),
        }
    }

    /// Source `source` fed from a thread of its own: push its samples there
    /// ([`Feeder::push`]), each source's decoding on its own core. The
    /// session no longer fills in its quiet spells ([`Feeder::idle`] does).
    pub fn feeder(&mut self, source: usize) -> Feeder {
        self.stats[source].lock().unwrap().threaded = true;
        self.feed_of(source)
    }

    /// Fill in for sources that went quiet while running (unplugged, a
    /// driver reopening): silence for the time they've sent nothing.
    fn fill_quiet(&mut self, now_ms: f64) {
        for i in 0..self.stats.len() {
            if !self.stats[i].lock().unwrap().threaded {
                self.feed_of(i).idle(now_ms);
            }
        }
    }

    /// Set each live source's clock offset ([`ClockFit`]).
    fn steer_clocks(&mut self) {
        for i in 0..self.stats.len() {
            let new = {
                let mut s = self.stats[i].lock().unwrap();
                let n = s.unreported - s.unreported_told;
                s.unreported_told = s.unreported;
                n
            };
            if new > 0 {
                let ms = new as f64 / self.engine.sources()[i].rate_hz * 1000.0;
                let text = format!("{}: about {new} samples ({ms:.0} ms) lost without its driver saying so — the USB bus or the computer couldn't keep up", self.cfg.sources[i].title(i));
                self.records.push(Record::text(Level::Warning, None, text));
            }
            let rate = self.engine.sources()[i].rate_hz;
            let steered = self.stats[i].lock().unwrap().clock.steer(rate);
            let Some((offset, step)) = steered else { continue };
            if step {
                let moved = offset - self.engine.clock_offset(i);
                self.records.push(Record::text(Level::Info, None, format!("{}: clock re-anchored to the wall clock ({:+.3} s)", self.cfg.sources[i].title(i), moved)));
            }
            self.engine.set_clock_offset(i, offset);
        }
    }

    /// Time spent decoding (for the load figure), measured by the caller.
    pub fn add_busy_ms(&mut self, ms: f64) {
        self.busy_ms += ms;
    }

    /// Time spent decoding, ms: on the caller's thread, and the busiest feeder's.
    fn busy_ms(&self) -> f64 {
        self.busy_ms + self.busy.iter().map(|b| b.load(Ordering::Relaxed)).max().unwrap_or(0) as f64 / 1000.0
    }

    pub fn source_error(&mut self, source: usize, error: &str) {
        self.meas.meter(source).errors += 1;
        {
            let mut s = self.stats[source].lock().unwrap();
            s.errors += 1;
            s.last_error = Some(error.to_string());
            s.last_error_s = self.meas.now();
        }
        let text = format!("{}: {error}", self.cfg.sources.get(source).map_or_else(|| format!("Source {}", source + 1), |s| s.title(source)));
        self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": "error", "text": text }));
        self.records.push(Record::text(Level::Error, None, text));
    }

    /// The gain source `source`'s radio is set to, as it opened or changed:
    /// `set` when that isn't the gain `asked` for (the radio's nearest step, or
    /// within its range here), which the log says and the status shows.
    pub fn source_gain(&mut self, source: usize, asked: &str, set: Option<String>) {
        if let Some(set) = &set {
            let text = format!("{}: gain {asked} is set as {set} (the radio's nearest)", self.cfg.sources.get(source).map_or_else(|| format!("Source {}", source + 1), |s| s.title(source)));
            self.records.push(Record::text(Level::Info, None, text));
        }
        if let Some(s) = self.stats.get(source) {
            s.lock().unwrap().gain_set = set;
        }
    }

    /// A finite source ran out; true when all have.
    pub fn source_ended(&mut self, source: usize) -> bool {
        self.stats[source].lock().unwrap().ended = true;
        self.stats.iter().all(|s| s.lock().unwrap().ended)
    }

    /// Collect what happened since the last poll; status every 500 ms and
    /// spectra every 150 ms of wall clock. `now_ms`: the wall clock, Unix
    /// ms, on the same clock as [`Session::new`]'s `epoch_ms` and never
    /// jumping (the epoch plus a monotonic clock): sources' clocks are
    /// measured against it.
    pub fn poll(&mut self, now_ms: f64, out: &mut Vec<Output>) {
        let start = *self.start_ms.get_or_insert(now_ms);
        self.steer_clocks();
        self.meas.set_now(now_ms / 1000.0);
        self.fill_quiet(now_ms);
        self.set_taps();
        for ev in self.engine.drain_events() {
            self.handle(ev, out);
        }
        self.log_rates_and_status(out);
        out.extend(self.records.drain(..).map(Output::Log));
        if now_ms - self.last_spec_ms >= 150.0 {
            self.last_spec_ms = now_ms;
            // Only for sources someone has a waterfall open on.
            for (i, s) in self.engine.sources().iter().enumerate() {
                let topic = format!("spectrum:{i}");
                if !self.meas.topics.has(&topic) {
                    continue;
                }
                let bins: Vec<f32> = self.engine.spectrum(i, 512).iter().map(|v| (v * 10.0).round() / 10.0).collect();
                out.push(Output::Topic { topic, msg: ToClient::Spectrum { source: i, center_hz: s.center_hz, rate_hz: s.rate_hz, bins } });
            }
        }
        {
            let busy = self.busy_ms();
            let (cfg, sites) = (&self.cfg, &self.sites);
            let names = |system: u16, site: Option<u16>| {
                (cfg.short_name_of(system).unwrap_or("conv").to_string(), site.and_then(|s| sites.get(s as usize)).map_or(String::new(), |s| s.1.clone()))
            };
            self.meas.tick(now_ms, busy, &self.engine, &names, decode_sites, out);
        }
        if now_ms - self.last_status_ms >= 500.0 {
            self.last_status_ms = now_ms;
            for s in &self.stats {
                let mut s = s.lock().unwrap();
                let Some((at, n)) = s.arrived else { continue };
                // Quiet since its last buffer: measured to now, so the rate falls.
                let t = if now_ms - at > QUIET_MS { now_ms } else { at };
                match s.rate_mark {
                    Some((t0, _)) if t - t0 < RATE_WINDOW_MS => continue,
                    Some((t0, n0)) => s.rate_measured = (n - n0) as f64 * 1000.0 / (t - t0),
                    None => {}
                }
                s.rate_mark = Some((t, n));
            }
            self.flush_log(out);
            if self.heard.take_unshown() {
                out.push(Output::Message(ToClient::Heard { heard: self.heard.json() }));
            }
            if self.plugin_topics.status && now_ms - self.last_plugin_status_ms >= 5000.0 {
                let dt = if self.last_plugin_status_ms == 0.0 { 0.0 } else { (now_ms - self.last_plugin_status_ms) / 1000.0 };
                self.last_plugin_status_ms = now_ms;
                out.push(Output::Plugin(HostMessage::Status(self.plugin_status(dt))));
            }
            if self.plugin_topics.totals && now_ms - self.last_plugin_totals_ms >= TOTALS_EVERY_MS {
                self.last_plugin_totals_ms = now_ms;
                out.push(Output::Plugin(HostMessage::Totals(self.plugin_totals(start / 1000.0))));
            }
            self.poll_share(now_ms, out);
            let load = self.busy_ms() / (now_ms - start).max(1.0);
            out.push(Output::Message(self.status_message(load)));
        }
    }

    /// End of recording: release held frames, end every call.
    pub fn finish(&mut self, out: &mut Vec<Output>) {
        self.engine.finish();
        for ev in self.engine.drain_events() {
            self.handle(ev, out);
        }
        out.extend(self.records.drain(..).map(Output::Log));
        self.flush_log(out);
    }

    /// Preload each system's talker aliases: `units` gives the CSV saved
    /// for a system ([`Saved::Units`]).
    pub fn load_units(&mut self, units: &dyn Fn(SystemId) -> Option<String>) {
        for (i, id) in self.ids().into_iter().enumerate() {
            if let Some(csv) = units(id) {
                self.engine.load_units(i as u16, &csv);
                self.meas.shared().names.lock().unwrap().set_units(id, &csv);
            }
        }
    }

    /// Forget the talker aliases of system `system` (none: every system's),
    /// as deleted from the store: none is saved again unless heard again.
    pub fn forget_units(&mut self, system: Option<SystemId>) {
        let mut book = self.meas.shared().names.lock().unwrap();
        for (i, id) in self.ids().into_iter().enumerate().filter(|(_, id)| system.is_none_or(|s| s == *id)) {
            self.engine.load_units(i as u16, "");
            book.set_units(id, "");
        }
    }

    /// (system, talker alias CSV) of each system that learned an alias
    /// since the last call, to save; the names the dashboard shows take them.
    fn units_changed(&mut self) -> Vec<(SystemId, String)> {
        let changed: Vec<(SystemId, String)> = self.engine.units_changed().into_iter().map(|(i, csv)| (self.system_id(i), csv)).collect();
        let mut book = self.meas.shared().names.lock().unwrap();
        for (id, csv) in &changed {
            book.set_units(*id, csv);
        }
        changed
    }

    /// Start each AutoTune source from the score an earlier run kept (what
    /// [`Saved::Tune`] gave), shifted by any change to its ppm since.
    pub fn load_tune(&mut self, json: &str) {
        self.tune = serde_json::from_str(json).unwrap_or_default();
        self.tune_saved = serde_json::to_string_pretty(&self.tune).unwrap_or_default();
        for (i, c) in self.cfg.sources.iter().enumerate() {
            let v = self.tune.get(&c.label());
            let (e, at) = (v.and_then(|v| v["errorPpm"].as_f64()), v.and_then(|v| v["atPpm"].as_f64()));
            if let (true, Some(e), Some(at)) = (c.auto_tune, e, at) {
                self.engine.seed_tune(i, e + (c.ppm - at));
            }
        }
    }

    /// Each source's AutoTune score, to keep for the next run, as JSON
    /// `{label: {errorPpm, atPpm}}` (`atPpm`: the ppm set then), when it
    /// changed since the last call. Sources not measured keep what was loaded.
    fn tune_unsaved(&mut self) -> Option<String> {
        let st = self.engine.status();
        for (c, t) in self.cfg.sources.iter().zip(&st.sources) {
            if let Some(e) = t.error_ppm {
                self.tune.insert(c.label(), json!({ "errorPpm": (e * 100.0).round() / 100.0, "atPpm": c.ppm }));
            }
        }
        let text = serde_json::to_string_pretty(&self.tune).unwrap_or_default();
        (text != self.tune_saved && !self.tune.is_empty()).then(|| {
            self.tune_saved = text.clone();
            text
        })
    }

    /// Preload the codes conventional frequencies carried before (what
    /// [`Saved::Heard`] gave, saved as [`Session::HEARD_FILE`]).
    pub fn load_heard(&mut self, json: &str) {
        self.heard = HeardCodes::load(json);
    }

    /// What's kept between runs that is due to be written now ([`crate::saving`]
    /// says when each is); `all`: everything that changed (the recording's end).
    pub fn due(&mut self, now_ms: f64, all: bool) -> Vec<Saved> {
        let due = self.saving.tick(now_ms, all);
        let mut out = Vec::new();
        if due.soon {
            out.extend(self.units_changed().into_iter().map(|(system, csv)| Saved::Units { system, csv }));
            out.extend(self.share.unsaved().map(Saved::Sharing));
            for (key, plan) in self.bandplans() {
                if !plan.is_empty() && self.saving.plans.get(&key) != Some(&plan) {
                    self.saving.plans.insert(key.clone(), plan.clone());
                    out.push(Saved::Bandplan { key, plan });
                }
            }
        }
        if due.minute {
            out.extend(self.heard.take_unsaved().map(Saved::Heard));
            out.extend(self.tune_unsaved().map(Saved::Tune));
        }
        if due.registry {
            // (What hasn't been heard for the caps' days goes now, and is saved as gone.)
            let mut radio = self.meas.shared().radio.lock().unwrap();
            radio.expire((now_ms / 1000.0) as i64);
            let changes = radio.take_changes();
            drop(radio);
            out.extend(changes.into_iter().map(|(system, changes)| Saved::Registry { system, changes: Box::new(changes) }));
        }
        out
    }

    /// Write the radio registry every `ms` rather than [`crate::saving::REGISTRY_MS`]
    /// (the browser: a tab can be closed without the recording's end).
    pub fn save_registry_every(&mut self, ms: f64) {
        self.saving.registry_every_ms = Some(ms);
    }

    /// The file name (beside the config) the codes are kept in.
    pub const HEARD_FILE: &'static str = "conventional.heard.json";

    /// The voice channels' tapes finished since last asked.
    pub fn take_voice_tapes(&self) -> Vec<trunk_core::trunk::tape::VoiceTape> {
        self.engine.take_voice_tapes()
    }

    /// What the control channels have received lately.
    pub fn control_tapes(&self) -> Vec<trunk_core::trunk::tape::ControlTape> {
        self.engine.control_tapes()
    }

    /// Tape channels from now on, or stop.
    pub fn keep_tapes(&self, on: bool) {
        self.engine.keep_tapes(on);
    }

    /// How each source has done this run.
    pub fn source_counts(&self) -> Vec<SourceCounts> {
        self.stats
            .iter()
            .map(|s| {
                let s = s.lock().unwrap();
                SourceCounts { samples: s.samples, dropped: s.dropped, errors: s.errors, rate_measured: s.rate_measured, gain_set: s.gain_set.clone() }
            })
            .collect()
    }

    /// The engine's status, and each site's band plan in site order.
    pub fn status_and_bandplans(&self) -> (trunk_core::trunk::Status, Vec<String>) {
        (self.engine.status(), self.bandplans().into_iter().map(|b| b.1).collect())
    }

    /// Each site's (band plan key, band plan), to keep for the next run.
    fn bandplans(&self) -> Vec<(String, String)> {
        let systems = self.engine.systems();
        trunk_core::trunk::sites(&systems).enumerate().map(|(i, (system, s))| (bandplan_key(&systems[system as usize], s), self.engine.bandplan(i))).collect()
    }

    /// A system's short name (a call's `system`).
    /// The install's ID for the directory: sharing starts once there is one.
    pub fn set_recorder(&mut self, id: &str) {
        self.recorder_id = id.to_string();
    }

    /// Share first after `ms` of decoding, not 5 minutes (testing against a short capture).
    pub fn share_after(&mut self, ms: f64) {
        self.share.first_after(ms);
    }

    /// A system's config changed while recording: what sharing uses of it ([`crate::share::sync`]).
    pub fn set_share(&mut self, sys: &crate::config::System) {
        self.share.set(sys);
        if let Some(c) = self.cfg.systems.iter_mut().find(|s| s.short_name == sys.short_name) {
            crate::share::sync(c, sys);
        }
    }

    /// How system `short_name`'s share went.
    pub fn share_done(&mut self, short_name: &str, now_ms: f64, result: Result<Option<crate::share::Linked>, String>) {
        self.share.done(short_name, now_ms, result);
    }

    /// What sharing did before (what [`Saved::Sharing`] gave, kept as `sharing.json` beside the config).
    pub fn load_sharing(&mut self, json: &str) {
        self.share.load(json);
    }

    /// Each shared system: decoded now? due? its share, then.
    fn poll_share(&mut self, now_ms: f64, out: &mut Vec<Output>) {
        if self.recorder_id.is_empty() {
            return;
        }
        let st = self.engine.status();
        let systems = self.engine.systems();
        let shared: Vec<crate::config::System> = self.cfg.systems.iter().filter(|s| self.share.shares(&s.short_name)).cloned().collect();
        for sys in &shared {
            let sites: Vec<&trunk_core::trunk::SiteStatus> = st.sites.iter().filter(|y| y.short_name == sys.short_name).collect();
            if sites.iter().any(|y| y.good > 0 && y.mismatch.is_none()) {
                self.share.decoding(&sys.short_name, now_ms);
            }
            if !self.share.due(&sys.short_name, now_ms) {
                continue;
            }
            let Some(ev) = self.share.evidence(&sys.short_name) else { continue };
            let tgs = systems.iter().find(|y| y.short_name == sys.short_name).map(|y| &y.talkgroups);
            if let Some((observation, site_names)) = crate::share::observation(&self.recorder_id, concat!("trunk-pro/", env!("CARGO_PKG_VERSION")), sys, &sites, ev, tgs) {
                out.push(Output::Share { short_name: sys.short_name.clone(), observation, site_names });
            }
        }
        for o in out.iter() {
            if let Output::Share { short_name, .. } = o {
                self.share.sent(short_name);
            }
        }
    }

    fn system_name(&self, system: u16) -> &str {
        self.cfg.short_name_of(system).unwrap_or("conv")
    }

    /// The id of system `system` (a call's), as this run's config has it.
    pub fn system_id(&self, system: u16) -> SystemId {
        self.cfg.call_systems().get(system as usize).map_or_else(Default::default, |s| s.id)
    }

    /// System `id`'s short name, as this run's config has it ("": not one of the run's).
    pub fn short_name(&self, id: SystemId) -> &str {
        self.index_of(id).map_or("", |i| self.system_name(i))
    }

    /// The number calls of system `id` carry this run (none: not one of the run's).
    pub fn index_of(&self, id: SystemId) -> Option<u16> {
        self.cfg.call_systems().iter().find(|s| s.id == id).map(|s| s.index)
    }

    /// Each system's id, as calls number them.
    fn ids(&self) -> Vec<SystemId> {
        self.cfg.call_systems().iter().map(|s| s.id).collect()
    }

    /// The system of site `site` (a call's `site`).
    fn system_of(&self, site: u16) -> u16 {
        self.sites.get(site as usize).map_or(0, |s| s.0)
    }

    /// Site `site`'s name (empty: none, a conventional call's).
    fn site_name(&self, site: Option<u16>) -> String {
        site.and_then(|s| self.sites.get(s as usize)).map_or(String::new(), |s| s.1.clone())
    }

    /// Site `site`'s id, as plugins know it (0: none).
    fn site_id(&self, site: Option<u16>) -> u32 {
        site.and_then(|s| self.sites.get(s as usize)).map_or(0, |s| s.2 .0)
    }

    /// How the log names system `system` (its site `site`): "dcfd", or "dcfd/south".
    fn site_label(&self, system: u16, site: Option<u16>) -> String {
        trunk_core::trunk::SiteKey { system: self.system_name(system), site: &self.site_name(site) }.to_string()
    }

    /// The talkgroups patched with call `c`'s, named (its own aside: the call names that one),
    /// as the interface's calls and plugins' events both list them.
    fn patched(&self, c: &Call) -> Vec<TalkgroupName> {
        named(&self.meas.shared().names.lock().unwrap(), self.system_id(c.system), c.patched_talkgroups.iter().copied().filter(|&t| t != c.talkgroup))
    }

    /// Talkgroup `tg`'s name in system `system` ([`NameBook::tg`]; "": none), as plugins' events give it (`alpha_tag`).
    /// (Where there's a call, its own name: a conventional row names its calls more closely than a number can.)
    fn alpha_tag(&self, system: u16, tg: u32) -> String {
        self.meas.shared().names.lock().unwrap().alpha_tag(self.system_id(system), tg)
    }

    /// Radio `unit`'s name ("": none), as a call's JSON gives it (`tag`): by
    /// the rule [`NameBook::unit`] names it by, on the engine's aliases (an
    /// alias heard is the engine's at once, the book's when next saved).
    fn unit_tag(&self, system: u16, unit: u32) -> String {
        self.engine.unit_name(system, unit).unwrap_or_default()
    }

    /// Unix seconds of engine time `s`.
    fn wall(&self, s: f64) -> f64 {
        self.wall_of(s, s)
    }

    /// Unix seconds of engine time `s`, a time of a call begun at
    /// `start_s`: by the epoch it began under, as its record is.
    fn wall_of(&self, s: f64, start_s: f64) -> f64 {
        (self.engine.wall_epoch_at(start_s) + s * 1000.0) / 1000.0
    }

    /// The host's wall clock now reads `correction_ms` more than the clock
    /// `poll` is given (the epoch at Start carried on by a monotonic clock):
    /// a time service corrected it, or the monotonic clock runs at its
    /// crystal's rate (macOS's). Times given out from now on — calls begun
    /// from now, units, status — are by the corrected clock; calls up keep
    /// the epoch they began under. The engine's own time, and the sources'
    /// clocks, are untouched. A capture played as fast as it can be has no
    /// wall clock to keep to: ignored. True when it moved.
    pub fn correct_wall_clock(&mut self, correction_ms: f64) -> bool {
        if !correction_ms.is_finite() || self.cfg.sources.iter().any(|s| matches!(s.kind, SourceKind::File { realtime: false, .. })) {
            return false;
        }
        if !self.engine.set_wall_epoch(self.epoch_ms + correction_ms) {
            return false;
        }
        let moved = correction_ms - std::mem::replace(&mut self.wall_correction_ms, correction_ms);
        // (Drift moves it a ms at a time; a step is worth saying.)
        if moved.abs() >= STEP_TOLD_MS {
            self.records.push(Record::text(Level::Info, None, format!("The wall clock moved {:+.3} s: times are by it from now on", moved / 1000.0)));
        }
        true
    }

    /// The radio systems' own clocks (P25's SYNC_BCST, on Phase 2 systems)
    /// against the clock calls are stamped by: each site's latest. None from
    /// a capture: it carries its own day's.
    pub fn radio_clocks(&self) -> Vec<RadioClock> {
        self.engine.status().sites.iter().filter_map(|y| self.radio_clock(y)).collect()
    }

    /// Site `y`'s radio clock ([`Session::radio_clocks`]).
    fn radio_clock(&self, y: &trunk_core::trunk::SiteStatus) -> Option<RadioClock> {
        let (t, system_ms, valid) = y.system_time.filter(|_| !self.cfg.sources.iter().any(|s| matches!(s.kind, SourceKind::File { .. })))?;
        let at_s = self.wall(t);
        Some(RadioClock { site: y.site_name.clone(), offset_ms: system_ms as f64 - at_s * 1000.0, at_s, valid })
    }

    /// The log header for a call.
    fn call_tag(c: &Call) -> CallTag {
        CallTag {
            num: c.id as u64,
            talkgroup: c.talkgroup,
            tag: c.talkgroup_info.as_ref().map_or(String::new(), |t| t.alpha_tag.clone()),
            encrypted: c.encrypted,
            freq_hz: c.freq_hz as f64,
            ota: c.talkgroup_ota,
        }
    }

    fn call_record(&self, level: Level, c: &Call, body: Body) -> Record {
        Record { level, system: Some(self.site_label(c.system, c.site)), call: Some(Self::call_tag(c)), body }
    }

    /// Trunk Recorder's control channel decode rate check, and its status summary.
    fn log_rates_and_status(&mut self, _out: &mut [Output]) {
        let st = self.engine.status();
        let warn = self.cfg.log.control_warn_rate_per_s;
        self.rate_at.resize(self.sites.len(), (f64::NAN, 0));
        for y in &st.sites {
            let i = y.site as usize;
            let (t0, n0) = self.rate_at[i];
            if t0.is_nan() || y.now_s < t0 {
                self.rate_at[i] = (y.now_s, y.good);
                continue;
            }
            if y.now_s - t0 < RATE_EVERY_S {
                continue;
            }
            let per_s = y.good.saturating_sub(n0) as f64 / (y.now_s - t0);
            self.rate_at[i] = (y.now_s, y.good);
            let level = if per_s < warn { Level::Error } else if warn == -1.0 { Level::Info } else { continue };
            let body = Body::DecodeRate { freq_hz: y.control_channel_hz.map(|f| f as f64), per_s, count: y.good - n0 };
            self.records.push(Record { level, system: Some(self.site_label(y.system, Some(y.site))), call: None, body });
        }
        if st.now_s < self.next_status_s {
            return;
        }
        self.next_status_s = st.now_s + STATUS_EVERY_S;
        let calls = self.engine.active_calls();
        self.records.push(Record::text(Level::Info, None, format!("Active Calls: {}", calls.len())));
        for c in &calls {
            let state = if c.recording { CallState::Recording } else { CallState::Monitoring(c.reason.map_or("", |r| r.as_str())) };
            let r = self.call_record(Level::Info, c, Body::State { elapsed_s: (st.now_s - c.start_s).max(0.0), state });
            self.records.push(r);
        }
        self.records.push(Record::text(Level::Info, None, format!("Recorders: {} recording, {} channels open", st.recording, st.channels_open)));
        self.records.push(Record::text(Level::Info, None, "Control Channel Decode Rates:"));
        for y in &st.sites {
            let freq = y.control_channel_hz.map_or("-".to_string(), |f| self.cfg.log.format().freq(f as f64));
            let rate = if y.now_s > 0.0 { y.good as f64 / y.now_s } else { 0.0 };
            self.records.push(Record::text(Level::Info, Some(&self.site_label(y.system, Some(y.site))), format!("{freq}\t{rate:.1} msg/sec")));
        }
    }

    fn call_info(&self, c: &Call) -> CallInfo {
        CallInfo {
            id: c.id,
            system: c.system,
            system_id: self.system_id(c.system).0,
            short_name: self.system_name(c.system).to_string(),
            site: self.site_name(c.site),
            site_id: self.site_id(c.site),
            talkgroup: c.talkgroup,
            talkgroup_ota: c.talkgroup_ota,
            talkgroup_tag: c.talkgroup_info.as_ref().map_or(String::new(), |t| t.alpha_tag.clone()),
            freq_hz: c.freq_hz,
            tdma_slot: c.voice.slot(),
            analog: c.voice.analog(),
            encrypted: c.encrypted,
            algid: c.cipher.map(|k| k.algid),
            key_id: c.cipher.map(|k| k.keyid),
            decrypted: c.decrypted,
            emergency: c.emergency,
            duplex: c.duplex,
            packet: c.mode,
            priority: c.priority,
            recording: c.recording,
            reason: c.reason.map(|r| r.as_str().to_string()),
            start_time: self.wall_of(c.start_s, c.start_s),
            units: c.sources.iter().map(|s| s.src).collect(),
            sources: c
                .sources
                .iter()
                .map(|s| {
                    let f = s.flags;
                    trunk_recorder_plugin::CallSource { unit: s.src, tag: self.unit_tag(c.system, s.src), time: self.wall_of(s.time_s, c.start_s), emergency: f.emergency, encrypted: f.encrypted, duplex: f.duplex, packet: f.packet, priority: f.priority }
                })
                .collect(),
            patched_talkgroups: c.patched_talkgroups.clone(),
            patched: self.patched(c),
        }
    }

    /// `dt`: seconds since the last one (0: the first).
    fn plugin_status(&mut self, dt: f64) -> trunk_recorder_plugin::Status {
        let st = self.engine.status();
        self.plugin_good.resize(self.sites.len(), 0);
        let systems = st
            .sites
            .iter()
            .map(|y| {
                let i = y.site as usize;
                let rate = if dt > 0.0 { y.good.saturating_sub(self.plugin_good[i]) as f64 / dt } else { 0.0 };
                self.plugin_good[i] = y.good;
                PluginSystemStatus { scope: Self::site_scope(y), decode_rate: (rate * 10.0).round() / 10.0, active_calls: y.active_calls as u32, recording: y.recording as u32 }
            })
            .collect();
        trunk_recorder_plugin::Status { time: self.wall(st.now_s), systems }
    }

    /// Which site `y` is, as plugins are told ([`trunk_recorder_plugin::SiteScope`]).
    fn site_scope(y: &trunk_core::trunk::SiteStatus) -> trunk_recorder_plugin::SiteScope {
        trunk_recorder_plugin::SiteScope {
            index: y.system,
            system_id: y.system_id.0,
            short_name: y.short_name.clone(),
            site_name: y.site_name.clone(),
            site_id: y.site_id.0,
            key: y.key.clone(),
            protocol: y.kind.to_string(),
            control_channel_hz: y.control_channel_hz,
            source: y.source,
            symbol_rate: y.symbol_rate,
            identity: y.identity.to_site(),
        }
    }

    /// Every series' running totals for plugins, and which sites, channels and radios they're
    /// of (`since`: when recording started, Unix s).
    fn plugin_totals(&self, since: f64) -> trunk_recorder_plugin::Totals {
        use trunk_recorder_plugin::{ChannelScope, ClockState, SourceScope};
        let sites = self.engine.status().sites.iter().map(Self::site_scope).collect();
        let channels = self
            .engine
            .channels()
            .into_iter()
            .filter_map(|c| {
                let ChannelKind::Conventional(mode) = c.kind else { return None };
                let short_name = self.system_name(c.system).to_string();
                Some(ChannelScope { key: metrics::channel_key(&short_name, c.freq_hz as f64), index: c.system, short_name, freq_hz: c.freq_hz, mode: mode.as_str().to_string(), names: c.names, access: c.access, source: c.source })
            })
            .collect();
        let sources = self.cfg.sources.iter().enumerate().map(|(i, s)| SourceScope { key: format!("src/{}", metrics::key_part(&s.label())), index: i, name: s.label(), driver: s.kind_name().to_string(), auto_tune: s.auto_tune }).collect();
        // The recorder's clock less the systems': the latest any site said.
        let system_offset_ms = self.radio_clocks().into_iter().filter(|c| c.valid).max_by(|a, b| a.at_s.total_cmp(&b.at_s)).map(|c| -c.offset_ms);
        let clock = (self.clock_sync.is_some() || system_offset_ms.is_some()).then(|| ClockState { sync: self.clock_sync.clone(), system_offset_ms });
        trunk_recorder_plugin::Totals { time: self.meas.now(), since, series: self.meas.totals(), sites, channels, sources, clock }
    }

    fn flush_log(&mut self, out: &mut Vec<Output>) {
        if !self.log.is_empty() {
            out.push(Output::Topic { topic: "log".into(), msg: ToClient::Log { lines: std::mem::take(&mut self.log) } });
        }
    }

    /// The control channel log's lines are made only while someone shows it.
    fn logging(&self) -> bool {
        self.meas.topics.has("log")
    }

    fn handle(&mut self, ev: Event, out: &mut Vec<Output>) {
        if let Some(w) = self.watch.as_mut() {
            w(&ev, &self.engine);
        }
        if let Some(sys) = self.event_system(&ev) {
            let name = self.system_name(sys).to_string();
            self.meas.observe(&ev, self.system_id(sys), &name, &self.engine, out);
        }
        match ev {
            Event::Message { site, msg: m } => {
                let system = self.system_of(site);
                let name = self.system_name(system).to_string();
                // For plugins taking topic `unit`, and interfaces subscribed to it: the same event.
                let watched = self.meas.topics.has(UNIT);
                if self.plugin_topics.units || watched {
                    if let Some(kind) = unit_kind(m.kind) {
                        let talkgroup = matches!(kind, "affiliation" | "location" | "answer_request" | "call_alert" | "emergency_alarm").then_some(m.talkgroup).filter(|&tg| tg != 0);
                        if m.source >= 0 {
                            let unit = m.source as u32;
                            let (tag, alpha_tag) = (self.unit_tag(system, unit), talkgroup.map(|tg| self.alpha_tag(system, tg)).unwrap_or_default());
                            let e = UnitEvent { system, system_id: self.system_id(system).0, short_name: name.clone(), site: self.site_name(Some(site)), site_id: self.site_id(Some(site)), kind: kind.into(), unit, tag, talkgroup, alpha_tag, emergency: m.emergency, time: self.wall(m.time_s) };
                            if watched {
                                out.push(Output::Topic { topic: UNIT.into(), msg: ToClient::UnitEvent { event: e.clone() } });
                            }
                            if self.plugin_topics.units {
                                out.push(Output::Plugin(HostMessage::Unit(e)));
                            }
                        }
                    }
                }
                self.records.push(Record::text(Level::Trace, Some(&self.site_label(system, Some(site))), m.meta.clone()));
                if self.logging() {
                    self.log.push(json!({ "timeS": m.time_s, "kind": m.kind.as_str(), "text": m.meta, "system": name, "site": self.site_name(Some(site)) }))
                }
            }
            Event::ControlChannel { site, freq_hz } => {
                let system = self.system_of(site);
                let name = self.system_name(system).to_string();
                self.records.push(Record { level: Level::Info, system: Some(self.site_label(system, Some(site))), call: None, body: Body::ControlChannel { freq_hz: freq_hz as f64 } });
                self.log.push(json!({ "timeS": 0, "kind": "control", "text": format!("Control channel {:.5} MHz", freq_hz as f64 / 1e6), "system": name, "site": self.site_name(Some(site)) }))
            }
            Event::Note { system, site, level, text } => {
                let name = self.system_name(system).to_string();
                if self.want_facts && matches!(level, NoteLevel::Warning) {
                    out.push(Output::Fact { on: "note.warning", facts: json!({ "note": { "system": name, "site": self.site_name(site), "text": text } }) });
                }
                let (level, kind) = match level {
                    NoteLevel::Info => (Level::Info, "note"),
                    NoteLevel::Warning => (Level::Warning, "error"),
                };
                self.records.push(Record::text(level, Some(&self.site_label(system, site)), text.clone()));
                self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": kind, "text": text, "system": name, "site": self.site_name(site) }))
            }
            Event::Audio(audio) => {
                if !self.want_audio {
                    return;
                }
                let short_name = self.system_name(audio.system).to_string();
                let voice_listened = self.meas.topics.has("voice");
                let voice = (self.plugin_topics.voice || voice_listened)
                    .then(|| {
                        let unit_name = if audio.unit != 0 { self.unit_tag(audio.system, audio.unit) } else { String::new() };
                        voice_chunk(self.system_id(audio.system).0, &short_name, &unit_name, &audio)
                    })
                    .flatten();
                out.push(Output::Audio { short_name, audio, voice, voice_listened });
            }
            Event::Tap { time_s, mut chunk } => {
                let id = self.system_id(chunk.system);
                chunk.system_id = id.0;
                chunk.short_name = self.system_name(chunk.system).to_string();
                chunk.time = self.wall(time_s);
                // Saved from its start, when a row says so (a tap on for a plugin alone isn't): each
                // stream beside its system's calls, named for when and where it began.
                if chunk.first && self.cfg.recording.saved_taps(id).has(chunk.tap) {
                    let start = chunk.time as i64;
                    let base = format!("{start}_{:.0}-tap_{}.{}", chunk.freq_hz, chunk.stream, chunk.tap);
                    self.tap_files.insert(chunk.stream, filename::default_path(&chunk.short_name, &base, start, (self.local_offset)(start)));
                }
                let save = if chunk.last { self.tap_files.remove(&chunk.stream) } else { self.tap_files.get(&chunk.stream).cloned() };
                out.push(Output::Tap { chunk, save });
            }
            Event::ConvSkipped { freq_hz, code } => {
                let ms = self.wall(self.engine.status().now_s) * 1000.0;
                self.heard.note(freq_hz, &code, false, ms);
            }
            Event::Concluded(k) => {
                if let Some(code) = heard_code(&k.call) {
                    self.heard.note(k.call.freq_hz, &code, true, self.wall_of(k.call.start_s, k.call.start_s) * 1000.0);
                }
                // Trunk Recorder's layout: <shortName>/<year>/<month>/<day>/, local time; or the system's filename format.
                let r = &k.record;
                let offset = (self.local_offset)(r.start_time);
                let rules = self.recordings.get(k.call.system as usize);
                let format = rules.map_or("", |r| r.filename_format.as_str());
                let rel = if format.trim().is_empty() {
                    let path = filename::default_path(&k.short_name, &k.base_name, r.start_time, offset);
                    // A call per transmission: two may begin in the same second on one channel.
                    if rules.is_some_and(|r| !r.conversation_mode) { filename::numbered(&path, r.call_num) } else { path }
                } else {
                    // (A format's tokens are the record's field names.)
                    filename::render(format, &serde_json::to_value(r).unwrap_or_default(), k.call.system, offset)
                };
                let body = Body::Concluded { length_s: k.audio.len() as f64 / 8000.0, signal_db: r.signal, noise_db: r.noise, snr_db: r.snr, clean_pct: r.clean_voice_pct, ended: r.end_reason.clone() };
                out.push(Output::File { rel, system: k.call.system, wav: trunk_core::wav::encode(&k.audio, 8000), record: k.record, decrypted: k.call.decrypted });
                let r = self.call_record(Level::Info, &k.call, body);
                self.records.push(r);
            }
            // Once a run for each frequency and call sign: the platform takes it into the config.
            Event::CallSign { system, freq_hz, call_sign } => {
                let short = self.system_name(system).to_string();
                self.share.call_sign(&short, freq_hz, &call_sign);
                if self.call_signs.get(&(system, freq_hz)) != Some(&call_sign) {
                    self.call_signs.insert((system, freq_hz), call_sign.clone());
                    let name = self.system_name(system).to_string();
                    self.records.push(Record::text(Level::Info, Some(&name), format!("{:.4} MHz is call sign {call_sign} (base station ID)", freq_hz as f64 / 1e6)));
                    out.push(Output::CallSign { short_name: name, freq_hz: freq_hz as f64, call_sign });
                }
            }
            Event::TonePattern(t) => {
                let name = self.system_name(t.system).to_string();
                let pattern = trunk_core::trunk::record::tone_pattern(&t.pattern, t.src, t.signals.clone());
                let named = t.signals.iter().map(|s| format!("{name}:{s}")).collect::<Vec<_>>().join(", ");
                // Its talkgroup named as its call is (a conventional channel's by its row).
                let tag = if t.alpha_tag.is_empty() { String::new() } else { format!(" {}", t.alpha_tag) };
                let text = format!("TG {}{tag}: {}{}", t.talkgroup, pattern.text, if named.is_empty() { String::new() } else { format!(" = {named}") });
                self.records.push(Record::text(Level::Info, Some(&self.site_label(t.system, t.site)), text.clone()));
                self.log.push(json!({ "timeS": t.time_s, "kind": "tonePattern", "text": text, "system": name, "site": self.site_name(t.site) }));
                if self.plugin_topics.tone_patterns {
                    let alpha_tag = t.alpha_tag.clone();
                    let time = (self.wall(t.time_s) * 1000.0).round() / 1000.0;
                    let ev = trunk_recorder_plugin::TonePatternEvent { short_name: name, system_id: self.system_id(t.system).0, site: self.site_name(t.site), call_num: t.call_id as u64, talkgroup: t.talkgroup, alpha_tag, src_tag: self.unit_tag(t.system, t.src), decrypted: t.decrypted, time, pattern };
                    out.push(Output::Plugin(HostMessage::TonePattern(ev)));
                }
            }
            Event::UnitAlias { system, unit, alias, talkgroup } => {
                let name = self.system_name(system).to_string();
                self.records.push(Record::text(Level::Info, Some(&name), format!("Unit {unit} is \"{alias}\" (TG {talkgroup})")));
                self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": "alias", "text": format!("Unit {unit} is \"{alias}\" (TG {talkgroup})"), "system": name }));
                out.push(Output::Message(ToClient::UnitAlias { system: name, unit, alias }));
            }
            Event::UnitPosition { system, unit, lat, lon, talkgroup } => {
                // (Each transmission may move it: a trace line, as control messages are.)
                let name = self.system_name(system).to_string();
                let text = if talkgroup == 0 { format!("Unit {unit} at {lat:.5}, {lon:.5}") } else { format!("Unit {unit} at {lat:.5}, {lon:.5} (TG {talkgroup})") };
                self.records.push(Record::text(Level::Trace, Some(&name), text.clone()));
                if self.logging() {
                    self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": "position", "text": text, "system": name }));
                }
            }
            Event::Duplicate { call, kept } => {
                let tag = call.talkgroup_info.as_ref().map_or(String::new(), |t| format!(" {}", t.alpha_tag));
                let (kept_at, also_at) = (self.site_name(kept.site), self.site_name(call.site));
                let text = format!("TG {}{tag} was heard on sites {kept_at} and {also_at}: saved {kept_at}'s copy", call.talkgroup);
                self.records.push(Record::text(Level::Info, Some(&self.site_label(call.system, call.site)), text.clone()));
                self.log.push(json!({ "timeS": call.start_s, "kind": "duplicate", "text": text, "system": self.system_name(call.system), "site": also_at }));
            }
            Event::CopyStart(c) => {
                let short = self.system_name(c.system).to_string();
                self.share.copy(&short, &c);
                let r = match c.reason {
                    _ if c.recording => {
                        use trunk_core::trunk::Voice;
                        let kind = match c.voice {
                            Voice::Fm | Voice::Am => "Analog",
                            Voice::Nxdn(trunk_core::nxdn::Rate::N48) => "NXDN48",
                            Voice::Nxdn(_) => "NXDN96",
                            Voice::Dmr { .. } => "DMR",
                            Voice::P25Tdma { .. } => "P25 Phase 2",
                            Voice::P25 => "P25",
                            Voice::ProVoice => "ProVoice",
                        };
                        let slot = c.voice.slot();
                        self.call_record(Level::Info, &c, Body::Recording { kind, slot })
                    }
                    Some(why) => {
                        let level = match why.as_str() {
                            "no_source" => Level::Error,
                            "no_recorder" => Level::Warning,
                            _ => Level::Info,
                        };
                        self.call_record(level, &c, Body::NotRecording(why.as_str()))
                    }
                    None => self.call_record(Level::Debug, &c, Body::Text("Following (not recorded)".into())),
                };
                self.records.push(r);
            }
            Event::CopyEnd(_) => {}
            Event::CallStart(c) => {
                let short = self.system_name(c.system).to_string();
                self.share.call(&short, c.talkgroup);
                if self.plugin_topics.calls {
                    out.push(Output::Plugin(HostMessage::CallStart(self.call_info(&c))));
                }
            }
            Event::NotSaved(u) => {
                if self.want_facts {
                    out.push(Output::Fact { on: "call.not_saved", facts: self.unsaved_facts(&u).facts() });
                }
                let r = self.call_record(Level::Info, &u.call, Body::Dropped);
                self.records.push(r);
                let d = DroppedCall {
                    source: Some(u.on.source),
                    signal: u.reception.signal_db(),
                    noise: u.reception.noise_db(),
                    voice_frames: Some(u.frames),
                    bad_frames: Some(u.bad_frames),
                    ..self.dropped(&u.call, u.call_num as u64, DropStage::NotSaved, u.why.key())
                };
                out.push(Output::Message(ToClient::CallDropped(d)));
            }
            Event::CallEnd(c) => {
                if self.plugin_topics.calls {
                    out.push(Output::Plugin(HostMessage::CallEnd(self.call_info(&c))));
                }
                // Followed, and never recorded for want of a radio (calls passed over on purpose aren't lost).
                if let Some(why) = c.reason.filter(|r| !c.recording && matches!(r, Reason::NoSource | Reason::NoRecorder)) {
                    out.push(Output::Message(ToClient::CallDropped(self.dropped(&c, c.id as u64, DropStage::NotRecorded, why.key()))));
                }
            }
            Event::CallUpdate(c) => {
                if self.plugin_topics.call_updates {
                    out.push(Output::Plugin(HostMessage::CallUpdate(self.call_info(&c))));
                }
            }
        }
    }

    /// Call `c` (its call's number `call_num`), lost at `stage` for `why`: what
    /// any lost call says (what it had when recorded is the caller's).
    fn dropped(&self, c: &Call, call_num: u64, stage: DropStage, why: &'static str) -> DroppedCall {
        DroppedCall {
            call_num,
            system: self.system_name(c.system).to_string(),
            site: Some(self.site_name(c.site)).filter(|s| !s.is_empty()),
            talkgroup: c.talkgroup,
            alpha_tag: c.talkgroup_info.as_ref().map_or(String::new(), |t| t.alpha_tag.clone()),
            freq_hz: c.freq_hz,
            source: None,
            start_time: self.wall_of(c.start_s, c.start_s),
            length_s: (c.last_update_s - c.start_s).max(0.0),
            stage,
            why,
            detail: None,
            signal: None,
            noise: None,
            voice_frames: None,
            bad_frames: None,
        }
    }

    /// A call of system `system` saved by the engine whose files then couldn't be
    /// written (the platform writes them): counted as not saved.
    pub fn write_failed(&mut self, system: u16) {
        let name = self.system_name(system).to_string();
        self.meas.call_not_saved(&name, trunk_core::trunk::NotSaved::WriteFailed);
    }

    /// A call not saved, as capture rules see it.
    fn unsaved_facts(&self, u: &trunk_core::trunk::Unsaved) -> crate::facts::CallFacts {
        let c = &u.call;
        let wall = |s: f64| self.wall_of(s, c.start_s);
        let db = |v: Option<f64>| v.unwrap_or(0.0);
        let units: Vec<u32> = c.sources.iter().map(|s| s.src).collect();
        crate::facts::CallFacts {
            system: self.system_name(c.system).to_string(),
            site: self.site_name(c.site),
            talkgroup: c.talkgroup,
            freq: c.freq_hz,
            start_time: wall(c.start_s),
            stop_time: wall(c.last_update_s.max(c.start_s)),
            call_length: (c.last_update_s - c.start_s).max(0.0),
            clean_voice_pct: if u.frames > 0 { (u.frames - u.bad_frames) as f64 / u.frames as f64 * 100.0 } else { 0.0 },
            snr: db(u.reception.snr_db()),
            signal: db(u.reception.signal_db()),
            noise: db(u.reception.noise_db()),
            encrypted: c.encrypted,
            decrypted: c.decrypted,
            emergency: c.emergency,
            analog: c.voice.analog(),
            voice: c.voice.mode_name().to_string(),
            tdma_slot: c.voice.slot().unwrap_or(0) as i64,
            units: units.len(),
            unit_ids: units,
            not_saved: u.why.key().to_string(),
            audio_s: u.audio_s,
            frames: u.frames,
            bad_frames: u.bad_frames,
            recorder: u.on.recorder as i64,
            source: u.on.source as i64,
            ..Default::default()
        }
    }

    /// The system an event is counted for in the stats.
    fn event_system(&self, ev: &Event) -> Option<u16> {
        match ev {
            Event::Message { site, .. } => Some(self.system_of(*site)),
            Event::CallStart(c) | Event::CallEnd(c) => Some(c.system),
            Event::NotSaved(u) => Some(u.call.system),
            Event::Concluded(k) => Some(k.call.system),
            Event::Duplicate { call, .. } => Some(call.system),
            Event::TonePattern(t) => Some(t.system),
            _ => None,
        }
    }

    fn status_message(&self, load: f64) -> ToClient {
        let st = self.engine.status();
        let sources: Vec<Value> = self
            .cfg
            .sources
            .iter()
            .zip(self.engine.sources())
            .zip(&self.stats)
            .enumerate()
            .map(|(i, ((s, sc), ss))| {
                let ss = ss.lock().unwrap();
                let label = s.label();
                let tune = st.sources.get(i).copied().unwrap_or_default();
                // An error long past isn't news (its count stays).
                let recent = ss.last_error.as_ref().filter(|_| ss.last_error_s == 0.0 || self.meas.now() - ss.last_error_s < ERROR_SHOWN_S);
                json!({ "index": i, "label": label, "centerHz": sc.center_hz, "rateHz": sc.rate_hz, "rateMeasured": ss.rate_measured, "gainSet": ss.gain_set,
                        "dropped": ss.dropped, "errors": ss.errors, "lastError": recent, "lastErrorS": recent.filter(|_| ss.last_error_s > 0.0).map(|_| ss.last_error_s.round()), "ended": ss.ended,
                        "errorPpm": tune.error_ppm, "tunePpm": tune.applied_ppm, "tuneSeeded": tune.seeded,
                        // The ppm that removes the error: it was measured against the ppm this run opened the radio with.
                        "ppmApply": tune.error_ppm.map(|e| s.settable_ppm(s.ppm - e)),
                        // The settings it runs with: Setup's may have changed since Start.
                        "settings": s,
                        "voiceChannels": tune.voice_channels,
                        // Nothing listed needs it and no call has used it: it can probably go.
                        "unused": self.listed.get(i) == Some(&0) && tune.voice_channels == 0,
                        "clockPpm": ss.clock.ppm().map(|p| (p * 100.0).round() / 100.0) })
            })
            .collect();
        // Each running site, as its control channel describes it.
        let site_json = |y: &trunk_core::trunk::SiteStatus| {
            json!({
                "index": y.site, "name": y.site_name, "nowS": y.now_s, "controlChannelHz": y.control_channel_hz, "source": y.source,
                "identity": identity_json(&y.identity),
                "good": y.good, "bad": y.bad, "modulation": if y.modulation.is_empty() { None } else { Some(y.modulation) },
                "activeCalls": y.active_calls, "recording": y.recording, "callsConcluded": y.calls_concluded,
                "mismatch": y.mismatch,
                "adjacent": y.adjacent,
                "callsigns": y.callsigns.iter().map(|(c, hz)| json!({ "callSign": c, "freqHz": if *hz == 0 { None } else { Some(hz) } })).collect::<Vec<_>>(),
                "radioClock": self.radio_clock(y).map(|r| json!({ "offsetMs": (r.offset_ms * 10.0).round() / 10.0, "at": r.at_s.round(), "valid": r.valid })),
            })
        };
        // Each system being recorded (trunked or conventional), its running sites in order.
        let systems: Vec<Value> = st
            .systems
            .iter()
            .map(|v| {
                json!({
                    "index": v.system, "shortName": v.short_name, "share": self.share.status_json(&v.short_name),
                    "activeCalls": v.active_calls, "recording": v.recording, "conventionalOpen": v.conventional_open, "callsConcluded": v.calls_concluded,
                    "sites": st.sites.iter().filter(|y| y.system == v.system).map(site_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        let calls = self
            .engine
            .active_calls()
            .iter()
            .map(|c| {
                let mut v = call_view(c, self.system_name(c.system), self.patched(c).iter().map(tg_view).collect());
                let site = self.site_name(c.site);
                if !site.is_empty() {
                    v["site"] = json!(site);
                }
                let twins: Vec<String> = self.engine.twins(c.id).iter().map(|&(_, at)| self.site_name(Some(at))).collect();
                if !twins.is_empty() {
                    v["alsoOn"] = json!(twins);
                }
                v
            })
            .collect();
        let status = json!({
            "nowS": st.now_s,
            "activeCalls": st.active_calls, "recording": st.recording, "channelsOpen": st.channels_open, "conventionalOpen": st.conventional_open,
            "callsConcluded": st.calls_concluded,
            "systems": systems,
            // Every system's short name, by the number its calls and audio carry.
            "systemNames": self.cfg.call_systems().iter().map(|s| s.short_name).collect::<Vec<_>>(),
        });
        ToClient::Status { status, sources, load, calls }
    }
}

/// The unit events plugins hear about, by their name there.
/// The topic of what radios do on the control channel (`unitEvent`), for interfaces.
pub const UNIT: &str = "unit";

fn unit_kind(k: MessageType) -> Option<&'static str> {
    Some(match k {
        MessageType::Registration => "registration",
        MessageType::Deregistration => "deregistration",
        MessageType::Affiliation => "affiliation",
        MessageType::Acknowledge => "acknowledge",
        MessageType::Location => "location",
        MessageType::DataGrant => "data_grant",
        MessageType::UuAnsReq => "answer_request",
        MessageType::CallAlert => "call_alert",
        MessageType::EmergencyAlarm => "emergency_alarm",
        _ => return None,
    })
}

/// A stored call's history entry (`concluded`, `history`, protocol.ts
/// `CallEntry`): where it is under the recordings folder, and its record.
pub fn call_entry(rel: &str, record: &trunk_recorder_plugin::CallRecord) -> Value {
    json!({ "path": rel, "record": record })
}

/// A site's identity as messages carry it: the config's [`crate::config::SiteIdentity`]
/// (`{nac, wacn, sysId, rfss, site, colorCode, network, ran}`, a field not stated yet left out).
pub(crate) fn identity_json(id: &Identity) -> Value {
    json!(id.to_site())
}

/// The name a site's band plan is kept under ([`trunk_core::trunk::SiteKey::file_stem`]).
pub fn bandplan_key(sys: &trunk_core::trunk::SystemConfig, s: &trunk_core::trunk::SiteConfig) -> String {
    sys.site_key(s).file_stem()
}

fn call_view(c: &Call, system_name: &str, patched: Vec<Value>) -> Value {
    json!({
        "id": c.id,
        "system": c.system,
        "systemName": system_name,
        "talkgroup": c.talkgroup,
        // Conventional digital: the talkgroup the air named.
        "talkgroupOta": c.talkgroup_ota,
        "alphaTag": c.talkgroup_info.as_ref().map_or("", |t| t.alpha_tag.as_str()),
        "freqHz": c.freq_hz,
        "slot": c.voice.slot(),
        "analog": c.voice.analog(),
        "nxdn": c.voice.nxdn().map(|r| r.name()),
        "ran": c.ran,
        // "151.4 Hz" or "D023N".
        "tone": c.tone.map(|h| h.tone.label()),
        "state": if c.recording { "recording" } else { "monitoring" },
        "reason": c.reason.map(|r| r.as_str()),
        "encrypted": c.encrypted,
        "decrypted": c.decrypted,
        "emergency": c.emergency,
        "startS": c.start_s,
        "sources": c.sources.iter().map(|s| s.src).collect::<Vec<_>>(),
        "patched": patched,
    })
}


/// Talkgroups of system `system` with their names ([`NameBook::tg`]).
fn named(names: &NameBook, system: SystemId, tgs: impl IntoIterator<Item = u32>) -> Vec<TalkgroupName> {
    tgs.into_iter().map(|talkgroup| TalkgroupName { talkgroup, alpha_tag: names.alpha_tag(system, talkgroup) }).collect()
}

/// A named talkgroup as the interface shows it (its `TalkgroupName`).
fn tg_view(t: &TalkgroupName) -> Value {
    json!({ "talkgroup": t.talkgroup, "alphaTag": t.alpha_tag })
}

/// [`named`], as the interface shows them: number and alpha tag ("": none).
fn tg_names(names: &NameBook, system: SystemId, tgs: impl IntoIterator<Item = u32>) -> Vec<Value> {
    named(names, system, tgs).iter().map(tg_view).collect()
}

/// A system's `decodeDetail` beyond its channels: each running site's
/// DMR / NXDN channels and carriers, and the system's patches
/// (each site hears them: once each).
fn decode_sites(engine: &Engine, names: &NameBook, system: &str) -> (Vec<Value>, Vec<Value>) {
    let (mut sites, mut patches) = (Vec::new(), Vec::<Value>::new());
    let id = engine.systems().iter().find(|s| s.short_name == system).map_or_else(Default::default, |s| s.id);
    for y in engine.status().sites.iter().filter(|y| y.short_name == system) {
        let (dmr, nxdn) = protocol_detail(&y.protocol, &|tg| tg_names(names, id, [tg]).remove(0));
        sites.push(json!({ "index": y.site, "name": y.site_name, "dmr": dmr, "nxdn": nxdn }));
        for (sg, members) in &y.patches {
            let p = json!({ "supergroup": tg_names(names, id, [*sg])[0], "members": tg_names(names, id, members.iter().copied()) });
            if !patches.iter().any(|h| h["supergroup"] == p["supergroup"]) {
                patches.push(p);
            }
        }
    }
    (sites, patches)
}

/// A DMR or NXDN site's channel table and carriers (its kind, colour code or
/// RAN, rest channel…), as (DMR, NXDN): the other is none. `tg`: a
/// talkgroup on a carrier as it's shown.
pub fn protocol_detail(p: &ProtocolStatus, tg: &dyn Fn(u32) -> Value) -> (Option<Value>, Option<Value>) {
    (
        match p { ProtocolStatus::Dmr(d) => Some(d), _ => None }.map(|d| {
            json!({
                "variant": d.variant.map(|v| v.name()),
                "colorCode": d.color_code,
                // Tier III / Capacity Max: its System Identity Code, beside the network and
                // site it gives the identity.
                "sysCode": d.sys_code.map(|c| json!({ "code": c.0, "model": c.model(), "category": c.category() })),
                "rest": d.rest.map(|(lsn, hz)| json!({ "lsn": lsn, "freqHz": if hz > 0 { Some(hz) } else { None } })),
                "keyed": d.keyed,
                "channels": d.channels.iter().map(|c| json!({ "lcn": c.lcn, "freqHz": c.hz, "configured": c.configured })).collect::<Vec<_>>(),
                "carriers": d.carriers.iter().map(|c| json!({
                    "freqHz": c.hz, "control": c.control, "colorCode": c.color_code,
                    "slots": c.slots.iter().map(|s| s.map(|(t, src)| json!({ "talkgroup": tg(t), "source": src }))).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        }),
        match p { ProtocolStatus::Nxdn(n) => Some(n), _ => None }.map(|n| {
            json!({
                "kind": n.kind.name(),
                "rate": n.rate.map(|r| r.name()),
                "location": n.location.map(|(cat, sys, site)| json!({ "category": cat, "system": sys, "site": site })),
                "ran": n.ran,
                "dfa": n.dfa.map(|(base, step)| json!({ "baseHz": base, "stepHz": step })),
                "channels": n.channels.iter().map(|c| json!({ "channel": c.number, "freqHz": c.hz, "configured": c.configured })).collect::<Vec<_>>(),
                "unknownChannels": n.unknown,
                "carriers": n.carriers.iter().map(|c| json!({
                    "freqHz": c.hz, "control": c.control, "ran": c.ran, "repeater": c.repeater,
                    "call": c.call.map(|(t, src)| json!({ "talkgroup": tg(t), "source": src })),
                })).collect::<Vec<_>>(),
            })
        }),
    )
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    /// A live clock fed `secs` of 1 MS/s samples, handed over every 100 ms
    /// with 20 ms (+ up to 8 ms) of latency, plus `extra` samples beyond the
    /// wall time at `jump_at` s; steered every 100 ms. The offsets set.
    fn run(secs: f64, ppm: f64, jump_at: f64, extra: u64) -> (Vec<(f64, bool)>, ClockFit) {
        let mut c = ClockFit { live: true, ..Default::default() };
        let (mut set, mut t, mut x) = (Vec::new(), 0.0, 12345u64);
        while t < secs {
            t += 0.1;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            c.samples = (t * 1e6 * (1.0 + ppm * 1e-6)) as u64 + if t > jump_at { extra } else { 0 };
            c.observe(1000.0 * (t + 0.02 + (x % 8000) as f64 / 1e6), 0.0, 1e6);
            if let Some(s) = c.steer(1e6) {
                set.push(s);
            }
        }
        (set, c)
    }

    #[test]
    fn crystal_drift_is_followed_without_steps() {
        let (set, c) = run(600.0, 20.0, f64::INFINITY, 0);
        assert!(set.iter().all(|&(_, step)| !step));
        // The offset: the least latency, less the 20 ppm the samples run ahead.
        let last = set.last().unwrap().0;
        assert!((last - (0.02 - 20e-6 * 600.0)).abs() < 0.001, "{last}");
        // (Its rate settles over minutes: the loop slows as it locks, against the host's jitter.)
        assert!((c.ppm().unwrap() - 20.0).abs() < 2.5, "{:?}", c.ppm());
    }

    #[test]
    fn samples_gained_or_lost_are_re_anchored() {
        // 2 s of samples more than the wall clock allows at 30 s (a stalled
        // radio's backlog after its quiet was filled in): one step, no loss.
        let (set, c) = run(60.0, 0.0, 30.0, 2_000_000);
        let steps: Vec<f64> = set.iter().filter(|s| s.1).map(|s| s.0).collect();
        assert_eq!(steps.len(), 1, "{set:?}");
        assert!((steps[0] - (0.02 - 2.0)).abs() < 0.01, "{steps:?}");
        assert_eq!(c.unreported, 0);
    }
}

#[cfg(test)]
mod unreported_tests {
    use super::*;

    /// An RTL-SDR's stream: 54.6 ms blocks at 2.4 MS/s, arriving up to 8 ms
    /// late, its crystal 60 ppm slow; after `lose_at` s, `lose_s` of samples
    /// simply aren't there. Returns the samples found lost unreported.
    fn run(secs: f64, lose_at: f64, lose_s: f64) -> u64 {
        let rate = 2_400_000.0;
        let mut c = ClockFit { live: true, ..Default::default() };
        let (block, mut sent, mut lost, mut x) = (131_072u64, 0u64, false, 12345u64);
        let mut found = 0;
        loop {
            // Wall time the block's last sample was made at (60 ppm slow), plus the device's missing stretch.
            let mut t = (sent + block) as f64 / (rate * (1.0 - 60e-6));
            if lost {
                t += lose_s;
            }
            if t > secs {
                break;
            }
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let jitter = (x % 8000) as f64 / 1e6;
            sent += block;
            c.samples += block;
            c.observe((t + 0.05 + jitter) * 1000.0, 0.0, rate);
            found += std::mem::take(&mut c.unreported);
            if !lost && t >= lose_at {
                lost = true;
            }
        }
        found
    }

    #[test]
    fn drift_and_jitter_are_no_loss() {
        assert_eq!(run(600.0, f64::INFINITY, 0.0), 0);
    }

    #[test]
    fn samples_missing_are_found() {
        let n = run(180.0, 90.0, 0.02);
        let want = (0.02 * 2_400_000.0) as u64;
        assert!(n.abs_diff(want) < want / 10, "found {n} of {want} lost");
    }
}

#[cfg(test)]
mod wall_tests {
    use super::*;

    /// What plugins are told of a call up across a wall clock correction:
    /// all its times by the epoch it began under, so a talker who came on
    /// after the correction is still where it was in the call.
    #[test]
    fn a_calls_live_times_keep_the_epoch_it_began_under() {
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "conventional": [{ "shortName": "conv", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire" }] }]
        }))
        .unwrap();
        let epoch = 1.75e12;
        let mut s = Session::new(cfg, &Default::default(), epoch, &|_| None, |_| 0).unwrap();
        // 2 s of samples: the engine's time is 2 s.
        s.push_iq(0, &vec![Complex32::new(0.0, 0.0); 4_800_000], 0, epoch + 2000.0);
        let mut out = Vec::new();
        s.poll(epoch + 2000.0, &mut out);
        assert!(s.engine.status().now_s >= 1.9, "{}", s.engine.status().now_s);
        assert!(s.correct_wall_clock(10_000.0));
        // Begun at 1 s, under the old epoch; a second talker at 2.5 s, after the correction.
        let src = |src: u32, time_s: f64| trunk_core::trunk::calls::CallSource { src, time_s, ..Default::default() };
        let call = Call { start_s: 1.0, sources: vec![src(7, 1.0), src(9, 2.5)], ..Default::default() };
        let info = s.call_info(&call);
        assert_eq!(info.start_time, 1_750_000_001.0);
        assert_eq!(info.sources.iter().map(|x| x.time).collect::<Vec<_>>(), [1_750_000_001.0, 1_750_000_002.5]);
        // One begun after it is by the new epoch.
        let later = Call { start_s: 3.0, sources: vec![src(7, 3.0)], ..Default::default() };
        assert_eq!(s.call_info(&later).start_time, 1_750_000_013.0);
        // Unit events, status and the like: by the epoch at their own time.
        assert_eq!(s.wall(3.0), 1_750_000_013.0);
        assert_eq!(s.wall(0.5), 1_750_000_000.5);
    }
}

#[cfg(test)]
mod patch_tests {
    use super::*;

    /// A patched call names the talkgroups patched with its own (its own aside, which the call
    /// names), to plugins and to the interface alike: one list, by the system's talkgroups ("": none).
    #[test]
    fn a_calls_patched_talkgroups_are_named() {
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "conventional": [{ "shortName": "conv", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire" }] }]
        }))
        .unwrap();
        let mut s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        let named = |number: u32, tag: &str| trunk_core::trunk::Talkgroup { number, alpha_tag: tag.into(), ..Default::default() };
        let system = s.system_id(0);
        s.set_talkgroup(system, 202, Some(named(202, "EMS")));
        let patched = Call { talkgroup: 101, talkgroup_info: Some(named(101, "Fire Dispatch")), patched_talkgroups: vec![101, 202, 303], ..Default::default() };
        let info = s.call_info(&patched);
        assert_eq!(info.patched_talkgroups, [101, 202, 303]);
        assert_eq!(info.talkgroup_tag, "Fire Dispatch");
        assert_eq!(info.patched, [TalkgroupName { talkgroup: 202, alpha_tag: "EMS".into() }, TalkgroupName { talkgroup: 303, alpha_tag: String::new() }]);
        // The interface's call lists the same ones.
        let view = call_view(&patched, "conv", s.patched(&patched).iter().map(tg_view).collect());
        assert_eq!(view["patched"], json!([{ "talkgroup": 202, "alphaTag": "EMS" }, { "talkgroup": 303, "alphaTag": "" }]));
        // On the wire a talkgroup with no name has no `alpha_tag`.
        assert_eq!(serde_json::to_value(&info).unwrap()["patched"], json!([{ "talkgroup": 202, "alpha_tag": "EMS" }, { "talkgroup": 303 }]));
        let alone = Call { talkgroup: 101, talkgroup_info: Some(named(101, "Fire Dispatch")), ..Default::default() };
        assert!(s.call_info(&alone).patched.is_empty(), "not patched: none");
    }
}

#[cfg(test)]
mod tone_tests {
    use super::*;

    /// A tone on a conventional channel names its talkgroup as its call does
    /// (by the channel's row): in the log, the dashboard's feed and plugins' event.
    #[test]
    fn a_tone_names_its_talkgroup_everywhere_it_goes() {
        use trunk_core::dsp::inband::{pattern::{Pattern, PatternKind}, Origin};
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "conventional": [{ "shortName": "wood", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire Dispatch" }] }]
        }))
        .unwrap();
        let mut s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        s.plugin_topics.tone_patterns = true;
        let pattern = Pattern { kind: PatternKind::TwoTone, hz: vec![470.5, 1034.7], lengths: vec![1.0, 3.0], digits: String::new(), knox: false, cycles: 0, format: Some("Quick Call II"), start: 0, len: 0, origin: Origin::Audio };
        let heard = trunk_core::trunk::PatternHeard { system: 0, site: None, call_id: 7, talkgroup: 1, alpha_tag: "Fire Dispatch".into(), src: 0, pattern, signals: vec!["FD_PAGE".into()], decrypted: false, time_s: 1.0 };
        let mut out = Vec::new();
        s.handle(Event::TonePattern(heard), &mut out);
        assert!(s.log.iter().any(|l| l["kind"] == "tonePattern" && l["text"].as_str().is_some_and(|t| t.starts_with("TG 1 Fire Dispatch: "))), "{:?}", s.log);
        let plugin = out.iter().find_map(|o| match o { Output::Plugin(HostMessage::TonePattern(e)) => Some(e.alpha_tag.clone()), _ => None });
        assert_eq!(plugin.as_deref(), Some("Fire Dispatch"));
        let feed = out.iter().find_map(|o| match o { Output::Message(ToClient::MonitorEvent(b)) => b.get("alphaTag").cloned(), _ => None });
        assert_eq!(feed, Some(json!("Fire Dispatch")));
    }

    /// A call lost says so once, with why: recorded and not kept (with what it
    /// had), or never recorded for want of a radio; one passed over on purpose doesn't.
    #[test]
    fn a_lost_call_is_said_once_with_why() {
        use trunk_core::trunk::record::{RecordedOn, Reception};
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "conventional": [{ "shortName": "wood", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire Dispatch" }] }]
        }))
        .unwrap();
        let mut s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        let call = |id, reason| Call { id, talkgroup: 1, freq_hz: 155_200_000, start_s: 1.0, last_update_s: 16.0, reason, ..Default::default() };
        let unsaved = trunk_core::trunk::Unsaved { call: Call { recording: true, ..call(7, None) }, call_num: 7, why: trunk_core::trunk::NotSaved::Silent, audio_s: 0.0, frames: 0, bad_frames: 0, on: RecordedOn { recorder: 0, source: 0 }, reception: Reception::default() };
        let mut out = Vec::new();
        s.handle(Event::NotSaved(unsaved), &mut out);
        s.handle(Event::CallEnd(call(8, Some(Reason::NoRecorder))), &mut out);
        s.handle(Event::CallEnd(call(9, Some(Reason::Encrypted))), &mut out);
        let dropped: Vec<Value> = out.iter().filter_map(|o| match o { Output::Message(m @ ToClient::CallDropped(_)) => Some(m.to_value()), _ => None }).collect();
        assert_eq!(dropped.len(), 2, "the encrypted grant isn't lost: {dropped:?}");
        let pick = |v: &Value| (v["callNum"].clone(), v["system"].clone(), v["stage"].clone(), v["why"].clone(), v["lengthS"].clone());
        assert_eq!(pick(&dropped[0]), (json!(7), json!("wood"), json!("notSaved"), json!("silent"), json!(15.0)));
        assert_eq!((&dropped[0]["source"], &dropped[0]["voiceFrames"], dropped[0].get("site")), (&json!(0), &json!(0), None), "what it had; a conventional call has no site");
        assert_eq!(pick(&dropped[1]), (json!(8), json!("wood"), json!("notRecorded"), json!("noRecorder"), json!(15.0)));
        assert_eq!((dropped[1].get("source"), dropped[1].get("voiceFrames")), (None, None), "never recorded");
    }

    /// An RTL-SDR whose samples hardly move (the corpus capture at 25 dB:
    /// ±3 counts) is a `sourceQuiet` event once, after a minute; turned up,
    /// `sourceRecovered`. Clipping is still its own.
    #[test]
    fn a_source_too_quiet_for_a_minute_is_an_event() {
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "conventional": [{ "shortName": "wood", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire Dispatch" }] }]
        }))
        .unwrap();
        let mut s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        let iq = |counts: u8| (0..4096).map(|i| if i % 2 == 0 { 128 + counts } else { 127 - counts }).collect::<Vec<u8>>();
        let mut t = 1.75e12;
        // (Its meter and ticks alone: a second of 2.4 MS/s through the engine each would be slow.)
        let mut seconds = |s: &mut Session, b: &[u8], n: usize| {
            let mut kinds = Vec::new();
            for _ in 0..n {
                t += 1000.0;
                s.meas.meter(0).u8(b);
                s.meas.set_now(t / 1000.0);
                let mut out = Vec::new();
                s.meas.tick(t, 0.0, &s.engine, &|_, _| (String::new(), String::new()), decode_sites, &mut out);
                kinds.extend(out.iter().filter_map(|o| match o { Output::Message(ToClient::MonitorEvent(b)) => Some((b["kind"].as_str().unwrap_or("").to_string(), b.get("peakDb").cloned())), _ => None }));
            }
            kinds
        };
        assert_eq!(seconds(&mut s, &iq(3), 70), [("sourceQuiet".to_string(), Some(json!(-31.0)))]);
        assert_eq!(seconds(&mut s, &iq(20), 60), [("sourceRecovered".to_string(), None)]);
        let rails: Vec<u8> = (0..4096).map(|i| if i % 2 == 0 { 255 } else { 0 }).collect();
        assert_eq!(seconds(&mut s, &rails, 3).first().map(|k| k.0.as_str()), Some("sourceClipping"));
    }

    /// Live voice's frames are built only while plugins take topic `voice` or
    /// interfaces listen with it, and go to interfaces only in the second case.
    #[test]
    fn live_voice_is_built_for_whoever_takes_it() {
        use trunk_recorder_plugin::{Codec, FrameKind, VoiceFrame};
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "conventional": [{ "shortName": "wood", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire Dispatch" }] }]
        }))
        .unwrap();
        let mut s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        let frame = VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind: FrameKind::Voice, tone: None };
        let audio = LiveAudio { call_id: 7, system: 0, talkgroup: 1, unit: 0, samples: vec![0.0; 160], frames: vec![frame], decrypted: false };
        let live = |s: &mut Session| {
            let mut out = Vec::new();
            s.handle(Event::Audio(audio.clone()), &mut out);
            out.into_iter().find_map(|o| match o { Output::Audio { voice, voice_listened, .. } => Some((voice.is_some(), voice_listened)), _ => None }).unwrap()
        };
        assert_eq!(live(&mut s), (false, false), "nobody takes it");
        s.plugin_topics.voice = true;
        assert_eq!(live(&mut s), (true, false), "plugins only");
        s.plugin_topics.voice = false;
        s.set_topics(crate::stats::Topics::new(["voice".to_string()]));
        assert_eq!(live(&mut s), (true, true), "an interface listens with voice");
    }

    /// A radio's control channel events are built only while plugins take
    /// topic `unit` or an interface subscribes to it, and go to each that does.
    #[test]
    fn unit_events_go_to_whoever_takes_them() {
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 858000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "systems": [{ "shortName": "dcfd", "type": "p25", "sites": [{ "name": "north", "controlChannelsHz": [857_987_500.0] }] }]
        }))
        .unwrap();
        let mut s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        let aff = trunk_core::trunk::Message { kind: MessageType::Affiliation, source: 1234, talkgroup: 101, ..Default::default() };
        let sent = |s: &mut Session| {
            let mut out = Vec::new();
            s.handle(Event::Message { site: 0, msg: aff.clone() }, &mut out);
            let topic = out.iter().find_map(|o| match o { Output::Topic { topic, msg: ToClient::UnitEvent { event } } => Some((topic.clone(), event.clone())), _ => None });
            let plugin = out.iter().find_map(|o| match o { Output::Plugin(HostMessage::Unit(e)) => Some(e.clone()), _ => None });
            (topic, plugin)
        };
        let (topic, plugin) = sent(&mut s);
        assert!(topic.is_none() && plugin.is_none(), "nobody takes them");
        s.plugin_topics.units = true;
        let (topic, plugin) = sent(&mut s);
        assert!(topic.is_none() && plugin.is_some(), "plugins only");
        s.plugin_topics.units = false;
        s.set_topics(crate::stats::Topics::new([UNIT.to_string()]));
        let (topic, plugin) = sent(&mut s);
        let (topic, e) = topic.expect("to the topic");
        assert!(plugin.is_none());
        assert_eq!((topic.as_str(), e.short_name.as_str(), e.site.as_str(), e.kind.as_str(), e.unit, e.talkgroup), (UNIT, "dcfd", "north", "affiliation", 1234, Some(101)));
    }

    /// A tap's chunks go out named for their system; a stream is saved — one path from its
    /// first chunk to its last, beside the system's calls — only when a row of the config
    /// says so. The engine is told each system's taps: the rows' and the plugins'.
    #[test]
    fn a_taps_stream_is_saved_when_a_row_says_so() {
        use trunk_core::trunk::{Tap, TapChunk, TapSet};
        use trunk_recorder_plugin::TapData;
        let cfg = |taps: serde_json::Value| -> Config {
            serde_json::from_value(json!({
                "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
                "conventional": [{ "shortName": "wood", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire Dispatch" }] }],
                "recording": { "taps": taps }
            }))
            .unwrap()
        };
        let chunk = |stream: u64, seq: u32, last: bool| TapChunk {
            tap: Tap::FmDiscriminator,
            stream,
            seq,
            first: seq == 0,
            last,
            offset: seq as u64 * 4,
            system: 0,
            system_id: 0,
            short_name: String::new(),
            freq_hz: 155.2e6,
            slot: 0,
            call_id: None,
            talkgroup: None,
            unit: None,
            voice_mode: String::new(),
            decrypted: false,
            time: 0.0,
            data: TapData::Pcm { rate: 22_050, samples: vec![0; 4] },
            notes: Vec::new(),
        };
        let sent = |s: &mut Session, c: TapChunk| {
            let mut out = Vec::new();
            s.handle(Event::Tap { time_s: 10.0, chunk: c }, &mut out);
            out.into_iter().find_map(|o| match o { Output::Tap { chunk, save } => Some((chunk, save)), _ => None }).unwrap()
        };
        // No row: a plugin's alone. On in the engine, not saved.
        let mut s = Session::new(cfg(json!([])), &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        s.set_taps();
        assert_eq!(s.taps, vec![TapSet::NONE]);
        s.plugin_topics.taps = TapSet::of([Tap::FmDiscriminator]);
        s.set_taps();
        assert_eq!(s.taps, vec![TapSet::of([Tap::FmDiscriminator])]);
        let (c, save) = sent(&mut s, chunk(1, 0, true));
        assert_eq!((c.short_name.as_str(), c.system_id, save), ("wood", s.system_id(0).0, None));
        assert!(c.time > 1.7e9);
        // A row: every chunk of the stream under the path its first was given.
        let mut s = Session::new(cfg(json!([{ "tap": "fm.discriminator" }])), &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        s.set_taps();
        assert_eq!(s.taps, vec![TapSet::of([Tap::FmDiscriminator])]);
        let (c, first) = sent(&mut s, chunk(7, 0, false));
        let path = first.expect("saved");
        let start = c.time as i64;
        assert!(path.starts_with("wood/") && path.ends_with(&format!("/{start}_155200000-tap_7.fm.discriminator")), "{path}");
        assert_eq!(sent(&mut s, chunk(7, 1, false)).1.as_deref(), Some(path.as_str()));
        assert_eq!(sent(&mut s, chunk(7, 2, true)).1.as_deref(), Some(path.as_str()));
        assert!(s.tap_files.is_empty(), "forgotten at its end");
        // A row for another system alone: not this one's.
        let mut s = Session::new(cfg(json!([{ "tap": "fm.discriminator", "systems": [4_000_000] }])), &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        s.set_taps();
        assert_eq!(s.taps, vec![TapSet::NONE]);
        assert_eq!(sent(&mut s, chunk(1, 0, true)).1, None);
    }
}
#[cfg(test)]
mod name_tests {
    use super::*;

    /// A talkgroup a conventional row names is named so where there's no call
    /// at hand (plugins' unit events, decodeDetail), as the Radio pages name
    /// it: the one lookup, not the system's talkgroups alone.
    #[test]
    fn a_conventional_rows_talkgroup_is_named_by_the_book() {
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "conventional": [{ "shortName": "wood", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire Dispatch" }] }]
        }))
        .unwrap();
        let s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        assert_eq!(s.alpha_tag(0, 155_200), "Fire Dispatch");
        assert_eq!(tg_names(&s.meas.shared().names.lock().unwrap(), s.system_id(0), [155_200, 7]), [json!({ "talkgroup": 155_200, "alphaTag": "Fire Dispatch" }), json!({ "talkgroup": 7, "alphaTag": "" })]);
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    /// A call changing as it runs (another radio talking) is told to plugins that take
    /// `call.update`, and only to them; a second site's copy of it isn't.
    #[test]
    fn a_calls_changes_reach_plugins_that_take_them() {
        let cfg: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
            "conventional": [{ "shortName": "conv", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire" }] }]
        }))
        .unwrap();
        let mut s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
        let mut call = trunk_core::trunk::Call { talkgroup: 1001, ..Default::default() };
        call.sources.push(trunk_core::trunk::CallSource { src: 1234567, time_s: 1.0, ..Default::default() });
        let updates = |s: &mut Session, c: &trunk_core::trunk::Call| {
            let mut out = Vec::new();
            s.handle(Event::CallUpdate(c.clone()), &mut out);
            out.into_iter().filter_map(|o| if let Output::Plugin(HostMessage::CallUpdate(c)) = o { Some(c) } else { None }).collect::<Vec<_>>()
        };
        assert!(updates(&mut s, &call).is_empty(), "no plugin asked");
        s.plugin_topics.call_updates = true;
        let got = updates(&mut s, &call);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].talkgroup, got[0].sources.iter().map(|x| x.unit).collect::<Vec<_>>()), (1001, vec![1234567]));
    }
}
