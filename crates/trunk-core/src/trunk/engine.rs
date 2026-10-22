//! Trunked systems above the radio(s) — Trunk Recorder's
//! monitor_messages() loop plus its recorders, for each system:
//!
//! ```text
//! source(s) u8 IQ → Channelizer per source
//!   control channel head → receiver bank → TSDU groups → TSBKs → P25Parser → CallManager
//!   CallManager → RecorderHost::record → a voice head on whichever source covers the
//!     frequency (with pre-roll) → receiver bank → VoiceTracker → audio
//!     (Phase 2 TDMA: H-DQPSK receiver → slot framer → TdmaTracker, one head
//!     for both slots)
//!   call end → Concluded (TR JSON + audio)
//! ```
//!
//! Several sources (dongles) feed every system: each system's control channel
//! runs on the source that covers it, each voice channel on the source that
//! covers its frequency. The systems share the sources and the recorder pool;
//! each has its own control channel, band plan, talkgroups and calls, and its
//! own time — its control channel's sample clock. A system may have several
//! sites ([`SystemConfig::sites`]), each its own [`Trunk`] with its own
//! control channel, sharing the system's talkgroups and rules; a call heard
//! on several sites is recorded on each and the best copy saved (see
//! [`super::multisite`]).
//!
//! Conventional channels (analog FM, P25) ride the same channelizers: see
//! [`super::conventional`]. A config may have trunked systems, conventional
//! channels, or both. Everything that happens is reported as [`Event`]s; the
//! engine does no I/O.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use num_complex::Complex32;

use super::calls::{Call, CallConfig, CallEvent, CallId, CallIds, CallManager, Policy, Reason, RecorderHost};
use super::conventional::{ConvChannel, ConvConfig, ConvOut, Conventional};
use super::message::{Message, MessageType};
use super::multisite::{Held, MultiSite};
use super::record::{save_call, Reception, SaveContext, Transmissions};
use super::record::{Concluded, SaveRules};
use super::talkgroups::Talkgroups;
use super::frames::CallFrames;
use super::voice::TrackerOut;
use super::units::{UnitAlias, UnitAliases, UnitTags};
use super::identity::{IdField, Identity};
use super::control::{self, CarrierPlan, ControlChannel, Protocol, ProtocolStatus, Step};
use super::voice::{self, ChannelQuality, CipherId, VoiceDecoder, VoiceParams, VoiceSpec};
use crate::alias::Alias;
use crate::dsp::channelizer::{floor_slice, floor_slices};
use crate::dsp::{AnalogTuning, Channelizer, HeadId};
use crate::mbe;
use crate::metrics::{self, Scoped, Sink};
use crate::p25::alias::AliasLc;
use crate::p25::crypt::Keyring;
use crate::p25::diversity::BankConfig;
use crate::dsp::fm::ChannelFilter;
use crate::smartnet::Bandplan;

/// A SmartNet system (its control channels are SmartNet, not P25).
#[derive(Clone, Debug)]
pub struct SmartnetConfig {
    pub bandplan: Bandplan,
    /// Voice mode of a talkgroup whose grant was never heard: analog FM?
    pub analog_default: bool,
}

/// Analog voice squelch: carrier this far above the noise floor, dB.
const ANALOG_SQUELCH_DB: f64 = 6.0;

/// Lowest per-channel rate the P25 receivers need.
const MIN_CHANNEL_RATE: f64 = 24_000.0;
/// Default guard band at each edge of a source, Hz: channels this close to
/// the band's edge aren't used (the SDR's anti-alias roll-off is there).
pub const DEFAULT_GUARD_HZ: f64 = 75_000.0;
/// No good control message for this long → hunt to the next control channel.
const CC_HUNT_S: f64 = 5.0;

#[derive(Clone, Debug, Default)]
pub struct SourceConfig {
    pub center_hz: f64,
    pub rate_hz: f64,
    /// Correct channels for the frequency error measured on its control
    /// channels and its calls' voice (Trunk Recorder's autoTune). Measured either way.
    pub auto_tune: bool,
    /// Left unused at each edge, Hz (see [`DEFAULT_GUARD_HZ`]).
    pub guard_hz: f64,
}

impl SourceConfig {
    /// How far from its centre a channel may be, Hz.
    pub fn usable_half_width(&self) -> f64 {
        usable_half_width(self.rate_hz, self.guard_hz)
    }
}

/// How far from a source's centre a channel may be, Hz: half its bandwidth
/// less the guard band (never under a quarter of the bandwidth).
pub fn usable_half_width(rate_hz: f64, guard_hz: f64) -> f64 {
    (rate_hz / 2.0 - guard_hz.max(0.0)).max(rate_hz / 4.0)
}


/// A system: what it decides about its calls, once for all of it, and where
/// they come from — its sites, each a control channel to follow, or (none)
/// the conventional channels that name it ([`ConvChannel::system`]). A call
/// heard on several of its sites is one call ([`super::multisite`]).
#[derive(Clone, Debug)]
pub struct SystemConfig {
    /// Its folder and record name (Trunk Recorder's shortName): its identity.
    pub short_name: String,
    pub calls: CallConfig,
    pub save: SaveRules,
    pub talkgroups: Talkgroups,
    /// Its own names for its radios (the unit names file).
    pub unit_tags: UnitTags,
    /// Its encryption keys (P25): voice it has the key for is decrypted.
    pub keys: Option<Arc<Keyring>>,
    /// Trunked: its sites (one at least). Conventional: none.
    pub sites: Vec<SiteConfig>,
}

impl SystemConfig {
    /// A trunked system of these sites, with default rules.
    pub fn trunked(short_name: &str, sites: Vec<SiteConfig>) -> Self {
        SystemConfig { sites, ..Self::conventional(short_name) }
    }

    /// A conventional system (no sites), with default rules.
    pub fn conventional(short_name: &str) -> Self {
        SystemConfig {
            short_name: short_name.into(),
            calls: CallConfig::default(),
            save: SaveRules::default(),
            talkgroups: Talkgroups::default(),
            unit_tags: UnitTags::default(),
            keys: None,
            sites: Vec::new(),
        }
    }

    pub fn is_conventional(&self) -> bool {
        self.sites.is_empty()
    }

    /// What its call managers go by while recording (both can change then).
    pub fn policy(&self) -> Policy<'_> {
        Policy { talkgroups: &self.talkgroups, can_decrypt: self.keys.as_ref().is_some_and(|k| !k.is_empty()) }
    }

    /// How its site `site` is named outside the engine.
    pub fn site_key<'a>(&'a self, site: &'a SiteConfig) -> SiteKey<'a> {
        SiteKey { system: &self.short_name, site: &site.name }
    }
}

/// How a site is named outside the engine: its system's short name, and
/// its own name when that is another (a system of one site usually has
/// none of its own). Every name for a site — its stats, its files, its log
/// lines — comes from here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SiteKey<'a> {
    pub system: &'a str,
    pub site: &'a str,
}

impl<'a> SiteKey<'a> {
    /// Its own name, when it has one other than its system's.
    pub fn own_name(&self) -> Option<&'a str> {
        Some(self.site).filter(|s| !s.is_empty() && *s != self.system)
    }

    /// Where its control channel's stats go: `sys/<system>`, or
    /// `sys/<system>/site/<site>` for a site with a name of its own.
    pub fn stats_key(&self) -> String {
        let sys = format!("sys/{}", metrics::key_part(self.system));
        self.own_name().map_or_else(|| sys.clone(), |s| format!("{sys}/site/{}", metrics::key_part(s)))
    }

    /// The name its files (band plan) are kept under: `<system>`, or `<system>.<site>`.
    pub fn file_stem(&self) -> String {
        self.own_name().map_or_else(|| self.system.to_string(), |s| format!("{}.{s}", self.system))
    }
}

/// `<system>`, or `<system>/<site>` (log lines).
impl std::fmt::Display for SiteKey<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self.own_name() {
            Some(s) => write!(f, "{}/{s}", self.system),
            None => f.write_str(self.system),
        }
    }
}

/// One site of a trunked system: a control channel to follow.
#[derive(Clone, Debug)]
pub struct SiteConfig {
    /// Its name, unique in its system (left empty, the engine gives it its
    /// system's short name): the talkgroup file's Preferred Site, its band
    /// plan's file.
    pub name: String,
    pub control_channels: Vec<f64>,
    /// Receivers for its control and voice channels (the modulation).
    pub bank: BankConfig,
    /// Only follow a control channel whose identity agrees (a field left
    /// None matches anything) — keeps a site off a neighbour's control channel.
    pub expect: Identity,
    /// Its trunking protocol (P25 by default).
    pub protocol: Protocol,
}

impl Default for SiteConfig {
    fn default() -> Self {
        SiteConfig { name: String::new(), control_channels: vec![], bank: BankConfig::default(), expect: Identity::default(), protocol: Protocol::P25 }
    }
}

/// Every site of `systems`, numbered as calls number them ([`Call::site`]):
/// each system's in turn, as (its system's number, the site).
pub fn sites(systems: &[SystemConfig]) -> impl Iterator<Item = (u16, &SiteConfig)> {
    systems.iter().enumerate().flat_map(|(i, s)| s.sites.iter().map(move |site| (i as u16, site)))
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// The systems, trunked and conventional: [`Call::system`] indexes
    /// them, and [`Call::site`] their sites in order ([`sites`]).
    pub systems: Vec<SystemConfig>,
    pub sources: Vec<SourceConfig>,
    /// Seconds of air a voice channel replays from before its grant.
    pub preroll_s: f64,
    /// Recorders shared by every system.
    pub max_recorders: usize,
    /// Wall-clock epoch ms at sample-clock time 0.
    pub epoch_ms_at_zero: f64,
    /// Receivers for conventional P25 channels.
    pub bank: BankConfig,
    /// Conventional channels, energy-detected on whichever source covers them.
    pub conventional: Vec<ConvChannel>,
    pub conv: ConvConfig,
    /// Keep each call's vocoder frames ([`Concluded::frames`]).
    pub capture_frames: bool,
    /// Save a call heard on several sites of one system once ([`super::multisite`]).
    pub drop_duplicates: bool,
    /// The IMBE vocoder for P25 Phase 1 voice, trunked and conventional.
    pub vocoder: mbe::Profile,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            systems: vec![],
            sources: vec![],
            preroll_s: 1.0,
            max_recorders: 32,
            epoch_ms_at_zero: 0.0,
            bank: BankConfig::default(),
            conventional: vec![],
            conv: ConvConfig::default(),
            capture_frames: false,
            drop_duplicates: true,
            vocoder: mbe::Profile::Enhanced,
        }
    }
}

/// How much an [`Event::Note`] matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteLevel {
    /// What the system said of itself, or what was learned.
    Info,
    /// Something wrong (another system's control channel, a channel not in the table).
    Warning,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Site `site` tuned a control channel.
    ControlChannel { site: u16, freq_hz: u64 },
    /// A control channel message of site `site`.
    Message { site: u16, msg: Message },
    /// Something worth a log line about system `system` (its site `site`,
    /// for what a control channel did): news (a colour code, a channel
    /// learned) or, as a warning, something wrong.
    Note { system: u16, site: Option<u16>, level: NoteLevel, text: String },
    CallStart(Call),
    CallUpdate(Call),
    CallEnd(Call),
    /// Live audio for a recording call.
    /// `decrypted`: the call is encrypted on the air, so this audio is of a cipher we hold the key for.
    Audio { call_id: CallId, system: u16, talkgroup: u32, samples: Vec<f32>, decrypted: bool },
    /// System `system` (any of its sites) heard a radio's talker alias it didn't know (or knew by another name).
    UnitAlias { system: u16, unit: u32, alias: String, talkgroup: u32 },
    /// A frequency of system `system` is identified by this FCC call sign
    /// (Motorola's base station ID: a control channel's for one of its
    /// channels, or a voice channel's for itself). Each time it's heard anew.
    CallSign { system: u16, freq_hz: u64, call_sign: String },
    /// A radio on a call of system `system` gave a new GPS position (decimal degrees).
    UnitPosition { system: u16, unit: u32, lat: f64, lon: f64, talkgroup: u32 },
    Concluded(Concluded),
    /// A recorded call wasn't saved: no audio, or less than its system's minimum.
    NotSaved(Call),
    /// Multi-site: `call` was another site's copy of `kept` (saved instead).
    Duplicate { call: Call, kept: Call },
    /// A conventional transmission no channel row took (it had another
    /// code, or none where every row has one): its frequency and code.
    ConvSkipped { freq_hz: u64, code: String },
}

/// A neighbouring site a control channel announces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdjacentSite {
    pub sys_id: u32,
    pub rfss: u32,
    pub site: u32,
    pub freq_hz: u64,
}

/// One site's state, for the interface.
#[derive(Clone, Debug, Default)]
pub struct SiteStatus {
    /// The number its calls carry (`Call::site`). Sites without a control
    /// channel are left out of [`Status::sites`], so this isn't always the
    /// position there.
    pub site: u16,
    /// Its system's number (`Call::system`) and short name.
    pub system: u16,
    pub short_name: String,
    /// The site's name ([`SiteConfig::name`]).
    pub site_name: String,
    /// Its control channel's clock.
    pub now_s: f64,
    pub control_channel_hz: Option<u64>,
    pub identity: Identity,
    pub good: u64,
    pub bad: u64,
    pub modulation: &'static str,
    pub active_calls: usize,
    pub recording: usize,
    pub calls_concluded: u64,
    /// The control channel is another site's (see [`SiteConfig::expect`]).
    pub mismatch: Option<String>,
    /// Neighbouring sites its control channel announces.
    pub adjacent: Vec<AdjacentSite>,
    /// The FCC call signs its control channel's base station IDs say
    /// (Motorola's MOT_BSI_GRANT), each with the channel it's for, Hz: a
    /// site's licenses can be several, one a channel. Never another site's.
    pub callsigns: Vec<(String, u64)>,
    /// Patches standing now: (supergroup, the talkgroups patched into it).
    pub patches: Vec<(u32, Vec<u32>)>,
    /// A trunked DMR site: its kind, rest channel, channel table, carriers.
    pub protocol: ProtocolStatus,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// The first source's sample clock.
    pub now_s: f64,
    pub sites: Vec<SiteStatus>,
    pub active_calls: usize,
    /// Recorders in use (trunked calls; conventional channels don't use the pool).
    pub recording: usize,
    pub channels_open: usize,
    /// Conventional channels with a head open (a signal on them now).
    pub conventional_open: usize,
    pub calls_concluded: u64,
    /// Each source's frequency error, as measured and as corrected.
    pub sources: Vec<SourceTune>,
}

/// One channel the engine listens to, as the dashboard shows it ([`Engine::channels`]).
#[derive(Clone, Debug)]
pub struct ChannelSnapshot {
    /// The system it is for, and its site (none: a conventional channel).
    pub system: u16,
    pub site: Option<u16>,
    pub freq_hz: u64,
    pub source: usize,
    pub kind: ChannelKind,
    /// The power in the channel and the noise floor under it, dBFS per FFT bin.
    pub power_db: f64,
    pub noise_db: f64,
    /// How far above the channel the carrier is, Hz, when a receiver can tell.
    pub offset_hz: Option<f32>,
    /// How well its receiver hears it.
    pub quality: ChannelQuality,
    /// Calls on it now.
    pub calls: usize,
}

