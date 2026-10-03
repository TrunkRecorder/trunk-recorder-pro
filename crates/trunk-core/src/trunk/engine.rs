//! Trunked systems above the radio(s) — Trunk Recorder's
//! monitor_messages() loop plus its recorders, for each system:
//!
//! ```text
//! source(s) u8 IQ → Channelizer per source
//!   control channel head → receiver bank → TSDU groups → TSBKs → TsbkParser → CallManager
//!   CallManager.start_recording → a voice head on whichever source covers the
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
//! system is a system of its own (see [`SystemConfig::expect`]); a call heard
//! on several sites is recorded on each and the best copy saved (see
//! [`super::multisite`]).
//!
//! Conventional channels (analog FM, P25) ride the same channelizers: see
//! [`super::conventional`]. A config may have trunked systems, conventional
//! channels, or both. Everything that happens is reported as [`Event`]s; the
//! engine does no I/O.

use std::collections::{HashMap, VecDeque};

use num_complex::Complex32;

use super::calls::{conventional_index, conventional_system, Call, CallConfig, CallEvent, CallId, CallIds, CallManager, Reason, RecorderHost, MAX_CONVENTIONAL};
use super::conventional::{CallRules, ConvChannel, ConvConfig, ConvOut, Conventional};
use super::message::{Message, MessageType};
use super::multisite::{Held, MultiSite, SiteKey};
use super::record::{save_call, Reception, SaveContext, Transmissions};
use super::record::{Concluded, SaveRules};
use super::talkgroups::Talkgroups;
use super::frames::CallFrames;
use super::voice::TrackerOut;
use super::units::{AliasBook, UnitAlias, UnitTags};
use super::identity::Identity;
use super::control::{self, CarrierPlan, ControlChannel, Protocol, ProtocolStatus, Step};
use super::voice::{self, VoiceDecoder, VoiceKind, VoiceParams, VoiceSpec};
use crate::dsp::{Channelizer, HeadId};
use crate::mbe;
use crate::p25::alias::Alias;
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
/// Usable fraction of a source's bandwidth (the anti-alias roll-off eats the edges).
const USABLE: f64 = 0.9;
/// No good control message for this long → hunt to the next control channel.
const CC_HUNT_S: f64 = 5.0;

#[derive(Clone, Debug, Default)]
pub struct SourceConfig {
    pub center_hz: f64,
    pub rate_hz: f64,
    /// Correct channels for the frequency error measured on its control
    /// channels (Trunk Recorder's autoTune). Measured either way.
    pub auto_tune: bool,
}


/// One trunked system — or one site of a multi-site system: each site with
/// its own control channel is a system here, as in Trunk Recorder.
#[derive(Clone, Debug)]
pub struct SystemConfig {
    /// Folder and record name (Trunk Recorder's shortName); unique.
    pub short_name: String,
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
    /// Multi-site: the system this site belongs to, by name. Empty: what its
    /// control channel says (P25 WACN and System ID, SmartNet System ID).
    pub site_group: String,
    pub save: SaveRules,
    /// Its own names for its radios (the unit names file).
    pub unit_tags: UnitTags,
}

