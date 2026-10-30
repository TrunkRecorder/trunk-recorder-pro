//! One recording session, independent of platform: the engine plus what the
//! interface needs — status, spectrum, log lines, concluded calls (with their
//! files) and live audio — as [`Output`]s. The platform supplies samples, the
//! wall clock (`now_ms`), the local date for folder names, and does the I/O.

use serde_json::{json, Value};
use trunk_core::trunk::{heard_code, Call, Engine, Event, IdField, Identity, MessageType, NoteLevel, ProtocolStatus};

use crate::filename;
use crate::log::{Body, CallState, CallTag, Level, Record};
use crate::heard::HeardCodes;
use crate::saving::Saved;
use trunk_core::dsp::inband::subaudible::Tone;
use trunk_core::Complex32;

use crate::config::{Config, Source};
use crate::stats::{Rollup, SampleMeter, Shared, Stats, Topics};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use trunk_recorder_plugin::{CallInfo, HostMessage, SystemStatus as PluginSystemStatus, UnitEvent};

use crate::protocol::ToClient;

pub enum Output {
    /// A message for the interface.
    Message(ToClient),
    /// Live audio frame `[2][u16 system][u32 call id][u32 talkgroup][i16…]`
    /// for talkgroup `tg` of `system` — that system's number for this run
    /// (from 0, trunked then conventional: `status.systemNames`) — whose
    /// short name, its identity, is `short_name`.
    /// `decrypted`: of a call decrypted with our keys. (A call heard on
    /// several sites of a system plays from one of them at a time.)
    Audio { system: u16, short_name: String, tg: u32, frame: Vec<u8>, decrypted: bool },
    /// A concluded call to store at `<rel>.wav` / `<rel>.json` (relative to
    /// the recordings folder); `record` is its call JSON, and [`call_entry`]
    /// its history entry, which the platform sends as a `concluded` message
    /// once the files are stored.
    /// `frames`: the frame capture, for `<rel>.sdr` (NXDN: `<rel>.frames.jsonl`).
    /// `decrypted`: some of it was decrypted with our keys.
    File { rel: String, system: u16, wav: Vec<u8>, record: trunk_recorder_plugin::CallRecord, frames: Option<trunk_core::trunk::Capture>, decrypted: bool },
    /// An event for plugins (only those [`Session::plugin_topics`] asks for).
    Plugin(HostMessage),
    /// A line for the log (the platform's logger formats and routes it).
    Log(Record),
    /// A message only for interfaces watching `topic` ([`crate::stats::Topics`]).
    Topic { topic: String, msg: ToClient },
    /// A minute of the dashboard's series, for the history (and its files).
    Rollup(Rollup),
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
/// The most silence fed for one gap, s: a driver's count of samples lost.
const MAX_GAP_S: f64 = 10.0;
/// A source's clock is measured against the wall clock over this long:
/// the least lag in it (samples only ever arrive late) is the clock's
/// ([`ClockFit`]). ms.
const CLOCK_WINDOW_MS: f64 = 10_000.0;
/// A source's clock offset moves toward each window's measure at most this
/// much per second of wall clock (1000 ppm: far more than a crystal's
/// error, so it keeps up, yet smooth).
const CLOCK_SLEW: f64 = 0.001;
/// A window's measure further than this from the offset set is no drift
/// but samples lost or invented (a driver's unreported overflow, a stalled
/// radio's backlog after its quiet was filled in): the offset is set to it
/// at once, s.
const REANCHOR_S: f64 = 0.25;
/// A window's least lag this much past the drift's trend: samples lost
/// that the driver didn't report (an RTL-SDR's FIFO overflowing when its
/// USB bus is busy; it has no way to say so), s. (A crystal's error is
/// ~1 ms a window at 100 ppm; the least lag jitters well under that.)
const LOSS_MIN_S: f64 = 0.005;

/// What plugins subscribe to, so nothing else is built.
#[derive(Clone, Copy, Debug, Default)]
pub struct PluginTopics {
    pub calls: bool,
    pub units: bool,
    pub status: bool,
    pub tone_patterns: bool,
}

/// Local time's offset from UTC at a Unix time, s (east positive) — Trunk
/// Recorder's folders are in local time, which only the platform knows.
pub type LocalOffset = fn(i64) -> i32;

#[derive(Default, Clone)]
struct SourceStats {
    samples: u64,
    dropped: u64,
    errors: u64,
    last_error: Option<String>,
    /// When it came, Unix s (the error is shown for [`ERROR_SHOWN_S`]).
    last_error_s: f64,
    rate_measured: f64,
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
/// count. The offset that makes sample time wall time is set on the engine
/// ([`Engine::set_clock_offset`]): at once on the first samples; then every
/// window, anchored afresh to that window's measure — slewed to it when
/// it's drift, set at once when it's off by more than [`REANCHOR_S`].
#[derive(Default, Clone)]
struct ClockFit {
    /// Whether the wall clock means anything for it (not a capture played
    /// as fast as it can be).
    live: bool,
    /// Samples fed, silence filled in for gaps included.
    samples: u64,
    /// The least lag (wall time since the start, less sample time, s) seen
    /// in this window, and when the window began (ms).
    window_min: Option<f64>,
    window_ms: Option<f64>,
    /// The offset being steered to, and the one set.
    target: Option<f64>,
    applied: Option<f64>,
    /// The last window's least lag and when it ended (ms); the lag's trend
    /// (the crystal's error, s per s); samples lost unreported, not yet taken.
    last: Option<(f64, f64)>,
    slope: Option<f64>,
    unreported: u64,
}

impl ClockFit {
    /// The samples so far ended at `at_ms` on the wall clock.
    fn observe(&mut self, at_ms: f64, epoch_ms: f64, rate: f64) {
        if !self.live {
            return;
        }
        // Samples only ever arrive late (USB and driver buffers): the least
        // lag over a window is the clock's own.
        let lag = (at_ms - epoch_ms) / 1000.0 - self.samples as f64 / rate;
        self.window_min = Some(self.window_min.map_or(lag, |m| m.min(lag)));
        match self.window_ms {
            // The first samples: anchored at once (the radio's open time).
            None => {
                self.target = Some(lag);
                self.window_ms = Some(at_ms);
            }
            Some(w) if at_ms - w >= CLOCK_WINDOW_MS => {
                self.target = self.window_min.take();
                self.window_ms = Some(at_ms);
                self.unreported_loss(at_ms, rate);
            }
            Some(_) => {}
        }
    }