/// What a channel is to the engine ([`ChannelSnapshot`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelKind {
    /// A site's control channel (a DMR site's: the one control is on now).
    #[default]
    Control,
    /// Another carrier a DMR / NXDN site's channels are watched on.
    Carrier,
    Voice,
    Conventional,
}

impl ChannelKind {
    /// As the dashboard names it.
    pub fn name(self) -> &'static str {
        match self {
            ChannelKind::Control => "control",
            ChannelKind::Carrier => "carrier",
            ChannelKind::Voice => "voice",
            ChannelKind::Conventional => "conventional",
        }
    }
}

/// A channel [`Engine::channels`] measures on its source, and what the engine knows of it.
#[derive(Default)]
struct Want {
    system: u16,
    site: Option<u16>,
    hz: f64,
    src: usize,
    kind: ChannelKind,
    offset_hz: Option<f32>,
    quality: ChannelQuality,
    calls: usize,
}

/// Noise profile slices per source for the dashboard.
const PROFILE_SLICES: usize = 64;
/// Half the width a channel's power is measured over, Hz.
const CC_HALF_HZ: f64 = 3000.0;

/// The noise floor under `hz` from source `s`'s profile `p` (|X|² per bin).
fn slice_at(p: &[f64], s: &Source, hz: f64) -> f64 {
    p[floor_slice(p.len(), s.cfg().rate_hz, hz - s.cfg().center_hz)]
}

/// A source's frequency error (Trunk Recorder's autoTune report).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SourceTune {
    /// Its running score, ppm, against the source's ppm as set (+ = signals
    /// come in high): setting the source's ppm to `ppm − error_ppm` removes it
    /// (as Setup suggests). The median of its last measurements, control
    /// channels' and calls' together. None until one was measured.
    pub error_ppm: Option<f64>,
    /// The correction applied to channels opened now, ppm (0 without autoTune).
    pub applied_ppm: f64,
    /// The correction comes from an earlier run's score (nothing measured yet).
    pub seeded: bool,
}

/// Measurements kept, control channels' and calls' together (Trunk Recorder
/// keeps the last 20). Their median is the score: a mis-lock or an
/// off-frequency transmitter doesn't move it, drift does.
const TUNE_KEEP: usize = 20;
/// A call counts once it lasted this long, s.
const TUNE_CALL_MIN_S: f64 = 1.0;
/// A control channel is measured this often, s.
const TUNE_EVERY_S: f64 = 10.0;
/// A P25 control channel is reopened at the corrected frequency when it is
/// off by more than this, Hz, but not more often than every TUNE_RETUNE_S.
const TUNE_RETUNE_HZ: f64 = 150.0;
const TUNE_RETUNE_S: f64 = 200.0;

struct Source {
    /// Every source's settings (the engine's one copy), and which this is.
    cfgs: Arc<[SourceConfig]>,
    index: usize,
    chz: Channelizer,
    /// Recent frequency errors measured on it, ppm: its control channels'
    /// (every TUNE_EVERY_S) and its calls' (one each).
    errors: VecDeque<f64>,
    /// The score an earlier run left, ppm, until something is measured.
    seed: Option<f64>,
    /// The correction new channels get, ppm.
    tune_ppm: f64,
    /// Being fed silence for samples that never came ([`Engine::push_gap`]).
    in_gap: bool,
    /// u8 IQ: an I byte whose Q comes in the next buffer.
    half: Option<u8>,
    /// Added to its sample clock to give the engine's clock
    /// ([`Engine::set_clock_offset`]), s.
    clock_offset_s: f64,
}

impl Source {
    fn new(cfgs: &Arc<[SourceConfig]>, index: usize, history_s: f64) -> Source {
        let rate = cfgs[index].rate_hz;
        Source { cfgs: cfgs.clone(), index, chz: Channelizer::new(rate, MIN_CHANNEL_RATE, history_s), errors: VecDeque::new(), seed: None, tune_ppm: 0.0, in_gap: false, half: None, clock_offset_s: 0.0 }
    }
    /// Its settings.
    fn cfg(&self) -> &SourceConfig {
        &self.cfgs[self.index]
    }
    /// A frequency error measured on it (a control channel, a call), ppm.
    fn measured(&mut self, ppm: f64) {
        if !ppm.is_finite() || ppm.abs() > 50.0 {
            return;
        }
        self.errors.push_back(ppm);
        if self.errors.len() > TUNE_KEEP {
            self.errors.pop_front();
        }
        self.retune();
    }
    fn retune(&mut self) {
        if self.cfg().auto_tune {
            self.tune_ppm = self.error_ppm().or(self.seed).unwrap_or(0.0);
        }
    }
    /// The running score: the median of the measurements kept.
    fn error_ppm(&self) -> Option<f64> {
        let mut x: Vec<f64> = self.errors.iter().copied().collect();
        x.sort_by(f64::total_cmp);
        let n = x.len();
        (n > 0).then(|| (x[(n - 1) / 2] + x[n / 2]) / 2.0)
    }
    /// Its time on the engine's clock: its sample clock, plus its offset.
    fn time(&self) -> f64 {
        self.chz.sample_position() as f64 / self.chz.fs() + self.clock_offset_s
    }
    /// A channel's offset from the tuned centre, corrected.
    fn offset(&self, hz: f64) -> f64 {
        hz * (1.0 + self.tune_ppm * 1e-6) - self.cfg().center_hz
    }
}

struct Channel {
    /// The site whose grant opened it.
    site: u16,
    head: HeadId,
    /// The call on each TDMA slot (Phase 1: slot 0).
    calls: [Option<OnSlot>; 2],
    voice: Box<dyn VoiceDecoder>,
    freq_hz: f64,
    /// The correction it was opened with, ppm.
    tune_ppm: f64,
    /// Its power, and the noise floor under it when it opened ([`Reception`]).
    meter: ChannelFilter,
    noise: f64,
}

/// A call on a voice channel's slot, and the talkgroup it was granted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OnSlot {
    id: CallId,
    talkgroup: u32,
}

impl OnSlot {
    fn of(c: &Call) -> Self {
        OnSlot { id: c.id, talkgroup: c.talkgroup }
    }
}

struct Recording {
    audio: Vec<f32>,
    frames: CallFrames,
    /// Its slot in the recorder pool (reused once free): the call JSON's `recorder_num`.
    recorder_num: u32,
    tx: Transmissions,
    /// How far off its channel the voice came in, Hz from the nominal
    /// frequency: the sum of the measurements and their number.
    freq_error: (f64, u32),
    /// The source it is on (AutoTune).
    source: usize,
    reception: Reception,
    /// Some of its voice was decrypted: kept here as well as on the call, which may be gone by the time the voice
    /// arrives (a call that expires in the same block), so the audio and the saved call are still known for what they are.
    decrypted: bool,
}

/// A control channel's decoding, shared by its site (which follows what
/// it decodes) and the sources its carriers are on (which feed it).
type Cc = Arc<Mutex<Box<dyn ControlChannel>>>;

/// A control channel carrier on a source: each block its head's output is
/// decoded ([`ControlChannel::push`]) before the system follows the steps.
struct Tap {
    site: u16,
    /// Its number among the system's carriers (0 for a hunted control channel).
    carrier: usize,
    head: HeadId,
    cc: Cc,
    at: TapTime,
}

/// Where a tap's samples sit on the system's clock.
#[derive(Clone, Copy)]
enum TapTime {
    /// A hunted control channel opened at `start_s`, when its source's clock offset was `offset_s`.
    Hunt { start_s: f64, offset_s: f64 },
    /// A watched carrier whose head began at source sample `start`.
    Watch { start: u64 },
}

/// One source's radio side: its channelizer and everything that decodes its
/// channels each block — voice channels, control channel carriers. Behind a
/// lock of its own, so each source can decode on a thread of its own while
/// the calls are followed in one place ([`Core`]).
struct SourceRx {
    src: Source,
    /// Keyed by (site, frequency): sites never share a voice channel.
    /// (A `BTreeMap`: channels run in one order, run after run.)
    channels: BTreeMap<(u16, u64), Channel>,
    /// In (site, carrier) order.
    taps: Vec<Tap>,
    /// What each site's control channel tells its voice channels (Phase 2: the scrambler key, once it gave it).
    voice_params: Vec<VoiceParams>,
    tout: Vec<(usize, TrackerOut)>,
}

/// What one voice channel's decoding of a block came to, for the calls.
#[derive(Default)]
struct VoiceResult {
    /// (site, call, output), in the order produced.
    routed: Vec<(u16, CallId, TrackerOut)>,
    /// A call's channel power and the noise under it while it carried voice.
    reception: Vec<(CallId, f64, f64)>,
    /// A call's voice frequency error, Hz from nominal.
    freq_error: Vec<(CallId, f64)>,
}

/// A block of one source, decoded: what its voice channels heard (by
/// channel, in key order) and its control channels' carriers' samples.
struct BlockDsp {
    voice: Vec<VoiceResult>,
    taps: Vec<(u16, usize, HeadId, Cc, Vec<Complex32>, f64)>,
    rate: f64,
    /// The source's time at the block's end, its clock offset, and whether it was a gap.
    time: f64,
    in_gap: bool,
}

/// A source's control channels' decoding of a block: per site, its steps
/// and samples, and what else its trunk follows each block, read under the
/// control channel's lock with the decoding (so the trunk needn't take it).
struct CcResult {
    site: u16,
    /// The (first) carrier's head.
    head: Option<HeadId>,
    steps: Vec<Step>,
    /// What each step tells the site's voice channels ([`ControlChannel::voice_params`] of its identity).
    params: Vec<VoiceParams>,
    samples: u64,
    notes: Vec<(NoteLevel, String)>,
    /// Watching: the carrier control is on now.
    control_hz: Option<u64>,
    /// Messages decoded and failed, so far.
    counts: (u64, u64),
    /// How far off the control channel comes in, Hz (AutoTune).
    offset_hz: Option<f32>,
}

impl SourceRx {
    /// Decode a channel's latest head output (or flush it): its outputs routed to its calls' slots.
    fn decode_channel(&mut self, key: (u16, u64), flush: bool) -> Option<VoiceResult> {
        let SourceRx { src, channels, voice_params, tout, .. } = self;
        let ch = channels.get_mut(&key)?;
        let iq: &[Complex32] = if flush { &[] } else { src.chz.output(ch.head).unwrap_or(&[]) };
        let params = voice_params.get(ch.site as usize).copied().unwrap_or_default();
        tout.clear();
        Radio::run_channel(ch, iq, &params, tout, flush);
        let mut r = VoiceResult::default();
        // Reception: the channel's power while it carries a call's voice.
        let voiced = |slot: usize| tout.iter().any(|(s, o)| *s & 1 == slot && matches!(o, TrackerOut::Audio(..) | TrackerOut::AnalogAudio(_)));
        let on_air = [voiced(0), voiced(1)];
        if !iq.is_empty() && (on_air[0] || on_air[1]) {
            let p = ch.meter.meter(iq) as f64;
            for (slot, c) in ch.calls.iter().enumerate() {
                if let Some(c) = c.filter(|_| on_air[slot]) {
                    r.reception.push((c.id, p, ch.noise));
                }
            }
        }
        Radio::route(ch, tout, &mut r.routed);
        if let Some(off) = ch.voice.offset_hz() {
            let err = off as f64 + ch.tune_ppm * 1e-6 * ch.freq_hz;
            for c in ch.calls.iter().flatten() {
                r.freq_error.push((c.id, err));
            }
        }
        Some(r)
    }

    /// A block ran: decode its voice channels; take its carriers' samples for their control channels.
    fn decode_block(&mut self) -> BlockDsp {
        let keys: Vec<(u16, u64)> = self.channels.keys().copied().collect();
        let voice = keys.into_iter().filter_map(|k| self.decode_channel(k, false)).collect();
        let (rate, fs, offset) = (self.src.chz.output_rate(), self.src.cfg().rate_hz, self.src.clock_offset_s);
        let taps = self
            .taps
            .iter()
            .map(|t| {
                let iq = self.src.chz.output(t.head).map(|v| v.to_vec()).unwrap_or_default();
                let t0 = match t.at {
                    TapTime::Hunt { start_s, offset_s } => start_s + (offset - offset_s),
                    TapTime::Watch { start } => start as f64 / fs + offset,
                };
                (t.site, t.carrier, t.head, t.cc.clone(), iq, t0)
            })
            .collect();
        BlockDsp { voice, taps, rate, time: self.src.time(), in_gap: self.src.in_gap }
    }

    fn set_tap(&mut self, tap: Tap) {
        self.taps.retain(|t| (t.site, t.carrier) != (tap.site, tap.carrier));
        let at = self.taps.partition_point(|t| (t.site, t.carrier) < (tap.site, tap.carrier));
        self.taps.insert(at, tap);
    }

    fn remove_taps(&mut self, site: u16) {
        self.taps.retain(|t| t.site != site);
    }
}

impl BlockDsp {
    /// Its control channels' samples decoded, each control channel under its own lock.
    fn decode_control(&mut self) -> Vec<CcResult> {
        let mut out: Vec<CcResult> = Vec::new();
        for (site, carrier, head, cc, iq, t0) in self.taps.drain(..) {
            let mut steps = Vec::new();
            let (params, notes, control_hz, counts, offset_hz) = {
                let mut cc = cc.lock().unwrap();
                cc.push(carrier, &iq, t0, self.rate, &mut steps);
                (steps.iter().map(|st| cc.voice_params(&st.identity)).collect::<Vec<_>>(), cc.take_notes(), cc.control_hz(), cc.counts(), cc.offset_hz())
            };
            match out.last_mut().filter(|r| r.site == site) {
                Some(r) => {
                    r.steps.extend(steps);
                    r.params.extend(params);
                    r.notes.extend(notes);
                    r.samples += iq.len() as u64;
                    (r.control_hz, r.counts, r.offset_hz) = (control_hz, counts, offset_hz);
                }
                None => out.push(CcResult { site, head: Some(head), steps, params, samples: iq.len() as u64, notes, control_hz, counts, offset_hz }),
            }
        }
        out
    }
}

/// The radio side every site shares: the sources (each behind its lock),
/// the recordings and the recorder pool. Kept apart from the sites' call
/// managers so both can be borrowed at once.
struct Radio {
    rx: Arc<Vec<Mutex<SourceRx>>>,
    /// The sources' settings (each source's own, without its lock).
    cfgs: Arc<[SourceConfig]>,
    recordings: HashMap<CallId, Recording>,
    free_nums: Vec<u32>,
    next_num: u32,
    max_recorders: usize,
    preroll_s: f64,
    capture_frames: bool,
    vocoder: mbe::Profile,
    /// Audio / info produced by the voice channels, as (site, call, output), applied after.
    pending: Vec<(u16, CallId, TrackerOut)>,
    /// Calls followed without a recorder now ([`Call::followed`]): a
    /// recorder's worth of room is kept for recordings.
    followed: usize,
}

impl Radio {
    fn source_for(&self, hz: f64) -> Option<usize> {
        self.cfgs.iter().position(|s| (hz - s.center_hz).abs() <= s.usable_half_width())
    }