impl Default for SystemConfig {
    fn default() -> Self {
        SystemConfig {
            short_name: "sys1".into(),
            control_channels: vec![],
            calls: CallConfig::default(),
            bank: BankConfig::default(),
            talkgroups: Talkgroups::default(),
            expect: Identity::default(),
            protocol: Protocol::P25,
            site_group: String::new(),
            save: SaveRules::default(),
            unit_tags: UnitTags::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// The trunked systems (sites), in order; [`Call::system`] indexes them.
    pub systems: Vec<SystemConfig>,
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
            systems: vec![],
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
}

impl Default for ConvSystem {
    fn default() -> Self {
        ConvSystem { short_name: "conv".into(), calls: CallConfig::default(), save: SaveRules::default(), talkgroups: Talkgroups::default(), unit_tags: UnitTags::default() }
    }
}


#[derive(Clone, Debug)]
pub enum Event {
    /// System `system` tuned a control channel.
    ControlChannel { system: u16, freq_hz: u64 },
    /// A control channel message of system `system`.
    Message { system: u16, msg: Message },
    /// Something worth a log line about system `system`.
    Note { system: u16, text: String },
    CallStart(Call),
    CallUpdate(Call),
    CallEnd(Call),
    /// Live audio for a recording call.
    Audio { call_id: CallId, system: u16, talkgroup: u32, samples: Vec<f32> },
    /// System `system` heard a radio's talker alias it didn't know (or knew by another name).
    UnitAlias { system: u16, unit: u32, alias: String, talkgroup: u32 },
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

/// One system's state, for the interface.
#[derive(Clone, Debug, Default)]
pub struct SystemStatus {
    /// The number its calls carry (`Call::system`). Systems without a
    /// control channel are left out of [`Status::systems`], so this isn't
    /// always the position there.
    pub system: u16,
    pub short_name: String,
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
    /// The control channel is another system's / site's (see [`SystemConfig::expect`]).
    pub mismatch: Option<String>,
    /// Neighbouring sites its control channel announces.
    pub adjacent: Vec<AdjacentSite>,
    /// Patches standing now: (supergroup, the talkgroups patched into it).
    pub patches: Vec<(u32, Vec<u32>)>,
    /// A trunked DMR site: its kind, rest channel, channel table, carriers.
    pub protocol: ProtocolStatus,
    /// Multi-site: the system it is a site of ([`SiteKey::group`]), once known.
    pub site_group: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// The first source's sample clock.
    pub now_s: f64,
    pub systems: Vec<SystemStatus>,
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

/// A source's frequency error (Trunk Recorder's autoTune report).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SourceTune {
    /// The average of the last measurements, ppm (+ = signals come in high:
    /// add it to the source's ppm). None until a control channel was measured.
    pub error_ppm: Option<f64>,
    /// The correction applied to channels opened now, ppm (0 without autoTune).
    pub applied_ppm: f64,
}

/// Measurements averaged (Trunk Recorder keeps 20).
const TUNE_KEEP: usize = 20;
/// A control channel is measured this often, s.
const TUNE_EVERY_S: f64 = 10.0;
/// A P25 control channel is reopened at the corrected frequency when it is
/// off by more than this, Hz, but not more often than every TUNE_RETUNE_S.
const TUNE_RETUNE_HZ: f64 = 150.0;
const TUNE_RETUNE_S: f64 = 200.0;

struct Source {
    cfg: SourceConfig,
    chz: Channelizer,
    /// Recent frequency errors measured on it, ppm.
    errors: VecDeque<f64>,
    /// The correction new channels get, ppm.
    tune_ppm: f64,
    /// Being fed silence for samples that never came ([`Engine::push_gap`]).
    in_gap: bool,
    /// Added to its sample clock to give the engine's clock
    /// ([`Engine::set_clock_offset`]), s.
    clock_offset_s: f64,
}

impl Source {
    fn measured(&mut self, ppm: f64) {
        if !ppm.is_finite() || ppm.abs() > 50.0 {
            return;
        }
        self.errors.push_back(ppm);
        if self.errors.len() > TUNE_KEEP {
            self.errors.pop_front();
        }
        if self.cfg.auto_tune {
            self.tune_ppm = self.error_ppm().unwrap_or(0.0);
        }
    }
    fn error_ppm(&self) -> Option<f64> {
        (!self.errors.is_empty()).then(|| self.errors.iter().sum::<f64>() / self.errors.len() as f64)
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
    system: u16,
    source: usize,
    head: HeadId,
    /// The call on each TDMA slot (Phase 1: slot 0).
    calls: [Option<CallId>; 2],
    voice: Box<dyn VoiceDecoder>,
    freq_hz: f64,
    /// The correction it was opened with, ppm.
    tune_ppm: f64,
    /// Its power, and the noise floor under it when it opened ([`Reception`]).
    meter: ChannelFilter,
    noise: f64,
}

struct Recording {
    audio: Vec<f32>,
    frames: CallFrames,
    recorder_num: u32,
    tx: Transmissions,
    /// How far off its channel the voice came in, Hz from the nominal
    /// frequency: the sum of the measurements and their number.
    freq_error: (f64, u32),
    reception: Reception,
}

/// The radio side every system shares: sources and their channelizers, the
/// voice channels and the recorder pool. Kept apart from the systems' call
/// managers so both can be borrowed at once.
struct Radio {
    sources: Vec<Source>,
    /// Keyed by (system, frequency): systems never share a voice channel.
    channels: HashMap<(u16, u64), Channel>,
    recordings: HashMap<CallId, Recording>,
    free_nums: Vec<u32>,
    next_num: u32,
    max_recorders: usize,
    preroll_s: f64,
    capture_frames: bool,
    vocoder: mbe::Profile,
    tout: Vec<(usize, TrackerOut)>,
    /// What each system's control channel tells its voice channels (Phase 2: the scrambler key, once it gave it).
    voice_params: Vec<VoiceParams>,
    /// Audio / info produced by the voice channels, as (system, call, output), applied after.
    pending: Vec<(u16, CallId, TrackerOut)>,
}

impl Radio {
    fn source_for(&self, hz: f64) -> Option<usize> {
        self.sources.iter().position(|s| (hz - s.cfg.center_hz).abs() <= s.cfg.rate_hz / 2.0 * USABLE)
    }

    /// Run a channel's decoder over `iq` (then flush it, at the end of
    /// input), collecting what it produced as (slot, output).
    fn run_channel(ch: &mut Channel, iq: &[Complex32], params: &VoiceParams, tout: &mut Vec<(usize, TrackerOut)>, flush: bool) {
        ch.voice.set_params(params);
        // Vocode only the slots somebody is recording.
        ch.voice.listen([ch.calls[0].is_some(), ch.calls[1].is_some()]);
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
            if let Some(id) = ch.calls[slot & 1] {
                pending.push((ch.system, id, o));
            }
        }
    }

    /// Run a channel's receivers over its head's latest output and route the result.
    fn run_head(&mut self, key: (u16, u64), flush: bool) {
        let Some(ch) = self.channels.get_mut(&key) else { return };
        let s = &self.sources[ch.source];
        let iq: &[Complex32] = if flush { &[] } else { s.chz.output(ch.head).unwrap_or(&[]) };
        let params = self.voice_params.get(ch.system as usize).copied().unwrap_or_default();
        self.tout.clear();
        Self::run_channel(ch, iq, &params, &mut self.tout, flush);
        // Reception: the channel's power while it carries a call's voice.
        let voiced = |slot: usize| self.tout.iter().any(|(s, o)| *s & 1 == slot && matches!(o, TrackerOut::Audio(..) | TrackerOut::AnalogAudio(_)));
        let on_air = [voiced(0), voiced(1)];
        if !iq.is_empty() && (on_air[0] || on_air[1]) {
            let p = ch.meter.meter(iq) as f64;
            for (slot, id) in ch.calls.iter().enumerate() {
                if let Some(r) = id.filter(|_| on_air[slot]).and_then(|id| self.recordings.get_mut(&id)) {
                    r.reception.signal(p);
                    r.reception.noise(ch.noise);
                }
            }
        }
        Self::route(ch, &mut self.tout, &mut self.pending);
        if let Some(off) = ch.voice.offset_hz() {
            let err = off as f64 + ch.tune_ppm * 1e-6 * ch.freq_hz;
            for id in ch.calls.iter().flatten() {
                if let Some(r) = self.recordings.get_mut(id) {
                    r.freq_error.0 += err;
                    r.freq_error.1 += 1;
                }
            }
        }
    }
}

/// The recorder host one system's call manager sees.
struct SysHost<'a> {
    radio: &'a mut Radio,
    system: u16,
    bank: BankConfig,
}

impl SysHost<'_> {
    /// Open `call`'s voice channel on source `src` (a newer call on one
    /// already open takes its slot over, as in Trunk Recorder).
    fn open_channel(&mut self, call: &Call, src: usize) {
        let r = &mut *self.radio;
        let kind = VoiceKind::of(call);
        let slot = if kind.slotted() { call.tdma_slot as usize & 1 } else { 0 };
        let key = (self.system, call.freq_hz);
        if let Some(ch) = r.channels.get_mut(&key) {
            if ch.voice.kind() == kind {
                ch.calls[slot] = Some(call.id);
                return;
            }
            // The frequency carries another kind of call now (P25 Dynamic
            // Dual Mode: Phase 1 then Phase 2; SmartNet: analog then
            // digital): a channel of that kind instead. The calls on the old
            // one are over; they end when their timeout comes.
            let old = r.channels.remove(&key).unwrap();
            r.sources[old.source].chz.remove_head(old.head);
        }
        let s = &mut r.sources[src];
        let rate = s.chz.output_rate();
        let cutoff = if call.color_code.is_some() { dmr::CHANNEL_CUTOFF_HZ } else { CHANNEL_CUTOFF_HZ };
        let tune_ppm = s.tune_ppm;
        let (head, pre, start_sample) = s.chz.add_head(s.offset(call.freq_hz as f64), cutoff, r.preroll_s);
        // The noise floor under the channel, from the source's spectrum: analog squelch, and reception.
        let mut prof = vec![0.0f64; 64];
        s.chz.noise_profile(&mut prof);
        let off = call.freq_hz as f64 - s.cfg.center_hz;
        let slice = (((off + s.cfg.rate_hz / 2.0) / s.cfg.rate_hz * 64.0) as usize).min(63);
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
        });
        let mut calls = [None, None];
        calls[slot] = Some(call.id);
        let mut ch = Channel { system: self.system, source: src, head, calls, voice, freq_hz: call.freq_hz as f64, tune_ppm, meter: ChannelFilter::new(rate), noise };
        // Pre-roll: decode the replayed air now.
        let params = r.voice_params.get(self.system as usize).copied().unwrap_or_default();
        r.tout.clear();
        Radio::run_channel(&mut ch, &pre, &params, &mut r.tout, false);
        Radio::route(&ch, &mut r.tout, &mut r.pending);
        r.channels.insert(key, ch);
    }
}

