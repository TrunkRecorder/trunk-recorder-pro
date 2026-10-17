//! Trunked systems above the radio(s) — Trunk Recorder's
//! monitor_messages() loop plus its recorders, for each system:
//!
//! ```text
//! source(s) u8 IQ → Channelizer per source
//!   control channel head → receiver bank → TSDU groups → TSBKs → TsbkParser → CallManager
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
//! own time — its control channel's sample clock. Each site of a multi-site
//! system is a system of its own (see [`SiteConfig::expect`]); a call heard
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

use super::calls::{conventional_index, conventional_system, Call, CallConfig, CallEvent, CallId, CallIds, CallManager, Reason, RecorderHost, MAX_CONVENTIONAL};
use super::conventional::{CallRules, ConvChannel, ConvConfig, ConvOut, Conventional};
use super::message::{Message, MessageType};
use super::multisite::{Held, MultiSite};
use super::record::{save_call, Reception, SaveContext, Transmissions};
use super::record::{Concluded, SaveRules};
use super::talkgroups::Talkgroups;
use super::frames::CallFrames;
use super::voice::TrackerOut;
use super::units::{AliasBook, UnitAlias, UnitTags};
use super::identity::Identity;
use super::control::{self, CarrierPlan, ControlChannel, Protocol, ProtocolStatus, Step};
use super::voice::{self, CipherId, VoiceDecoder, VoiceKind, VoiceParams, VoiceSpec};
use crate::alias::Alias;
use crate::dsp::channelizer::floor_slices;
use crate::dsp::{AnalogTuning, Channelizer, HeadId};
use crate::mbe;
use crate::metrics::{self, Scoped, Sink};
use crate::p25::alias::AliasLc;
use crate::p25::crypt::Keyring;
use crate::p25::diversity::BankConfig;
use crate::dsp::fm::ChannelFilter;
use crate::smartnet::Bandplan;
use crate::dmr;

/// A SmartNet system (its control channels are SmartNet, not P25).
#[derive(Clone, Debug)]
pub struct SmartnetConfig {
    pub bandplan: Bandplan,
    /// Voice mode of a talkgroup whose grant was never heard: analog FM?
    pub analog_default: bool,
}

/// Analog voice squelch: carrier this far above the noise floor, dB.
const ANALOG_SQUELCH_DB: f64 = 6.0;

/// One-sided channel filter cutoff for P25, Hz.
const CHANNEL_CUTOFF_HZ: f64 = 7000.0;
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


/// One site of a trunked system: a control channel to follow. Sites with
/// the same short name are one system — a call heard on several of them is
/// one call ([`super::multisite`]) — and share its talkgroups, call and save
/// rules, radio names and keys (each site carries a copy).
#[derive(Clone, Debug)]
pub struct SiteConfig {
    /// Its system's folder and record name (Trunk Recorder's shortName).
    pub short_name: String,
    /// The site's own name, unique in its system (empty: the system's short
    /// name): the talkgroup file's Preferred Site, its band plan's file.
    pub site: String,
    pub control_channels: Vec<f64>,
    pub calls: CallConfig,
    /// Receivers for its control and voice channels (the modulation).
    pub bank: BankConfig,
    pub talkgroups: Talkgroups,
    /// Only follow a control channel whose identity agrees (a field left
    /// None matches anything) — keeps a system off a neighbour's or another
    /// site's control channel.
    pub expect: Identity,
    /// Its trunking protocol (P25 by default).
    pub protocol: Protocol,
    pub save: SaveRules,
    /// Its own names for its radios (the unit names file).
    pub unit_tags: UnitTags,
    /// Its encryption keys (P25): voice it has the key for is decrypted.
    pub keys: Option<Arc<Keyring>>,
}

/// Each site's system number ([`Call::system`]): sites with one short name
/// are one system, numbered in the order its first site comes.
pub fn system_numbers(sites: &[SiteConfig]) -> Vec<u16> {
    let mut names: Vec<&str> = Vec::new();
    sites
        .iter()
        .map(|s| match names.iter().position(|x| *x == s.short_name) {
            Some(k) => k as u16,
            None => {
                names.push(&s.short_name);
                (names.len() - 1) as u16
            }
        })
        .collect()
}

impl SiteConfig {
    /// The site's name: its own, else its system's.
    pub fn site_name(&self) -> &str {
        if self.site.is_empty() { &self.short_name } else { &self.site }
    }

    /// Where its control channel's stats go: `sys/<system>` for a site named
    /// as its system (a system of one site, the first of a system made of
    /// systems), else `sys/<system>/site/<site>`.
    pub fn stats_key(&self) -> String {
        let sys = format!("sys/{}", metrics::key_part(&self.short_name));
        if self.site_name() == self.short_name { sys } else { format!("{sys}/site/{}", metrics::key_part(self.site_name())) }
    }
}