    fn rx(&self, source: usize) -> MutexGuard<'_, SourceRx> {
        self.rx[source].lock().unwrap()
    }

    /// What a voice channel's block came to: its calls' reception and frequency error, its outputs to apply.
    fn take(&mut self, v: VoiceResult) {
        for (id, sig, noise) in v.reception {
            if let Some(r) = self.recordings.get_mut(&id) {
                r.reception.signal(sig);
                r.reception.noise(noise);
            }
        }
        self.pending.extend(v.routed);
        for (id, err) in v.freq_error {
            if let Some(r) = self.recordings.get_mut(&id) {
                r.freq_error.0 += err;
                r.freq_error.1 += 1;
            }
        }
    }

    /// Voice channels open, on every source (status and the dashboard's: each source's lock in turn).
    fn channel_count(&self) -> usize {
        self.rx.iter().map(|r| r.lock().unwrap().channels.len()).sum()
    }

    /// Run a channel's decoder over `iq` (then flush it, at the end of
    /// input), collecting what it produced as (slot, output).
    fn run_channel(ch: &mut Channel, iq: &[Complex32], params: &VoiceParams, tout: &mut Vec<(usize, TrackerOut)>, flush: bool) {
        ch.voice.set_params(params);
        // Vocode only the slots somebody is recording.
        ch.voice.listen(ch.calls.map(|c| c.map(|c| c.talkgroup)));
        let mut out = Vec::new();
        ch.voice.push(iq, &mut out);
        if flush {
            ch.voice.flush(&mut out);
        }
        tout.extend(out.into_iter().map(|v| (v.slot as usize, v.out)));
    }

    /// Hand a channel's outputs to the calls on its slots.
    fn route(ch: &Channel, tout: &mut Vec<(usize, TrackerOut)>, pending: &mut Vec<(u16, CallId, TrackerOut)>) {
        for (slot, o) in tout.drain(..) {
            if let Some(c) = ch.calls[slot & 1] {
                pending.push((ch.site, c.id, o));
            }
        }
    }
}

/// The recorder host one site's call manager sees.
struct SysHost<'a> {
    radio: &'a mut Radio,
    site: u16,
    bank: BankConfig,
    keys: Option<Arc<Keyring>>,
    /// How its analog calls are demodulated ([`ControlChannel::analog`]).
    analog: AnalogTuning,
}

impl SysHost<'_> {
    /// Open `call`'s voice channel on source `src` (a newer call on one
    /// already open takes its slot over, as in Trunk Recorder).
    fn open_channel(&mut self, call: &Call, src: usize) {
        let r = &mut *self.radio;
        let kind = call.voice.kind();
        let slot = call.voice.slot().map_or(0, |s| s as usize & 1);
        let key = (self.site, call.freq_hz);
        let mut guard = r.rx[src].lock().unwrap();
        let rx = &mut *guard;
        if let Some(ch) = rx.channels.get_mut(&key) {
            if ch.voice.kind() == kind {
                ch.calls[slot] = Some(OnSlot::of(call));
                return;
            }
            // The frequency carries another kind of call now (P25 Dynamic
            // Dual Mode: Phase 1 then Phase 2; SmartNet: analog then
            // digital): a channel of that kind instead. The calls on the old
            // one are over; they end when their timeout comes.
            let old = rx.channels.remove(&key).unwrap();
            rx.src.chz.remove_head(old.head);
        }
        let s = &mut rx.src;
        let rate = s.chz.output_rate();
        let cutoff = kind.cutoff_hz();
        let tune_ppm = s.tune_ppm;
        let (head, pre, start_sample) = s.chz.add_head(s.offset(call.freq_hz as f64), cutoff, r.preroll_s);
        // The noise floor under the channel, from the source's spectrum: analog squelch, and reception.
        let n = floor_slices(s.cfg().rate_hz);
        let mut prof = vec![0.0f64; n];
        s.chz.noise_profile(&mut prof);
        let slice = floor_slice(n, s.cfg().rate_hz, call.freq_hz as f64 - s.cfg().center_hz);
        let noise = s.chz.noise_in_band(prof[slice].max(1e-30), ChannelFilter::noise_bandwidth());
        let voice = voice::build(&VoiceSpec {
            kind,
            rate,
            t0: start_sample as f64 / s.cfg().rate_hz + s.clock_offset_s,
            seed: call.freq_hz as u32,
            vocoder: r.vocoder,
            bank: self.bank,
            // Analog squelch: this far above the noise under the channel.
            squelch: s.chz.threshold(prof[slice].max(1e-30), ChannelFilter::noise_bandwidth(), ANALOG_SQUELCH_DB),
            subaudible: false,
            analog: self.analog,
            ignore_hz: Vec::new(),
            keys: self.keys.clone(),
        });
        let mut calls = [None, None];
        calls[slot] = Some(OnSlot::of(call));
        let mut ch = Channel { site: self.site, head, calls, voice, freq_hz: call.freq_hz as f64, tune_ppm, meter: ChannelFilter::new(rate), noise };
        // Pre-roll: decode the replayed air now.
        let params = rx.voice_params.get(self.site as usize).copied().unwrap_or_default();
        rx.tout.clear();
        Radio::run_channel(&mut ch, &pre, &params, &mut rx.tout, false);
        Radio::route(&ch, &mut rx.tout, &mut r.pending);
        rx.channels.insert(key, ch);
    }

    /// Record `call`: a recorder of the pool, its voice channel opened.
    fn take_recorder(&mut self, call: &Call) -> Result<(), Reason> {
        let r = &mut *self.radio;
        let Some(src) = r.source_for(call.freq_hz as f64) else {
            return Err(Reason::NoSource);
        };
        let recorder_num = r.free_nums.pop().unwrap_or_else(|| {
            r.next_num += 1;
            r.next_num - 1
        });
        r.recordings.insert(
            call.id,
            Recording {
                audio: Vec::new(),
                frames: CallFrames::new(r.capture_frames),
                recorder_num,
                tx: Transmissions::default(),
                freq_error: (0.0, 0),
                source: src,
                reception: Reception::default(),
                decrypted: false,
            },
        );
        self.open_channel(call, src);
        Ok(())
    }
}

impl RecorderHost for SysHost<'_> {
    fn record(&mut self, call: &Call) -> Result<(), Reason> {
        if self.radio.recordings.len() >= self.radio.max_recorders {
            return Err(Reason::NoRecorder);
        }
        self.take_recorder(call)
    }

    fn record_continued(&mut self, _old: &Call, new: &Call) -> Result<(), Reason> {
        self.take_recorder(new)
    }


    fn follow(&mut self, call: &Call) -> bool {
        // Only with a recorder's worth of room to spare, and never on analog.
        let r = &*self.radio;
        if call.voice.analog() || r.recordings.len() + r.followed >= r.max_recorders {
            return false;
        }
        let Some(src) = r.source_for(call.freq_hz as f64) else {
            return false;
        };
        self.open_channel(call, src);
        self.radio.followed += 1;
        true
    }

    fn release(&mut self, call: &Call) {
        // (A call never given a channel — not recorded, not followed — needs no source's lock.)
        if call.followed {
            self.radio.followed = self.radio.followed.saturating_sub(1);
        } else if !call.recording {
            return;
        }
        let key = (self.site, call.freq_hz);
        let Some(src) = self.radio.source_for(call.freq_hz as f64) else { return };
        let mut rx = self.radio.rx(src);
        let Some(ch) = rx.channels.get_mut(&key) else { return };
        for c in ch.calls.iter_mut() {
            if c.is_some_and(|c| c.id == call.id) {
                *c = None;
            }
        }
        if ch.calls.iter().all(Option::is_none) {
            let ch = rx.channels.remove(&key).unwrap();
            rx.src.chz.remove_head(ch.head);
        }
    }
}

/// How a site listens, decided once when it starts from its protocol's
/// [`CarrierPlan`].
enum Listening {
    /// Not (yet): no control channel inside a source.
    Off,
    /// One control channel at a time, through the site's list.
    Hunting(Hunt),
    /// Every carrier at once (a DMR / NXDN site).
    Watching(Vec<Carrier>),
}

/// The control channel a hunting site has open.
struct Hunt {
    /// The control channels' bandwidth ([`CarrierPlan::Hunt`]).
    cutoff_hz: f64,
    /// Which of the control channels a source covers it is.
    index: usize,
    source: usize,
    head: HeadId,
    /// When it opened, and the samples decoded since: the site's clock.
    start_s: f64,
    samples: u64,
    /// Its source's clock offset when it opened (time since is samples
    /// since, plus how much the offset moved).
    offset_s: f64,
    /// AutoTune: the correction it was opened with, ppm; when it was last
    /// measured, and when the site last reopened one at a correction.
    ppm: f64,
    measured_s: f64,
    retuned_s: f64,
}

/// A carrier a watching site listens to.
struct Carrier {
    hz: f64,
    source: usize,
    head: HeadId,
    /// The source sample its head began at.
    start: u64,
}

/// One site of a trunked system: its control channel, parser (band plan) and
/// calls. What its system decides (talkgroups, rules, keys) is its system's
/// ([`System`]), passed in where it's needed.
struct Trunk {
    cfg: SiteConfig,
    /// Its place in the engine's sites (`Call::site`), and its system's number (`Call::system`).
    site: u16,
    system: u16,
    calls: CallManager,
    /// Its protocol's control channel decoding ([`super::control`]), shared with the taps on its carriers' sources.
    cc: Cc,
    /// How it listens: one control channel at a time, or every carrier.
    listening: Listening,
    /// The control channel's frequency (watching: the carrier control is on now).
    cc_hz: Option<u64>,
    last_good_s: f64,
    now_s: f64,
    good: u64,
    bad: u64,
    concluded: u64,
    identity: Identity,
    /// Why this control channel is not ours, while it isn't.
    mismatch: Option<String>,
    adjacent: std::collections::BTreeMap<(u32, u32, u32), AdjacentSite>,
    /// The call signs its base station IDs say, by the channel (Hz) each is for.
    callsigns: std::collections::BTreeMap<u64, String>,
    /// What its protocol says of itself, once: the identity fields its site
    /// lock goes by, and whether a control channel is reopened at a correction.
    id_fields: &'static [IdField],
    reopens: bool,
    /// What its control channel tells its voice channels (each source has a copy).
    voice_params: VoiceParams,
    /// How far off the control channel came in at the last block, Hz (AutoTune).
    cc_offset_hz: Option<f32>,
    /// Terminators' link control, as talker aliases see it: encrypted,
    /// clear, alias fragments; then Phase 2 alias messages ([`AliasLc`]).
    alias_lc: [u64; 4],
    /// The talkgroups whose terminators' link control was heard encrypted (noted once each).
    lc_protected_tgs: BTreeSet<u32>,
}