impl RecorderHost for SysHost<'_> {
    fn start_recording(&mut self, call: &Call) -> Result<(), Reason> {
        let r = &mut *self.radio;
        let Some(src) = r.source_for(call.freq_hz as f64) else {
            return Err(Reason::NoSource);
        };
        if r.recordings.len() >= r.max_recorders {
            return Err(Reason::NoRecorder);
        }
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
                reception: Reception::default(),
            },
        );
        self.open_channel(call, src);
        Ok(())
    }

    fn follow(&mut self, call: &Call) -> bool {
        // Only with a recorder's worth of room to spare, and never on analog.
        let r = &*self.radio;
        if call.analog || r.recordings.len() + r.channels.len() >= r.max_recorders {
            return false;
        }
        let Some(src) = r.source_for(call.freq_hz as f64) else {
            return false;
        };
        self.open_channel(call, src);
        true
    }

    fn stop_recording(&mut self, call: &Call) {
        let key = (self.system, call.freq_hz);
        let r = &mut *self.radio;
        let Some(ch) = r.channels.get_mut(&key) else { return };
        for c in ch.calls.iter_mut() {
            if *c == Some(call.id) {
                *c = None;
            }
        }
        if ch.calls.iter().all(Option::is_none) {
            let ch = r.channels.remove(&key).unwrap();
            r.sources[ch.source].chz.remove_head(ch.head);
        }
    }
}

/// One trunked system: its control channel, parser (band plan) and calls.
struct Trunk {
    cfg: SystemConfig,
    idx: u16,
    calls: CallManager,
    /// Its protocol's control channel decoding ([`super::control`]).
    cc: Box<dyn ControlChannel>,
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
    adjacent: std::collections::BTreeMap<(u32, u32), AdjacentSite>,
    /// AutoTune: the correction its control channel was opened with, ppm;
    /// when it was last measured, and last reopened.
    cc_ppm: f64,
    cc_measured_s: f64,
    cc_retuned_s: f64,
}