    /// A window closed: its least lag past the trend is samples lost that
    /// nobody reported; within it, the trend learns (a crystal's drift).
    fn unreported_loss(&mut self, at_ms: f64, rate: f64) {
        let Some(lag) = self.target else { return };
        if let Some((prev, prev_ms)) = self.last {
            let dt = (at_ms - prev_ms) / 1000.0;
            let excess = (lag - prev) - self.slope.unwrap_or(0.0) * dt;
            if excess > LOSS_MIN_S {
                self.unreported += (excess * rate).round() as u64;
            } else if excess.abs() <= LOSS_MIN_S && dt > 0.0 {
                let s = ((lag - prev) / dt).clamp(-2e-4, 2e-4);
                self.slope = Some(self.slope.map_or(s, |o| o + 0.2 * (s - o)));
            }
        }
        self.last = Some((lag, at_ms));
    }

    /// The offset to set after `dt` wall seconds, when it moved, and whether
    /// it was re-anchored (a step, not a slew).
    fn steer(&mut self, dt: f64) -> Option<(f64, bool)> {
        let target = self.target?;
        let (next, step) = match self.applied {
            Some(a) if (target - a).abs() <= REANCHOR_S => (a + (target - a).clamp(-CLOCK_SLEW * dt, CLOCK_SLEW * dt), false),
            Some(_) => (target, true),
            None => (target, false),
        };
        if self.applied == Some(next) {
            return None;
        }
        self.applied = Some(next);
        Some((next, step))
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
    /// (Each behind a lock: a source's [`Feeder`] updates it from its thread.)
    stats: Vec<Arc<Mutex<SourceStats>>>,
    /// Time each source's feeder spent decoding, µs (the load figure: the busiest's).
    busy: Vec<Arc<AtomicU64>>,
    /// The dashboard's measurements ([`crate::stats`]).
    meas: Stats,
    log: Vec<Value>,
    /// Log records not yet handed out.
    records: Vec<Record>,
    local_offset: LocalOffset,
    busy_ms: f64,
    start_ms: Option<f64>,
    last_poll_ms: Option<f64>,
    last_status_ms: f64,
    last_spec_ms: f64,
    /// Build audio frames (skip when nobody is listening).
    pub want_audio: bool,
    /// Each site, as calls number them: its system's number and its name.
    sites: Vec<(u16, String)>,
    /// Each system's recording rules, as calls number them ([`Config::recordings`]).
    recordings: Vec<crate::config::Recording>,
    pub plugin_topics: PluginTopics,
    /// Wall clock (Unix ms) at the engine's time 0.
    epoch_ms: f64,
    last_plugin_status_ms: f64,
    plugin_good: Vec<u64>,
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
}

impl Session {
    /// `bandplan`: the band plan saved for a site, by its key
    /// ([`Session::bandplans`], [`bandplan_key`]).
    /// `talkgroups`: each system's talkgroup file ([`crate::config::TalkgroupFiles`]).
    pub fn new(cfg: Config, talkgroups: &crate::config::TalkgroupFiles, epoch_ms: f64, bandplan: &dyn Fn(&str) -> Option<String>, local_offset: LocalOffset) -> Result<Session, String> {
        if let Some(p) = cfg.problem() {
            return Err(p);
        }
        let engine = Engine::new(cfg.engine_config(epoch_ms, talkgroups))?;
        let systems = engine.systems();
        let mut sites = Vec::new();
        for (i, (system, s)) in trunk_core::trunk::sites(&systems).enumerate() {
            if let Some(b) = bandplan(&bandplan_key(&systems[system as usize], s)).or_else(|| bandplan(crate::migrate::old_bandplan(s))) {
                engine.load_bandplan(i, &b);
            }
            sites.push((system, s.name.clone()));
        }
        let n = cfg.sources.len();
        // (A capture played as fast as it can be has no wall clock to keep to.)
        let stats = cfg
            .sources
            .iter()
            .map(|s| Arc::new(Mutex::new(SourceStats { clock: ClockFit { live: !matches!(s, Source::File { realtime: false, .. }), ..Default::default() }, ..Default::default() })))
            .collect();
        let recordings = cfg.recordings();
        let meas = Stats::new(&cfg, Shared::new(), local_offset);
        {
            let mut book = meas.shared().names.lock().unwrap();
            book.set_systems(&systems);
            book.set_rows(&cfg);
        }
        let mut session = Session {
            meas,
            cfg,
            engine,
            stats,
            busy: (0..n).map(|_| Default::default()).collect(),
            log: Vec::new(),
            records: Vec::new(),
            local_offset,
            busy_ms: 0.0,
            start_ms: None,
            last_poll_ms: None,
            last_status_ms: 0.0,
            last_spec_ms: 0.0,
            want_audio: true,
            sites,
            recordings,
            plugin_topics: PluginTopics::default(),
            epoch_ms,
            last_plugin_status_ms: 0.0,
            plugin_good: Vec::new(),
            heard: HeardCodes::default(),
            tune: Default::default(),
            tune_saved: String::new(),
            saving: Default::default(),
            rate_at: Vec::new(),
            next_status_s: STATUS_EVERY_S,
            call_signs: Default::default(),
            share: Default::default(),
            recorder_id: String::new(),
        };
        session.share = crate::share::Ledger::new(session.cfg.systems.iter());
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

    /// The registry and monitor this session feeds.
    pub fn shared(&self) -> Arc<Shared> {
        self.meas.shared().clone()
    }

    /// What the connected interfaces watch: costly outputs are made for these only.
    pub fn set_topics(&mut self, topics: Topics) {
        self.meas.set_topics(topics);
    }

    /// A system's talkgroup file changed (an Ignore flag set from the
    /// dashboard): calls from now on go by it.
    pub fn set_talkgroups(&mut self, short_name: &str, csv: &str) -> bool {
        let set = self.engine.set_talkgroups(short_name, trunk_core::trunk::parse_csv(csv));
        self.meas.shared().names.lock().unwrap().set_systems(&self.engine.systems());
        set
    }

    /// One talkgroup of the system named `short_name`, as its file now has it (none: not in it).
    pub fn set_talkgroup(&mut self, short_name: &str, number: u32, row: Option<trunk_core::trunk::Talkgroup>) -> bool {
        self.meas.shared().names.lock().unwrap().set_talkgroup(short_name, number, row.clone());
        self.engine.set_talkgroup(short_name, number, row)
    }

    /// The encryption keys of the system named `short_name` (trunked or
    /// conventional): voice channels opened from now on decrypt with them.
    pub fn set_keys(&mut self, short_name: &str, keys: Option<std::sync::Arc<trunk_core::p25::crypt::Keyring>>) -> bool {
        self.engine.set_keys(short_name, keys)
    }

    /// A radio's talker alias on the system named `short_name`.
    pub fn unit_alias(&self, short_name: &str, unit: u32) -> Option<String> {
        let sys = self.cfg.call_systems().into_iter().find(|s| s.short_name == short_name)?.index;
        self.engine.unit_alias(sys, unit)
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

    /// Steer each live source's clock offset ([`ClockFit`]). `dt`: wall
    /// seconds since the last poll.
    fn steer_clocks(&mut self, dt: f64) {
        for i in 0..self.stats.len() {
            let new = {
                let mut s = self.stats[i].lock().unwrap();
                let n = s.unreported - s.unreported_told;
                s.unreported_told = s.unreported;
                n
            };
            if new > 0 {
                let ms = new as f64 / self.engine.sources()[i].rate_hz * 1000.0;
                let text = format!("Source {i}: about {new} samples ({ms:.0} ms) lost without its driver saying so — the USB bus or the computer couldn't keep up");
                self.records.push(Record::text(Level::Warning, None, text));
            }
            let steered = self.stats[i].lock().unwrap().clock.steer(dt);
            let Some((offset, step)) = steered else { continue };
            if step {
                let moved = offset - self.engine.clock_offset(i);
                self.records.push(Record::text(Level::Info, None, format!("Source {i}: clock re-anchored to the wall clock ({:+.3} s)", moved)));
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
        self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": "error", "text": format!("source {source}: {error}") }));
        self.records.push(Record::text(Level::Error, None, format!("Source {source}: {error}")));
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
        let dt = (now_ms - self.last_poll_ms.unwrap_or(now_ms)).max(0.0) / 1000.0;
        self.last_poll_ms = Some(now_ms);
        self.steer_clocks(dt);
        self.meas.set_now(now_ms / 1000.0);
        self.fill_quiet(now_ms);
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
    /// for a short name ([`Saved::Units`]).
    pub fn load_units(&mut self, units: &dyn Fn(&str) -> Option<String>) {
        for n in self.engine.systems().iter().map(|s| s.short_name.clone()) {
            // A system's (and what its sites learned as systems of their own: newest wins).
            let mut names = vec![n.clone()];
            names.extend(self.cfg.systems.iter().filter(|s| s.short_name == n).flat_map(crate::migrate::old_names));
            let csv: Vec<String> = names.iter().filter_map(|x| units(x)).collect();
            if !csv.is_empty() {
                self.engine.load_units(&n, &csv.join("\n"));
                self.meas.shared().names.lock().unwrap().set_units(&n, &csv.join("\n"));
            }
        }
    }

    /// (short name, talker alias CSV) of each system that learned an alias
    /// since the last call, to save; the names the dashboard shows take them.
    fn units_changed(&mut self) -> Vec<(String, String)> {
        let changed = self.engine.units_changed();
        let mut book = self.meas.shared().names.lock().unwrap();
        for (name, csv) in &changed {
            book.set_units(name, csv);
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
            if let (true, Some(e), Some(at)) = (c.auto_tune(), e, at) {
                self.engine.seed_tune(i, e + (c.ppm() - at));
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
                self.tune.insert(c.label(), json!({ "errorPpm": (e * 100.0).round() / 100.0, "atPpm": c.ppm() }));
            }
        }
        let text = serde_json::to_string_pretty(&self.tune).unwrap_or_default();
        (text != self.tune_saved && !self.tune.is_empty()).then(|| {
            self.tune_saved = text.clone();
            text
        })
    }

    /// Preload the codes conventional frequencies carried before (what
    /// [`Saved::Heard`] gave, saved as [`Session::heard_file`]).
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
            let changed = self.meas.shared().radio.lock().unwrap().take_dirty_copies();
            out.extend(changed.into_iter().map(|(system, radio)| Saved::Registry { system, radio }));
        }
        out
    }

    /// Write the radio registry every `ms` rather than [`crate::saving::REGISTRY_MS`]
    /// (the browser: a tab can be closed without the recording's end).
    pub fn save_registry_every(&mut self, ms: f64) {
        self.saving.registry_every_ms = Some(ms);
    }

    /// The file name (beside the config) the codes are kept in.
    pub fn heard_file(_cfg: &Config) -> String {
        "conventional.heard.json".into()
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

    /// The system of site `site` (a call's `site`).
    fn system_of(&self, site: u16) -> u16 {
        self.sites.get(site as usize).map_or(0, |s| s.0)
    }

    /// Site `site`'s name (empty: none, a conventional call's).
    fn site_name(&self, site: Option<u16>) -> String {
        site.and_then(|s| self.sites.get(s as usize)).map_or(String::new(), |s| s.1.clone())
    }

    /// How the log names system `system` (its site `site`): "dcfd", or "dcfd/south".
    fn site_label(&self, system: u16, site: Option<u16>) -> String {
        trunk_core::trunk::SiteKey { system: self.system_name(system), site: &self.site_name(site) }.to_string()
    }

    /// Talkgroups as the interface shows them: number and alpha tag (from system `system`'s file; "" when not in it).
    fn tg_names(&self, system: u16, tgs: impl IntoIterator<Item = u32>) -> Vec<Value> {
        tg_names(&self.engine, system, tgs)
    }

    /// Talkgroup `tg`'s name in system `system`'s file ("": none), as plugins' events give it (`alpha_tag`).
    fn alpha_tag(&self, system: u16, tg: u32) -> String {
        alpha_tag(&self.engine, system, tg)
    }

    /// Radio `unit`'s name ("": none), as a call's JSON gives it (`tag`).
    fn unit_tag(&self, system: u16, unit: u32) -> String {
        self.engine.unit_name(system, unit).unwrap_or_default()
    }

    /// Unix seconds of engine time `s`.
    fn wall(&self, s: f64) -> f64 {
        self.epoch_ms / 1000.0 + s
    }

    /// The radio systems' own clocks (P25's SYNC_BCST, on Phase 2 systems)
    /// against the clock calls are stamped by: each site's latest. None from
    /// a capture: it carries its own day's.
    pub fn radio_clocks(&self) -> Vec<RadioClock> {
        self.engine.status().sites.iter().filter_map(|y| self.radio_clock(y)).collect()
    }

    /// Site `y`'s radio clock ([`Session::radio_clocks`]).
    fn radio_clock(&self, y: &trunk_core::trunk::SiteStatus) -> Option<RadioClock> {
        let (t, system_ms, valid) = y.system_time.filter(|_| !self.cfg.sources.iter().any(|s| matches!(s, Source::File { .. })))?;
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
            short_name: self.system_name(c.system).to_string(),
            site: self.site_name(c.site),
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
            start_time: self.wall(c.start_s),
            units: c.sources.iter().map(|s| s.src).collect(),
            sources: c
                .sources
                .iter()
                .map(|s| {
                    let f = s.flags;
                    trunk_recorder_plugin::CallSource { unit: s.src, tag: self.unit_tag(c.system, s.src), time: self.wall(s.time_s), emergency: f.emergency, encrypted: f.encrypted, duplex: f.duplex, packet: f.packet, priority: f.priority }
                })
                .collect(),
            patched_talkgroups: c.patched_talkgroups.clone(),
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
                PluginSystemStatus {
                    index: y.system,
                    short_name: y.short_name.clone(),
                    site_name: y.site_name.clone(),
                    control_channel_hz: y.control_channel_hz,
                    decode_rate: (rate * 10.0).round() / 10.0,
                    active_calls: y.active_calls as u32,
                    recording: y.recording as u32,
                    nac: y.identity.get(IdField::Nac),
                    wacn: y.identity.get(IdField::Wacn),
                    sys_id: y.identity.get(IdField::SysId),
                    rfss: y.identity.get(IdField::Rfss),
                    site: y.identity.get(IdField::Site),
                }
            })
            .collect();
        trunk_recorder_plugin::Status { time: self.wall(st.now_s), systems }
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
        if let Some(sys) = self.event_system(&ev) {
            let name = self.system_name(sys).to_string();
            self.meas.observe(&ev, &name, &self.engine, out);
        }
        match ev {
            Event::Message { site, msg: m } => {
                let system = self.system_of(site);
                let name = self.system_name(system).to_string();
                if self.plugin_topics.units {
                    if let Some(kind) = unit_kind(m.kind) {
                        let talkgroup = matches!(kind, "affiliation" | "location" | "answer_request" | "call_alert" | "emergency_alarm").then_some(m.talkgroup);
                        if m.source >= 0 {
                            let unit = m.source as u32;
                            let (tag, alpha_tag) = (self.unit_tag(system, unit), talkgroup.map(|tg| self.alpha_tag(system, tg)).unwrap_or_default());
                            let e = UnitEvent { system, short_name: name.clone(), site: self.site_name(Some(site)), kind: kind.into(), unit, tag, talkgroup, alpha_tag, emergency: m.emergency, time: self.wall(m.time_s) };
                            out.push(Output::Plugin(HostMessage::Unit(e)));
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
                let (level, kind) = match level {
                    NoteLevel::Info => (Level::Info, "note"),
                    NoteLevel::Warning => (Level::Warning, "error"),
                };
                self.records.push(Record::text(level, Some(&self.site_label(system, site)), text.clone()));
                self.log.push(json!({ "timeS": self.engine.status().now_s, "kind": kind, "text": text, "system": name, "site": self.site_name(site) }))
            }
            Event::Audio { call_id, system, talkgroup, samples, decrypted } => {
                if !self.want_audio {
                    return;
                }
                let mut frame = Vec::with_capacity(11 + samples.len() * 2);
                frame.push(2u8);
                frame.extend_from_slice(&system.to_le_bytes());
                frame.extend_from_slice(&call_id.to_le_bytes());
                frame.extend_from_slice(&talkgroup.to_le_bytes());
                for s in samples {
                    frame.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                out.push(Output::Audio { system, short_name: self.system_name(system).to_string(), tg: talkgroup, frame, decrypted });
            }
            Event::ConvSkipped { freq_hz, code } => {
                let ms = self.wall(self.engine.status().now_s) * 1000.0;
                self.heard.note(freq_hz, &code, false, ms);
            }
            Event::Concluded(k) => {
                if let Some(code) = heard_code(&k.call) {
                    self.heard.note(k.call.freq_hz, &code, true, self.wall(k.call.start_s) * 1000.0);
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
                let body = Body::Concluded { length_s: k.audio.len() as f64 / 8000.0, signal_db: r.signal, noise_db: r.noise, snr_db: r.snr, clean_pct: r.clean_voice_pct };
                out.push(Output::File { rel, system: k.call.system, wav: trunk_core::wav::encode(&k.audio, 8000), record: k.record, frames: k.frames, decrypted: k.call.decrypted });
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
                let text = format!("TG {}: {}{}", t.talkgroup, pattern.text, if named.is_empty() { String::new() } else { format!(" = {named}") });
                self.records.push(Record::text(Level::Info, Some(&self.site_label(t.system, t.site)), text.clone()));
                self.log.push(json!({ "timeS": t.time_s, "kind": "tonePattern", "text": text, "system": name, "site": self.site_name(t.site) }));
                if self.plugin_topics.tone_patterns {
                    let alpha_tag = self.alpha_tag(t.system, t.talkgroup);
                    let time = (self.wall(t.time_s) * 1000.0).round() / 1000.0;
                    let ev = trunk_recorder_plugin::TonePatternEvent { short_name: name, site: self.site_name(t.site), call_num: t.call_id as u64, talkgroup: t.talkgroup, alpha_tag, src_tag: self.unit_tag(t.system, t.src), decrypted: t.decrypted, time, pattern };
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
                let text = format!("Unit {unit} at {lat:.5}, {lon:.5} (TG {talkgroup})");
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
            Event::CallStart(c) => {
                let short = self.system_name(c.system).to_string();
                self.share.call(&short, &c);
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
                // (A call heard on several sites is one call: told by its first copy.)
                if self.plugin_topics.calls && !c.twin {
                    out.push(Output::Plugin(HostMessage::CallStart(self.call_info(&c))));
                }
            }
            Event::NotSaved(c) => {
                let r = self.call_record(Level::Info, &c, Body::Dropped);
                self.records.push(r);
            }
            Event::CallEnd(c) => {
                if self.plugin_topics.calls && !c.twin {
                    out.push(Output::Plugin(HostMessage::CallEnd(self.call_info(&c))));
                }
            }
            Event::CallUpdate(_) => {}
        }
    }

    /// The system an event is counted for in the stats (a call heard on
    /// several sites: by its first copy).
    fn event_system(&self, ev: &Event) -> Option<u16> {
        match ev {
            Event::Message { site, .. } => Some(self.system_of(*site)),
            Event::CallStart(c) | Event::CallEnd(c) if c.twin => None,
            Event::CallStart(c) | Event::CallEnd(c) | Event::NotSaved(c) => Some(c.system),
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
                json!({ "index": i, "label": label, "centerHz": sc.center_hz, "rateHz": sc.rate_hz, "rateMeasured": ss.rate_measured,
                        "dropped": ss.dropped, "errors": ss.errors, "lastError": recent, "lastErrorS": recent.filter(|_| ss.last_error_s > 0.0).map(|_| ss.last_error_s.round()), "ended": ss.ended,
                        "errorPpm": tune.error_ppm, "tunePpm": tune.applied_ppm, "tuneSeeded": tune.seeded })
            })
            .collect();
        // Each running site, as its control channel describes it.
        let site_json = |y: &trunk_core::trunk::SiteStatus| {
            json!({
                "index": y.site, "name": y.site_name, "nowS": y.now_s, "controlChannelHz": y.control_channel_hz,
                "identity": identity_json(&y.identity),
                "good": y.good, "bad": y.bad, "modulation": if y.modulation.is_empty() { None } else { Some(y.modulation) },
                "activeCalls": y.active_calls, "recording": y.recording, "callsConcluded": y.calls_concluded,
                "mismatch": y.mismatch,
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
                let mut v = call_view(c, self.system_name(c.system), self.tg_names(c.system, c.patched_talkgroups.iter().copied().filter(|&t| t != c.talkgroup)));
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
/// (`{nac, wacn, sysId, rfss, site}`, a field not stated yet left out).
pub(crate) fn identity_json(id: &Identity) -> Value {
    json!(crate::config::SiteIdentity::from(id))
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
        "tone": c.tone.map(|h| match h.tone {
            Tone::Ctcss(_) => format!("{} Hz", h.tone),
            Tone::Dcs(..) => h.tone.to_string(),
        }),
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


/// Talkgroups with their names in system `system`'s talkgroup file.
fn tg_names(engine: &Engine, system: u16, tgs: impl IntoIterator<Item = u32>) -> Vec<Value> {
    tgs.into_iter().map(|tg| json!({ "talkgroup": tg, "alphaTag": alpha_tag(engine, system, tg) })).collect()
}

/// Talkgroup `tg`'s name in system `system`'s talkgroup file ("": not in it).
fn alpha_tag(engine: &Engine, system: u16, tg: u32) -> String {
    engine.systems().get(system as usize).and_then(|s| s.talkgroups.get(&tg)).map_or(String::new(), |t| t.alpha_tag.clone())
}

/// A system's `decodeDetail` beyond its channels: each running site's
/// neighbours and DMR / NXDN channels and carriers, and the system's patches
/// (each site hears them: once each).
fn decode_sites(engine: &Engine, system: &str) -> (Vec<Value>, Vec<Value>) {
    let (mut sites, mut patches) = (Vec::new(), Vec::<Value>::new());
    for y in engine.status().sites.iter().filter(|y| y.short_name == system) {
        let at = y.system;
        sites.push(json!({
            "index": y.site, "name": y.site_name,
            "adjacent": y.adjacent.iter().map(|a| json!({ "sysId": a.sys_id, "rfss": a.rfss, "site": a.site, "freqHz": a.freq_hz })).collect::<Vec<_>>(),
            "dmr": match &y.protocol { ProtocolStatus::Dmr(d) => Some(d), _ => None }.map(|d| {
                json!({
                    "variant": d.variant.map(|v| v.name()),
                    "colorCode": d.color_code,
                    "rest": d.rest.map(|(lsn, hz)| json!({ "lsn": lsn, "freqHz": if hz > 0 { Some(hz) } else { None } })),
                    "keyed": d.keyed,
                    "channels": d.channels.iter().map(|c| json!({ "lcn": c.lcn, "freqHz": c.hz, "configured": c.configured })).collect::<Vec<_>>(),
                    "carriers": d.carriers.iter().map(|c| json!({
                        "freqHz": c.hz, "control": c.control, "colorCode": c.color_code,
                        "slots": c.slots.iter().map(|s| s.map(|(tg, src)| json!({ "talkgroup": tg_names(engine, at, [tg])[0], "source": src }))).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })
            }),
            "nxdn": match &y.protocol { ProtocolStatus::Nxdn(n) => Some(n), _ => None }.map(|n| {
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
                        "call": c.call.map(|(tg, src)| json!({ "talkgroup": tg_names(engine, at, [tg])[0], "source": src })),
                    })).collect::<Vec<_>>(),
                })
            }),
        }));
        for (sg, members) in &y.patches {
            let p = json!({ "supergroup": tg_names(engine, at, [*sg])[0], "members": tg_names(engine, at, members.iter().copied()) });
            if !patches.iter().any(|h| h["supergroup"] == p["supergroup"]) {
                patches.push(p);
            }
        }
    }
    (sites, patches)
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    /// A live clock fed `secs` of 1 MS/s samples, handed over every 100 ms
    /// with `lag` s of latency, plus `extra` samples beyond the wall time
    /// at `jump_at` s; steered every 100 ms. The offsets set, per 100 ms.
    fn run(secs: f64, ppm: f64, jump_at: f64, extra: u64) -> Vec<(f64, bool)> {
        let mut c = ClockFit { live: true, ..Default::default() };
        let mut set = Vec::new();
        let mut t = 0.0;
        while t < secs {
            t += 0.1;
            c.samples = (t * 1e6 * (1.0 + ppm * 1e-6)) as u64 + if t > jump_at { extra } else { 0 };
            c.observe(1000.0 * (t + 0.02), 0.0, 1e6);
            if let Some(s) = c.steer(0.1) {
                set.push(s);
            }
        }
        set
    }

    #[test]
    fn crystal_drift_is_followed_in_small_steps() {
        // 20 ppm fast: 0.2 ms more every 10 s, never a step.
        let set = run(120.0, 20.0, f64::INFINITY, 0);
        assert!(set.iter().skip(1).all(|&(_, step)| !step));
        let last = set.last().unwrap().0;
        // Lag = 0.02 s latency − 20 ppm × 120 s (the window's least lag lags by up to a window).
        assert!((last - (0.02 - 20e-6 * 120.0)).abs() < 0.0005, "{last}");
    }

    #[test]
    fn samples_gained_or_lost_are_re_anchored_at_once() {
        // 2 s of samples more than the wall clock allows at 30 s (a stalled
        // radio's backlog after its quiet was filled in).
        let set = run(60.0, 0.0, 30.0, 2_000_000);
        let steps: Vec<f64> = set.iter().filter(|s| s.1).map(|s| s.0).collect();
        assert_eq!(steps.len(), 1, "{set:?}");
        assert!((steps[0] - (0.02 - 2.0)).abs() < 1e-6, "{steps:?}");
        // And 2 s lost (not reported by the driver).
        let set = run(60.0, 0.0, 30.0, 0);
        assert!((set.last().unwrap().0 - 0.02).abs() < 1e-6);
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
        assert_eq!(run(180.0, f64::INFINITY, 0.0), 0);
    }

    #[test]
    fn samples_missing_are_found() {
        let n = run(180.0, 90.0, 0.02);
        let want = (0.02 * 2_400_000.0) as u64;
        assert!(n.abs_diff(want) < want / 10, "found {n} of {want} lost");
    }
}