impl Trunk {
    fn host<'a>(&self, radio: &'a mut Radio, sys: &SystemConfig) -> SysHost<'a> {
        SysHost { radio, site: self.site, bank: self.cfg.bank, keys: sys.keys.clone(), analog: self.cc().analog() }
    }

    /// A note about this site.
    fn note(&self, level: NoteLevel, text: String) -> Event {
        Event::Note { system: self.system, site: Some(self.site), level, text }
    }

    /// The talkgroup file names this site as the one to keep `tg`'s calls
    /// from: by its name, or by its NAC or RFSS and site number.
    fn preferred_for(&self, tg: &super::talkgroups::Talkgroup) -> bool {
        let id = &self.identity;
        let by_name = !tg.preferred_site.is_empty() && tg.preferred_site.eq_ignore_ascii_case(&self.cfg.name);
        let n = tg.preferred_nac;
        let by_number = n != 0 && (id.nac().is_some_and(|x| x as u32 == n) || id.site().is_some_and(|s| id.rfss().unwrap_or(0) * 10000 + s == n));
        by_name || by_number
    }

    /// Start listening as its protocol says: the first control channel, or every carrier.
    fn start(&mut self, radio: &mut Radio, sys: &SystemConfig, events: &mut Vec<Event>) -> Result<(), String> {
        let plan = self.cc().plan();
        match plan {
            CarrierPlan::Hunt { cutoff_hz } if !self.cfg.control_channels.is_empty() => self.tune(radio, sys, cutoff_hz, 0, events),
            CarrierPlan::Watch { carriers, cutoff_hz, note } if !carriers.is_empty() => self.watch(radio, sys, &carriers, cutoff_hz, note, events),
            _ => Ok(()),
        }
    }

    /// Its control channel decoding, for what isn't asked every block
    /// (starting, a new control channel, status, the dashboard's figures):
    /// each block's results come in its [`CcResult`], read by the source's
    /// thread under the lock it decodes with. (Not held across a source's
    /// lock: sources take the two the other way round while they decode.)
    fn cc(&self) -> MutexGuard<'_, Box<dyn ControlChannel>> {
        self.cc.lock().unwrap()
    }

    /// Whether it is receiving: a control channel open, or carriers watched.
    fn running(&self) -> bool {
        !matches!(self.listening, Listening::Off)
    }

    /// Hunting: the control channel open, its source and frequency.
    fn hunted(&self) -> Option<(usize, f64)> {
        match (&self.listening, self.cc_hz) {
            (Listening::Hunting(h), Some(hz)) => Some((h.source, hz as f64)),
            _ => None,
        }
    }

    /// Hunting: open the control channel `index` (of those a source covers).
    fn tune(&mut self, radio: &mut Radio, sys: &SystemConfig, cutoff_hz: f64, index: usize, events: &mut Vec<Event>) -> Result<(), String> {
        let list: Vec<(f64, usize)> = self.cfg.control_channels.iter().filter_map(|&f| radio.source_for(f).map(|s| (f, s))).collect();
        if list.is_empty() {
            return Err(format!("{}: no control channel falls inside a source's bandwidth — move the center frequency.", sys.short_name));
        }
        let index = index % list.len();
        let (hz, src) = list[index];
        // (When it last reopened at a correction, from one control channel to the next.)
        let mut retuned_s = 0.0;
        if let Listening::Hunting(old) = &self.listening {
            retuned_s = old.retuned_s;
            let mut rx = radio.rx(old.source);
            rx.src.chz.remove_head(old.head);
            rx.remove_taps(self.site);
        }
        let retune = self.cc_hz.is_some();
        let (hunt, rate) = {
            let mut rx = radio.rx(src);
            let s = &mut rx.src;
            let (head, _, _) = s.chz.add_head(s.offset(hz), cutoff_hz, 0.0);
            let hunt = Hunt { cutoff_hz, index, source: src, head, start_s: self.now_s, samples: 0, offset_s: s.clock_offset_s, ppm: s.tune_ppm, measured_s: self.now_s, retuned_s };
            let rate = s.chz.output_rate();
            rx.set_tap(Self::hunt_tap(self.site, &self.cc, &hunt));
            (hunt, rate)
        };
        self.listening = Listening::Hunting(hunt);
        self.cc_hz = Some(hz.round() as u64);
        self.last_good_s = self.now_s;
        self.cc().restart(rate);
        if retune {
            // Another control channel may be another site: learn it afresh.
            self.identity = Identity::default();
            self.cc().forget_site();
            self.mismatch = None;
            self.adjacent.clear();
            self.callsigns.clear();
        }
        events.push(Event::ControlChannel { site: self.site, freq_hz: hz.round() as u64 });
        Ok(())
    }

    /// Hunting: its control channel's tap.
    fn hunt_tap(site: u16, cc: &Cc, h: &Hunt) -> Tap {
        Tap { site, carrier: 0, head: h.head, cc: cc.clone(), at: TapTime::Hunt { start_s: h.start_s, offset_s: h.offset_s } }
    }

    /// Watching: heads on the sources that cover each carrier.
    fn watch(&mut self, radio: &mut Radio, sys: &SystemConfig, carriers: &[f64], cutoff_hz: f64, note: String, events: &mut Vec<Event>) -> Result<(), String> {
        let mut heads = Vec::new();
        let mut outside = Vec::new();
        for &hz in carriers {
            let Some(src) = radio.source_for(hz) else {
                outside.push(format!("{:.5}", hz / 1e6));
                continue;
            };
            let mut rx = radio.rx(src);
            let s = &mut rx.src;
            let (head, _, start) = s.chz.add_head(s.offset(hz), cutoff_hz, 0.0);
            heads.push(Carrier { hz, source: src, head, start });
        }
        if !outside.is_empty() {
            let name = self.cc().name();
            return Err(format!("{}: {} frequencies outside every source's bandwidth: {} MHz — move a center frequency.", sys.short_name, name, outside.join(", ")));
        }
        for (i, c) in heads.iter().enumerate() {
            radio.rx(c.source).set_tap(Tap { site: self.site, carrier: i, head: c.head, cc: self.cc.clone(), at: TapTime::Watch { start: c.start } });
        }
        self.listening = Listening::Watching(heads);
        events.push(self.note(NoteLevel::Info, note));
        Ok(())
    }

    /// A block ran on `source` and its carriers on it were decoded (`cc`):
    /// follow what they say. `time`: the source's time after the block.
    fn on_block(&mut self, radio: &mut Radio, sys: &SystemConfig, source: usize, time: f64, cc: CcResult, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        match &self.listening {
            Listening::Hunting(_) => self.on_block_hunting(radio, sys, source, cc, events, call_events),
            Listening::Watching(_) => self.on_block_watching(radio, sys, source, time, cc, events, call_events),
            Listening::Off => {}
        }
    }

    fn on_block_hunting(&mut self, radio: &mut Radio, sys: &SystemConfig, source: usize, mut cc: CcResult, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        let Listening::Hunting(h) = &self.listening else { return };
        // (Another source's samples: decoded as it moved to this one. What it noted is still the site's.)
        if h.source != source || cc.head != Some(h.head) {
            events.extend(std::mem::take(&mut cc.notes).into_iter().map(|(level, text)| self.note(level, text)));
            return;
        }
        let Listening::Hunting(h) = &mut self.listening else { return };
        let (rate, offset) = {
            let rx = radio.rx(source);
            (rx.src.chz.output_rate(), rx.src.clock_offset_s)
        };
        h.samples += cc.samples;
        let moved = offset - h.offset_s;
        self.now_s = h.start_s + h.samples as f64 / rate + moved;
        let (index, cutoff_hz, measured_s) = (h.index, h.cutoff_hz, h.measured_s);
        self.cc_offset_hz = cc.offset_hz;
        self.follow(radio, sys, cc, events, call_events);
        if self.now_s - self.last_good_s > CC_HUNT_S && self.cfg.control_channels.len() > 1 {
            let _ = self.tune(radio, sys, cutoff_hz, index + 1, events);
        } else if self.now_s - measured_s >= TUNE_EVERY_S {
            self.measure(radio);
        }
        let mut host = self.host(radio, sys);
        self.calls.tick(self.now_s, &mut host, call_events);
    }

    fn on_block_watching(&mut self, radio: &mut Radio, sys: &SystemConfig, source: usize, time: f64, cc: CcResult, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        let Listening::Watching(carriers) = &self.listening else { return };
        if !carriers.iter().any(|c| c.source == source) {
            return;
        }
        self.now_s = self.now_s.max(time);
        // Where the control channel is now (Capacity Plus: the rest channel moves).
        if let Some(hz) = cc.control_hz.filter(|&h| Some(h) != self.cc_hz) {
            self.cc_hz = Some(hz);
            events.push(Event::ControlChannel { site: self.site, freq_hz: hz });
        }
        if cc.control_hz.is_some() {
            self.last_good_s = self.now_s;
        }
        self.follow(radio, sys, cc, events, call_events);
        let mut host = self.host(radio, sys);
        self.calls.tick(self.now_s, &mut host, call_events);
    }

    /// Follow what the control channel decoded, step by step: the site lock
    /// first (another site's grants aren't followed, and until every locked
    /// field has been heard nothing is), then the call manager.
    fn follow(&mut self, radio: &mut Radio, sys: &SystemConfig, cc: CcResult, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        for (level, text) in cc.notes {
            events.push(self.note(level, text));
        }
        // The site lock: on the fields its protocol states (none: no lock).
        let fields = self.id_fields;
        let lock = !fields.is_empty();
        for (st, p) in cc.steps.into_iter().zip(cc.params) {
            self.identity = st.identity;
            for m in st.msgs.iter().filter(|m| m.kind == MessageType::Adjacent && m.freq_hz > 0) {
                self.adjacent.insert((m.sys_id, m.rfss, m.site), AdjacentSite { sys_id: m.sys_id, rfss: m.rfss, site: m.site, freq_hz: m.freq_hz });
            }
            for m in st.msgs.iter().filter(|m| m.kind == MessageType::BaseStationId) {
                if let Some(c) = m.callsign.as_ref().filter(|_| m.freq_hz > 0) {
                    if self.callsigns.insert(m.freq_hz, c.clone()).as_ref() != Some(c) {
                        events.push(Event::CallSign { system: self.system, freq_hz: m.freq_hz, call_sign: c.clone() });
                    }
                }
            }
            if lock {
                let conflict = self.identity.conflict(&self.cfg.expect, fields);
                if conflict != self.mismatch {
                    if let Some(c) = &conflict {
                        let hz = self.cc_hz.unwrap_or(0) as f64 / 1e6;
                        events.push(self.note(NoteLevel::Warning, format!("Control channel {hz:.5} MHz is not this system: {c}")));
                    }
                    self.mismatch = conflict;
                }
            }
            events.extend(st.msgs.iter().cloned().map(|msg| Event::Message { site: self.site, msg }));
            if self.mismatch.is_some() {
                // Another system's (or site's) grants: not ours to follow — and
                // it doesn't count as a good control channel, so we hunt on.
                continue;
            }
            if st.good {
                self.last_good_s = self.now_s;
            }
            if lock && !self.identity.confirms(&self.cfg.expect, fields) {
                // Not yet known to be ours (the site comes every few seconds):
                // hold the grants — a call still going is granted again.
                continue;
            }
            // What the voice channels need from it (Phase 2's scrambler key), to every source's copy as it changes.
            let was = self.voice_params;
            self.voice_params.tdma_key = p.tdma_key.or(was.tdma_key);
            self.voice_params.nac = p.nac.or(was.nac);
            if self.voice_params != was {
                for rx in radio.rx.iter() {
                    rx.lock().unwrap().voice_params[self.site as usize] = self.voice_params;
                }
            }
            let mut host = self.host(radio, sys);
            self.calls.handle(&st.msgs, self.now_s, sys.policy(), &mut host, call_events);
        }
        (self.good, self.bad) = cc.counts;
    }

    /// AutoTune: how far off the control channel comes in (while it decodes),
    /// into its source's average; a control channel well off the correction
    /// is reopened at it, when its protocol allows.
    fn measure(&mut self, radio: &mut Radio) {
        let now = self.now_s;
        let (Listening::Hunting(h), Some(hz)) = (&mut self.listening, self.cc_hz) else { return };
        h.measured_s = now;
        if now - self.last_good_s > 1.0 {
            return;
        }
        let (Some(off), reopens) = (self.cc_offset_hz, self.reopens) else { return };
        let hz = hz as f64;
        let mut rx = radio.rx(h.source);
        let s = &mut rx.src;
        s.measured(h.ppm + off as f64 / hz * 1e6);
        let off_by = (s.tune_ppm - h.ppm) * 1e-6 * hz;
        if reopens && off_by.abs() > TUNE_RETUNE_HZ && now - h.retuned_s >= TUNE_RETUNE_S {
            s.chz.remove_head(h.head);
            let (head, _, _) = s.chz.add_head(s.offset(hz), h.cutoff_hz, 0.0);
            h.head = head;
            h.ppm = s.tune_ppm;
            h.retuned_s = now;
            h.start_s = now;
            h.samples = 0;
            h.offset_s = s.clock_offset_s;
            let rate = s.chz.output_rate();
            let tap = Self::hunt_tap(self.site, &self.cc, h);
            rx.set_tap(tap);
            drop(rx);
            self.cc().restart(rate);
        }
    }

    fn status(&self, radio: &Radio, sys: &SystemConfig) -> SiteStatus {
        // (One lock at a time: a temporary guard lasts the whole expression.)
        let (modulation, protocol) = {
            let c = self.cc();
            (c.modulation(), c.status())
        };
        SiteStatus {
            site: self.site,
            system: self.system,
            short_name: sys.short_name.clone(),
            site_name: self.cfg.name.clone(),
            now_s: self.now_s,
            control_channel_hz: self.cc_hz,
            identity: self.identity.clone(),
            good: self.good,
            bad: self.bad,
            modulation,
            active_calls: self.calls.calls.len(),
            recording: self.calls.calls.iter().filter(|c| radio.recordings.contains_key(&c.id)).count(),
            calls_concluded: self.concluded,
            mismatch: self.mismatch.clone(),
            adjacent: self.adjacent.values().copied().collect(),
            callsigns: if self.mismatch.is_none() { self.callsigns.iter().map(|(&hz, c)| (c.clone(), hz)).collect() } else { vec![] },
            patches: self.calls.patches.active(),
            protocol,
        }
    }
}

/// A system while recording: its config (talkgroups and keys as last set),
/// its radios' talker aliases, how many of its calls were saved.
struct System {
    cfg: SystemConfig,
    aliases: UnitAliases,
    concluded: u64,
}

/// Everything the engine follows calls with (behind [`Engine`]'s lock).
struct Core {
    systems: Vec<System>,
    radio: Radio,
    /// Every site of every trunked system, numbered as [`sites`] numbers them.
    trunks: Vec<Trunk>,
    conv: Conventional,
    /// Call ids, shared by every system (the conventional calls draw theirs here; they live in `conv`).
    call_ids: CallIds,
    conv_out: Vec<ConvOut>,
    /// Copies of one call on several sites.
    multisite: MultiSite,
    /// (system, ALGID, key ID) of keys found to decrypt to noise, told once.
    wrong_keys: BTreeSet<(u16, u8, u16)>,
    now_s: f64,
    events: Vec<Event>,
    call_events: Vec<CallEvent>,
    /// Wall-clock epoch ms at sample-clock time 0 ([`EngineConfig::epoch_ms_at_zero`]).
    epoch_ms_at_zero: f64,
    /// One copy of a call heard on several sites ([`EngineConfig::drop_duplicates`]).
    drop_duplicates: bool,
}

impl Core {
    fn new(mut cfg: EngineConfig) -> Result<Self, String> {
        // Every conventional channel's system exists (a default one, "conv", if none was given).
        let need = cfg.conventional.iter().map(|c| c.system + 1).max().unwrap_or(0);
        if need > cfg.systems.len() + 1 {
            return Err(format!("A conventional channel names system {}, which isn't there.", need - 1));
        }
        if need > cfg.systems.len() {
            cfg.systems.push(SystemConfig::conventional("conv"));
        }
        if let Some(c) = cfg.conventional.iter().find(|c| !cfg.systems[c.system].is_conventional()) {
            return Err(format!("Conventional channel {:.5} MHz is in {}, a trunked system.", c.freq_hz / 1e6, cfg.systems[c.system].short_name));
        }
        if cfg.sources.is_empty() {
            return Err("no sources configured".into());
        }
        if cfg.systems.iter().all(|s| s.sites.iter().all(|s| s.control_channels.is_empty())) && cfg.conventional.is_empty() {
            return Err("Add a control channel or a conventional channel.".into());
        }
        // A short name is a system's identity (its folder, its talker aliases,
        // its plugin settings); a site's name is its own within its system.
        let mut names = std::collections::HashSet::new();
        for sys in cfg.systems.iter_mut() {
            if !names.insert(sys.short_name.clone()) {
                return Err(format!("Two systems are named \"{}\" — each needs its own short name.", sys.short_name));
            }
            let mut seen = std::collections::HashSet::new();
            for site in sys.sites.iter_mut() {
                if site.name.is_empty() {
                    site.name = sys.short_name.clone();
                }
                if !seen.insert(site.name.clone()) {
                    return Err(format!("Two sites of {} are named \"{}\" — each needs its own name.", sys.short_name, site.name));
                }
            }
        }
        let history = cfg.preroll_s.max(cfg.conv.preroll_s).max(0.1);
        let spans: Vec<(f64, f64, f64)> = cfg.sources.iter().map(|s| (s.center_hz, s.rate_hz, s.usable_half_width())).collect();
        let conv = Conventional::new(&cfg.conventional, &spans, ConvConfig { vocoder: cfg.vocoder, ..cfg.conv }, cfg.bank, cfg.capture_frames)?;
        // (The engine's one copy of the sources' settings, shared.)
        let cfgs: Arc<[SourceConfig]> = std::mem::take(&mut cfg.sources).into();
        let sources: Vec<Source> = (0..cfgs.len()).map(|i| Source::new(&cfgs, i, history)).collect();
        let rate = sources[0].chz.output_rate();
        let ids = CallIds::default();
        let n_sites = sites(&cfg.systems).count();
        let rx = sources
            .into_iter()
            .map(|src| Mutex::new(SourceRx { src, channels: BTreeMap::new(), taps: Vec::new(), voice_params: vec![VoiceParams::default(); n_sites], tout: Vec::new() }))
            .collect();
        let mut radio = Radio {
            rx: Arc::new(rx),
            cfgs: cfgs.clone(),
            recordings: HashMap::new(),
            free_nums: Vec::new(),
            next_num: 0,
            max_recorders: cfg.max_recorders,
            preroll_s: cfg.preroll_s,
            capture_frames: cfg.capture_frames,
            vocoder: cfg.vocoder,
            pending: Vec::new(),
            followed: 0,
        };
        let mut events = Vec::new();
        let mut trunks = Vec::new();
        for (i, (system, sc)) in sites(&cfg.systems).enumerate() {
            let sys = &cfg.systems[system as usize];
            let mut t = Trunk {
                site: i as u16,
                system,
                calls: CallManager::with_ids(sys.calls, i as u16, system, ids.clone()),
                cc: Arc::new(Mutex::new(control::build(sc, rate))),
                listening: Listening::Off,
                cc_hz: None,
                last_good_s: 0.0,
                now_s: 0.0,
                good: 0,
                bad: 0,
                concluded: 0,
                identity: Identity::default(),
                mismatch: None,
                adjacent: Default::default(),
                callsigns: Default::default(),
                id_fields: &[],
                reopens: false,
                voice_params: VoiceParams::default(),
                cc_offset_hz: None,
                alias_lc: [0; 4],
                lc_protected_tgs: BTreeSet::new(),
                cfg: sc.clone(),
            };
            let (hold_s, id_fields, reopens) = {
                let c = t.cc();
                (c.patch_hold_s(), c.identity_fields(), c.reopens())
            };
            (t.calls.patches.hold_s, t.id_fields, t.reopens) = (hold_s, id_fields, reopens);
            t.start(&mut radio, sys, &mut events)?;
            trunks.push(t);
        }
        let systems = std::mem::take(&mut cfg.systems).into_iter().map(|cfg| System { cfg, aliases: UnitAliases::default(), concluded: 0 }).collect();
        Ok(Core {
            systems,
            radio,
            trunks,
            conv,
            call_ids: ids,
            conv_out: Vec::new(),
            multisite: MultiSite::default(),
            wrong_keys: BTreeSet::new(),
            now_s: 0.0,
            events,
            call_events: Vec::new(),
            epoch_ms_at_zero: cfg.epoch_ms_at_zero,
            drop_duplicates: cfg.drop_duplicates,
        })
    }