impl Trunk {
    fn host<'a>(&self, radio: &'a mut Radio) -> SysHost<'a> {
        SysHost { radio, system: self.idx, bank: self.cfg.bank }
    }

    /// The system this is a site of, and which site: by the configured
    /// group, else by what the control channel says (if its protocol says).
    fn site_key(&self) -> Option<SiteKey> {
        let id = &self.identity;
        let group = if !self.cfg.site_group.is_empty() { format!("group:{}", self.cfg.site_group) } else { self.cc.site_group(id)? };
        Some(SiteKey { group, site: id.site().map(|s| (id.rfss().unwrap_or(0), s)), cc_hz: self.cc_hz })
    }

    /// The talkgroup file names this site as the one to keep `tg`'s calls from.
    fn preferred_for(&self, tg: &super::talkgroups::Talkgroup) -> bool {
        let id = &self.identity;
        let by_name = !tg.preferred_site.is_empty() && tg.preferred_site.eq_ignore_ascii_case(&self.cfg.short_name);
        let n = tg.preferred_nac;
        let by_number = n != 0 && (id.nac().is_some_and(|x| x as u32 == n) || id.site().is_some_and(|s| id.rfss().unwrap_or(0) * 10000 + s == n));
        by_name || by_number
    }

    /// Start listening as its protocol says: the first control channel, or every carrier.
    fn start(&mut self, radio: &mut Radio, events: &mut Vec<Event>) -> Result<(), String> {
        match self.cc.plan() {
            CarrierPlan::Hunt { .. } if !self.cfg.control_channels.is_empty() => self.tune(radio, 0, events),
            CarrierPlan::Watch { carriers, cutoff_hz, note } if !carriers.is_empty() => self.watch(radio, &carriers, cutoff_hz, note, events),
            _ => Ok(()),
        }
    }

    /// Whether it is receiving: a control channel open, or carriers watched.
    fn running(&self) -> bool {
        self.cc_head.is_some() || !self.carriers.is_empty()
    }

    /// Whether any of its carriers is on `source`.
    fn on_source(&self, source: usize) -> bool {
        (self.cc_head.is_some() && self.cc_source == source) || self.carriers.iter().any(|c| c.0 == source)
    }

    /// Hunting: open the control channel `index` (of those a source covers).
    fn tune(&mut self, radio: &mut Radio, index: usize, events: &mut Vec<Event>) -> Result<(), String> {
        let CarrierPlan::Hunt { cutoff_hz } = self.cc.plan() else { return Ok(()) };
        let list: Vec<(f64, usize)> = self.cfg.control_channels.iter().filter_map(|&f| radio.source_for(f).map(|s| (f, s))).collect();
        if list.is_empty() {
            return Err(format!("{}: no control channel falls inside a source's bandwidth — move the center frequency.", self.cfg.short_name));
        }
        self.cc_index = index % list.len();
        let (hz, src) = list[self.cc_index];
        if let Some(h) = self.cc_head.take() {
            radio.sources[self.cc_source].chz.remove_head(h);
        }
        let retune = self.cc_hz.is_some();
        self.cc_source = src;
        let s = &mut radio.sources[src];
        let (head, _, _) = s.chz.add_head(s.offset(hz), cutoff_hz, 0.0);
        self.cc_ppm = s.tune_ppm;
        self.cc_measured_s = self.now_s;
        self.cc_head = Some(head);
        self.cc_hz = Some(hz.round() as u64);
        self.cc_start_s = self.now_s;
        self.cc_samples = 0;
        self.cc_offset_s = s.clock_offset_s;
        self.last_good_s = self.now_s;
        self.cc.restart(s.chz.output_rate());
        if retune {
            // Another control channel may be another site: learn it afresh.
            self.identity = Identity::default();
            self.cc.forget_site();
            self.mismatch = None;
            self.adjacent.clear();
        }
        events.push(Event::ControlChannel { system: self.idx, freq_hz: hz.round() as u64 });
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
            let s = &mut radio.sources[src];
            let (head, _, start) = s.chz.add_head(hz - s.cfg.center_hz, cutoff_hz, 0.0);
            heads.push((src, head, start));
        }
        if !outside.is_empty() {
            return Err(format!("{}: {} frequencies outside every source's bandwidth: {} MHz — move a center frequency.", self.cfg.short_name, self.cc.name(), outside.join(", ")));
        }
        self.carriers = heads;
        events.push(Event::Note { system: self.idx, text: note });
        Ok(())
    }

    /// A block ran on `source`: decode its carriers on it and follow what they say.
    fn on_block(&mut self, radio: &mut Radio, source: usize, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        if self.carriers.is_empty() {
            self.on_block_hunting(radio, events, call_events);
        } else {
            self.on_block_watching(radio, source, events, call_events);
        }
    }

    fn on_block_hunting(&mut self, radio: &mut Radio, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        let Some(head) = self.cc_head else { return };
        let src = self.cc_source;
        let rate = radio.sources[src].chz.output_rate();
        let iq = radio.sources[src].chz.output(head).map(|v| v.to_vec()).unwrap_or_default();
        self.cc_samples += iq.len() as u64;
        let moved = radio.sources[src].clock_offset_s - self.cc_offset_s;
        self.now_s = self.cc_start_s + self.cc_samples as f64 / rate + moved;
        let mut steps = Vec::new();
        self.cc.push(0, &iq, self.cc_start_s + moved, rate, &mut steps);
        self.follow(radio, steps, events, call_events);
        if self.now_s - self.last_good_s > CC_HUNT_S && self.cfg.control_channels.len() > 1 {
            let _ = self.tune(radio, self.cc_index + 1, events);
        } else if self.now_s - self.cc_measured_s >= TUNE_EVERY_S {
            self.measure(radio);
        }
        let mut host = self.host(radio);
        self.calls.tick(self.now_s, &mut host, call_events);
    }

    fn on_block_watching(&mut self, radio: &mut Radio, source: usize, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        if !self.carriers.iter().any(|c| c.0 == source) {
            return;
        }
        let s = &radio.sources[source];
        let (rate, fs, offset) = (s.chz.output_rate(), s.cfg.rate_hz, s.clock_offset_s);
        let mut steps = Vec::new();
        for (i, &(src, head, start)) in self.carriers.iter().enumerate() {
            if src != source {
                continue;
            }
            let iq = radio.sources[src].chz.output(head).map(|v| v.to_vec()).unwrap_or_default();
            self.cc.push(i, &iq, start as f64 / fs + offset, rate, &mut steps);
        }
        self.now_s = self.now_s.max(radio.sources[source].time());
        for text in self.cc.take_notes() {
            events.push(Event::Note { system: self.idx, text });
        }
        // Where the control channel is now (Capacity Plus: the rest channel moves).
        let cc = self.cc.control_hz();
        if let Some(hz) = cc.filter(|&h| Some(h) != self.cc_hz) {
            self.cc_hz = Some(hz);
            events.push(Event::ControlChannel { system: self.idx, freq_hz: hz });
        }
        if cc.is_some() {
            self.last_good_s = self.now_s;
        }
        self.follow(radio, steps, events, call_events);
        let mut host = self.host(radio);
        self.calls.tick(self.now_s, &mut host, call_events);
    }

    /// Follow what the control channel decoded, step by step: the site lock
    /// first (another site's grants aren't followed, and until every locked
    /// field has been heard nothing is), then the call manager.
    fn follow(&mut self, radio: &mut Radio, steps: Vec<Step>, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        // The site lock: on the fields its protocol states (none: no lock).
        let fields = self.cc.identity_fields();
        let lock = !fields.is_empty();
        for st in steps {
            self.identity = st.identity;
            for m in st.msgs.iter().filter(|m| m.kind == MessageType::Adjacent && m.freq_hz > 0) {
                self.adjacent.insert((m.rfss, m.site), AdjacentSite { sys_id: m.sys_id, rfss: m.rfss, site: m.site, freq_hz: m.freq_hz });
            }
            if lock {
                let conflict = self.identity.conflict(&self.cfg.expect, fields);
                if conflict != self.mismatch {
                    if let Some(c) = &conflict {
                        let hz = self.cc_hz.unwrap_or(0) as f64 / 1e6;
                        events.push(Event::Note { system: self.idx, text: format!("Control channel {hz:.5} MHz is not this system: {c}") });
                    }
                    self.mismatch = conflict;
                }
            }
            events.extend(st.msgs.iter().cloned().map(|msg| Event::Message { system: self.idx, msg }));
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
            if let Some(k) = self.cc.voice_params(&self.identity).tdma_key {
                radio.voice_params[self.idx as usize].tdma_key = Some(k);
            }
            let mut host = self.host(radio);
            self.calls.handle(&st.msgs, &mut host, call_events);
        }
        (self.good, self.bad) = self.cc.counts();
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
        let Some(off) = self.cc.offset_hz() else { return };
        let hz = hz as f64;
        let s = &mut radio.sources[self.cc_source];
        s.measured(self.cc_ppm + off as f64 / hz * 1e6);
        let off_by = (s.tune_ppm - self.cc_ppm) * 1e-6 * hz;
        if self.cc.reopens() && off_by.abs() > TUNE_RETUNE_HZ && self.now_s - self.cc_retuned_s >= TUNE_RETUNE_S {
            let CarrierPlan::Hunt { cutoff_hz } = self.cc.plan() else { return };
            s.chz.remove_head(head);
            let (head, _, _) = s.chz.add_head(s.offset(hz), cutoff_hz, 0.0);
            self.cc_head = Some(head);
            self.cc_ppm = s.tune_ppm;
            self.cc_retuned_s = self.now_s;
            self.cc_start_s = self.now_s;
            self.cc_samples = 0;
            self.cc_offset_s = s.clock_offset_s;
            self.cc.restart(s.chz.output_rate());
        }
    }

    fn status(&self, radio: &Radio) -> SystemStatus {
        SystemStatus {
            system: self.idx,
            short_name: self.cfg.short_name.clone(),
            now_s: self.now_s,
            control_channel_hz: self.cc_hz,
            identity: self.identity.clone(),
            good: self.good,
            bad: self.bad,
            modulation: self.cc.modulation(),
            active_calls: self.calls.calls.len(),
            recording: self.calls.calls.iter().filter(|c| radio.recordings.contains_key(&c.id)).count(),
            calls_concluded: self.concluded,
            mismatch: self.mismatch.clone(),
            adjacent: self.adjacent.values().copied().collect(),
            patches: self.calls.patches.active(),
            protocol: self.cc.status(),
            site_group: self.site_key().map(|k| k.group),
        }
    }
}