impl Default for SiteConfig {
    fn default() -> Self {
        SiteConfig {
            short_name: "sys1".into(),
            site: String::new(),
            control_channels: vec![],
            calls: CallConfig::default(),
            bank: BankConfig::default(),
            talkgroups: Talkgroups::default(),
            expect: Identity::default(),
            protocol: Protocol::P25,
            save: SaveRules::default(),
            unit_tags: UnitTags::default(),
            keys: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// The trunked systems' sites, in order: [`Call::site`] indexes them. A
    /// system is numbered ([`Call::system`]) in the order its first site is.
    pub sites: Vec<SiteConfig>,
    pub sources: Vec<SourceConfig>,
    /// Seconds of air a voice channel replays from before its grant.
    pub preroll_s: f64,
    /// Recorders shared by every system.
    pub max_recorders: usize,
    /// The conventional systems: each its own short name, rules and names
    /// (a conventional channel's `system` indexes them).
    pub conv_systems: Vec<ConvSystem>,
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
            sites: vec![],
            sources: vec![],
            preroll_s: 1.0,
            max_recorders: 32,
            conv_systems: vec![],
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

/// A conventional system: a set of conventional channels with their own
/// short name (folder), call rules and names.
#[derive(Clone, Debug)]
pub struct ConvSystem {
    pub short_name: String,
    /// Call rules: timeout, encrypted, length (`max_call_s`).
    pub calls: CallConfig,
    pub save: SaveRules,
    /// Names for talkgroups its P25 / DMR channels report.
    pub talkgroups: Talkgroups,
    /// Its names for its radios.
    pub unit_tags: UnitTags,
    /// Its encryption keys, for its P25 channels.
    pub keys: Option<Arc<Keyring>>,
}

impl Default for ConvSystem {
    fn default() -> Self {
        ConvSystem { short_name: "conv".into(), calls: CallConfig::default(), save: SaveRules::default(), talkgroups: Talkgroups::default(), unit_tags: UnitTags::default(), keys: None }
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
    /// Something worth a log line about site `site` (a conventional
    /// system's: its number): news (a colour code, a channel learned) or, as
    /// a warning, something wrong.
    Note { site: u16, level: NoteLevel, text: String },
    CallStart(Call),
    CallUpdate(Call),
    CallEnd(Call),
    /// Live audio for a recording call.
    /// `decrypted`: the call is encrypted on the air, so this audio is of a cipher we hold the key for.
    Audio { call_id: CallId, system: u16, talkgroup: u32, samples: Vec<f32>, decrypted: bool },
    /// System `system` (any of its sites) heard a radio's talker alias it didn't know (or knew by another name).
    UnitAlias { system: u16, unit: u32, alias: String, talkgroup: u32 },
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
    /// The site's name ([`SiteConfig::site_name`]).
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
    /// The control channel is another system's / site's (see [`SiteConfig::expect`]).
    pub mismatch: Option<String>,
    /// Neighbouring sites its control channel announces.
    pub adjacent: Vec<AdjacentSite>,
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
    /// The site it is for (a conventional channel's: its conventional system's number).
    pub site: u16,
    pub freq_hz: u64,
    pub source: usize,
    /// "control" | "voice" | "carrier" (a DMR site's) | "conventional"
    pub kind: &'static str,
    /// The power in the channel and the noise floor under it, dBFS per FFT bin.
    pub power_db: f64,
    pub noise_db: f64,
    /// How far above the channel the carrier is, Hz, when a receiver can tell.
    pub offset_hz: Option<f32>,
    /// The receiver's eye opening (C4FM / DMR 4FSK; see [`crate::dsp::Receiver::quality`]).
    pub quality: Option<f32>,
    /// CQPSK's phase error, RMS degrees (see [`crate::dsp::cqpsk::Cqpsk::phase_error_deg`]).
    pub phase_err: Option<f32>,
    /// Calls on it now.
    pub calls: usize,
}

/// A channel [`Engine::channels`] measures on its source, and what the engine knows of it.
#[derive(Default)]
struct Want {
    site: u16,
    hz: f64,
    src: usize,
    kind: &'static str,
    offset_hz: Option<f32>,
    quality: Option<f32>,
    phase_err: Option<f32>,
    calls: usize,
}

/// Noise profile slices per source for the dashboard.
const PROFILE_SLICES: usize = 64;
/// Half the width a channel's power is measured over, Hz.
const CC_HALF_HZ: f64 = 3000.0;

/// The noise floor under `hz` from source `s`'s profile `p` (|X|² per bin).
fn slice_at(p: &[f64], s: &Source, hz: f64) -> f64 {
    let off = hz - s.cfg.center_hz;
    let i = ((off + s.cfg.rate_hz / 2.0) / s.cfg.rate_hz * p.len() as f64).floor().clamp(0.0, p.len() as f64 - 1.0) as usize;
    p[i]
}

/// Picks the receiver quality out of a report.
/// The eye opening (`sep`) and phase error (`phaseErr`) from a report.
#[derive(Default)]
struct Quality(Option<f32>, Option<f32>);
impl Sink for Quality {
    fn counter(&mut self, _: &str, _: u64) {}
    fn gauge(&mut self, name: &str, value: f64) {
        match name {
            "sep" => self.0 = Some(value as f32),
            "phaseErr" => self.1 = Some(value as f32),
            _ => {}
        }
    }
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
    cfg: SourceConfig,
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
        if self.cfg.auto_tune {
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
        hz * (1.0 + self.tune_ppm * 1e-6) - self.cfg.center_hz
    }
}

struct Channel {
    /// The system whose grant opened it.
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

/// A control channel's decoding, shared by its system (which follows what
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
    /// Keyed by (system, frequency): systems never share a voice channel.
    /// (A `BTreeMap`: channels run in one order, run after run.)
    channels: BTreeMap<(u16, u64), Channel>,
    /// In (system, carrier) order.
    taps: Vec<Tap>,
    /// What each system's control channel tells its voice channels (Phase 2: the scrambler key, once it gave it).
    voice_params: Vec<VoiceParams>,
    tout: Vec<(usize, TrackerOut)>,
}

/// What one voice channel's decoding of a block came to, for the calls.
#[derive(Default)]
struct VoiceResult {
    /// (system, call, output), in the order produced.
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

/// A source's control channels' decoding of a block: per system, its steps and samples.
struct CcResult {
    site: u16,
    /// The (first) carrier's head.
    head: Option<HeadId>,
    steps: Vec<Step>,
    samples: u64,
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
        let (rate, fs, offset) = (self.src.chz.output_rate(), self.src.cfg.rate_hz, self.src.clock_offset_s);
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

    fn remove_taps(&mut self, system: u16) {
        self.taps.retain(|t| t.site != system);
    }
}

impl BlockDsp {
    /// Its control channels' samples decoded, each control channel under its own lock.
    fn decode_control(&mut self) -> Vec<CcResult> {
        let mut out: Vec<CcResult> = Vec::new();
        for (system, carrier, head, cc, iq, t0) in self.taps.drain(..) {
            let mut steps = Vec::new();
            {
                let mut cc = cc.lock().unwrap();
                cc.push(carrier, &iq, t0, self.rate, &mut steps);
            }
            match out.last_mut().filter(|r| r.site == system) {
                Some(r) => {
                    r.steps.extend(steps);
                    r.samples += iq.len() as u64;
                }
                None => out.push(CcResult { site: system, head: Some(head), steps, samples: iq.len() as u64 }),
            }
        }
        out
    }
}

/// The radio side every system shares: the sources (each behind its lock),
/// the recordings and the recorder pool. Kept apart from the systems' call
/// managers so both can be borrowed at once.
struct Radio {
    rx: Arc<Vec<Mutex<SourceRx>>>,
    /// The sources' settings (as `rx`'s, without the lock).
    cfgs: Vec<SourceConfig>,
    recordings: HashMap<CallId, Recording>,
    free_nums: Vec<u32>,
    next_num: u32,
    max_recorders: usize,
    preroll_s: f64,
    capture_frames: bool,
    vocoder: mbe::Profile,
    /// What each system's control channel tells its voice channels (each source keeps a copy).
    voice_params: Vec<VoiceParams>,
    /// Audio / info produced by the voice channels, as (system, call, output), applied after.
    pending: Vec<(u16, CallId, TrackerOut)>,
    /// Each voice channel open (on whichever source) and the calls on its
    /// slots, as the sources' own have them: the calls are followed without
    /// waiting on a source busy decoding.
    chans: BTreeMap<(u16, u64), [Option<OnSlot>; 2]>,
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

    /// Voice channels open, on every source.
    fn channel_count(&self) -> usize {
        self.chans.len()
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

/// The recorder host one system's call manager sees.
struct SysHost<'a> {
    radio: &'a mut Radio,
    site: u16,
    bank: BankConfig,
    keys: Option<Arc<Keyring>>,
}

impl SysHost<'_> {
    /// Open `call`'s voice channel on source `src` (a newer call on one
    /// already open takes its slot over, as in Trunk Recorder).
    fn open_channel(&mut self, call: &Call, src: usize) {
        let r = &mut *self.radio;
        let kind = VoiceKind::of(call);
        let slot = if kind.slotted() { call.tdma_slot as usize & 1 } else { 0 };
        let key = (self.site, call.freq_hz);
        let mut guard = r.rx[src].lock().unwrap();
        let rx = &mut *guard;
        if let Some(ch) = rx.channels.get_mut(&key) {
            if ch.voice.kind() == kind {
                ch.calls[slot] = Some(OnSlot::of(call));
                r.chans.insert(key, ch.calls);
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
        let cutoff = if let Some(r) = call.nxdn {
            r.cutoff_hz()
        } else if call.color_code.is_some() {
            dmr::CHANNEL_CUTOFF_HZ
        } else {
            CHANNEL_CUTOFF_HZ
        };
        let tune_ppm = s.tune_ppm;
        let (head, pre, start_sample) = s.chz.add_head(s.offset(call.freq_hz as f64), cutoff, r.preroll_s);
        // The noise floor under the channel, from the source's spectrum: analog squelch, and reception.
        let n = floor_slices(s.cfg.rate_hz);
        let mut prof = vec![0.0f64; n];
        s.chz.noise_profile(&mut prof);
        let off = call.freq_hz as f64 - s.cfg.center_hz;
        let slice = (((off + s.cfg.rate_hz / 2.0) / s.cfg.rate_hz * n as f64) as usize).min(n - 1);
        let noise = s.chz.noise_in_band(prof[slice].max(1e-30), ChannelFilter::noise_bandwidth());
        let voice = voice::build(&VoiceSpec {
            kind,
            rate,
            t0: start_sample as f64 / s.cfg.rate_hz + s.clock_offset_s,
            seed: call.freq_hz as u32,
            vocoder: r.vocoder,
            bank: self.bank,
            // Analog squelch: this far above the noise under the channel.
            squelch: (noise * 10f64.powf(ANALOG_SQUELCH_DB / 10.0)) as f32,
            subaudible: false,
            analog: AnalogTuning::default(),
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
        r.chans.insert(key, ch.calls);
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
        // A recording's channel is counted with its recording, not again.
        let r = &*self.radio;
        let following = r.chans.values().filter(|calls| !calls.iter().flatten().any(|c| r.recordings.contains_key(&c.id))).count();
        if call.analog || r.recordings.len() + following >= r.max_recorders {
            return false;
        }
        let Some(src) = r.source_for(call.freq_hz as f64) else {
            return false;
        };
        self.open_channel(call, src);
        true
    }

    fn release(&mut self, call: &Call) {
        let key = (self.site, call.freq_hz);
        // (A call never given a channel — not recorded, not followed — needs no source's lock.)
        let Some(calls) = self.radio.chans.get_mut(&key) else { return };
        for c in calls.iter_mut() {
            if c.is_some_and(|c| c.id == call.id) {
                *c = None;
            }
        }
        let (calls, empty) = (*calls, calls.iter().all(Option::is_none));
        if empty {
            self.radio.chans.remove(&key);
        }
        let Some(src) = self.radio.source_for(call.freq_hz as f64) else { return };
        let mut rx = self.radio.rx(src);
        if empty {
            if let Some(ch) = rx.channels.remove(&key) {
                rx.src.chz.remove_head(ch.head);
            }
        } else if let Some(ch) = rx.channels.get_mut(&key) {
            ch.calls = calls;
        }
    }
}

/// One site of a trunked system: its control channel, parser (band plan) and calls.
struct Trunk {
    cfg: SiteConfig,
    /// Its place in the engine's sites (`Call::site`), and its system's number (`Call::system`).
    site: u16,
    system: u16,
    calls: CallManager,
    /// Its protocol's control channel decoding ([`super::control`]), shared with the taps on its carriers' sources.
    cc: Cc,
    /// Hunting ([`CarrierPlan::Hunt`]): the control channel's head, on `cc_source`.
    cc_source: usize,
    cc_head: Option<HeadId>,
    /// Watching ([`CarrierPlan::Watch`]): each carrier's (source, head, first sample of the head).
    carriers: Vec<(usize, HeadId, u64)>,
    cc_index: usize,
    cc_hz: Option<u64>,
    cc_start_s: f64,
    cc_samples: u64,
    /// Its source's clock offset when the control channel opened (time
    /// since is samples since, plus how much the offset moved).
    cc_offset_s: f64,
    last_good_s: f64,
    now_s: f64,
    good: u64,
    bad: u64,
    concluded: u64,
    identity: Identity,
    /// Why this control channel is not ours, while it isn't.
    mismatch: Option<String>,
    adjacent: std::collections::BTreeMap<(u32, u32, u32), AdjacentSite>,
    /// AutoTune: the correction its control channel was opened with, ppm;
    /// when it was last measured, and last reopened.
    cc_ppm: f64,
    cc_measured_s: f64,
    cc_retuned_s: f64,
    /// Terminators' link control, as talker aliases see it: encrypted,
    /// clear, alias fragments; then Phase 2 alias messages ([`AliasLc`]).
    alias_lc: [u64; 4],
    /// The talkgroups whose terminators' link control was heard encrypted (noted once each).
    lc_protected_tgs: BTreeSet<u32>,
}

impl Trunk {
    fn host<'a>(&self, radio: &'a mut Radio) -> SysHost<'a> {
        SysHost { radio, site: self.site, bank: self.cfg.bank, keys: self.cfg.keys.clone() }
    }

    /// The talkgroup file names this site as the one to keep `tg`'s calls
    /// from: by its name, or by its NAC or RFSS and site number.
    fn preferred_for(&self, tg: &super::talkgroups::Talkgroup) -> bool {
        let id = &self.identity;
        let by_name = !tg.preferred_site.is_empty() && tg.preferred_site.eq_ignore_ascii_case(self.cfg.site_name());
        let n = tg.preferred_nac;
        let by_number = n != 0 && (id.nac().is_some_and(|x| x as u32 == n) || id.site().is_some_and(|s| id.rfss().unwrap_or(0) * 10000 + s == n));
        by_name || by_number
    }

    /// Start listening as its protocol says: the first control channel, or every carrier.
    fn start(&mut self, radio: &mut Radio, events: &mut Vec<Event>) -> Result<(), String> {
        let plan = self.cc().plan();
        match plan {
            CarrierPlan::Hunt { .. } if !self.cfg.control_channels.is_empty() => self.tune(radio, 0, events),
            CarrierPlan::Watch { carriers, cutoff_hz, note } if !carriers.is_empty() => self.watch(radio, &carriers, cutoff_hz, note, events),
            _ => Ok(()),
        }
    }

    /// Its control channel decoding. (Not held across a source's lock:
    /// sources take the two the other way round while they decode.)
    fn cc(&self) -> MutexGuard<'_, Box<dyn ControlChannel>> {
        self.cc.lock().unwrap()
    }

    /// Whether it is receiving: a control channel open, or carriers watched.
    fn running(&self) -> bool {
        self.cc_head.is_some() || !self.carriers.is_empty()
    }

    /// Hunting: its control channel's tap, as the trunk has it now.
    fn hunt_tap(&self, head: HeadId) -> Tap {
        Tap { site: self.site, carrier: 0, head, cc: self.cc.clone(), at: TapTime::Hunt { start_s: self.cc_start_s, offset_s: self.cc_offset_s } }
    }

    /// Hunting: open the control channel `index` (of those a source covers).
    fn tune(&mut self, radio: &mut Radio, index: usize, events: &mut Vec<Event>) -> Result<(), String> {
        let CarrierPlan::Hunt { cutoff_hz } = self.cc().plan() else { return Ok(()) };
        let list: Vec<(f64, usize)> = self.cfg.control_channels.iter().filter_map(|&f| radio.source_for(f).map(|s| (f, s))).collect();
        if list.is_empty() {
            return Err(format!("{}: no control channel falls inside a source's bandwidth — move the center frequency.", self.cfg.short_name));
        }
        self.cc_index = index % list.len();
        let (hz, src) = list[self.cc_index];
        if let Some(h) = self.cc_head.take() {
            let mut rx = radio.rx(self.cc_source);
            rx.src.chz.remove_head(h);
            rx.remove_taps(self.site);
        }
        let retune = self.cc_hz.is_some();
        self.cc_source = src;
        let rate = {
            let mut rx = radio.rx(src);
            let s = &mut rx.src;
            let (head, _, _) = s.chz.add_head(s.offset(hz), cutoff_hz, 0.0);
            self.cc_ppm = s.tune_ppm;
            self.cc_offset_s = s.clock_offset_s;
            let rate = s.chz.output_rate();
            self.cc_head = Some(head);
            self.cc_start_s = self.now_s;
            rx.set_tap(self.hunt_tap(head));
            rate
        };
        self.cc_measured_s = self.now_s;
        self.cc_hz = Some(hz.round() as u64);
        self.cc_samples = 0;
        self.last_good_s = self.now_s;
        self.cc().restart(rate);
        if retune {
            // Another control channel may be another site: learn it afresh.
            self.identity = Identity::default();
            self.cc().forget_site();
            self.mismatch = None;
            self.adjacent.clear();
        }
        events.push(Event::ControlChannel { site: self.site, freq_hz: hz.round() as u64 });
        Ok(())
    }

    /// Watching: heads on the sources that cover each carrier.
    fn watch(&mut self, radio: &mut Radio, carriers: &[f64], cutoff_hz: f64, note: String, events: &mut Vec<Event>) -> Result<(), String> {
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
            heads.push((src, head, start));
        }
        if !outside.is_empty() {
            let name = self.cc().name();
            return Err(format!("{}: {} frequencies outside every source's bandwidth: {} MHz — move a center frequency.", self.cfg.short_name, name, outside.join(", ")));
        }
        for (i, &(src, head, start)) in heads.iter().enumerate() {
            radio.rx(src).set_tap(Tap { site: self.site, carrier: i, head, cc: self.cc.clone(), at: TapTime::Watch { start } });
        }
        self.carriers = heads;
        events.push(Event::Note { site: self.site, level: NoteLevel::Info, text: note });
        Ok(())
    }

    /// A block ran on `source` and its carriers on it were decoded (`cc`):
    /// follow what they say. `time`: the source's time after the block.
    fn on_block(&mut self, radio: &mut Radio, source: usize, time: f64, cc: CcResult, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        if self.carriers.is_empty() {
            self.on_block_hunting(radio, source, cc, events, call_events);
        } else {
            self.on_block_watching(radio, source, time, cc, events, call_events);
        }
    }

    fn on_block_hunting(&mut self, radio: &mut Radio, source: usize, cc: CcResult, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        // (Another source's samples: decoded as it moved to this one.)
        if self.cc_head.is_none() || self.cc_source != source || cc.head != self.cc_head {
            return;
        }
        let (rate, offset) = {
            let rx = radio.rx(source);
            (rx.src.chz.output_rate(), rx.src.clock_offset_s)
        };
        self.cc_samples += cc.samples;
        let moved = offset - self.cc_offset_s;
        self.now_s = self.cc_start_s + self.cc_samples as f64 / rate + moved;
        let notes = self.cc().take_notes();
        for (level, text) in notes {
            events.push(Event::Note { site: self.site, level, text });
        }
        self.follow(radio, cc.steps, events, call_events);
        if self.now_s - self.last_good_s > CC_HUNT_S && self.cfg.control_channels.len() > 1 {
            let _ = self.tune(radio, self.cc_index + 1, events);
        } else if self.now_s - self.cc_measured_s >= TUNE_EVERY_S {
            self.measure(radio);
        }
        let mut host = self.host(radio);
        self.calls.tick(self.now_s, &mut host, call_events);
    }

    fn on_block_watching(&mut self, radio: &mut Radio, source: usize, time: f64, cc: CcResult, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        if !self.carriers.iter().any(|c| c.0 == source) {
            return;
        }
        self.now_s = self.now_s.max(time);
        let (notes, control_hz) = {
            let mut c = self.cc();
            (c.take_notes(), c.control_hz())
        };
        for (level, text) in notes {
            events.push(Event::Note { site: self.site, level, text });
        }
        // Where the control channel is now (Capacity Plus: the rest channel moves).
        if let Some(hz) = control_hz.filter(|&h| Some(h) != self.cc_hz) {
            self.cc_hz = Some(hz);
            events.push(Event::ControlChannel { site: self.site, freq_hz: hz });
        }
        if control_hz.is_some() {
            self.last_good_s = self.now_s;
        }
        self.follow(radio, cc.steps, events, call_events);
        let mut host = self.host(radio);
        self.calls.tick(self.now_s, &mut host, call_events);
    }

    /// Follow what the control channel decoded, step by step: the site lock
    /// first (another site's grants aren't followed, and until every locked
    /// field has been heard nothing is), then the call manager.
    fn follow(&mut self, radio: &mut Radio, steps: Vec<Step>, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        // The site lock: on the fields its protocol states (none: no lock).
        let fields = self.cc().identity_fields();
        let lock = !fields.is_empty();
        for st in steps {
            self.identity = st.identity;
            for m in st.msgs.iter().filter(|m| m.kind == MessageType::Adjacent && m.freq_hz > 0) {
                self.adjacent.insert((m.sys_id, m.rfss, m.site), AdjacentSite { sys_id: m.sys_id, rfss: m.rfss, site: m.site, freq_hz: m.freq_hz });
            }
            if lock {
                let conflict = self.identity.conflict(&self.cfg.expect, fields);
                if conflict != self.mismatch {
                    if let Some(c) = &conflict {
                        let hz = self.cc_hz.unwrap_or(0) as f64 / 1e6;
                        events.push(Event::Note { site: self.site, level: NoteLevel::Warning, text: format!("Control channel {hz:.5} MHz is not this system: {c}") });
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
            let p = self.cc().voice_params(&self.identity);
            let vp = &mut radio.voice_params[self.site as usize];
            let was = *vp;
            vp.tdma_key = p.tdma_key.or(vp.tdma_key);
            vp.nac = p.nac.or(vp.nac);
            if *vp != was {
                let vp = *vp;
                for rx in radio.rx.iter() {
                    rx.lock().unwrap().voice_params[self.site as usize] = vp;
                }
            }
            let mut host = self.host(radio);
            self.calls.handle(&st.msgs, &mut host, call_events);
        }
        (self.good, self.bad) = self.cc().counts();
    }

    /// AutoTune: how far off the control channel comes in (while it decodes),
    /// into its source's average; a control channel well off the correction
    /// is reopened at it, when its protocol allows.
    fn measure(&mut self, radio: &mut Radio) {
        self.cc_measured_s = self.now_s;
        let (Some(hz), Some(head)) = (self.cc_hz, self.cc_head) else { return };
        if self.now_s - self.last_good_s > 1.0 {
            return;
        }
        let (off, reopens, plan) = {
            let c = self.cc();
            (c.offset_hz(), c.reopens(), c.plan())
        };
        let Some(off) = off else { return };
        let hz = hz as f64;
        let mut rx = radio.rx(self.cc_source);
        let s = &mut rx.src;
        s.measured(self.cc_ppm + off as f64 / hz * 1e6);
        let off_by = (s.tune_ppm - self.cc_ppm) * 1e-6 * hz;
        if reopens && off_by.abs() > TUNE_RETUNE_HZ && self.now_s - self.cc_retuned_s >= TUNE_RETUNE_S {
            let CarrierPlan::Hunt { cutoff_hz } = plan else { return };
            s.chz.remove_head(head);
            let (head, _, _) = s.chz.add_head(s.offset(hz), cutoff_hz, 0.0);
            self.cc_head = Some(head);
            self.cc_ppm = s.tune_ppm;
            self.cc_retuned_s = self.now_s;
            self.cc_start_s = self.now_s;
            self.cc_samples = 0;
            self.cc_offset_s = s.clock_offset_s;
            let rate = s.chz.output_rate();
            rx.set_tap(self.hunt_tap(head));
            drop(rx);
            self.cc().restart(rate);
        }
    }

    fn status(&self, radio: &Radio) -> SiteStatus {
        // (One lock at a time: a temporary guard lasts the whole expression.)
        let (modulation, protocol) = {
            let c = self.cc();
            (c.modulation(), c.status())
        };
        SiteStatus {
            site: self.site,
            system: self.system,
            short_name: self.cfg.short_name.clone(),
            site_name: self.cfg.site_name().to_string(),
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
            patches: self.calls.patches.active(),
            protocol,
        }
    }
}

/// Everything the engine follows calls with (behind [`Engine`]'s lock).
struct Core {
    cfg: EngineConfig,
    radio: Radio,
    trunks: Vec<Trunk>,
    conv: Conventional,
    /// Call ids, shared by every system (the conventional calls draw theirs here; they live in `conv`).
    call_ids: CallIds,
    conv_out: Vec<ConvOut>,
    conv_concluded: u64,
    /// Every system's radios' talker aliases.
    aliases: AliasBook,
    /// Copies of one call on several sites.
    multisite: MultiSite,
    /// (system, ALGID, key ID) of keys found to decrypt to noise, told once.
    wrong_keys: BTreeSet<(u16, u8, u16)>,
    now_s: f64,
    events: Vec<Event>,
    call_events: Vec<CallEvent>,
}

impl Core {
    fn new(mut cfg: EngineConfig) -> Result<Self, String> {
        // Every conventional channel's system exists (a default one if none was given).
        let need = cfg.conventional.iter().map(|c| c.system + 1).max().unwrap_or(0);
        if need.max(cfg.conv_systems.len()) > MAX_CONVENTIONAL {
            return Err(format!("At most {MAX_CONVENTIONAL} conventional systems."));
        }
        while cfg.conv_systems.len() < need {
            cfg.conv_systems.push(ConvSystem::default());
        }
        if cfg.sources.is_empty() {
            return Err("no sources configured".into());
        }
        let trunked: Vec<&SiteConfig> = cfg.sites.iter().filter(|s| !s.control_channels.is_empty()).collect();
        if trunked.is_empty() && cfg.conventional.is_empty() {
            return Err("Add a control channel or a conventional channel.".into());
        }
        // A short name is a system's identity (its folder, its talker aliases,
        // its plugin settings), shared by its sites; a site's name is its own.
        let mut seen = std::collections::HashSet::new();
        if let Some(s) = cfg.sites.iter().find(|s| !seen.insert((s.short_name.as_str(), s.site_name()))) {
            return Err(format!("Two sites of {} are named \"{}\" — each needs its own name.", s.short_name, s.site_name()));
        }
        let mut seen: std::collections::HashSet<&str> = cfg.sites.iter().map(|s| s.short_name.as_str()).collect();
        let conv_names = cfg.conv_systems.iter().enumerate().filter(|(k, _)| cfg.conventional.iter().any(|c| c.system == *k)).map(|(_, c)| c.short_name.as_str());
        if let Some(d) = conv_names.into_iter().find(|n| !seen.insert(*n)) {
            return Err(format!("Two systems are named \"{d}\" — each needs its own short name."));
        }
        let history = cfg.preroll_s.max(cfg.conv.preroll_s).max(0.1);
        let spans: Vec<(f64, f64, f64)> = cfg.sources.iter().map(|s| (s.center_hz, s.rate_hz, s.usable_half_width())).collect();
        let conv = Conventional::new(&cfg.conventional, &spans, ConvConfig { vocoder: cfg.vocoder, ..cfg.conv }, cfg.bank)?;
        let sources: Vec<Source> =
            cfg.sources.iter().map(|s| Source { cfg: s.clone(), chz: Channelizer::new(s.rate_hz, MIN_CHANNEL_RATE, history), errors: VecDeque::new(), seed: None, tune_ppm: 0.0, in_gap: false, half: None, clock_offset_s: 0.0 }).collect();
        let rate = sources[0].chz.output_rate();
        let ids = CallIds::default();
        let n_sites = cfg.sites.len();
        let rx = sources
            .into_iter()
            .map(|src| Mutex::new(SourceRx { src, channels: BTreeMap::new(), taps: Vec::new(), voice_params: vec![VoiceParams::default(); n_sites], tout: Vec::new() }))
            .collect();
        let mut radio = Radio {
            rx: Arc::new(rx),
            cfgs: cfg.sources.clone(),
            recordings: HashMap::new(),
            free_nums: Vec::new(),
            next_num: 0,
            max_recorders: cfg.max_recorders,
            preroll_s: cfg.preroll_s,
            capture_frames: cfg.capture_frames,
            vocoder: cfg.vocoder,
            voice_params: vec![VoiceParams::default(); n_sites],
            pending: Vec::new(),
            chans: BTreeMap::new(),
        };
        let mut events = Vec::new();
        let mut trunks = Vec::new();
        let numbers = system_numbers(&cfg.sites);
        for (i, sc) in cfg.sites.iter().enumerate() {
            let mut t = Trunk {
                site: i as u16,
                system: numbers[i],
                calls: CallManager::with_ids(sc.calls, sc.talkgroups.clone(), i as u16, numbers[i], ids.clone()),
                cc: Arc::new(Mutex::new(control::build(sc, rate))),
                cc_source: 0,
                cc_head: None,
                carriers: Vec::new(),
                cc_index: 0,
                cc_hz: None,
                cc_start_s: 0.0,
                cc_samples: 0,
                cc_offset_s: 0.0,
                last_good_s: 0.0,
                now_s: 0.0,
                good: 0,
                bad: 0,
                concluded: 0,
                identity: Identity::default(),
                mismatch: None,
                adjacent: Default::default(),
                cc_ppm: 0.0,
                cc_measured_s: 0.0,
                cc_retuned_s: 0.0,
                alias_lc: [0; 4],
                lc_protected_tgs: BTreeSet::new(),
                cfg: sc.clone(),
            };
            let hold_s = t.cc().patch_hold_s();
            t.calls.patches.hold_s = hold_s;
            t.calls.can_decrypt = sc.keys.as_ref().is_some_and(|k| !k.is_empty());
            t.start(&mut radio, &mut events)?;
            trunks.push(t);
        }
        let mut names: Vec<String> = Vec::new();
        for s in &cfg.sites {
            if !names.contains(&s.short_name) {
                names.push(s.short_name.clone());
            }
        }
        let aliases = AliasBook::new(
            names,
            cfg.conv_systems.iter().enumerate().map(|(k, c)| (c.short_name.clone(), cfg.conventional.iter().any(|ch| ch.system == k))),
        );
        Ok(Core {
            radio,
            trunks,
            conv,
            call_ids: ids,
            conv_out: Vec::new(),
            conv_concluded: 0,
            aliases,
            multisite: MultiSite::default(),
            wrong_keys: BTreeSet::new(),
            now_s: 0.0,
            events,
            call_events: Vec::new(),
            cfg,
        })
    }

    /// Preload system `system`'s band plan saved by [`Engine::bandplan`] (a
    /// grant heard before the next IDEN broadcast can then be followed at once).
    pub fn load_bandplan(&mut self, system: usize, s: &str) {
        if let Some(t) = self.trunks.get_mut(system) {
            t.cc().load_bandplan(s);
        }
    }
    /// A system's band plan to save: P25's IDEN tables; DMR's logical channel → frequency table.
    pub fn bandplan(&self, system: usize) -> String {
        self.trunks.get(system).map_or_else(String::new, |t| t.cc().bandplan())
    }

    /// The short names that keep a talker alias table ([`AliasBook::names`]).
    pub fn unit_table_names(&self) -> Vec<String> {
        self.aliases.names()
    }
    /// Preload the talker aliases (Trunk Recorder's unitTagsOTA CSV) kept under `short_name`.
    pub fn load_units(&mut self, short_name: &str, csv: &str) {
        self.aliases.load(short_name, csv);
    }
    /// (short name, CSV) of each talker alias table that learned something since the last call.
    pub fn units_changed(&mut self) -> Vec<(String, String)> {
        self.aliases.changed()
    }
    /// A radio's talker alias on system `system` (a call's).
    pub fn unit_alias(&self, system: u16, unit: u32) -> Option<&str> {
        self.aliases.get(system, unit)
    }

    /// A key decrypted voice to noise on `site` (of `system`): say so, once a key and system.
    fn wrong_key(told: &mut BTreeSet<(u16, u8, u16)>, events: &mut Vec<Event>, site: u16, system: u16, c: CipherId, talkgroup: Option<u32>) {
        if told.insert((system, c.algid, c.keyid)) {
            let on = talkgroup.map_or(String::new(), |tg| format!(" (talkgroup {tg})"));
            let text = format!("Key ID {:#06X} ({}) decrypts this system's voice{on} to noise: it isn't the key the radios use", c.keyid, crate::p25::crypt::algid_name(c.algid));
            events.push(Event::Note { site, level: NoteLevel::Warning, text });
        }
    }

    /// Note a talker alias heard on a call of site `site` (a conventional
    /// system's: its number), for its system.
    fn learn_alias(&mut self, site: u16, a: Alias, call_tg: Option<u32>) {
        let tg = a.talkgroup.or(call_tg);
        let (now_s, wacn, sys_id, system) = match self.trunks.get(site as usize) {
            Some(t) => (t.now_s, t.identity.wacn(), t.identity.sys_id(), t.system),
            None => (self.now_s, None, None, site),
        };
        let hex = |v: Option<u32>, w: usize| v.map_or(String::new(), |v| format!("{v:0w$x}"));
        let learned = UnitAlias {
            alias: a.alias.clone(),
            source: a.source.to_string(),
            time: ((self.cfg.epoch_ms_at_zero + now_s * 1000.0) / 1000.0) as i64,
            wacn: hex(wacn, 5),
            sys: hex(sys_id, 3),
            talkgroup: tg,
        };
        if self.aliases.learn(system, a.unit, learned) {
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
            let plan = t.cc().plan();
            if let CarrierPlan::Watch { carriers, .. } = plan {
                for (&hz, &(src, _, _)) in carriers.iter().zip(&t.carriers) {
                    control.push(Want { site: t.site, hz, src, kind: if Some(hz.round() as u64) == t.cc_hz { "control" } else { "carrier" }, ..Default::default() });
                }
                continue;
            }
            let Some(hz) = t.cc_hz.filter(|_| t.cc_head.is_some()) else { continue };
            let mut q = Quality::default();
            let offset_hz = {
                let c = t.cc();
                c.report(&mut q);
                c.offset_hz()
            };
            control.push(Want { site: t.site, hz: hz as f64, src: t.cc_source, kind: "control", offset_hz, quality: q.0, phase_err: q.1, calls: 0 });
        }
        let conv = self
            .cfg
            .conventional
            .iter()
            .filter_map(|c| {
                let src = self.radio.source_for(c.freq_hz)?;
                let calls = self.conv.calls().filter(|k| (k.freq_hz as f64 - c.freq_hz).abs() < 1.0).count();
                Some(Want { site: conventional_system(c.system), hz: c.freq_hz, src, kind: "conventional", calls, ..Default::default() })
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
        // Calls per system, all its sites'.
        let mut calls: Vec<(&str, usize, usize)> = Vec::new();
        for t in &self.trunks {
            let recording = t.calls.calls.iter().filter(|c| self.radio.recordings.contains_key(&c.id)).count();
            match calls.iter_mut().find(|c| c.0 == t.cfg.short_name) {
                Some(c) => (c.1, c.2) = (c.1 + t.calls.calls.len(), c.2 + recording),
                None => calls.push((&t.cfg.short_name, t.calls.calls.len(), recording)),
            }
        }
        for (name, active, recording) in calls {
            let mut k = Scoped::new(sink, format!("sys/{}", metrics::key_part(name)));
            k.gauge("active", active as f64);
            k.gauge("recording", recording as f64);
        }
        for t in self.trunks.iter().filter(|t| t.running()) {
            let key = t.cfg.stats_key();
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
            if let Some(hz) = t.cc_hz.filter(|_| t.cc_head.is_some()) {
                hunted.push((key, t.cc_source, hz as f64));
            }
        }
        sink.gauge("eng/recording", self.radio.recordings.len() as f64);
        sink.gauge("eng/channels", (self.radio.channel_count() + self.conv.open_count()) as f64);
        sink.gauge("eng/recorders", self.radio.max_recorders as f64);
        hunted
    }

    /// Replace the talkgroup table of the system named `short_name` (trunked,
    /// every site of it, or conventional) while recording: calls from now on
    /// go by it (an Ignore flag set, a tag fixed). False when there's no such
    /// system.
    pub fn set_talkgroups(&mut self, short_name: &str, talkgroups: Talkgroups) -> bool {
        let mut found = false;
        for t in self.trunks.iter_mut().filter(|t| t.cfg.short_name == short_name) {
            t.cfg.talkgroups = talkgroups.clone();
            t.calls.talkgroups = talkgroups.clone();
            found = true;
        }
        for c in self.cfg.sites.iter_mut().filter(|c| c.short_name == short_name) {
            c.talkgroups = talkgroups.clone();
        }
        if found {
            return true;
        }
        if let Some(k) = self.cfg.conv_systems.iter().position(|c| c.short_name == short_name) {
            self.cfg.conv_systems[k].talkgroups = talkgroups;
            return true;
        }
        false
    }

    /// The encryption keys of the system named `short_name` (trunked, every
    /// site of it, or conventional): its voice channels decrypt with them,
    /// those open now from their next encryption sync (with none, at once).
    /// False when there's no such system.
    pub fn set_keys(&mut self, short_name: &str, keys: Option<Arc<Keyring>>) -> bool {
        let mut found = false;
        for t in self.trunks.iter_mut().filter(|t| t.cfg.short_name == short_name) {
            t.calls.can_decrypt = keys.as_ref().is_some_and(|k| !k.is_empty());
            t.cfg.keys = keys.clone();
            // Channels already open (one is reused by the next call on its frequency) change too:
            // unlocking reaches them, and locking takes the keys from them at once.
            let site = t.site;
            for r in self.radio.rx.iter() {
                for ch in r.lock().unwrap().channels.iter_mut().filter(|((s, _), _)| *s == site).map(|(_, c)| c) {
                    ch.voice.set_keys(keys.clone());
                }
            }
            found = true;
        }
        for c in self.cfg.sites.iter_mut().filter(|c| c.short_name == short_name) {
            c.keys = keys.clone();
        }
        if found {
            return true;
        }
        if let Some(k) = self.cfg.conv_systems.iter().position(|c| c.short_name == short_name) {
            self.conv.set_system_keys(k, keys.as_ref());
            self.cfg.conv_systems[k].keys = keys;
            return true;
        }
        false
    }

    pub fn status(&self) -> Status {
        let sites: Vec<SiteStatus> = self.trunks.iter().filter(|t| t.running()).map(|t| t.status(&self.radio)).collect();
        Status {
            now_s: self.now_s,
            active_calls: sites.iter().map(|s| s.active_calls).sum::<usize>() + self.conv.calls().count(),
            recording: self.radio.recordings.len(),
            channels_open: self.radio.channel_count() + self.conv.open_count(),
            conventional_open: self.conv.open_count(),
            calls_concluded: sites.iter().map(|s| s.calls_concluded).sum::<u64>() + self.conv_concluded,
            sites,
            // ([`Engine::status`] fills these in, from the sources.)
            sources: Vec::new(),
        }
    }

    /// End of input: release what the receivers still hold and end every call.
    pub fn finish(&mut self) {
        for t in self.trunks.iter_mut() {
            let mut steps = Vec::new();
            t.cc().flush(&mut steps);
            t.follow(&mut self.radio, steps, &mut self.events, &mut self.call_events);
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
            let mut host = t.host(&mut self.radio);
            t.calls.end_all(&mut host, &mut self.call_events);
        }
        self.emit_call_events();
        let mut out = std::mem::take(&mut self.conv_out);
        self.conv.finish(&mut out);
        self.emit_conv(out);
    }

    /// Each conventional system's call rules.
    fn call_rules(cfg: &EngineConfig) -> Vec<CallRules<'_>> {
        cfg.conv_systems
            .iter()
            .map(|c| CallRules {
                call_timeout_s: c.calls.call_timeout_s,
                max_call_s: c.calls.max_call_s,
                capture_frames: cfg.capture_frames,
                talkgroups: &c.talkgroups,
                keys: c.keys.as_ref(),
            })
            .collect()
    }

    /// Report what the conventional channels did.
    fn emit_conv(&mut self, mut out: Vec<ConvOut>) {
        for o in out.drain(..) {
            match o {
                ConvOut::Start(c) => self.events.push(Event::CallStart(c)),
                ConvOut::Update(c) => self.events.push(Event::CallUpdate(c)),
                ConvOut::Audio { call_id, system, talkgroup, samples, decrypted } => self.events.push(Event::Audio { call_id, system, talkgroup, samples, decrypted }),
                ConvOut::End { call, audio, frames, recorder_num, tx, reception } => {
                    self.write_call(Held { call: call.clone(), audio, frames, recorder_num, tx, reception, freq_error_hz: None });
                    self.events.push(Event::CallEnd(call));
                }
                ConvOut::Alias(system, a) => self.learn_alias(system, a, None),
                ConvOut::WrongKey(system, c) => Self::wrong_key(&mut self.wrong_keys, &mut self.events, system, system, c, None),
                ConvOut::Position(system, p, talkgroup) => self.events.push(Event::UnitPosition { system, unit: p.unit, lat: p.lat, lon: p.lon, talkgroup }),
                ConvOut::Skipped { freq_hz, code } => self.events.push(Event::ConvSkipped { freq_hz, code }),
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
                t.on_block(&mut self.radio, source, dsp.time, r, &mut self.events, &mut self.call_events);
            }
        }
        self.apply_pending();
        if source == 0 {
            self.now_s = dsp.time;
        }
        if !self.conv.is_empty() {
            let rules = Self::call_rules(&self.cfg);
            let mut out = std::mem::take(&mut self.conv_out);
            {
                let mut rx = self.radio.rx[source].lock().unwrap();
                self.conv.on_block(source, &mut rx.src.chz, dsp.time, dsp.in_gap, &self.call_ids, &rules, &mut out);
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
            match o {
                TrackerOut::Audio(samples, frame) => {
                    let Some(rec) = self.radio.recordings.get_mut(&id) else { continue };
                    rec.tx.note(rec.audio.len(), t.now_s);
                    rec.audio.extend_from_slice(&samples);
                    rec.frames.push(frame);
                    let known_decrypted = rec.decrypted;
                    t.calls.note_audio(id, t.now_s);
                    // Fail closed: a call that is gone already (it expired in this same block) is not known to be
                    // clear, and its voice is not given to a plugin as if it were.
                    let (tg, decrypted) = t.calls.calls.iter().find(|c| c.id == id).map_or((0, true), |c| (c.talkgroup, c.encrypted || c.decrypted || known_decrypted));
                    if self.multisite.audio_lead(id, t.now_s) {
                        self.events.push(Event::Audio { call_id: id, system, talkgroup: tg, samples, decrypted });
                    }
                }
                TrackerOut::AnalogAudio(samples) => {
                    let Some(rec) = self.radio.recordings.get_mut(&id) else { continue };
                    rec.tx.note(rec.audio.len(), t.now_s);
                    rec.audio.extend_from_slice(&samples);
                    t.calls.note_audio(id, t.now_s);
                    let tg = t.calls.calls.iter().find(|c| c.id == id).map_or(0, |c| c.talkgroup);
                    if self.multisite.audio_lead(id, t.now_s) {
                        self.events.push(Event::Audio { call_id: id, system, talkgroup: tg, samples, decrypted: false });
                    }
                }
                TrackerOut::Info { source, flags } => {
                    let now = t.now_s;
                    if flags.encrypted {
                        if let Some(rec) = self.radio.recordings.get_mut(&id) {
                            rec.tx.mark_encrypted(now);
                        }
                    }
                    if let Some(c) = t.calls.call_mut(id) {
                        let mut changed = false;
                        if flags.encrypted && !c.encrypted {
                            c.encrypted = true;
                            changed = true;
                        }
                        if flags.emergency && !c.emergency {
                            c.emergency = true;
                            changed = true;
                        }
                        changed |= CallManager::note_source(c, source, now, flags);
                        if changed {
                            self.call_events.push(CallEvent::Update(c.clone()));
                        }
                    }
                }
                TrackerOut::Cipher(cipher) => {
                    if let Some(c) = t.calls.call_mut(id).filter(|c| c.cipher != Some(cipher)) {
                        c.cipher = Some(cipher);
                        self.call_events.push(CallEvent::Update(c.clone()));
                    }
                }
                TrackerOut::WrongKey(c) => {
                    let tg = t.calls.calls.iter().find(|c| c.id == id).map(|c| c.talkgroup);
                    Self::wrong_key(&mut self.wrong_keys, &mut self.events, site, system, c, tg);
                }
                TrackerOut::Decrypted => {
                    if let Some(rec) = self.radio.recordings.get_mut(&id) {
                        rec.tx.mark_decrypted(t.now_s);
                        rec.decrypted = true;
                    }
                    if let Some(c) = t.calls.call_mut(id).filter(|c| !c.decrypted) {
                        c.decrypted = true;
                        self.call_events.push(CallEvent::Update(c.clone()));
                    }
                }
                // (Only conventional FM asks for it.)
                TrackerOut::Subaudible(_) => {}
                TrackerOut::Position(p) => {
                    if let Some(c) = t.calls.call_mut(id) {
                        if CallManager::note_position(c, &p) {
                            self.events.push(Event::UnitPosition { system, unit: p.unit, lat: p.lat, lon: p.lon, talkgroup: c.talkgroup });
                            self.call_events.push(CallEvent::Update(c.clone()));
                        }
                    }
                }
                TrackerOut::Alias(a) => {
                    let tg = t
                        .calls
                        .calls
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| c.talkgroup);
                    self.learn_alias(site, a, tg);
                }
                TrackerOut::AliasLc(sign) => {
                    t.alias_lc[sign as usize] += 1;
                    let tg = t.calls.calls.iter().find(|c| c.id == id).map_or(0, |c| c.talkgroup);
                    if sign == AliasLc::Protected && t.lc_protected_tgs.insert(tg) {
                        let text = format!("Talkgroup {tg}: link control is encrypted, so its radios' talker aliases can't be read");
                        self.events.push(Event::Note { site, level: NoteLevel::Info, text });
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
        if !self.cfg.drop_duplicates || conventional_index(c.site).is_some() {
            return false;
        }
        let others = self.trunks.iter().filter(|t| t.system == c.system && t.site != c.site).map(|t| t.calls.calls.as_slice());
        self.multisite.link_new(c, others);
        let twin = !self.multisite.twins(c.id).is_empty();
        if let Some(own) = self.trunks.get_mut(c.site as usize).and_then(|t| t.calls.call_mut(c.id)).filter(|_| twin) {
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
            h.trim(self.trunks.get(h.call.site as usize).map_or(0.0, |t| t.cfg.save.min_transmission_s));
            h
        });
        let trunks = &self.trunks;
        let Some(copies) = self.multisite.conclude(call.id, held, |site, tg| trunks.get(site as usize).is_some_and(|t| t.preferred_for(tg))) else { return };
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
        let conv = conventional_index(system).and_then(|k| self.cfg.conv_systems.get(k));
        let trunk = self.trunks.get(site as usize);
        let short_name = match trunk {
            Some(t) => t.cfg.short_name.clone(),
            None => conv.map_or_else(|| "conv".to_string(), |c| c.short_name.clone()),
        };
        let saved = save_call(
            h,
            &SaveContext {
                rules: conv.map_or_else(|| trunk.map_or(SaveRules::default(), |t| t.cfg.save), |c| c.save),
                short_name: &short_name,
                site: trunk.map_or("", |t| t.cfg.site_name()),
                epoch_ms_at_zero: self.cfg.epoch_ms_at_zero,
                units: self.aliases.of(system),
                unit_tags: conv.map(|c| &c.unit_tags).or_else(|| trunk.map(|t| &t.cfg.unit_tags)).filter(|t| !t.is_empty() || t.mode != Default::default()),
                wacn: trunk.and_then(|t| t.identity.wacn()),
                sys_id: trunk.and_then(|t| t.identity.sys_id()),
                nac: trunk.and_then(|t| t.identity.nac()),
            },
        );
        match saved {
            Ok(k) => {
                match self.trunks.get_mut(site as usize) {
                    Some(t) => t.concluded += 1,
                    None => self.conv_concluded += 1,
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
    sources: Vec<SourceConfig>,
    /// The sites as configured (talkgroups and keys as last set).
    sites: Mutex<Arc<Vec<SiteConfig>>>,
}

// (Each source can be fed from a thread of its own.)
const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Engine>();
};

impl Engine {
    pub fn new(cfg: EngineConfig) -> Result<Self, String> {
        let core = Core::new(cfg)?;
        let shared = Shared { rx: core.radio.rx.clone(), sources: core.cfg.sources.clone(), sites: Mutex::new(Arc::new(core.cfg.sites.clone())), core: Mutex::new(core) };
        Ok(Engine { shared: Arc::new(shared) })
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        self.shared.core.lock().unwrap()
    }

    /// Preload system `system`'s band plan saved by [`Engine::bandplan`] (a
    /// grant heard before the next IDEN broadcast can then be followed at once).
    pub fn load_bandplan(&self, system: usize, s: &str) {
        self.core().load_bandplan(system, s)
    }
    /// A system's band plan to save: P25's IDEN tables; DMR's logical channel → frequency table.
    pub fn bandplan(&self, system: usize) -> String {
        self.core().bandplan(system)
    }
    /// The short names that keep a talker alias table ([`AliasBook::names`]).
    pub fn unit_table_names(&self) -> Vec<String> {
        self.core().unit_table_names()
    }
    /// Preload the talker aliases (Trunk Recorder's unitTagsOTA CSV) kept under `short_name`.
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

    /// The trunked systems' sites, as configured (with the talkgroups and keys last set).
    pub fn sites(&self) -> Arc<Vec<SiteConfig>> {
        self.shared.sites.lock().unwrap().clone()
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
                site: w.site,
                freq_hz: w.hz.round() as u64,
                source: w.src,
                kind: w.kind,
                power_db: metrics::bin_dbfs(s.chz.band_power(s.offset(w.hz), CC_HALF_HZ), n),
                noise_db: metrics::bin_dbfs(slice_at(profile, s, w.hz), n),
                offset_hz: w.offset_hz,
                quality: w.quality,
                phase_err: w.phase_err,
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
                let mut q = Quality::default();
                ch.voice.report(&mut q);
                let w = Want { site: ch.site, hz: ch.freq_hz, src, kind: "voice", offset_hz: ch.voice.offset_hz(), quality: q.0, phase_err: q.1, calls: ch.calls.iter().flatten().count() };
                voice.push(snap(&r.src, &profile, &w));
            }
        }
        let mut out: Vec<ChannelSnapshot> = control_out.into_iter().flatten().chain(voice).chain(conv_out.into_iter().flatten()).collect();
        out.sort_by_key(|c| c.freq_hz);
        out.dedup_by_key(|c| (c.freq_hz, c.site));
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
        *self.shared.sites.lock().unwrap() = Arc::new(c.cfg.sites.clone());
        ok
    }
    /// The encryption keys of the system named `short_name` (trunked or
    /// conventional). False when there's no such system.
    pub fn set_keys(&self, short_name: &str, keys: Option<Arc<Keyring>>) -> bool {
        let mut c = self.core();
        let ok = c.set_keys(short_name, keys);
        *self.shared.sites.lock().unwrap() = Arc::new(c.cfg.sites.clone());
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
    /// The other sites' copies of call `id` still going or held, as (call, system).
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

    fn tuned_source(auto_tune: bool) -> Source {
        let cfg = SourceConfig { center_hz: 770e6, rate_hz: 2.4e6, auto_tune, guard_hz: DEFAULT_GUARD_HZ };
        Source { chz: Channelizer::new(cfg.rate_hz, MIN_CHANNEL_RATE, 0.1), cfg, errors: VecDeque::new(), seed: None, tune_ppm: 0.0, in_gap: false, half: None, clock_offset_s: 0.0 }
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
        let sys = SiteConfig { short_name: "s".into(), control_channels: vec![770.5e6], ..Default::default() };
        let e = Engine::new(EngineConfig { sites: vec![sys], sources: vec![SourceConfig { center_hz: 770e6, rate_hz: 2.4e6, auto_tune: true, guard_hz: DEFAULT_GUARD_HZ }], ..Default::default() }).unwrap();
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

    /// A call split at `maxCallS` with every recorder in use goes on
    /// recorded: its new part takes over the old part's recorder and channel.
    #[test]
    fn a_split_call_keeps_its_recorder() {
        let sys = SiteConfig { short_name: "s".into(), control_channels: vec![851.0125e6], calls: CallConfig { max_call_s: 2.0, ..Default::default() }, ..Default::default() };
        let cfg = EngineConfig { sites: vec![sys], sources: vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }], max_recorders: 1, ..Default::default() };
        let mut e = Core::new(cfg).unwrap();
        let f = 851_500_000;
        for t in [0.0, 1.0, 2.0] {
            let Core { trunks, radio, call_events, .. } = &mut e;
            let mut host = trunks[0].host(radio);
            let m = Message { kind: MessageType::Grant, time_s: t, talkgroup: 101, freq_hz: f, ..Default::default() };
            trunks[0].calls.handle(&[m], &mut host, call_events);
            trunks[0].calls.tick(t + 0.5, &mut host, call_events);
        }
        let calls = &e.trunks[0].calls.calls;
        assert_eq!(calls.len(), 1);
        assert!(calls[0].recording && calls[0].start_s > 2.0, "{:?}", calls[0]);
        assert_eq!(e.radio.rx(0).channels[&(0, f)].calls[0].map(|c| c.id), Some(calls[0].id), "the channel is still open, for the new part");
    }

    #[test]
    fn at_most_256_conventional_systems() {
        let mut e = plain_engine().core().cfg.clone();
        e.conv_systems = vec![ConvSystem::default(); MAX_CONVENTIONAL + 1];
        assert!(Engine::new(e).err().unwrap().contains("256"));
    }

    #[test]
    fn sites_need_their_own_names_and_share_call_ids() {
        let src = vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }];
        let site = |n: &str, s: &str| SiteConfig { short_name: n.into(), site: s.into(), control_channels: vec![851.0125e6], ..Default::default() };
        let cfg = EngineConfig { sites: vec![site("a", ""), site("a", "")], sources: src.clone(), ..Default::default() };
        assert!(Engine::new(cfg).err().unwrap().contains("Two sites of a are named \"a\""));
        let cfg = EngineConfig { sites: vec![site("a", ""), site("b", ""), site("a", "a2")], sources: src, ..Default::default() };
        assert_eq!(system_numbers(&cfg.sites), [0, 1, 0], "a system is numbered where its first site is");
        let e = Engine::new(cfg).unwrap();
        let st = e.status();
        assert_eq!(st.sites.iter().map(|s| (s.site, s.system, s.site_name.as_str())).collect::<Vec<_>>(), [(0, 0, "a"), (1, 1, "b"), (2, 0, "a2")]);
        let ids = CallIds::default();
        let (x, y) = (ids.clone(), ids);
        assert_eq!([x.next(), y.next(), x.next()], [1, 2, 3]);
    }

    /// Three sites grant TG 101: east and west of one system, far of another.
    /// East decodes `east_good` clean frames, west 100. What gets saved?
    fn two_sites(east_good: usize, prefer: &str, dedupe: bool) -> (Vec<Event>, [CallId; 3]) {
        two_sites_with(east_good, prefer, dedupe, 0.0)
    }

    /// [`two_sites`], east keeping only calls of at least `east_min_call_s`.
    fn two_sites_with(east_good: usize, prefer: &str, dedupe: bool, east_min_call_s: f64) -> (Vec<Event>, [CallId; 3]) {
        use super::super::frames::{Codec, VoiceFrame};
        let tgs: Talkgroups = [(101, super::super::talkgroups::Talkgroup { number: 101, preferred_site: prefer.into(), ..Default::default() })].into_iter().collect();
        let save = SaveRules { normalize: false, ..Default::default() };
        let sys = |n: &str, cc: f64, g: &str| SiteConfig { short_name: g.into(), site: n.into(), control_channels: vec![cc], talkgroups: tgs.clone(), save, ..Default::default() };
        let cfg = EngineConfig {
            sites: vec![
                SiteConfig { save: SaveRules { min_call_s: east_min_call_s, ..save }, ..sys("east", 851.0125e6, "dc") },
                sys("west", 851.2125e6, "dc"),
                sys("far", 851.4125e6, "md"),
            ],
            sources: vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }],
            drop_duplicates: dedupe,
            ..Default::default()
        };
        let mut e = Core::new(cfg).unwrap();
        let mut ids = [0; 3];
        for (i, (t, f)) in [(0.0, 851_500_000), (0.4, 851_600_000), (0.2, 851_700_000)].into_iter().enumerate() {
            let Core { trunks, radio, call_events, .. } = &mut e;
            let mut host = trunks[i].host(radio);
            let grant = Message { kind: MessageType::Grant, time_s: t, talkgroup: 101, freq_hz: f, ..Default::default() };
            trunks[i].calls.handle(&[grant], &mut host, call_events);
            ids[i] = trunks[i].calls.calls[0].id;
        }
        e.emit_call_events();
        for (i, good) in [east_good, 100, 100].into_iter().enumerate() {
            let rec = e.radio.recordings.get_mut(&ids[i]).unwrap();
            for k in 0..100 {
                let kind = if k < good { mbe::Kind::Voice } else { mbe::Kind::Repeat };
                rec.frames.push(VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind });
                rec.audio.extend_from_slice(&[0.1; mbe::FRAME_SAMPLES]);
            }
        }
        e.drain_events();
        // West ends last.
        for i in [0, 2, 1] {
            let Core { trunks, radio, call_events, .. } = &mut e;
            let mut host = trunks[i].host(radio);
            trunks[i].calls.tick(10.0, &mut host, call_events);
            e.emit_call_events();
        }
        (e.drain_events(), ids)
    }

    /// A frequency that carried a Phase 1 call is granted for Phase 2 (P25
    /// Dynamic Dual Mode) before that call has timed out: the new call gets
    /// a Phase 2 channel, not the Phase 1 one.
    #[test]
    fn a_frequency_changing_mode_gets_a_channel_of_the_new_kind() {
        let cfg = EngineConfig {
            sites: vec![SiteConfig { short_name: "ddm".into(), control_channels: vec![851.0125e6], ..Default::default() }],
            sources: vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }],
            ..Default::default()
        };
        let mut e = Core::new(cfg).unwrap();
        let f = 851_500_000;
        fn grant(e: &mut Core, f: u64, t: f64, tg: u32, phase2: bool, slot: u8) {
            let Core { trunks, radio, call_events, .. } = e;
            let mut host = trunks[0].host(radio);
            let m = Message { kind: MessageType::Grant, time_s: t, talkgroup: tg, freq_hz: f, phase2_tdma: phase2, tdma_slot: slot, ..Default::default() };
            trunks[0].calls.handle(&[m], &mut host, call_events);
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
        let cfg = EngineConfig {
            sites: vec![SiteConfig { short_name: "d".into(), control_channels: vec![851.0125e6], ..Default::default() }],
            sources: vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ }],
            ..Default::default()
        };
        let mut e = Core::new(cfg).unwrap();
        {
            let Core { trunks, radio, call_events, .. } = &mut e;
            let mut host = trunks[0].host(radio);
            let m = Message { kind: MessageType::Grant, time_s: 0.0, talkgroup: 101, freq_hz: 851_500_000, ..Default::default() };
            trunks[0].calls.handle(&[m], &mut host, call_events);
        }
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
        assert!(k.json.contains("\"decrypted\":1"), "{}", k.json);
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
        // East decodes best, but its site keeps no call that short: west's copy is saved.
        let (ev, [east, west, far]) = two_sites_with(100, "", true, 100.0);
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