    /// The systems' configs, as they are now.
    fn system_configs(&self) -> Vec<SystemConfig> {
        self.systems.iter().map(|s| s.cfg.clone()).collect()
    }

    /// Preload site `site`'s band plan saved by [`Engine::bandplan`] (a
    /// grant heard before the next IDEN broadcast can then be followed at once).
    pub fn load_bandplan(&mut self, site: usize, s: &str) {
        if let Some(t) = self.trunks.get_mut(site) {
            t.cc().load_bandplan(s);
        }
    }
    /// A site's band plan to save: P25's IDEN tables; DMR's logical channel → frequency table.
    pub fn bandplan(&self, site: usize) -> String {
        self.trunks.get(site).map_or_else(String::new, |t| t.cc().bandplan())
    }

    /// Preload the talker aliases (Trunk Recorder's unitTagsOTA CSV) of the system named `short_name`.
    pub fn load_units(&mut self, short_name: &str, csv: &str) {
        if let Some(s) = self.systems.iter_mut().find(|s| s.cfg.short_name == short_name) {
            s.aliases = UnitAliases::parse_csv(csv);
        }
    }
    /// (short name, CSV) of each system's talker aliases that learned something since the last call.
    pub fn units_changed(&mut self) -> Vec<(String, String)> {
        self.systems.iter_mut().filter_map(|s| s.aliases.take_changed().then(|| (s.cfg.short_name.clone(), s.aliases.to_csv()))).collect()
    }
    /// A radio's talker alias on system `system` (a call's).
    pub fn unit_alias(&self, system: u16, unit: u32) -> Option<&str> {
        self.systems.get(system as usize)?.aliases.get(unit)
    }

    /// A key decrypted voice to noise on `system` (on its site `site`): say so, once a key and system.
    fn wrong_key(told: &mut BTreeSet<(u16, u8, u16)>, events: &mut Vec<Event>, system: u16, site: Option<u16>, c: CipherId, talkgroup: Option<u32>) {
        if told.insert((system, c.algid, c.keyid)) {
            let on = talkgroup.map_or(String::new(), |tg| format!(" (talkgroup {tg})"));
            let text = format!("Key ID {:#06X} ({}) decrypts this system's voice{on} to noise: it isn't the key the radios use", c.keyid, crate::p25::crypt::algid_name(c.algid));
            events.push(Event::Note { system, site, level: NoteLevel::Warning, text });
        }
    }

    /// Note a talker alias heard on a call of `system` (on its site `site`).
    fn learn_alias(&mut self, system: u16, site: Option<u16>, a: Alias, call_tg: Option<u32>) {
        let tg = a.talkgroup.or(call_tg);
        let (now_s, wacn, sys_id) = match site.and_then(|s| self.trunks.get(s as usize)) {
            Some(t) => (t.now_s, t.identity.wacn(), t.identity.sys_id()),
            None => (self.now_s, None, None),
        };
        let hex = |v: Option<u32>, w: usize| v.map_or(String::new(), |v| format!("{v:0w$x}"));
        let learned = UnitAlias {
            alias: a.alias.clone(),
            source: a.source.to_string(),
            time: ((self.epoch_ms_at_zero + now_s * 1000.0) / 1000.0) as i64,
            wacn: hex(wacn, 5),
            sys: hex(sys_id, 3),
            talkgroup: tg,
        };
        if self.systems.get_mut(system as usize).is_some_and(|s| s.aliases.learn(a.unit, learned)) {
            self.events.push(Event::UnitAlias {
                system,
                unit: a.unit,
                alias: a.alias,
                talkgroup: tg.unwrap_or(0),
            });
        }
    }

    /// Calls in progress (recording or monitoring), trunked then conventional.
    pub fn active_calls(&self) -> Vec<Call> {
        self.trunks.iter().flat_map(|t| t.calls.calls.iter()).chain(self.conv.calls()).cloned().collect()
    }

    /// Everything that happened since the last call.
    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// What [`Engine::channels`] measures on the sources, with what only the
    /// engine knows of each: control channels and carriers, then conventional channels.
    fn channel_plan(&self) -> (Vec<Want>, Vec<Want>) {
        let mut control = Vec::new();
        for t in &self.trunks {
            // Watching every carrier (a DMR site): each, the control one marked.
            if let Listening::Watching(carriers) = &t.listening {
                for c in carriers {
                    control.push(Want { system: t.system, site: Some(t.site), hz: c.hz, src: c.source, kind: if Some(c.hz.round() as u64) == t.cc_hz { ChannelKind::Control } else { ChannelKind::Carrier }, ..Default::default() });
                }
                continue;
            }
            let Some((src, hz)) = t.hunted() else { continue };
            let (offset_hz, quality) = {
                let c = t.cc();
                (c.offset_hz(), c.quality())
            };
            control.push(Want { system: t.system, site: Some(t.site), hz, src, kind: ChannelKind::Control, offset_hz, quality, calls: 0 });
        }
        let conv = self
            .conv
            .channels()
            .filter_map(|(hz, system, calls)| {
                let src = self.radio.source_for(hz)?;
                Some(Want { system: system as u16, site: None, hz, src, kind: ChannelKind::Conventional, calls, ..Default::default() })
            })
            .collect();
        (control, conv)
    }

    /// [`Engine::report`]'s part only the engine knows: each trunked system's
    /// calls and control channel decoding, and the engine's counts. Returns
    /// the hunted control channels (system key, source, Hz), whose level the
    /// sources measure.
    fn report_systems(&self, sink: &mut dyn Sink) -> Vec<(String, usize, f64)> {
        let mut hunted = Vec::new();
        // Calls per trunked system, all its sites'.
        for (i, s) in self.systems.iter().enumerate().filter(|(_, s)| !s.cfg.is_conventional()) {
            let calls = self.trunks.iter().filter(|t| t.system as usize == i).flat_map(|t| t.calls.calls.iter());
            let (active, recording) = calls.fold((0, 0), |(a, r), c| (a + 1, r + self.radio.recordings.contains_key(&c.id) as usize));
            let mut k = Scoped::new(sink, format!("sys/{}", metrics::key_part(&s.cfg.short_name)));
            k.gauge("active", active as f64);
            k.gauge("recording", recording as f64);
        }
        for t in self.trunks.iter().filter(|t| t.running()) {
            let key = self.systems[t.system as usize].cfg.site_key(&t.cfg).stats_key();
            let mut k = Scoped::new(sink, key.clone());
            let mut al = Scoped::new(&mut k, "aliasLc");
            for (name, n) in ["protected", "clear", "fragment", "mac"].iter().zip(t.alias_lc) {
                al.counter(name, n);
            }
            let mut cc = Scoped::new(&mut k, "cc");
            cc.counter("good", t.good);
            cc.counter("bad", t.bad);
            cc.gauge("locked", (t.now_s - t.last_good_s < 2.0 && t.mismatch.is_none()) as u8 as f64);
            // What its protocol measures (framing, eye opening, deviation…).
            t.cc().report(&mut cc);
            if let Some((src, hz)) = t.hunted() {
                hunted.push((key, src, hz));
            }
        }
        sink.gauge("eng/recording", self.radio.recordings.len() as f64);
        sink.gauge("eng/channels", (self.radio.channel_count() + self.conv.open_count()) as f64);
        sink.gauge("eng/recorders", self.radio.max_recorders as f64);
        hunted
    }

    /// Replace the talkgroup table of the system named `short_name` while
    /// recording: calls from now on go by it (an Ignore flag set, a tag
    /// fixed). False when there's no such system.
    pub fn set_talkgroups(&mut self, short_name: &str, talkgroups: Talkgroups) -> bool {
        let Some(s) = self.systems.iter_mut().find(|s| s.cfg.short_name == short_name) else { return false };
        s.cfg.talkgroups = talkgroups;
        true
    }

    /// The encryption keys of the system named `short_name`: its voice
    /// channels decrypt with them, those open now from their next encryption
    /// sync (with none, at once). False when there's no such system.
    pub fn set_keys(&mut self, short_name: &str, keys: Option<Arc<Keyring>>) -> bool {
        let Some(system) = self.systems.iter().position(|s| s.cfg.short_name == short_name) else { return false };
        self.systems[system].cfg.keys = keys.clone();
        // Channels already open (one is reused by the next call on its frequency) change too:
        // unlocking reaches them, and locking takes the keys from them at once.
        let sites: Vec<u16> = self.trunks.iter().filter(|t| t.system as usize == system).map(|t| t.site).collect();
        for r in self.radio.rx.iter() {
            for ch in r.lock().unwrap().channels.iter_mut().filter(|((s, _), _)| sites.contains(s)).map(|(_, c)| c) {
                ch.voice.set_keys(keys.clone());
            }
        }
        self.conv.set_system_keys(system, keys.as_ref());
        true
    }

    pub fn status(&self) -> Status {
        let sites: Vec<SiteStatus> = self.trunks.iter().filter(|t| t.running()).map(|t| t.status(&self.radio, &self.systems[t.system as usize].cfg)).collect();
        Status {
            now_s: self.now_s,
            active_calls: sites.iter().map(|s| s.active_calls).sum::<usize>() + self.conv.calls().count(),
            recording: self.radio.recordings.len(),
            channels_open: self.radio.channel_count() + self.conv.open_count(),
            conventional_open: self.conv.open_count(),
            calls_concluded: self.systems.iter().map(|s| s.concluded).sum(),
            sites,
            // ([`Engine::status`] fills these in, from the sources.)
            sources: Vec::new(),
        }
    }

    /// End of input: release what the receivers still hold and end every call.
    pub fn finish(&mut self) {
        for t in self.trunks.iter_mut() {
            let cc = {
                let mut c = t.cc();
                let mut steps = Vec::new();
                c.flush(&mut steps);
                let params = steps.iter().map(|st| c.voice_params(&st.identity)).collect();
                CcResult { site: t.site, head: None, steps, params, samples: 0, notes: c.take_notes(), control_hz: c.control_hz(), counts: c.counts(), offset_hz: c.offset_hz() }
            };
            t.follow(&mut self.radio, &self.systems[t.system as usize].cfg, cc, &mut self.events, &mut self.call_events);
        }
        // (Every source's channels in one key order, as they always ran.)
        let mut keys: Vec<((u16, u64), usize)> = Vec::new();
        for (src, r) in self.radio.rx.iter().enumerate() {
            keys.extend(r.lock().unwrap().channels.keys().map(|&k| (k, src)));
        }
        keys.sort();
        for (k, src) in keys {
            let v = self.radio.rx(src).decode_channel(k, true);
            if let Some(v) = v {
                self.radio.take(v);
            }
        }
        self.apply_pending();
        for t in self.trunks.iter_mut() {
            let mut host = t.host(&mut self.radio, &self.systems[t.system as usize].cfg);
            t.calls.end_all(&mut host, &mut self.call_events);
        }
        self.emit_call_events();
        let mut out = std::mem::take(&mut self.conv_out);
        self.conv.finish(&mut out);
        self.emit_conv(out);
    }

    /// Report what the conventional channels did.
    fn emit_conv(&mut self, mut out: Vec<ConvOut>) {
        for o in out.drain(..) {
            match o {
                ConvOut::Event(e) => self.events.push(e),
                ConvOut::End { call, audio, frames, recorder_num, tx, reception, freq_error_hz } => {
                    self.write_call(Held { call: call.clone(), audio, frames, recorder_num, tx, reception, freq_error_hz });
                    self.events.push(Event::CallEnd(call));
                }
                ConvOut::Alias(system, a) => self.learn_alias(system, None, a, None),
                ConvOut::WrongKey(system, c) => Self::wrong_key(&mut self.wrong_keys, &mut self.events, system, None, c, None),
            }
        }
        self.conv_out = out;
    }

    /// A block of `source` was decoded — its voice channels (`dsp`), its
    /// control channels' carriers (`cc`, in system order): follow it.
    fn after_block(&mut self, source: usize, dsp: BlockDsp, cc: Vec<CcResult>) {
        // Voice channels on this source, then its control channels. What
        // the voice channels heard (and channels just opened heard in their
        // pre-roll) reaches the calls after both, once, at this block's time.
        for v in dsp.voice {
            self.radio.take(v);
        }
        let mut cc = cc.into_iter().peekable();
        for t in self.trunks.iter_mut() {
            while cc.next_if(|r| r.site < t.site).is_some() {}
            if let Some(r) = cc.next_if(|r| r.site == t.site) {
                t.on_block(&mut self.radio, &self.systems[t.system as usize].cfg, source, dsp.time, r, &mut self.events, &mut self.call_events);
            }
        }
        self.apply_pending();
        if source == 0 {
            self.now_s = dsp.time;
        }
        if !self.conv.is_empty() {
            let mut out = std::mem::take(&mut self.conv_out);
            {
                let mut rx = self.radio.rx[source].lock().unwrap();
                self.conv.on_block(source, &mut rx.src.chz, dsp.time, dsp.in_gap, &self.call_ids, &|i| &self.systems[i].cfg, &mut out);
            }
            self.emit_conv(out);
        }
        self.apply_pending();
        self.emit_call_events();
    }