pub struct Engine {
    cfg: EngineConfig,
    radio: Radio,
    trunks: Vec<Trunk>,
    conv: Conventional,
    /// Each conventional system's ids and talkgroup names (their calls live in `conv`).
    conv_calls: Vec<CallManager>,
    conv_out: Vec<ConvOut>,
    conv_concluded: u64,
    /// Every system's radios' talker aliases.
    aliases: AliasBook,
    /// Copies of one call on several sites.
    multisite: MultiSite,
    now_s: f64,
    events: Vec<Event>,
    call_events: Vec<CallEvent>,
}

impl Engine {
    pub fn new(mut cfg: EngineConfig) -> Result<Self, String> {
        // Every conventional channel's system exists (a default one if none was given).
        let need = cfg.conventional.iter().map(|c| c.system + 1).max().unwrap_or(0);
        if need > MAX_CONVENTIONAL {
            return Err(format!("At most {MAX_CONVENTIONAL} conventional systems."));
        }
        while cfg.conv_systems.len() < need {
            cfg.conv_systems.push(ConvSystem::default());
        }
        if cfg.sources.is_empty() {
            return Err("no sources configured".into());
        }
        let trunked: Vec<&SystemConfig> = cfg.systems.iter().filter(|s| !s.control_channels.is_empty()).collect();
        if trunked.is_empty() && cfg.conventional.is_empty() {
            return Err("Add a control channel or a conventional channel.".into());
        }
        // A short name is a system's identity (its folder, its talker aliases, its plugin settings).
        let mut seen = std::collections::HashSet::new();
        let conv_names = cfg.conv_systems.iter().enumerate().filter(|(k, _)| cfg.conventional.iter().any(|c| c.system == *k)).map(|(_, c)| c.short_name.as_str());
        if let Some(d) = cfg.systems.iter().map(|s| s.short_name.as_str()).chain(conv_names).find(|n| !seen.insert(*n)) {
            return Err(format!("Two systems are named \"{d}\" — each needs its own short name."));
        }
        let history = cfg.preroll_s.max(cfg.conv.preroll_s).max(0.1);
        let spans: Vec<(f64, f64)> = cfg.sources.iter().map(|s| (s.center_hz, s.rate_hz)).collect();
        let conv = Conventional::new(&cfg.conventional, &spans, ConvConfig { vocoder: cfg.vocoder, ..cfg.conv }, cfg.bank, USABLE)?;
        let sources: Vec<Source> =
            cfg.sources.iter().map(|s| Source { cfg: s.clone(), chz: Channelizer::new(s.rate_hz, MIN_CHANNEL_RATE, history), errors: VecDeque::new(), tune_ppm: 0.0, in_gap: false, clock_offset_s: 0.0 }).collect();
        let rate = sources[0].chz.output_rate();
        let ids = CallIds::default();
        let mut radio = Radio {
            sources,
            channels: HashMap::new(),
            recordings: HashMap::new(),
            free_nums: Vec::new(),
            next_num: 0,
            max_recorders: cfg.max_recorders,
            preroll_s: cfg.preroll_s,
            capture_frames: cfg.capture_frames,
            vocoder: cfg.vocoder,
            tout: Vec::new(),
            voice_params: vec![VoiceParams::default(); cfg.systems.len()],
            pending: Vec::new(),
        };
        let mut events = Vec::new();
        let mut trunks = Vec::new();
        for (i, sc) in cfg.systems.iter().enumerate() {
            let mut t = Trunk {
                idx: i as u16,
                calls: CallManager::with_ids(sc.calls, sc.talkgroups.clone(), i as u16, ids.clone()),
                cc: control::build(sc, rate),
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
                cfg: sc.clone(),
            };
            t.calls.patches.hold_s = t.cc.patch_hold_s();
            t.start(&mut radio, &mut events)?;
            trunks.push(t);
        }
        let conv_calls = cfg.conv_systems.iter().enumerate().map(|(k, c)| CallManager::with_ids(c.calls, c.talkgroups.clone(), conventional_system(k), ids.clone())).collect();
        let aliases = AliasBook::new(
            cfg.systems.iter().map(|s| s.short_name.clone()),
            cfg.conv_systems.iter().enumerate().map(|(k, c)| (c.short_name.clone(), cfg.conventional.iter().any(|ch| ch.system == k))),
        );
        Ok(Engine {
            radio,
            trunks,
            conv,
            conv_calls,
            conv_out: Vec::new(),
            conv_concluded: 0,
            aliases,
            multisite: MultiSite::default(),
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
            t.cc.load_bandplan(s);
        }
    }
    /// A system's band plan to save: P25's IDEN tables; DMR's logical channel → frequency table.
    pub fn bandplan(&self, system: usize) -> String {
        self.trunks.get(system).map_or_else(String::new, |t| t.cc.bandplan())
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

    /// Note a talker alias heard on a call of `system`'s.
    fn learn_alias(&mut self, system: u16, a: Alias, call_tg: Option<u32>) {
        let tg = a.talkgroup.or(call_tg);
        let (now_s, wacn, sys_id) = match self.trunks.get(system as usize) {
            Some(t) => (t.now_s, t.identity.wacn(), t.identity.sys_id()),
            None => (self.now_s, None, None),
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

    pub fn sources(&self) -> &[SourceConfig] {
        &self.cfg.sources
    }

    pub fn systems(&self) -> &[SystemConfig] {
        &self.cfg.systems
    }

    /// Calls in progress (recording or monitoring), trunked then conventional.
    pub fn active_calls(&self) -> Vec<Call> {
        self.trunks.iter().flat_map(|t| t.calls.calls.iter()).chain(self.conv.calls()).cloned().collect()
    }

    /// Everything that happened since the last call.
    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Power spectrum (dBFS, fft-shifted) of a source's latest block — for the waterfall.
    pub fn spectrum(&self, source: usize, bins: usize) -> Vec<f32> {
        self.radio.sources.get(source).map_or_else(Vec::new, |s| s.chz.power_spectrum(bins))
    }

    pub fn status(&self) -> Status {
        let systems: Vec<SystemStatus> = self.trunks.iter().filter(|t| t.running()).map(|t| t.status(&self.radio)).collect();
        Status {
            now_s: self.now_s,
            active_calls: systems.iter().map(|s| s.active_calls).sum::<usize>() + self.conv.calls().count(),
            recording: self.radio.recordings.len(),
            channels_open: self.radio.channels.len() + self.conv.open_count(),
            conventional_open: self.conv.open_count(),
            calls_concluded: systems.iter().map(|s| s.calls_concluded).sum::<u64>() + self.conv_concluded,
            systems,
            sources: self.radio.sources.iter().map(|s| SourceTune { error_ppm: s.error_ppm(), applied_ppm: s.tune_ppm }).collect(),
        }
    }

    /// Feed a source's RTL-SDR native u8 IQ (any length).
    pub fn push_u8(&mut self, source: usize, data: &[u8]) {
        let mut off = 0;
        while off < data.len() {
            let (used, ran) = self.radio.sources[source].chz.feed_u8(&data[off..]);
            off += used;
            if ran {
                self.on_block(source);
            }
            if used == 0 {
                break;
            }
        }
    }

    /// Feed a source's float IQ.
    pub fn push_iq(&mut self, source: usize, iq: &[Complex32]) {
        let mut off = 0;
        while off < iq.len() {
            let (used, ran) = self.radio.sources[source].chz.feed(&iq[off..]);
            off += used;
            if ran {
                self.on_block(source);
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
    pub fn push_gap(&mut self, source: usize, n: u64) {
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
        self.radio.sources[source].in_gap = true;
        let mut left = n;
        while left > 0 {
            let k = left.min(CHUNK) as usize;
            self.push_iq(source, &quiet[..k]);
            left -= k as u64;
        }
        self.radio.sources[source].in_gap = false;
    }

    /// Source `source`'s sample clock is this far behind the engine's
    /// clock, s: added to every time taken from its samples from now on.
    /// Every clock here is a sample count; a source's runs slow or fast by
    /// its crystal's error, and starts late by the time it took to open.
    /// The platform measures it against the wall clock (trunk-app's
    /// `Session` does) and sets it — in small steps once running, so time
    /// never runs backwards. 0 by default (replays).
    pub fn set_clock_offset(&mut self, source: usize, offset_s: f64) {
        if let Some(s) = self.radio.sources.get_mut(source) {
            s.clock_offset_s = offset_s;
        }
    }

    /// End of input: release what the receivers still hold and end every call.
    pub fn finish(&mut self) {
        for t in self.trunks.iter_mut() {
            let mut steps = Vec::new();
            t.cc.flush(&mut steps);
            t.follow(&mut self.radio, steps, &mut self.events, &mut self.call_events);
        }
        let keys: Vec<(u16, u64)> = self.radio.channels.keys().copied().collect();
        for k in keys {
            self.radio.run_head(k, true);
        }
        self.apply_pending();
        for t in self.trunks.iter_mut() {
            let mut host = t.host(&mut self.radio);
            t.calls.end_all(&mut host, &mut self.call_events);
        }
        self.emit_call_events();
        let rules = self.call_rules();
        let mut out = std::mem::take(&mut self.conv_out);
        self.conv.finish(&rules, &mut out);
        self.emit_conv(out);
    }

    /// Each conventional system's call rules.
    fn call_rules(&self) -> Vec<CallRules> {
        self.cfg
            .conv_systems
            .iter()
            .map(|c| CallRules {
                call_timeout_s: c.calls.call_timeout_s,
                max_call_s: c.calls.max_call_s,
                record_encrypted: c.calls.record_encrypted,
                capture_frames: self.cfg.capture_frames,
            })
            .collect()
    }

    /// Report what the conventional channels did.
    fn emit_conv(&mut self, mut out: Vec<ConvOut>) {
        for o in out.drain(..) {
            match o {
                ConvOut::Start(c) => self.events.push(Event::CallStart(c)),
                ConvOut::Update(c) => self.events.push(Event::CallUpdate(c)),
                ConvOut::Audio { call_id, system, talkgroup, samples } => self.events.push(Event::Audio { call_id, system, talkgroup, samples }),
                ConvOut::End { call, audio, frames, recorder_num, tx, reception } => {
                    self.write_call(Held { call: call.clone(), audio, frames, recorder_num, tx, reception, freq_error_hz: None });
                    self.events.push(Event::CallEnd(call));
                }
                ConvOut::Alias(system, a) => self.learn_alias(system, a, None),
                ConvOut::Skipped { freq_hz, code } => self.events.push(Event::ConvSkipped { freq_hz, code }),
            }
        }
        self.conv_out = out;
    }

    fn on_block(&mut self, source: usize) {
        // Voice channels on this source, then its control channels. What
        // the voice channels heard (and channels just opened heard in their
        // pre-roll) reaches the calls after both, once, at this block's time.
        let keys: Vec<(u16, u64)> = self.radio.channels.iter().filter(|(_, c)| c.source == source).map(|(&k, _)| k).collect();
        for k in keys {
            self.radio.run_head(k, false);
        }
        for t in self.trunks.iter_mut().filter(|t| t.on_source(source)) {
            t.on_block(&mut self.radio, source, &mut self.events, &mut self.call_events);
        }
        self.apply_pending();
        if source == 0 {
            self.now_s = self.radio.sources[0].time();
        }
        if !self.conv.is_empty() {
            let t = self.radio.sources[source].time();
            let rules = self.call_rules();
            let mut out = std::mem::take(&mut self.conv_out);
            let gap = self.radio.sources[source].in_gap;
            self.conv.on_block(source, &mut self.radio.sources[source].chz, t, gap, &mut self.conv_calls, &rules, &mut out);
            self.emit_conv(out);
        }
        self.apply_pending();
        self.emit_call_events();
    }

    /// Route what the voice trackers produced to their calls.
    fn apply_pending(&mut self) {
        for (sys, id, o) in std::mem::take(&mut self.radio.pending) {
            let Some(t) = self.trunks.get_mut(sys as usize) else { continue };
            match o {
                TrackerOut::Audio(samples, frame) => {
                    let Some(rec) = self.radio.recordings.get_mut(&id) else { continue };
                    rec.tx.note(rec.audio.len(), t.now_s);
                    rec.audio.extend_from_slice(&samples);
                    rec.frames.push(frame);
                    t.calls.note_audio(id, t.now_s);
                    let tg = t.calls.calls.iter().find(|c| c.id == id).map_or(0, |c| c.talkgroup);
                    if self.multisite.audio_lead(id, t.now_s) {
                        self.events.push(Event::Audio { call_id: id, system: sys, talkgroup: tg, samples });
                    }
                }
                TrackerOut::AnalogAudio(samples) => {
                    let Some(rec) = self.radio.recordings.get_mut(&id) else { continue };
                    rec.tx.note(rec.audio.len(), t.now_s);
                    rec.audio.extend_from_slice(&samples);
                    t.calls.note_audio(id, t.now_s);
                    let tg = t.calls.calls.iter().find(|c| c.id == id).map_or(0, |c| c.talkgroup);
                    if self.multisite.audio_lead(id, t.now_s) {
                        self.events.push(Event::Audio { call_id: id, system: sys, talkgroup: tg, samples });
                    }
                }
                TrackerOut::Info { source, emergency, encrypted } => {
                    let now = t.now_s;
                    if let Some(c) = t.calls.call_mut(id) {
                        let mut changed = false;
                        if encrypted && !c.encrypted {
                            c.encrypted = true;
                            changed = true;
                        }
                        if emergency && !c.emergency {
                            c.emergency = true;
                            changed = true;
                        }
                        if let Some(src) = source {
                            changed |= CallManager::note_source(c, src, now, emergency);
                        }
                        if changed {
                            self.call_events.push(CallEvent::Update(c.clone()));
                        }
                    }
                }
                // (Only conventional FM asks for it.)
                TrackerOut::Subaudible(_) => {}
                TrackerOut::Alias(a) => {
                    let tg = t
                        .calls
                        .calls
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| c.talkgroup);
                    self.learn_alias(sys, a, tg);
                }
            }
        }
    }

    fn emit_call_events(&mut self) {
        for ev in std::mem::take(&mut self.call_events) {
            match ev {
                CallEvent::Start(c) => {
                    self.link_twins(&c);
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

    /// Multi-site: link a call just granted with its copies on other sites of its system.
    fn link_twins(&mut self, c: &Call) {
        if !self.cfg.drop_duplicates {
            return;
        }
        let Some(key) = self.trunks.get(c.system as usize).and_then(Trunk::site_key) else { return };
        let sites = self.trunks.iter().filter(|t| t.idx != c.system).map(|t| (t.site_key(), t.calls.calls.as_slice()));
        self.multisite.link_new(c, &key, sites);
    }

    /// The other sites' copies of call `id` still going or held, as (call, system).
    pub fn twins(&self, id: CallId) -> Vec<(CallId, u16)> {
        self.multisite.twins(id)
    }

    /// A call ended: save it, or (multi-site) the best of its copies once the last one ends.
    fn conclude(&mut self, call: &Call) {
        let held = self.radio.recordings.remove(&call.id).map(|rec| {
            self.radio.free_nums.push(rec.recorder_num);
            Held { call: call.clone(), audio: rec.audio, frames: rec.frames, recorder_num: rec.recorder_num, tx: rec.tx, reception: rec.reception, freq_error_hz: (rec.freq_error.1 > 0).then(|| rec.freq_error.0 / rec.freq_error.1 as f64) }
        });
        let trunks = &self.trunks;
        let Some((kept, others)) = self.multisite.conclude(call.id, held, |system, tg| trunks.get(system as usize).is_some_and(|t| t.preferred_for(tg))) else { return };
        let kept_call = kept.call.clone();
        if self.write_call(kept) {
            for h in others {
                self.events.push(Event::Duplicate { call: h.call, kept: kept_call.clone() });
            }
        }
    }

    /// A finished call's record and audio, as [`Event::Concluded`]; false
    /// when not kept (silent, or shorter than its system's minimum).
    fn write_call(&mut self, h: Held) -> bool {
        let system = h.call.system;
        let conv = conventional_index(system).and_then(|k| self.cfg.conv_systems.get(k));
        let trunk = self.trunks.get(system as usize);
        let short_name = match trunk {
            Some(t) => t.cfg.short_name.clone(),
            None => conv.map_or_else(|| "conv".to_string(), |c| c.short_name.clone()),
        };
        let saved = save_call(
            h,
            &SaveContext {
                rules: conv.map_or_else(|| trunk.map_or(SaveRules::default(), |t| t.cfg.save), |c| c.save),
                short_name: &short_name,
                epoch_ms_at_zero: self.cfg.epoch_ms_at_zero,
                units: self.aliases.of(system),
                unit_tags: conv.map(|c| &c.unit_tags).or_else(|| trunk.map(|t| &t.cfg.unit_tags)).filter(|t| !t.is_empty() || t.mode != Default::default()),
            },
        );
        match saved {
            Ok(k) => {
                match self.trunks.get_mut(system as usize) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systems_need_their_own_names_and_share_call_ids() {
        let src = vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false }];
        let sys = |n: &str| SystemConfig { short_name: n.into(), control_channels: vec![851.0125e6], ..Default::default() };
        let cfg = EngineConfig { systems: vec![sys("a"), sys("a")], sources: src.clone(), ..Default::default() };
        assert!(Engine::new(cfg).err().unwrap().contains("\"a\""));
        let cfg = EngineConfig { systems: vec![sys("a"), sys("b")], sources: src, ..Default::default() };
        let e = Engine::new(cfg).unwrap();
        assert_eq!(e.status().systems.len(), 2);
        let ids = CallIds::default();
        let (mut x, mut y) = (CallManager::with_ids(CallConfig::default(), Talkgroups::default(), 0, ids.clone()), CallManager::with_ids(CallConfig::default(), Talkgroups::default(), 1, ids));
        assert_eq!([x.allocate_id(), y.allocate_id(), x.allocate_id()], [1, 2, 3]);
    }

    /// Three sites grant TG 101: east and west of one system, far of another.
    /// East decodes `east_good` clean frames, west 100. What gets saved?
    fn two_sites(east_good: usize, prefer: &str, dedupe: bool) -> (Vec<Event>, [CallId; 3]) {
        use super::super::frames::{Codec, VoiceFrame};
        let tgs: Talkgroups = [(101, super::super::talkgroups::Talkgroup { number: 101, preferred_site: prefer.into(), ..Default::default() })].into_iter().collect();
        let save = SaveRules { normalize: false, ..Default::default() };
        let sys = |n: &str, cc: f64, g: &str| SystemConfig { short_name: n.into(), control_channels: vec![cc], site_group: g.into(), talkgroups: tgs.clone(), save, ..Default::default() };
        let cfg = EngineConfig {
            systems: vec![sys("east", 851.0125e6, "dc"), sys("west", 851.2125e6, "dc"), sys("far", 851.4125e6, "md")],
            sources: vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false }],
            drop_duplicates: dedupe,
            ..Default::default()
        };
        let mut e = Engine::new(cfg).unwrap();
        let mut ids = [0; 3];
        for (i, (t, f)) in [(0.0, 851_500_000), (0.4, 851_600_000), (0.2, 851_700_000)].into_iter().enumerate() {
            let Engine { trunks, radio, call_events, .. } = &mut e;
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
            let Engine { trunks, radio, call_events, .. } = &mut e;
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
            systems: vec![SystemConfig { short_name: "ddm".into(), control_channels: vec![851.0125e6], ..Default::default() }],
            sources: vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6, auto_tune: false }],
            ..Default::default()
        };
        let mut e = Engine::new(cfg).unwrap();
        let f = 851_500_000;
        fn grant(e: &mut Engine, f: u64, t: f64, tg: u32, phase2: bool, slot: u8) {
            let Engine { trunks, radio, call_events, .. } = e;
            let mut host = trunks[0].host(radio);
            let m = Message { kind: MessageType::Grant, time_s: t, talkgroup: tg, freq_hz: f, phase2_tdma: phase2, tdma_slot: slot, ..Default::default() };
            trunks[0].calls.handle(&[m], &mut host, call_events);
        }
        grant(&mut e, f, 0.0, 101, false, 0);
        assert_eq!(e.radio.channels[&(0, f)].voice.kind(), VoiceKind::Fdma);
        grant(&mut e, f, 1.0, 202, true, 1);
        let ch = &e.radio.channels[&(0, f)];
        assert_eq!(ch.voice.kind(), VoiceKind::Tdma);
        let tg202 = e.trunks[0].calls.calls.iter().find(|c| c.talkgroup == 202).unwrap().id;
        assert_eq!(ch.calls, [None, Some(tg202)]);
        // The same kind again shares it, on its own slot.
        grant(&mut e, f, 1.5, 303, true, 0);
        assert_eq!(e.radio.channels.len(), 1);
        assert!(e.radio.channels[&(0, f)].calls.iter().all(Option::is_some));
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
    }
}