    /// Route what the voice trackers produced to their calls.
    fn apply_pending(&mut self) {
        for (site, id, o) in std::mem::take(&mut self.radio.pending) {
            let Some(t) = self.trunks.get_mut(site as usize) else { continue };
            let system = t.system;
            let now = t.now_s;
            // The call it's for, found once (gone already: it expired in this same block).
            let call = t.calls.calls.iter_mut().find(|c| c.id == id);
            let rec = self.radio.recordings.get_mut(&id);
            match o {
                TrackerOut::Audio(samples, frame) => {
                    let Some(rec) = rec else { continue };
                    rec.tx.note(rec.audio.len(), now);
                    rec.audio.extend_from_slice(&samples);
                    rec.frames.push(frame);
                    // Fail closed: a call that is gone already is not known to be clear, and its
                    // voice is not given to a plugin as if it were.
                    let (tg, decrypted) = match call {
                        Some(c) => {
                            c.note_audio(now);
                            (c.talkgroup, c.encrypted || c.decrypted || rec.decrypted)
                        }
                        None => (0, true),
                    };
                    if self.multisite.audio_lead(id, now) {
                        self.events.push(Event::Audio { call_id: id, system, talkgroup: tg, samples, decrypted });
                    }
                }
                TrackerOut::AnalogAudio(samples) => {
                    let Some(rec) = rec else { continue };
                    rec.tx.note(rec.audio.len(), now);
                    rec.audio.extend_from_slice(&samples);
                    let tg = match call {
                        Some(c) => {
                            c.note_audio(now);
                            c.talkgroup
                        }
                        None => 0,
                    };
                    if self.multisite.audio_lead(id, now) {
                        self.events.push(Event::Audio { call_id: id, system, talkgroup: tg, samples, decrypted: false });
                    }
                }
                TrackerOut::Info { source, flags } => match call {
                    Some(c) => {
                        if CallManager::note_info(c, rec.map(|r| &mut r.tx), source, flags, now) {
                            self.call_events.push(CallEvent::Update(c.clone()));
                        }
                    }
                    // (Gone already: its recording still drops an encrypted transmission.)
                    None if flags.encrypted => {
                        if let Some(rec) = rec {
                            rec.tx.mark_encrypted(now);
                        }
                    }
                    None => {}
                },
                TrackerOut::Cipher(cipher) => {
                    if let Some(c) = call.filter(|c| c.cipher != Some(cipher)) {
                        c.cipher = Some(cipher);
                        self.call_events.push(CallEvent::Update(c.clone()));
                    }
                }
                TrackerOut::WrongKey(c) => {
                    let tg = call.map(|c| c.talkgroup);
                    Self::wrong_key(&mut self.wrong_keys, &mut self.events, system, Some(site), c, tg);
                }
                TrackerOut::Decrypted => {
                    let tx = rec.map(|r| {
                        r.decrypted = true;
                        &mut r.tx
                    });
                    match call {
                        Some(c) => {
                            if CallManager::note_decrypted(c, tx, now) {
                                self.call_events.push(CallEvent::Update(c.clone()));
                            }
                        }
                        None => {
                            if let Some(tx) = tx {
                                tx.mark_decrypted(now);
                            }
                        }
                    }
                }
                // (Only conventional FM asks for it.)
                TrackerOut::Subaudible(_) => {}
                TrackerOut::Position(p) => {
                    if let Some(c) = call {
                        if CallManager::note_position(c, &p) {
                            self.events.push(Event::UnitPosition { system, unit: p.unit, lat: p.lat, lon: p.lon, talkgroup: c.talkgroup });
                            self.call_events.push(CallEvent::Update(c.clone()));
                        }
                    }
                }
                TrackerOut::Alias(a) => {
                    let tg = call.map(|c| c.talkgroup);
                    self.learn_alias(system, Some(site), a, tg);
                }
                TrackerOut::BaseStationId(call_sign) => {
                    if let Some(c) = call {
                        self.events.push(Event::CallSign { system, freq_hz: c.freq_hz, call_sign });
                    }
                }
                TrackerOut::AliasLc(sign) => {
                    let tg = call.map_or(0, |c| c.talkgroup);
                    t.alias_lc[sign as usize] += 1;
                    if sign == AliasLc::Protected && t.lc_protected_tgs.insert(tg) {
                        let text = format!("Talkgroup {tg}: link control is encrypted, so its radios' talker aliases can't be read");
                        self.events.push(t.note(NoteLevel::Info, text));
                    }
                }
            }
        }
    }

    fn emit_call_events(&mut self) {
        for ev in std::mem::take(&mut self.call_events) {
            match ev {
                CallEvent::Start(mut c) => {
                    c.twin = self.link_twins(&c);
                    self.events.push(Event::CallStart(c));
                }
                CallEvent::Update(c) => self.events.push(Event::CallUpdate(c)),
                CallEvent::End(c) => {
                    self.conclude(&c);
                    self.events.push(Event::CallEnd(c));
                }
            }
        }
    }

    /// Multi-site: link a call just granted with its copies on the other
    /// sites of its system. True: it is a copy of one already going (marked
    /// [`Call::twin`]).
    fn link_twins(&mut self, c: &Call) -> bool {
        let Some(site) = c.site.filter(|_| self.drop_duplicates) else { return false };
        let others = self.trunks.iter().filter(|t| t.system == c.system && t.site != site).map(|t| t.calls.calls.as_slice());
        self.multisite.link_new(c, site, others);
        let twin = !self.multisite.twins(c.id).is_empty();
        if let Some(own) = self.trunks.get_mut(site as usize).and_then(|t| t.calls.call_mut(c.id)).filter(|_| twin) {
            own.twin = true;
        }
        twin
    }

    /// The other sites' copies of call `id` still going or held, as (call, site).
    pub fn twins(&self, id: CallId) -> Vec<(CallId, u16)> {
        self.multisite.twins(id)
    }

    /// AutoTune: start source `source` from an earlier run's score, ppm,
    /// until it measures its own.
    pub fn seed_tune(&mut self, source: usize, ppm: f64) {
        if let Some(r) = self.radio.rx.get(source) {
            if ppm.is_finite() && ppm.abs() <= 50.0 {
                let s = &mut r.lock().unwrap().src;
                s.seed = Some(ppm);
                s.retune();
            }
        }
    }

    /// A call ended: save it, or (multi-site) the best of its copies once the last one ends.
    fn conclude(&mut self, call: &Call) {
        let held = self.radio.recordings.remove(&call.id).map(|rec| {
            self.radio.free_nums.push(rec.recorder_num);
            let freq_error_hz = (rec.freq_error.1 > 0).then(|| rec.freq_error.0 / rec.freq_error.1 as f64);
            // AutoTune: every call that lasted (each site's copy, encrypted
            // ones too) scores its source.
            if let Some(hz) = freq_error_hz.filter(|_| call.freq_hz > 0 && call.last_update_s - call.start_s >= TUNE_CALL_MIN_S) {
                if let Some(r) = self.radio.rx.get(rec.source) {
                    r.lock().unwrap().src.measured(hz / call.freq_hz as f64 * 1e6);
                }
            }
            // (A call whose record never learned it was decrypted, because it was already gone when the news came, is saved
            // as the decrypted call it is.)
            let mut call = call.clone();
            if rec.decrypted {
                call.encrypted = true;
                call.decrypted = true;
            }
            let mut h = Held { call, audio: rec.audio, frames: rec.frames, recorder_num: rec.recorder_num, tx: rec.tx, reception: rec.reception, freq_error_hz };
            // Now, so copies of a multi-site call are ranked by the audio they'd be saved with.
            h.trim(self.systems.get(h.call.system as usize).map_or(0.0, |s| s.cfg.save.min_transmission_s));
            h
        });
        let trunks = &self.trunks;
        let Some(copies) = self.multisite.conclude(call.id, held, |site, tg| site.and_then(|s| trunks.get(s as usize)).is_some_and(|t| t.preferred_for(tg))) else { return };
        // The best copy its site's rules keep is saved; the rest are its duplicates.
        // (None kept: each was reported not saved.)
        let mut kept: Option<Call> = None;
        for h in copies {
            match &kept {
                Some(k) => self.events.push(Event::Duplicate { call: h.call, kept: k.clone() }),
                None => {
                    let c = h.call.clone();
                    if self.write_call(h) {
                        kept = Some(c);
                    }
                }
            }
        }
    }

    /// A finished call's record and audio, as [`Event::Concluded`]; false
    /// when not kept (silent, or shorter than its system's minimum).
    fn write_call(&mut self, h: Held) -> bool {
        let (site, system) = (h.call.site, h.call.system);
        let Some(sys) = self.systems.get(system as usize) else { return false };
        let trunk = site.and_then(|s| self.trunks.get(s as usize));
        let saved = save_call(
            h,
            &SaveContext {
                rules: sys.cfg.save,
                short_name: &sys.cfg.short_name,
                site: trunk.map_or("", |t| &t.cfg.name),
                epoch_ms_at_zero: self.epoch_ms_at_zero,
                units: Some(&sys.aliases),
                unit_tags: Some(&sys.cfg.unit_tags).filter(|t| !t.is_empty() || t.mode != Default::default()),
                wacn: trunk.and_then(|t| t.identity.wacn()),
                sys_id: trunk.and_then(|t| t.identity.sys_id()),
                nac: trunk.and_then(|t| t.identity.nac()),
            },
        );
        match saved {
            Ok(k) => {
                self.systems[system as usize].concluded += 1;
                if let Some(t) = site.and_then(|s| self.trunks.get_mut(s as usize)) {
                    t.concluded += 1;
                }
                self.events.push(Event::Concluded(k));
                true
            }
            Err(call) => {
                self.events.push(Event::NotSaved(call));
                false
            }
        }
    }
}

/// The engine: trunked systems' and conventional channels' calls, from
/// sources' samples. A handle — clones share one engine — so each source can
/// be fed from a thread of its own ([`Engine::push_u8`], [`Engine::push_iq`],
/// [`Engine::push_gap`]) while another reads events and status: each block is
/// decoded under its source's own lock (the channelizer, the voice channels
/// and control channel carriers on it), then followed under the engine's.
/// Fed from one thread, it runs exactly as it always did.
#[derive(Clone)]
pub struct Engine {
    shared: Arc<Shared>,
}

struct Shared {
    core: Mutex<Core>,
    rx: Arc<Vec<Mutex<SourceRx>>>,
    sources: Arc<[SourceConfig]>,
    /// The systems as configured (talkgroups and keys as last set).
    systems: Mutex<Arc<Vec<SystemConfig>>>,
    /// Each site's system.
    site_systems: Vec<u16>,
}

// (Each source can be fed from a thread of its own.)
const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Engine>();
};

impl Engine {
    pub fn new(cfg: EngineConfig) -> Result<Self, String> {
        let core = Core::new(cfg)?;
        let systems = Arc::new(core.system_configs());
        let site_systems = sites(&systems).map(|(system, _)| system).collect();
        let shared = Shared { rx: core.radio.rx.clone(), sources: core.radio.cfgs.clone(), systems: Mutex::new(systems), site_systems, core: Mutex::new(core) };
        Ok(Engine { shared: Arc::new(shared) })
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        self.shared.core.lock().unwrap()
    }

    /// Preload site `site`'s band plan saved by [`Engine::bandplan`] (a
    /// grant heard before the next IDEN broadcast can then be followed at once).
    pub fn load_bandplan(&self, site: usize, s: &str) {
        self.core().load_bandplan(site, s)
    }
    /// A site's band plan to save: P25's IDEN tables; DMR's logical channel → frequency table.
    pub fn bandplan(&self, site: usize) -> String {
        self.core().bandplan(site)
    }
    /// Preload the talker aliases (Trunk Recorder's unitTagsOTA CSV) of the system named `short_name`.
    pub fn load_units(&self, short_name: &str, csv: &str) {
        self.core().load_units(short_name, csv)
    }
    /// (short name, CSV) of each talker alias table that learned something since the last call.
    pub fn units_changed(&self) -> Vec<(String, String)> {
        self.core().units_changed()
    }
    /// A radio's talker alias on system `system` (a call's).
    pub fn unit_alias(&self, system: u16, unit: u32) -> Option<String> {
        self.core().unit_alias(system, unit).map(str::to_string)
    }

    pub fn sources(&self) -> &[SourceConfig] {
        &self.shared.sources
    }

    /// The systems, as configured (with the talkgroups and keys last set);
    /// their sites are numbered as [`sites`] numbers them.
    pub fn systems(&self) -> Arc<Vec<SystemConfig>> {
        self.shared.systems.lock().unwrap().clone()
    }

    /// Calls in progress (recording or monitoring), trunked then conventional.
    pub fn active_calls(&self) -> Vec<Call> {
        self.core().active_calls()
    }
    /// Everything that happened since the last call.
    pub fn drain_events(&self) -> Vec<Event> {
        self.core().drain_events()
    }
    /// Power spectrum (dBFS, fft-shifted) of a source's latest block — for the waterfall.
    pub fn spectrum(&self, source: usize, bins: usize) -> Vec<f32> {
        self.shared.rx.get(source).map_or_else(Vec::new, |r| r.lock().unwrap().src.chz.power_spectrum(bins))
    }
    /// A source's noise floor across its band in `slices` equal slices
    /// (first at −fs/2), dBFS per FFT bin — the waterfall's floor.
    pub fn noise_profile(&self, source: usize, slices: usize) -> Vec<f64> {
        let Some(r) = self.shared.rx.get(source) else { return Vec::new() };
        let r = r.lock().unwrap();
        let s = &r.src;
        let mut p = vec![0.0f64; slices];
        s.chz.noise_profile(&mut p);
        p.iter().map(|&v| metrics::bin_dbfs(v, s.chz.fft_size())).collect()
    }

    /// Every channel listened to now — control channels, voice channels,
    /// conventional channels — with its power and the floor under it. About
    /// a millisecond's work: for the dashboard, once a second at most. (What
    /// only the engine knows is taken first; each source is then measured
    /// under its own lock alone, so decoding never waits on the engine's.)
    pub fn channels(&self) -> Vec<ChannelSnapshot> {
        let (control, conv) = self.core().channel_plan();
        let snap = |s: &Source, profile: &[f64], w: &Want| {
            let n = s.chz.fft_size();
            ChannelSnapshot {
                system: w.system,
                site: w.site,
                freq_hz: w.hz.round() as u64,
                source: w.src,
                kind: w.kind,
                power_db: metrics::bin_dbfs(s.chz.band_power(s.offset(w.hz), CC_HALF_HZ), n),
                noise_db: metrics::bin_dbfs(slice_at(profile, s, w.hz), n),
                offset_hz: w.offset_hz,
                quality: w.quality,
                calls: w.calls,
            }
        };
        let (mut control_out, mut conv_out): (Vec<Option<ChannelSnapshot>>, Vec<Option<ChannelSnapshot>>) = (control.iter().map(|_| None).collect(), conv.iter().map(|_| None).collect());
        let mut voice = Vec::new();
        for (src, r) in self.shared.rx.iter().enumerate() {
            let r = r.lock().unwrap();
            let mut profile = vec![0.0f64; PROFILE_SLICES];
            r.src.chz.noise_profile(&mut profile);
            for (w, o) in control.iter().zip(control_out.iter_mut()).chain(conv.iter().zip(conv_out.iter_mut())).filter(|(w, _)| w.src == src) {
                *o = Some(snap(&r.src, &profile, w));
            }
            for ch in r.channels.values() {
                let system = self.shared.site_systems[ch.site as usize];
                let w = Want { system, site: Some(ch.site), hz: ch.freq_hz, src, kind: ChannelKind::Voice, offset_hz: ch.voice.offset_hz(), quality: ch.voice.quality(), calls: ch.calls.iter().flatten().count() };
                voice.push(snap(&r.src, &profile, &w));
            }
        }
        let mut out: Vec<ChannelSnapshot> = control_out.into_iter().flatten().chain(voice).chain(conv_out.into_iter().flatten()).collect();
        out.sort_by_key(|c| c.freq_hz);
        out.dedup_by_key(|c| (c.freq_hz, c.system, c.site));
        out
    }

    /// What the engine has measured, for the dashboard's history: per source
    /// (named by `source_names`) its noise floor and frequency error; per
    /// trunked system its control channel's decoding and demodulation, and
    /// its calls now. See [`crate::metrics`]. (The engine's lock and each
    /// source's are taken one at a time.)
    pub fn report(&self, source_names: &[String], sink: &mut dyn Sink) {
        for (i, r) in self.shared.rx.iter().enumerate() {
            let r = r.lock().unwrap();
            let s = &r.src;
            let name = source_names.get(i).cloned().unwrap_or_else(|| i.to_string());
            let mut k = Scoped::new(sink, format!("src/{}", metrics::key_part(&name)));
            let mut p = vec![0.0f64; PROFILE_SLICES];
            s.chz.noise_profile(&mut p);
            p.sort_by(|a, b| a.total_cmp(b));
            if s.chz.sample_position() > 0 {
                k.gauge("noise", metrics::bin_dbfs(p[p.len() / 2], s.chz.fft_size()));
            }
            if let Some(e) = s.error_ppm() {
                k.gauge("ppm", e);
            }
            k.gauge("tune", s.tune_ppm);
        }
        let hunted = self.core().report_systems(sink);
        // A hunted control channel's level above the floor under it.
        for (key, src, hz) in hunted {
            let r = self.shared.rx[src].lock().unwrap();
            let s = &r.src;
            if s.chz.sample_position() == 0 {
                continue;
            }
            let n = s.chz.fft_size();
            let mut p = vec![0.0f64; PROFILE_SLICES];
            s.chz.noise_profile(&mut p);
            let (sig, noise) = (metrics::bin_dbfs(s.chz.band_power(s.offset(hz), CC_HALF_HZ), n), metrics::bin_dbfs(slice_at(&p, s, hz), n));
            let mut k = Scoped::new(sink, key);
            let mut cc = Scoped::new(&mut k, "cc");
            cc.gauge("signal", sig);
            cc.gauge("noise", noise);
            cc.gauge("snr", sig - noise);
        }
    }
    /// Replace the talkgroup table of the system named `short_name` (trunked
    /// or conventional): calls from now on go by it. False when there's no such system.
    pub fn set_talkgroups(&self, short_name: &str, talkgroups: Talkgroups) -> bool {
        let mut c = self.core();
        let ok = c.set_talkgroups(short_name, talkgroups);
        *self.shared.systems.lock().unwrap() = Arc::new(c.system_configs());
        ok
    }
    /// The encryption keys of the system named `short_name` (trunked or
    /// conventional). False when there's no such system.
    pub fn set_keys(&self, short_name: &str, keys: Option<Arc<Keyring>>) -> bool {
        let mut c = self.core();
        let ok = c.set_keys(short_name, keys);
        *self.shared.systems.lock().unwrap() = Arc::new(c.system_configs());
        ok
    }
    pub fn status(&self) -> Status {
        let mut st = self.core().status();
        st.sources = self
            .shared
            .rx
            .iter()
            .map(|r| {
                let s = &r.lock().unwrap().src;
                SourceTune { error_ppm: s.error_ppm(), applied_ppm: s.tune_ppm, seeded: s.seed.is_some() && s.errors.is_empty() }
            })
            .collect();
        st
    }
    /// The other sites' copies of call `id` still going or held, as (call, site).
    pub fn twins(&self, id: CallId) -> Vec<(CallId, u16)> {
        self.core().twins(id)
    }
    /// AutoTune: start source `source` from an earlier run's score, ppm, until it measures its own.
    pub fn seed_tune(&self, source: usize, ppm: f64) {
        self.core().seed_tune(source, ppm)
    }
    /// End of input: release what the receivers still hold and end every call.
    pub fn finish(&self) {
        self.core().finish()
    }

    /// Source `source`'s sample clock is this far behind the engine's
    /// clock, s: added to every time taken from its samples from now on.
    /// Every clock here is a sample count; a source's runs slow or fast by
    /// its crystal's error, starts late by the time it took to open, and
    /// jumps when samples are lost. The platform measures it against the
    /// wall clock (trunk-app's `Session` does) and sets it: in small steps
    /// for drift, at once for a jump. 0 by default (replays).
    pub fn set_clock_offset(&self, source: usize, offset_s: f64) {
        if let Some(r) = self.shared.rx.get(source) {
            r.lock().unwrap().src.clock_offset_s = offset_s;
        }
    }

    /// The offset [`set_clock_offset`](Self::set_clock_offset) last set, s.
    pub fn clock_offset(&self, source: usize) -> f64 {
        self.shared.rx.get(source).map_or(0.0, |r| r.lock().unwrap().src.clock_offset_s)
    }

    /// A block of `source` ran: its control channels decoded (each under
    /// its own lock), then everything followed under the engine's.
    fn follow_block(&self, source: usize, mut dsp: BlockDsp) {
        let cc = dsp.decode_control();
        self.core().after_block(source, dsp, cc);
    }

    /// Feed a source's RTL-SDR native u8 IQ (any length).
    pub fn push_u8(&self, source: usize, data: &[u8]) {
        let rx = &self.shared.rx[source];
        // An I byte left from the last buffer goes with this one's first (a
        // buffer of odd length must not swap I and Q from then on).
        let mut data = data;
        let first = {
            let mut r = rx.lock().unwrap();
            let mut block = None;
            if let (Some(i), Some(&q)) = (r.src.half, data.first()) {
                r.src.half = None;
                if r.src.chz.feed_u8(&[i, q]).1 {
                    block = Some(r.decode_block());
                }
                data = &data[1..];
            }
            if data.len() % 2 == 1 {
                r.src.half = data.last().copied();
                data = &data[..data.len() - 1];
            }
            block
        };
        if let Some(b) = first {
            self.follow_block(source, b);
        }
        let mut off = 0;
        while off < data.len() {
            let (used, block) = {
                let mut r = rx.lock().unwrap();
                let (used, ran) = r.src.chz.feed_u8(&data[off..]);
                (used, ran.then(|| r.decode_block()))
            };
            off += used;
            if let Some(b) = block {
                self.follow_block(source, b);
            }
            if used == 0 {
                break;
            }
        }
    }

    /// Feed a source's float IQ.
    pub fn push_iq(&self, source: usize, iq: &[Complex32]) {
        let rx = &self.shared.rx[source];
        let mut off = 0;
        while off < iq.len() {
            let (used, block) = {
                let mut r = rx.lock().unwrap();
                let (used, ran) = r.src.chz.feed(&iq[off..]);
                (used, ran.then(|| r.decode_block()))
            };
            off += used;
            if let Some(b) = block {
                self.follow_block(source, b);
            }
            if used == 0 {
                break;
            }
        }
    }

    /// `n` samples of `source` that never arrived — the driver dropped them,
    /// or the radio went quiet (unplugged, wedged) — fed as silence. Every
    /// clock here is a sample count, so this is what keeps a source's clock,
    /// and with it call times, timeouts and the matching of calls across
    /// sources, in step with the air. Conventional channels neither learn
    /// their noise floor from the silence nor open on it.
    pub fn push_gap(&self, source: usize, n: u64) {
        const CHUNK: u64 = 32768;
        // Faint noise (about −80 dBFS) rather than zeros: receivers' gain
        // control and equalisers see a signal of the kind they always idle on.
        let mut x = 0x9e37_79b9_7f4a_7c15u64 ^ n;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        let quiet: Vec<Complex32> = (0..n.min(CHUNK)).map(|_| Complex32::new(rnd(), rnd()) * 2e-4).collect();
        self.shared.rx[source].lock().unwrap().src.in_gap = true;
        let mut left = n;
        while left > 0 {
            let k = left.min(CHUNK) as usize;
            self.push_iq(source, &quiet[..k]);
            left -= k as u64;
        }
        self.shared.rx[source].lock().unwrap().src.in_gap = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::calls::Voice;
    use super::super::voice::VoiceKind;

    fn tuned_source(auto_tune: bool) -> Source {
        let cfg = SourceConfig { center_hz: 770e6, rate_hz: 2.4e6, auto_tune, guard_hz: DEFAULT_GUARD_HZ };
        Source::new(&Arc::from(vec![cfg]), 0, 0.1)
    }

    /// The score is the median of the last measurements: a few far off
    /// (a mis-lock) don't move it, drift does; without AutoTune nothing is corrected.
    #[test]
    fn autotune_scores_the_median_of_its_last_measurements() {
        let mut s = tuned_source(true);
        for ppm in [3.6, 3.62, 3.64, 12.0, 3.66, -9.0, 3.68] {
            s.measured(ppm);
        }
        assert_eq!(s.tune_ppm, 3.64);
        for _ in 0..30 {
            s.measured(3.0);
        }
        assert_eq!((s.errors.len(), s.tune_ppm), (TUNE_KEEP, 3.0), "the last 20");
        s.measured(f64::NAN);
        s.measured(80.0);
        assert_eq!(s.errors.len(), TUNE_KEEP, "nothing absurd is kept");
        let mut off = tuned_source(false);
        off.measured(2.0);
        assert_eq!((off.error_ppm(), off.tune_ppm), (Some(2.0), 0.0));
    }

    /// An earlier run's score corrects until something is measured.
    #[test]
    fn autotune_starts_from_a_saved_score() {
        let sys = SystemConfig::trunked("s", vec![SiteConfig { control_channels: vec![770.5e6], ..Default::default() }]);
        let e = Engine::new(EngineConfig { systems: vec![sys], sources: vec![SourceConfig { center_hz: 770e6, rate_hz: 2.4e6, auto_tune: true, guard_hz: DEFAULT_GUARD_HZ }], ..Default::default() }).unwrap();
        e.seed_tune(0, 2.5);
        let t = e.status().sources[0];
        assert_eq!((t.applied_ppm, t.error_ppm, t.seeded), (2.5, None, true));
        e.shared.rx[0].lock().unwrap().src.measured(1.0);
        let t = e.status().sources[0];
        assert_eq!((t.applied_ppm, t.seeded), (1.0, false));
    }

    /// An engine on one source, with a conventional channel (so it runs).
    fn plain_engine() -> Engine {
        use super::super::conventional::{ConvChannel, ConvMode};
        let ch = ConvChannel { talkgroup: 1, ..ConvChannel::new(155.2e6, ConvMode::Fm) };
        Engine::new(EngineConfig { sources: vec![SourceConfig { center_hz: 155e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }], conventional: vec![ch], ..Default::default() }).unwrap()
    }

    /// u8 IQ in buffers of odd length: the same as in even ones (no I/Q swap).
    #[test]
    fn odd_length_buffers_keep_i_and_q_paired() {
        let bytes: Vec<u8> = (0..100_001u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        let (even, odd) = (plain_engine(), plain_engine());
        even.push_u8(0, &bytes[..100_000]);
        for chunk in bytes[..100_000].chunks(4097) {
            odd.push_u8(0, chunk);
        }
        let pos = |e: &Engine| e.shared.rx[0].lock().unwrap().src.chz.sample_position();
        assert_eq!(pos(&even), pos(&odd));
        assert_eq!(even.spectrum(0, 256), odd.spectrum(0, 256));
        // (Asking for more cells than bins is answered, not a panic.)
        assert!(even.spectrum(0, 1 << 20).iter().all(|&v| v == -120.0));
    }

    /// Site `i` follows `msgs` as its control channel decoded them, the engine's clock at `now_s`.
    fn follow(e: &mut Core, i: usize, msgs: &[Message], now_s: f64) {
        let Core { trunks, radio, events, call_events, systems, .. } = e;
        let t = &mut trunks[i];
        t.now_s = now_s;
        let step = Step { good: true, msgs: msgs.to_vec(), identity: t.identity.clone() };
        let cc = CcResult { site: t.site, head: None, steps: vec![step], params: vec![VoiceParams::default()], samples: 0, notes: Vec::new(), control_hz: None, counts: (t.good, t.bad), offset_hz: None };
        t.follow(radio, &systems[t.system as usize].cfg, cc, events, call_events);
    }

    /// Site `i` follows `msgs`, taken in as they were on the air.
    fn handle(e: &mut Core, i: usize, msgs: &[Message]) {
        let now_s = msgs.iter().map(|m| m.time_s).fold(e.trunks[i].now_s, f64::max);
        follow(e, i, msgs, now_s);
    }

    /// Site `i`'s clock reads `now_s`: its quiet calls end.
    fn tick(e: &mut Core, i: usize, now_s: f64) {
        let Core { trunks, radio, call_events, systems, .. } = e;
        let mut host = trunks[i].host(radio, &systems[trunks[i].system as usize].cfg);
        trunks[i].calls.tick(now_s, &mut host, call_events);
    }

    /// One system of one site on 851.0125 MHz, on one source.
    fn one_site(short_name: &str, f: impl FnOnce(&mut SystemConfig)) -> EngineConfig {
        let mut sys = SystemConfig::trunked(short_name, vec![SiteConfig { control_channels: vec![851.0125e6], ..Default::default() }]);
        f(&mut sys);
        EngineConfig { systems: vec![sys], sources: vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }], ..Default::default() }
    }

    /// A control channel's messages come out after they were on the air (P25's diversity
    /// bank holds them 0.35 s): a call times out `callTimeoutS` after the engine took in its
    /// last update, not after that update's air time (#55).
    #[test]
    fn a_call_times_out_from_when_its_last_update_was_taken_in() {
        let mut e = Core::new(one_site("s", |s| s.calls.call_timeout_s = 1.0)).unwrap();
        let msg = |kind, t| Message { kind, time_s: t, talkgroup: 101, freq_hz: 851_500_000, ..Default::default() };
        follow(&mut e, 0, &[msg(MessageType::Grant, 0.0)], 0.45);
        follow(&mut e, 0, &[msg(MessageType::Update, 1.0)], 1.45);
        tick(&mut e, 0, 2.2);
        assert_eq!(e.trunks[0].calls.calls.len(), 1, "1.2 s after its last update was on the air, 0.75 s after it came in");
        tick(&mut e, 0, 2.5);
        assert!(e.trunks[0].calls.calls.is_empty(), "1.05 s after it came in");
    }

    /// A call split at `maxCallS` with every recorder in use goes on
    /// recorded: its new part takes over the old part's recorder and channel.
    #[test]
    fn a_split_call_keeps_its_recorder() {
        let cfg = EngineConfig { max_recorders: 1, ..one_site("s", |s| s.calls.max_call_s = 2.0) };
        let mut e = Core::new(cfg).unwrap();
        let f = 851_500_000;
        for t in [0.0, 1.0, 2.0] {
            handle(&mut e, 0, &[Message { kind: MessageType::Grant, time_s: t, talkgroup: 101, freq_hz: f, ..Default::default() }]);
            tick(&mut e, 0, t + 0.5);
        }
        let calls = &e.trunks[0].calls.calls;
        assert_eq!(calls.len(), 1);
        assert!(calls[0].recording && calls[0].start_s > 2.0, "{:?}", calls[0]);
        assert_eq!(e.radio.rx(0).channels[&(0, f)].calls[0].map(|c| c.id), Some(calls[0].id), "the channel is still open, for the new part");
    }

    /// An encrypted call that isn't recorded is followed (its link control:
    /// who talks) while a recorder's worth of room is left; when it ends its
    /// channel closes and the room comes back.
    #[test]
    fn encrypted_calls_are_followed_while_there_is_room() {
        let cfg = EngineConfig { max_recorders: 2, ..one_site("s", |_| ()) };
        let mut e = Core::new(cfg).unwrap();
        let grant = |tg: u32, f: u64, t: f64, encrypted: bool| Message { kind: MessageType::Grant, time_s: t, talkgroup: tg, freq_hz: f, encrypted, ..Default::default() };
        handle(&mut e, 0, &[grant(101, 851_500_000, 0.0, true), grant(102, 851_600_000, 0.0, true), grant(103, 851_700_000, 0.0, true)]);
        let followed: Vec<(u32, bool)> = e.trunks[0].calls.calls.iter().map(|c| (c.talkgroup, c.followed)).collect();
        assert_eq!(followed, [(101, true), (102, true), (103, false)], "two followed (the room for two), the third not");
        assert_eq!((e.radio.followed, e.radio.channel_count()), (2, 2));
        // A clear call is still recorded (following never takes a recorder).
        handle(&mut e, 0, &[grant(104, 851_800_000, 0.1, false)]);
        assert!(e.trunks[0].calls.calls.iter().any(|c| c.talkgroup == 104 && c.recording));
        // The followed ones end: their channels close, the room is back.
        tick(&mut e, 0, 10.0);
        assert_eq!((e.radio.followed, e.radio.channel_count()), (0, 0));
        handle(&mut e, 0, &[grant(105, 851_900_000, 11.0, true)]);
        assert!(e.trunks[0].calls.calls.iter().any(|c| c.talkgroup == 105 && c.followed));
    }

    #[test]
    fn a_site_is_named_by_its_system_and_its_own_name_if_it_has_one() {
        let own = SiteKey { system: "dcfd", site: "south" };
        assert_eq!((own.stats_key(), own.file_stem(), own.to_string()), ("sys/dcfd/site/south".into(), "dcfd.south".into(), "dcfd/south".into()));
        for site in ["dcfd", ""] {
            let k = SiteKey { system: "dcfd", site };
            assert_eq!((k.own_name(), k.stats_key(), k.file_stem(), k.to_string()), (None, "sys/dcfd".into(), "dcfd".into(), "dcfd".into()));
        }
    }

    #[test]
    fn a_conventional_channel_names_a_conventional_system() {
        use super::super::conventional::{ConvChannel, ConvMode};
        let mut cfg = one_site("t", |_| ());
        cfg.conventional = vec![ConvChannel { system: 0, ..ConvChannel::new(851.3e6, ConvMode::Fm) }];
        assert!(Engine::new(cfg.clone()).err().unwrap().contains("a trunked system"));
        cfg.conventional[0].system = 1;
        cfg.systems.push(SystemConfig::conventional("c"));
        let e = Engine::new(cfg).unwrap();
        assert_eq!(e.systems().iter().map(|s| (s.short_name.as_str(), s.is_conventional())).collect::<Vec<_>>(), [("t", false), ("c", true)]);
    }

    #[test]
    fn sites_need_their_own_names_and_share_call_ids() {
        let src = vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }];
        let site = |s: &str| SiteConfig { name: s.into(), control_channels: vec![851.0125e6], ..Default::default() };
        let cfg = EngineConfig { systems: vec![SystemConfig::trunked("a", vec![site(""), site("a")])], sources: src.clone(), ..Default::default() };
        assert!(Engine::new(cfg).err().unwrap().contains("Two sites of a are named \"a\""));
        let cfg = EngineConfig { systems: vec![SystemConfig::trunked("a", vec![site(""), site("a2")]), SystemConfig::trunked("b", vec![site("")])], sources: src.clone(), ..Default::default() };
        assert_eq!(sites(&cfg.systems).map(|(n, s)| (n, s.name.as_str())).collect::<Vec<_>>(), [(0, ""), (0, "a2"), (1, "")], "sites are numbered system by system");
        let e = Engine::new(cfg).unwrap();
        let st = e.status();
        assert_eq!(st.sites.iter().map(|s| (s.site, s.system, s.site_name.as_str())).collect::<Vec<_>>(), [(0, 0, "a"), (1, 0, "a2"), (2, 1, "b")]);
        let cfg = EngineConfig { systems: vec![SystemConfig::trunked("a", vec![site("")]), SystemConfig::conventional("a")], sources: src, ..Default::default() };
        assert!(Engine::new(cfg).err().unwrap().contains("Two systems are named \"a\""));
        let ids = CallIds::default();
        let (x, y) = (ids.clone(), ids);
        assert_eq!([x.next(), y.next(), x.next()], [1, 2, 3]);
    }

    /// Three sites grant TG 101: east and west of one system, far of another.
    /// East decodes `east_good` clean frames, west 100. What gets saved?
    fn two_sites(east_good: usize, prefer: &str, dedupe: bool) -> (Vec<Event>, [CallId; 3]) {
        two_sites_with(east_good, prefer, dedupe, 100, 0.0)
    }

    /// [`two_sites`], east's copy keeping audio for only `east_audio` of its
    /// frames, and calls kept from `min_call_s`.
    fn two_sites_with(east_good: usize, prefer: &str, dedupe: bool, east_audio: usize, min_call_s: f64) -> (Vec<Event>, [CallId; 3]) {
        use super::super::frames::{Codec, VoiceFrame};
        let tgs: Talkgroups = [(101, super::super::talkgroups::Talkgroup { number: 101, preferred_site: prefer.into(), ..Default::default() })].into_iter().collect();
        let save = SaveRules { normalize: false, min_call_s, ..Default::default() };
        let site = |n: &str, cc: f64| SiteConfig { name: n.into(), control_channels: vec![cc], ..Default::default() };
        let sys = |name: &str, sites| SystemConfig { talkgroups: tgs.clone(), save, ..SystemConfig::trunked(name, sites) };
        let cfg = EngineConfig {
            systems: vec![sys("dc", vec![site("east", 851.0125e6), site("west", 851.2125e6)]), sys("md", vec![site("far", 851.4125e6)])],
            sources: vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }],
            drop_duplicates: dedupe,
            ..Default::default()
        };
        let mut e = Core::new(cfg).unwrap();
        let mut ids = [0; 3];
        for (i, (t, f)) in [(0.0, 851_500_000), (0.4, 851_600_000), (0.2, 851_700_000)].into_iter().enumerate() {
            handle(&mut e, i, &[Message { kind: MessageType::Grant, time_s: t, talkgroup: 101, freq_hz: f, ..Default::default() }]);
            ids[i] = e.trunks[i].calls.calls[0].id;
        }
        e.emit_call_events();
        for (i, (good, audio)) in [(east_good, east_audio), (100, 100), (100, 100)].into_iter().enumerate() {
            let rec = e.radio.recordings.get_mut(&ids[i]).unwrap();
            for k in 0..100 {
                let kind = if k < good { mbe::Kind::Voice } else { mbe::Kind::Repeat };
                rec.frames.push(VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind });
                if k < audio {
                    rec.audio.extend_from_slice(&[0.1; mbe::FRAME_SAMPLES]);
                }
            }
        }
        e.drain_events();
        // West ends last.
        for i in [0, 2, 1] {
            tick(&mut e, i, 10.0);
            e.emit_call_events();
        }
        (e.drain_events(), ids)
    }

    /// A frequency that carried a Phase 1 call is granted for Phase 2 (P25
    /// Dynamic Dual Mode) before that call has timed out: the new call gets
    /// a Phase 2 channel, not the Phase 1 one.
    #[test]
    fn a_frequency_changing_mode_gets_a_channel_of_the_new_kind() {
        let mut e = Core::new(one_site("ddm", |_| ())).unwrap();
        let f = 851_500_000;
        fn grant(e: &mut Core, f: u64, t: f64, tg: u32, phase2: bool, slot: u8) {
            handle(e, 0, &[Message { kind: MessageType::Grant, time_s: t, talkgroup: tg, freq_hz: f, voice: if phase2 { Voice::P25Tdma { slot } } else { Voice::P25 }, ..Default::default() }]);
        }
        grant(&mut e, f, 0.0, 101, false, 0);
        assert_eq!(e.radio.rx(0).channels[&(0, f)].voice.kind(), VoiceKind::Fdma);
        grant(&mut e, f, 1.0, 202, true, 1);
        let rx = e.radio.rx(0);
        let ch = &rx.channels[&(0, f)];
        assert_eq!(ch.voice.kind(), VoiceKind::Tdma);
        let tg202 = e.trunks[0].calls.calls.iter().find(|c| c.talkgroup == 202).unwrap().id;
        assert_eq!(ch.calls.map(|c| c.map(|c| c.id)), [None, Some(tg202)]);
        drop(rx);
        // The same kind again shares it, on its own slot.
        grant(&mut e, f, 1.5, 303, true, 0);
        assert_eq!(e.radio.channel_count(), 1);
        assert!(e.radio.rx(0).channels[&(0, f)].calls.iter().all(Option::is_some));
    }

    /// Decrypted voice for a call that expired in the same block is still known for what it is: not passed on as
    /// clear (a plugin that may not have decrypted calls must not get it), and the call is saved as decrypted.
    #[test]
    fn decrypted_voice_for_a_call_that_is_already_gone_is_not_taken_for_clear() {
        use crate::trunk::frames::{Codec, VoiceFrame};
        let mut e = Core::new(one_site("d", |_| ())).unwrap();
        handle(&mut e, 0, &[Message { kind: MessageType::Grant, time_s: 0.0, talkgroup: 101, freq_hz: 851_500_000, ..Default::default() }]);
        let call = e.trunks[0].calls.calls[0].clone();
        let id = call.id;
        let frame = || VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind: crate::mbe::Kind::Voice };
        // The call is removed (by the control-channel tick) before the voice output is applied.
        e.trunks[0].calls.calls.clear();
        e.radio.pending.push((0, id, TrackerOut::Decrypted));
        e.radio.pending.push((0, id, TrackerOut::Audio(vec![0.0; 160], frame())));
        e.apply_pending();
        let audio: Vec<bool> = e.drain_events().iter().filter_map(|x| if let Event::Audio { decrypted, .. } = x { Some(*decrypted) } else { None }).collect();
        assert_eq!(audio, [true], "its audio is not passed on as clear");
        // And the call, concluded from the snapshot taken before the news, is saved as decrypted.
        e.conclude(&call);
        let ev = e.drain_events();
        let k = ev.iter().find_map(|x| if let Event::Concluded(k) = x { Some(k) } else { None }).expect("the call is saved");
        assert!(k.record.to_json().contains("\"decrypted\":1"), "{}", k.record.to_json());
    }

    fn saved(ev: &[Event]) -> Vec<CallId> {
        ev.iter().filter_map(|x| if let Event::Concluded(k) = x { Some(k.call.id) } else { None }).collect()
    }

    #[test]
    fn a_call_heard_on_two_sites_is_saved_once() {
        let (ev, [east, west, far]) = two_sites(80, "", true);
        assert_eq!(saved(&ev), [far, west], "west decoded more; far is another system");
        assert!(ev.iter().any(|x| matches!(x, Event::Duplicate { call, kept } if call.id == east && kept.id == west)));
        // The talkgroup prefers east, and east has 90 % of west's clean audio.
        let (ev, [east, _, far]) = two_sites(95, "EAST", true);
        assert_eq!(saved(&ev), [far, east]);
        // Switched off: every copy.
        let (ev, [east, west, far]) = two_sites(80, "", false);
        assert_eq!(saved(&ev), [east, far, west]);
        assert!(!ev.iter().any(|x| matches!(x, Event::Duplicate { .. })));
        // East decodes best, but kept too little audio to be saved: west's copy is.
        let (ev, [east, west, far]) = two_sites_with(100, "", true, 10, 1.0);
        assert_eq!(saved(&ev), [far, west]);
        assert!(ev.iter().any(|x| matches!(x, Event::NotSaved(c) if c.id == east)));
        assert!(!ev.iter().any(|x| matches!(x, Event::Duplicate { .. })));
    }

}

#[cfg(test)]
mod threads {
    use super::*;
    use crate::nxdn::layer3::{CallHead, CALL_CONFERENCE};
    use crate::nxdn::synth::{add_noise, dibits, modulate, Tx};
    use crate::nxdn::Rate;
    use crate::trunk::conventional::{ConvChannel, ConvMode};

    const FS: f64 = 240_000.0;

    /// A source with a keyed NXDN48 transmission (group `tg`) at centre + 50 kHz.
    fn air(tg: u16, seed: u64) -> Vec<Complex32> {
        let head = CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source: 77, destination: tg };
        let pad = 2400;
        let mut d = vec![0u8; pad];
        let mut on = vec![false; pad];
        let frames = dibits(&Tx::new(Rate::N48, 1, head, 2, 12).frames());
        on.extend(std::iter::repeat_n(true, frames.len()));
        d.extend(frames);
        d.extend(vec![0u8; 2 * pad]);
        on.extend(std::iter::repeat_n(false, 2 * pad));
        let mut ph = 0.0;
        let mut iq = modulate(&d, Some(&on), Rate::N48, FS, 50_000.0, 0.3, &mut ph);
        add_noise(&mut iq, 0.01, &mut { seed });
        iq
    }

    fn engine() -> Engine {
        let sources = [450e6, 460e6].map(|c| SourceConfig { center_hz: c, rate_hz: FS, auto_tune: false, guard_hz: 10_000.0 }).to_vec();
        let conventional = [450.05e6, 460.05e6].map(|f| ConvChannel::new(f, ConvMode::Nxdn(Rate::N48))).to_vec();
        Engine::new(EngineConfig { sources, conventional, ..Default::default() }).unwrap()
    }

    /// (talkgroup, audio) of each call saved, in talkgroup order.
    fn saved(e: &Engine) -> Vec<(u32, Vec<f32>)> {
        let mut out: Vec<(u32, Vec<f32>)> = e.drain_events().into_iter().filter_map(|ev| if let Event::Concluded(k) = ev { Some((k.call.talkgroup, k.audio)) } else { None }).collect();
        out.sort_by_key(|c| c.0);
        out
    }

    /// Two sources fed from threads of their own, while a third reads the
    /// engine all along: no deadlock, and (nothing crossing between them)
    /// the same calls, to the sample, as fed from one thread.
    #[test]
    fn sources_fed_from_threads_decode_as_from_one() {
        let feeds = [air(3001, 1), air(3002, 2)];
        let one = engine();
        for (a, b) in feeds[0].chunks(4096).zip(feeds[1].chunks(4096)) {
            one.push_iq(0, a);
            one.push_iq(1, b);
        }
        one.finish();
        let want = saved(&one);
        assert_eq!(want.len(), 2, "both calls, fed from one thread");
        let e = engine();
        let done = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for (i, f) in feeds.iter().enumerate() {
                let (e, done) = (&e, &done);
                s.spawn(move || {
                    for c in f.chunks(4096) {
                        e.push_iq(i, c);
                    }
                    done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                });
            }
            while done.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                let _ = (e.status(), e.channels(), e.active_calls(), e.spectrum(0, 64), e.noise_profile(1, 8));
                std::thread::yield_now();
            }
        });
        e.finish();
        assert_eq!(saved(&e), want);
    }
}
