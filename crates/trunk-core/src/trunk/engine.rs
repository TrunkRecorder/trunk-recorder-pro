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

use super::calls::{Call, CallConfig, CallEvent, CallId, CallIds, CallManager, Reason, RecorderHost, CONVENTIONAL};
use super::conventional::{CallRules, ConvChannel, ConvConfig, ConvOut, Conventional};
use super::message::{Message, MessageType, TsbkParser};
use super::multisite::{self, Ended, Held, MultiSite, SiteKey};
use super::patches;
use super::record::{call_record, ConcludeInfo, Reception, Transmissions};
use super::talkgroups::Talkgroups;
use super::tdma::TdmaTracker;
use super::frames::{frames_jsonl, CallFrames};
use super::tracker::{TrackerOut, VoiceTracker};
use super::units::{UnitAlias, UnitAliases, UnitTags};
use crate::dsp::cqpsk::{self, Cqpsk};
use crate::dsp::{Channelizer, HeadId, Receiver, Symbol};
use crate::loudness;
use crate::mbe;
use crate::p25::alias::Alias;
use crate::p25::diversity::{best_frame, best_tsbks, Bank, BankConfig, Group};
use crate::p25::frame::TSDU;
use crate::p25::phase2::{self, Packet};
use crate::dsp::fm::{ChannelFilter, Nbfm};
use crate::dsp::signalling::Signalling;
use crate::smartnet::{self, Bandplan};
use crate::dmr::{self, DmrConfig, DmrVoice};
use crate::dsp::c4fm::C4fm;

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

/// What becomes of a finished call's audio: which calls are kept, and how
/// they sound.
#[derive(Clone, Copy, Debug)]
pub struct SaveRules {
    /// Keep calls with no decoded audio (encrypted, lost).
    pub keep_silent: bool,
    /// Drop calls with less audio than this, s (Trunk Recorder's minDuration).
    pub min_call_s: f64,
    /// Leave out transmissions shorter than this, s (minTransmissionDuration).
    pub min_transmission_s: f64,
    /// Bring each call's speech to one level ([`crate::loudness`]).
    pub normalize: bool,
    /// Then raise (or lower) digital and analog audio by this much, dB
    /// (Trunk Recorder's digitalLevels / analogLevels).
    pub digital_gain_db: f32,
    pub analog_gain_db: f32,
}

impl Default for SaveRules {
    fn default() -> Self {
        SaveRules { keep_silent: false, min_call_s: 0.0, min_transmission_s: 0.0, normalize: true, digital_gain_db: 0.0, analog_gain_db: 0.0 }
    }
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
    /// SmartNet instead of P25 on the control channels (voice: P25 or FM per grant).
    pub smartnet: Option<SmartnetConfig>,
    /// Trunked DMR instead: every control channel (and [`DmrConfig::channels`]) is watched.
    pub dmr: Option<DmrConfig>,
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
            smartnet: None,
            dmr: None,
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
    /// Conventional channels' call rules (timeout, encrypted, length) and what is saved of them.
    pub calls: CallConfig,
    pub conv_save: SaveRules,
    /// The conventional channels' names for their radios.
    pub conv_unit_tags: UnitTags,
    /// Wall-clock epoch ms at sample-clock time 0.
    pub epoch_ms_at_zero: f64,
    /// Receivers for conventional P25 channels.
    pub bank: BankConfig,
    /// Conventional channels, energy-detected on whichever source covers them.
    pub conventional: Vec<ConvChannel>,
    pub conv: ConvConfig,
    /// Folder and record name of conventional calls.
    pub conv_short_name: String,
    /// Names for talkgroups a conventional P25 channel reports.
    pub conv_talkgroups: Talkgroups,
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
            calls: CallConfig::default(),
            conv_save: SaveRules::default(),
            conv_unit_tags: UnitTags::default(),
            epoch_ms_at_zero: 0.0,
            bank: BankConfig::default(),
            conventional: vec![],
            conv: ConvConfig::default(),
            conv_short_name: "conv".into(),
            conv_talkgroups: Talkgroups::default(),
            capture_frames: false,
            drop_duplicates: true,
            vocoder: mbe::Profile::Enhanced,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Concluded {
    pub call: Call,
    /// Trunk Recorder's call JSON.
    pub json: String,
    /// The call's system's short name (its folder).
    pub short_name: String,
    /// `<talkgroup>-<start epoch>_<freq>[.slot]`
    pub base_name: String,
    /// 8 kHz mono in [−1, 1].
    pub audio: Vec<f32>,
    /// With [`EngineConfig::capture_frames`]: the vocoder frames behind
    /// `audio`, as JSON lines (see [`super::frames::frames_jsonl`]).
    pub frames: Option<String>,
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identity {
    pub nac: Option<u16>,
    pub wacn: Option<u32>,
    pub sys_id: Option<u32>,
    pub rfss: Option<u32>,
    pub site: Option<u32>,
}

impl Identity {
    /// The fields both know and disagree on, e.g. "site 3 (expected 4)"; None when they agree.
    pub fn conflict(&self, expect: &Identity) -> Option<String> {
        let mut d = Vec::new();
        let mut cmp = |name: &str, hex: bool, got: Option<u32>, want: Option<u32>| {
            if let (Some(g), Some(w)) = (got, want) {
                if g != w {
                    d.push(if hex { format!("{name} {g:X} (expected {w:X})") } else { format!("{name} {g} (expected {w})") });
                }
            }
        };
        cmp("NAC", true, self.nac.map(u32::from), expect.nac.map(u32::from));
        cmp("WACN", true, self.wacn, expect.wacn);
        cmp("SysID", true, self.sys_id, expect.sys_id);
        cmp("RFSS", false, self.rfss, expect.rfss);
        cmp("site", false, self.site, expect.site);
        (!d.is_empty()).then(|| d.join(", "))
    }

    /// Every field `expect` names is known here.
    pub fn confirms(&self, expect: &Identity) -> bool {
        (expect.nac.is_none() || self.nac.is_some())
            && (expect.wacn.is_none() || self.wacn.is_some())
            && (expect.sys_id.is_none() || self.sys_id.is_some())
            && (expect.rfss.is_none() || self.rfss.is_some())
            && (expect.site.is_none() || self.site.is_some())
    }
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
    pub dmr: Option<dmr::SiteStatus>,
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
    /// A channel's offset from the tuned centre, corrected.
    fn offset(&self, hz: f64) -> f64 {
        hz * (1.0 + self.tune_ppm * 1e-6) - self.cfg.center_hz
    }
}

enum Voice {
    /// Phase 1: receiver bank → IMBE tracker.
    Fdma { bank: Bank, tracker: VoiceTracker },
    /// Phase 2: H-DQPSK receiver → slot framer → TDMA tracker (both slots).
    Tdma { rx: Cqpsk, framer: phase2::Framer, tracker: TdmaTracker, syms: Vec<Symbol>, pkts: Vec<Packet> },
    /// Analog FM (SmartNet analog grants), squelched at `open` carrier
    /// power; unit IDs from MDC1200 / FleetSync bursts.
    Analog { fm: Nbfm, open: f32, ids: Signalling },
    /// DMR: 4FSK receiver → framer → both slots.
    Dmr { rx: C4fm, voice: Box<DmrVoice>, syms: Vec<Symbol> },
}

struct Channel {
    /// The system whose grant opened it.
    system: u16,
    source: usize,
    head: HeadId,
    /// Absolute input sample (of its source) of the head's first output.
    start_sample: u64,
    /// The call on each TDMA slot (Phase 1: slot 0).
    calls: [Option<CallId>; 2],
    voice: Voice,
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
    groups: Vec<Group>,
    tout: Vec<(usize, TrackerOut)>,
    /// Each system's Phase 2 scrambler seed (NAC, System ID, WACN), once its control channel gave it.
    tdma_keys: Vec<Option<(u32, u32, u32)>>,
    /// Audio / info produced by the voice channels, as (system, call, output), applied after.
    pending: Vec<(u16, CallId, TrackerOut)>,
}

impl Radio {
    fn source_for(&self, hz: f64) -> Option<usize> {
        self.sources.iter().position(|s| (hz - s.cfg.center_hz).abs() <= s.cfg.rate_hz / 2.0 * USABLE)
    }

    /// Run a channel's receivers over `iq`, collecting what its tracker
    /// produced as (slot, output).
    #[allow(clippy::too_many_arguments)]
    fn run_channel(
        ch: &mut Channel,
        iq: &[Complex32],
        rate: f64,
        src_rate: f64,
        key: Option<(u32, u32, u32)>,
        groups: &mut Vec<Group>,
        tout: &mut Vec<(usize, TrackerOut)>,
        flush: bool,
    ) {
        let t0 = ch.start_sample as f64 / src_rate;
        match &mut ch.voice {
            Voice::Fdma { bank, tracker } => {
                groups.clear();
                bank.push(iq, groups);
                if flush {
                    bank.flush(groups);
                }
                let mut out = Vec::new();
                for g in groups.iter() {
                    tracker.group(g, t0 + best_frame(g).sample / rate, &mut out);
                }
                tout.extend(out.into_iter().map(|o| (0, o)));
            }
            Voice::Tdma { rx, framer, tracker, syms, pkts } => {
                if let Some((nac, sys, wacn)) = key {
                    tracker.set_key(nac, sys, wacn);
                }
                syms.clear();
                pkts.clear();
                rx.push(iq, syms);
                for s in syms.iter() {
                    framer.push(s, pkts);
                }
                for p in pkts.iter() {
                    tracker.packet(p, t0 + p.sample / rate, tout);
                }
            }
            Voice::Dmr { rx, voice, syms } => {
                // Vocode only the slots somebody is recording.
                voice.vocode = [ch.calls[0].is_some(), ch.calls[1].is_some()];
                syms.clear();
                rx.push(iq, syms);
                let mut vout = Vec::new();
                voice.push(syms, t0, rate, &mut vout);
                tout.extend(vout.into_iter().map(|o| (o.slot as usize, o.out)));
            }
            Voice::Analog { fm, open, ids } => {
                let mut audio = Vec::new();
                fm.push(iq, *open, &mut audio);
                let mut found = Vec::new();
                ids.push(&audio, &mut found);
                for u in found {
                    tout.push((0, TrackerOut::Info { source: Some(u.unit), emergency: u.emergency, encrypted: false }));
                }
                if !audio.is_empty() {
                    tout.push((0, TrackerOut::AnalogAudio(audio)));
                }
            }
        }
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
        let (rate, src_rate) = (s.chz.output_rate(), s.cfg.rate_hz);
        let iq: &[Complex32] = if flush { &[] } else { s.chz.output(ch.head).unwrap_or(&[]) };
        let tk = self.tdma_keys.get(ch.system as usize).copied().flatten();
        self.tout.clear();
        Self::run_channel(ch, iq, rate, src_rate, tk, &mut self.groups, &mut self.tout, flush);
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
        if let Voice::Fdma { bank, .. } = &ch.voice {
            if let Some(off) = bank.offset_hz() {
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
        let vocoder = r.vocoder;
        let slot = if call.phase2_tdma || call.color_code.is_some() { call.tdma_slot as usize & 1 } else { 0 };
        let key = (self.system, call.freq_hz);
        if let Some(ch) = r.channels.get_mut(&key) {
            ch.calls[slot] = Some(call.id);
            return;
        }
        let s = &mut r.sources[src];
        let rate = s.chz.output_rate();
        let cutoff = if call.color_code.is_some() { dmr::CHANNEL_CUTOFF_HZ } else { CHANNEL_CUTOFF_HZ };
        let tune_ppm = s.tune_ppm;
        let (head, pre, start_sample) = s.chz.add_head(s.offset(call.freq_hz as f64), cutoff, r.preroll_s);
        let seed = call.freq_hz as u32;
        // The noise floor under the channel, from the source's spectrum: analog squelch, and reception.
        let mut prof = vec![0.0f64; 64];
        s.chz.noise_profile(&mut prof);
        let off = call.freq_hz as f64 - s.cfg.center_hz;
        let slice = (((off + s.cfg.rate_hz / 2.0) / s.cfg.rate_hz * 64.0) as usize).min(63);
        let noise = s.chz.noise_in_band(prof[slice].max(1e-30), ChannelFilter::noise_bandwidth());
        let voice = if call.analog {
            Voice::Analog { fm: Nbfm::new(rate), open: (noise * 10f64.powf(ANALOG_SQUELCH_DB / 10.0)) as f32, ids: Signalling::default() }
        } else if call.color_code.is_some() {
            Voice::Dmr { rx: C4fm::dmr(rate), voice: Box::new(DmrVoice::new(seed)), syms: Vec::new() }
        } else if call.phase2_tdma {
            let mut tracker = TdmaTracker::new(seed);
            tracker.soft = self.bank.soft;
            // Decision-feedback differential detection: ~1 dB in noise on Phase 2
            // voice (tool snr); not used on Phase 1, where simulcast didn't like it.
            let rx = Cqpsk::new(rate, cqpsk::Options { baud: phase2::SYMBOL_RATE, df_beta: 0.5, ..Default::default() });
            Voice::Tdma { rx, framer: phase2::Framer::default(), tracker, syms: Vec::new(), pkts: Vec::new() }
        } else {
            Voice::Fdma { bank: Bank::new(rate, self.bank), tracker: VoiceTracker::new(mbe::lcg(seed), vocoder) }
        };
        let mut calls = [None, None];
        calls[slot] = Some(call.id);
        let mut ch = Channel { system: self.system, source: src, head, start_sample, calls, voice, freq_hz: call.freq_hz as f64, tune_ppm, meter: ChannelFilter::new(rate), noise };
        // Pre-roll: decode the replayed air now.
        let tk = r.tdma_keys.get(self.system as usize).copied().flatten();
        r.tout.clear();
        Radio::run_channel(&mut ch, &pre, rate, s.cfg.rate_hz, tk, &mut r.groups, &mut r.tout, false);
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
    parser: TsbkParser,
    cc_source: usize,
    cc_head: Option<HeadId>,
    cc_bank: Bank,
    /// SmartNet control channel receiver (instead of `cc_bank`).
    cc_sn: Option<smartnet::ControlChannel>,
    cc_index: usize,
    cc_hz: Option<u64>,
    cc_start_s: f64,
    cc_samples: u64,
    last_good_s: f64,
    now_s: f64,
    good: u64,
    bad: u64,
    concluded: u64,
    identity: Identity,
    nac_votes: HashMap<u16, u32>,
    /// Why this control channel is not ours, while it isn't.
    mismatch: Option<String>,
    adjacent: std::collections::BTreeMap<(u32, u32), AdjacentSite>,
    /// Its radios' talker aliases.
    units: UnitAliases,
    /// Trunked DMR: the site and where its carriers are.
    dmr: Option<DmrWatch>,
    /// AutoTune: the correction its control channel was opened with, ppm;
    /// when it was last measured, and last reopened.
    cc_ppm: f64,
    cc_measured_s: f64,
    cc_retuned_s: f64,
}

/// A DMR site's carriers: each on (source, head, first sample of the head).
struct DmrWatch {
    site: dmr::Site,
    heads: Vec<(usize, HeadId, u64)>,
}

impl Trunk {
    fn host<'a>(&self, radio: &'a mut Radio) -> SysHost<'a> {
        SysHost { radio, system: self.idx, bank: self.cfg.bank }
    }

    /// The system this is a site of, and which site: by the configured
    /// group, else by what the control channel says (DMR: only by name).
    fn site_key(&self) -> Option<SiteKey> {
        let id = &self.identity;
        let group = if !self.cfg.site_group.is_empty() {
            format!("group:{}", self.cfg.site_group)
        } else if self.dmr.is_some() {
            return None;
        } else if self.cc_sn.is_some() {
            format!("smartnet:{:x}", id.sys_id?)
        } else {
            format!("p25:{:x}.{:x}", id.wacn?, id.sys_id?)
        };
        Some(SiteKey { group, site: id.site.map(|s| (id.rfss.unwrap_or(0), s)), cc_hz: self.cc_hz })
    }

    /// The talkgroup file names this site as the one to keep `tg`'s calls from.
    fn preferred_for(&self, tg: &super::talkgroups::Talkgroup) -> bool {
        let id = &self.identity;
        let by_name = !tg.preferred_site.is_empty() && tg.preferred_site.eq_ignore_ascii_case(&self.cfg.short_name);
        let n = tg.preferred_nac;
        let by_number = n != 0 && (id.nac.is_some_and(|x| x as u32 == n) || id.site.is_some_and(|s| id.rfss.unwrap_or(0) * 10000 + s == n));
        by_name || by_number
    }

    fn tune(&mut self, radio: &mut Radio, index: usize, events: &mut Vec<Event>) -> Result<(), String> {
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
        let cutoff = if self.cfg.smartnet.is_some() { smartnet::CHANNEL_CUTOFF_HZ } else { CHANNEL_CUTOFF_HZ };
        let (head, _, _) = s.chz.add_head(s.offset(hz), cutoff, 0.0);
        self.cc_ppm = s.tune_ppm;
        self.cc_measured_s = self.now_s;
        self.cc_head = Some(head);
        self.cc_hz = Some(hz.round() as u64);
        self.cc_start_s = self.now_s;
        self.cc_samples = 0;
        self.last_good_s = self.now_s;
        self.cc_bank = Bank::new(s.chz.output_rate(), self.cfg.bank);
        if let Some(sn) = &self.cfg.smartnet {
            let mut p = smartnet::Parser::new(sn.bandplan.clone());
            p.analog_default = sn.analog_default;
            self.cc_sn = Some(smartnet::ControlChannel::new(s.chz.output_rate(), p));
        }
        if retune {
            // Another control channel may be another site: learn it afresh.
            self.identity = Identity::default();
            self.nac_votes.clear();
            self.mismatch = None;
            self.adjacent.clear();
        }
        events.push(Event::ControlChannel { system: self.idx, freq_hz: hz.round() as u64 });
        Ok(())
    }

    /// Watch every carrier of a DMR site: heads on the sources that cover them.
    fn watch_dmr(&mut self, radio: &mut Radio, dc: &DmrConfig, events: &mut Vec<Event>) -> Result<(), String> {
        let rate = radio.sources[0].chz.output_rate();
        let site = dmr::Site::new(&self.cfg.control_channels, rate, dc.clone());
        let mut heads = Vec::new();
        let mut outside = Vec::new();
        for c in &site.carriers {
            let Some(src) = radio.source_for(c.hz as f64) else {
                outside.push(format!("{:.5}", c.hz as f64 / 1e6));
                continue;
            };
            let s = &mut radio.sources[src];
            let (head, _, start) = s.chz.add_head(c.hz as f64 - s.cfg.center_hz, dmr::CHANNEL_CUTOFF_HZ, 0.0);
            heads.push((src, head, start));
        }
        if !outside.is_empty() {
            return Err(format!("{}: DMR frequencies outside every source's bandwidth: {} MHz — move a center frequency.", self.cfg.short_name, outside.join(", ")));
        }
        self.cc_source = heads[0].0;
        self.cc_head = Some(heads[0].1);
        self.dmr = Some(DmrWatch { site, heads });
        events.push(Event::Note { system: self.idx, text: format!("Watching {} DMR frequencies", self.cfg.control_channels.len() + dc.channels.len()) });
        Ok(())
    }

    /// A block ran on `source`: the DMR site's carriers on it.
    fn on_block_dmr(&mut self, radio: &mut Radio, source: usize, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        let Some(DmrWatch { site, heads }) = self.dmr.as_mut() else { return };
        if !heads.iter().any(|h| h.0 == source) {
            return;
        }
        let s = &radio.sources[source];
        let (rate, fs) = (s.chz.output_rate(), s.cfg.rate_hz);
        let mut msgs = Vec::new();
        for (i, &(src, head, start)) in heads.iter().enumerate() {
            if src != source {
                continue;
            }
            let iq = radio.sources[src].chz.output(head).map(|v| v.to_vec()).unwrap_or_default();
            site.push(i, &iq, start as f64 / fs, rate, &mut msgs);
        }
        let c = &radio.sources[source].chz;
        self.now_s = self.now_s.max(c.sample_position() as f64 / c.fs());
        for text in site.take_notes() {
            events.push(Event::Note { system: self.idx, text });
        }
        let cc = site.control_hz();
        if let Some(hz) = cc.filter(|&h| Some(h) != self.cc_hz) {
            self.cc_hz = Some(hz);
            events.push(Event::ControlChannel { system: self.idx, freq_hz: hz });
        }
        // Blocks (CSBKs, link control) decoded and lost, on every carrier.
        self.good = site.carriers.iter().flat_map(|c| c.chan.slots.iter()).map(|s| s.good_blocks).sum();
        self.bad = site.carriers.iter().flat_map(|c| c.chan.slots.iter()).map(|s| s.bad_blocks).sum();
        if cc.is_some() || !msgs.is_empty() {
            self.last_good_s = self.now_s;
        }
        events.extend(msgs.iter().cloned().map(|msg| Event::Message { system: self.idx, msg }));
        let mut host = self.host(radio);
        self.calls.handle(&msgs, &mut host, call_events);
        self.calls.tick(self.now_s, &mut host, call_events);
    }

    /// A block ran on the control channel's source.
    fn on_block(&mut self, radio: &mut Radio, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        let Some(head) = self.cc_head else { return };
        let src = self.cc_source;
        let rate = radio.sources[src].chz.output_rate();
        let iq = radio.sources[src].chz.output(head).map(|v| v.to_vec()).unwrap_or_default();
        self.cc_samples += iq.len() as u64;
        self.now_s = self.cc_start_s + self.cc_samples as f64 / rate;
        if self.cc_sn.is_some() {
            self.on_smartnet(radio, &iq, events, call_events);
        } else {
            let mut groups = Vec::new();
            self.cc_bank.push(&iq, &mut groups);
            self.on_groups(radio, groups, events, call_events);
        }
        if self.now_s - self.last_good_s > CC_HUNT_S && self.cfg.control_channels.len() > 1 {
            let _ = self.tune(radio, self.cc_index + 1, events);
        } else if self.now_s - self.cc_measured_s >= TUNE_EVERY_S {
            self.measure(radio);
        }
        let mut host = self.host(radio);
        self.calls.tick(self.now_s, &mut host, call_events);
    }

    /// AutoTune: how far off the control channel comes in (while it decodes),
    /// into its source's average; a P25 control channel well off the
    /// correction is reopened at it.
    fn measure(&mut self, radio: &mut Radio) {
        self.cc_measured_s = self.now_s;
        let (Some(hz), Some(head)) = (self.cc_hz, self.cc_head) else { return };
        if self.now_s - self.last_good_s > 1.0 {
            return;
        }
        let off = match &self.cc_sn {
            Some(cc) => Some(cc.offset_hz()),
            None => self.cc_bank.offset_hz(),
        };
        let Some(off) = off else { return };
        let hz = hz as f64;
        let s = &mut radio.sources[self.cc_source];
        s.measured(self.cc_ppm + off as f64 / hz * 1e6);
        let off_by = (s.tune_ppm - self.cc_ppm) * 1e-6 * hz;
        if self.cc_sn.is_none() && off_by.abs() > TUNE_RETUNE_HZ && self.now_s - self.cc_retuned_s >= TUNE_RETUNE_S {
            s.chz.remove_head(head);
            let (head, _, _) = s.chz.add_head(s.offset(hz), CHANNEL_CUTOFF_HZ, 0.0);
            self.cc_head = Some(head);
            self.cc_ppm = s.tune_ppm;
            self.cc_retuned_s = self.now_s;
            self.cc_start_s = self.now_s;
            self.cc_samples = 0;
            self.cc_bank = Bank::new(s.chz.output_rate(), self.cfg.bank);
        }
    }

    /// SmartNet: OSWs → messages. The identity is the System ID (and site,
    /// when OBT sends it); `expect` holds the system to them as for P25.
    fn on_smartnet(&mut self, radio: &mut Radio, iq: &[Complex32], events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        let Some(cc) = self.cc_sn.as_mut() else { return };
        let (good0, bad0) = cc.counts();
        let mut msgs = Vec::new();
        cc.push(iq, self.cc_start_s, &mut msgs);
        let (good, bad) = cc.counts();
        self.good += good - good0;
        self.bad += bad - bad0;
        self.identity.sys_id = cc.parser.sys_id;
        self.identity.site = cc.parser.site;
        let conflict = self.identity.conflict(&self.cfg.expect);
        if conflict != self.mismatch {
            if let Some(c) = &conflict {
                let hz = self.cc_hz.unwrap_or(0) as f64 / 1e6;
                events.push(Event::Note { system: self.idx, text: format!("Control channel {hz:.5} MHz is not this system: {c}") });
            }
            self.mismatch = conflict;
        }
        if good > good0 && self.mismatch.is_none() {
            self.last_good_s = self.now_s;
        }
        if msgs.is_empty() {
            return;
        }
        events.extend(msgs.iter().cloned().map(|msg| Event::Message { system: self.idx, msg }));
        if self.mismatch.is_some() || !self.identity.confirms(&self.cfg.expect) {
            return;
        }
        let mut host = self.host(radio);
        self.calls.handle(&msgs, &mut host, call_events);
    }

    fn on_groups(&mut self, radio: &mut Radio, groups: Vec<Group>, events: &mut Vec<Event>, call_events: &mut Vec<CallEvent>) {
        let rate = radio.sources[self.cc_source].chz.output_rate();
        for g in &groups {
            let f = best_frame(g);
            *self.nac_votes.entry(f.nid.nac).or_default() += 1;
            self.identity.nac = self.nac_votes.iter().max_by_key(|(_, &c)| c).map(|(&n, _)| n);
            if f.nid.duid != TSDU {
                continue;
            }
            let (blocks, missing) = best_tsbks(g);
            self.bad += missing as u64;
            let t = self.cc_start_s + f.sample / rate;
            for blk in &blocks {
                self.good += 1;
                let msgs = self.parser.parse(blk, f.nid.nac, t);
                for m in &msgs {
                    match m.kind {
                        MessageType::Status => {
                            self.identity.wacn = Some(m.wacn);
                            self.identity.sys_id = Some(m.sys_id);
                        }
                        MessageType::SysId => {
                            self.identity.sys_id = Some(m.sys_id);
                            self.identity.rfss = Some(m.rfss);
                            self.identity.site = Some(m.site);
                        }
                        MessageType::Adjacent if m.freq_hz > 0 => {
                            self.adjacent.insert((m.rfss, m.site), AdjacentSite { sys_id: m.sys_id, rfss: m.rfss, site: m.site, freq_hz: m.freq_hz });
                        }
                        _ => {}
                    }
                }
                let conflict = self.identity.conflict(&self.cfg.expect);
                if conflict != self.mismatch {
                    if let Some(c) = &conflict {
                        let hz = self.cc_hz.unwrap_or(0) as f64 / 1e6;
                        events.push(Event::Note { system: self.idx, text: format!("Control channel {hz:.5} MHz is not this system: {c}") });
                    }
                    self.mismatch = conflict;
                }
                events.extend(msgs.iter().cloned().map(|msg| Event::Message { system: self.idx, msg }));
                if self.mismatch.is_some() {
                    // Another system's (or site's) grants: not ours to follow — and
                    // it doesn't count as a good control channel, so we hunt on.
                    continue;
                }
                self.last_good_s = self.now_s;
                if !self.identity.confirms(&self.cfg.expect) {
                    // Not yet known to be ours (the site comes every few seconds):
                    // hold the grants — a call still going is granted again.
                    continue;
                }
                if let (Some(nac), Some(sys), Some(wacn)) = (self.identity.nac, self.identity.sys_id, self.identity.wacn) {
                    radio.tdma_keys[self.idx as usize] = Some((nac as u32, sys, wacn));
                }
                let mut host = self.host(radio);
                self.calls.handle(&msgs, &mut host, call_events);
            }
        }
    }

    fn status(&self, radio: &Radio) -> SystemStatus {
        let (q, c) = self.cc_bank.frames_per_rx().iter().enumerate().fold((0, 0), |(q, c), (i, &n)| if i < 2 { (q + n, c) } else { (q, c + n) });
        SystemStatus {
            short_name: self.cfg.short_name.clone(),
            now_s: self.now_s,
            control_channel_hz: self.cc_hz,
            identity: self.identity.clone(),
            good: self.good,
            bad: self.bad,
            modulation: if let Some(DmrWatch { site, .. }) = &self.dmr { site.variant.map_or("DMR", |v| v.name()) } else if self.cc_sn.is_some() { "2FSK" } else if q + c < 8 { "" } else if q >= c { "CQPSK" } else { "C4FM" },
            active_calls: self.calls.calls.len(),
            recording: self.calls.calls.iter().filter(|c| radio.recordings.contains_key(&c.id)).count(),
            calls_concluded: self.concluded,
            mismatch: self.mismatch.clone(),
            adjacent: self.adjacent.values().copied().collect(),
            patches: self.calls.patches.active(),
            dmr: self.dmr.as_ref().map(|w| w.site.status()),
            site_group: self.site_key().map(|k| k.group),
        }
    }
}

pub struct Engine {
    cfg: EngineConfig,
    radio: Radio,
    trunks: Vec<Trunk>,
    conv: Conventional,
    /// Conventional channels' ids and talkgroup names (their calls live in `conv`).
    conv_calls: CallManager,
    conv_out: Vec<ConvOut>,
    conv_concluded: u64,
    /// Conventional channels' radios' talker aliases (unless a trunked system has their short name: then its).
    conv_units: UnitAliases,
    /// Copies of one call on several sites.
    multisite: MultiSite,
    now_s: f64,
    events: Vec<Event>,
    call_events: Vec<CallEvent>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Result<Self, String> {
        if cfg.sources.is_empty() {
            return Err("no sources configured".into());
        }
        let trunked: Vec<&SystemConfig> = cfg.systems.iter().filter(|s| !s.control_channels.is_empty()).collect();
        if trunked.is_empty() && cfg.conventional.is_empty() {
            return Err("Add a control channel or a conventional channel.".into());
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(d) = cfg.systems.iter().find(|s| !seen.insert(s.short_name.as_str())) {
            return Err(format!("Two systems are named \"{}\" — each needs its own short name.", d.short_name));
        }
        let history = cfg.preroll_s.max(cfg.conv.preroll_s).max(0.1);
        let spans: Vec<(f64, f64)> = cfg.sources.iter().map(|s| (s.center_hz, s.rate_hz)).collect();
        let conv = Conventional::new(&cfg.conventional, &spans, ConvConfig { vocoder: cfg.vocoder, ..cfg.conv }, cfg.bank, USABLE)?;
        let sources: Vec<Source> =
            cfg.sources.iter().map(|s| Source { cfg: s.clone(), chz: Channelizer::new(s.rate_hz, MIN_CHANNEL_RATE, history), errors: VecDeque::new(), tune_ppm: 0.0 }).collect();
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
            groups: Vec::new(),
            tout: Vec::new(),
            tdma_keys: vec![None; cfg.systems.len()],
            pending: Vec::new(),
        };
        let mut events = Vec::new();
        let mut trunks = Vec::new();
        for (i, sc) in cfg.systems.iter().enumerate() {
            let mut t = Trunk {
                idx: i as u16,
                calls: CallManager::with_ids(sc.calls, sc.talkgroups.clone(), i as u16, ids.clone()),
                parser: TsbkParser::default(),
                cc_source: 0,
                cc_head: None,
                cc_bank: Bank::new(rate, sc.bank),
                cc_sn: None,
                cc_index: 0,
                cc_hz: None,
                cc_start_s: 0.0,
                cc_samples: 0,
                last_good_s: 0.0,
                now_s: 0.0,
                good: 0,
                bad: 0,
                concluded: 0,
                identity: Identity::default(),
                nac_votes: HashMap::new(),
                mismatch: None,
                adjacent: Default::default(),
                units: UnitAliases::default(),
                dmr: None,
                cc_ppm: 0.0,
                cc_measured_s: 0.0,
                cc_retuned_s: 0.0,
                cfg: sc.clone(),
            };
            t.calls.patches.hold_s = if sc.smartnet.is_some() { patches::SMARTNET_HOLD_S } else { patches::P25_HOLD_S };
            if let Some(dc) = &sc.dmr {
                if !sc.control_channels.is_empty() || !dc.channels.is_empty() {
                    t.watch_dmr(&mut radio, dc, &mut events)?;
                }
            } else if !sc.control_channels.is_empty() {
                t.tune(&mut radio, 0, &mut events)?;
            }
            trunks.push(t);
        }
        let conv_calls = CallManager::with_ids(cfg.calls, cfg.conv_talkgroups.clone(), CONVENTIONAL, ids);
        Ok(Engine {
            radio,
            trunks,
            conv,
            conv_calls,
            conv_out: Vec::new(),
            conv_concluded: 0,
            conv_units: UnitAliases::default(),
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
            match t.dmr.as_mut() {
                Some(w) => w.site.map_from_str(s),
                None => t.parser.bandplan_from_str(s),
            }
        }
    }
    /// A system's band plan to save: P25's IDEN tables; DMR's logical channel → frequency table.
    pub fn bandplan(&self, system: usize) -> String {
        self.trunks.get(system).map_or_else(String::new, |t| match &t.dmr {
            Some(w) => w.site.map_to_string(),
            None => t.parser.bandplan_to_string(),
        })
    }

    /// The trunked system conventional channels share talker aliases with:
    /// the one with their short name.
    fn conv_units_owner(&self) -> Option<usize> {
        self.trunks
            .iter()
            .position(|t| t.cfg.short_name == self.cfg.conv_short_name)
    }

    /// The talker alias table of `system` (a call's), [`CONVENTIONAL`] included.
    fn units(&self, system: u16) -> Option<&UnitAliases> {
        let i = if system == CONVENTIONAL {
            self.conv_units_owner()
        } else {
            Some(system as usize)
        };
        match i {
            Some(i) => self.trunks.get(i).map(|t| &t.units),
            None => Some(&self.conv_units),
        }
    }
    fn units_mut(&mut self, system: u16) -> Option<&mut UnitAliases> {
        let i = if system == CONVENTIONAL {
            self.conv_units_owner()
        } else {
            Some(system as usize)
        };
        match i {
            Some(i) => self.trunks.get_mut(i).map(|t| &mut t.units),
            None => Some(&mut self.conv_units),
        }
    }

    /// The short names that keep a talker alias table: each trunked system's,
    /// and the conventional channels' when they have their own.
    pub fn unit_table_names(&self) -> Vec<String> {
        let mut n: Vec<String> = self
            .trunks
            .iter()
            .map(|t| t.cfg.short_name.clone())
            .collect();
        if !self.cfg.conventional.is_empty() && self.conv_units_owner().is_none() {
            n.push(self.cfg.conv_short_name.clone());
        }
        n
    }
    /// Preload the talker aliases (Trunk Recorder's unitTagsOTA CSV) kept under `short_name`.
    pub fn load_units(&mut self, short_name: &str, csv: &str) {
        if let Some(i) = self
            .trunks
            .iter()
            .position(|t| t.cfg.short_name == short_name)
        {
            self.trunks[i].units = UnitAliases::parse_csv(csv);
        } else if short_name == self.cfg.conv_short_name {
            self.conv_units = UnitAliases::parse_csv(csv);
        }
    }
    /// (short name, CSV) of each talker alias table that learned something since the last call.
    pub fn units_changed(&mut self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .trunks
            .iter_mut()
            .filter_map(|t| {
                t.units
                    .take_changed()
                    .then(|| (t.cfg.short_name.clone(), t.units.to_csv()))
            })
            .collect();
        if self.conv_units.take_changed() {
            out.push((self.cfg.conv_short_name.clone(), self.conv_units.to_csv()));
        }
        out
    }
    /// A radio's talker alias on system `system` (a call's).
    pub fn unit_alias(&self, system: u16, unit: u32) -> Option<&str> {
        self.units(system)?.get(unit)
    }

    /// Note a talker alias heard on a call of `system`'s.
    fn learn_alias(&mut self, system: u16, a: Alias, call_tg: Option<u32>) {
        let tg = a.talkgroup.or(call_tg);
        let (now_s, wacn, sys_id) = match self.trunks.get(system as usize) {
            Some(t) => (t.now_s, t.identity.wacn, t.identity.sys_id),
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
        if self
            .units_mut(system)
            .is_some_and(|u| u.learn(a.unit, learned))
        {
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
        let systems: Vec<SystemStatus> = self.trunks.iter().filter(|t| t.cc_head.is_some()).map(|t| t.status(&self.radio)).collect();
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

    /// End of input: release what the receivers still hold and end every call.
    pub fn finish(&mut self) {
        for t in self.trunks.iter_mut() {
            let mut groups = Vec::new();
            t.cc_bank.flush(&mut groups);
            t.on_groups(&mut self.radio, groups, &mut self.events, &mut self.call_events);
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

    fn call_rules(&self) -> CallRules {
        CallRules { call_timeout_s: self.cfg.calls.call_timeout_s, record_encrypted: self.cfg.calls.record_encrypted, capture_frames: self.cfg.capture_frames }
    }

    /// Report what the conventional channels did.
    fn emit_conv(&mut self, mut out: Vec<ConvOut>) {
        for o in out.drain(..) {
            match o {
                ConvOut::Start(c) => self.events.push(Event::CallStart(c)),
                ConvOut::Update(c) => self.events.push(Event::CallUpdate(c)),
                ConvOut::Audio { call_id, talkgroup, samples } => self.events.push(Event::Audio { call_id, system: CONVENTIONAL, talkgroup, samples }),
                ConvOut::End { call, audio, frames, recorder_num, tx, reception } => {
                    self.write_call(Held { call: call.clone(), audio, frames, recorder_num, tx, reception, freq_error_hz: None });
                    self.events.push(Event::CallEnd(call));
                }
                ConvOut::Alias(a) => self.learn_alias(CONVENTIONAL, a, None),
                ConvOut::Skipped { freq_hz, code } => self.events.push(Event::ConvSkipped { freq_hz, code }),
            }
        }
        self.conv_out = out;
    }

    fn on_block(&mut self, source: usize) {
        // Voice channels on this source.
        let keys: Vec<(u16, u64)> = self.radio.channels.iter().filter(|(_, c)| c.source == source).map(|(&k, _)| k).collect();
        for k in keys {
            self.radio.run_head(k, false);
        }
        // The control channels on it.
        for i in 0..self.trunks.len() {
            if self.trunks[i].dmr.is_some() {
                self.trunks[i].on_block_dmr(&mut self.radio, source, &mut self.events, &mut self.call_events);
                self.apply_pending();
            } else if self.trunks[i].cc_head.is_some() && self.trunks[i].cc_source == source {
                self.trunks[i].on_block(&mut self.radio, &mut self.events, &mut self.call_events);
                self.apply_pending();
            }
        }
        if source == 0 {
            let c = &self.radio.sources[0].chz;
            self.now_s = c.sample_position() as f64 / c.fs();
        }
        if !self.conv.is_empty() {
            let c = &self.radio.sources[source].chz;
            let t = c.sample_position() as f64 / c.fs();
            let rules = self.call_rules();
            let mut out = std::mem::take(&mut self.conv_out);
            self.conv.on_block(source, &mut self.radio.sources[source].chz, t, &mut self.conv_calls, &rules, &mut out);
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

    /// Multi-site: the copies of a call just granted on `c`'s site — the
    /// same talkgroup on another site of its system, granted at about the same time.
    fn link_twins(&mut self, c: &Call) {
        if !self.cfg.drop_duplicates {
            return;
        }
        let Some(key) = self.trunks.get(c.system as usize).and_then(Trunk::site_key) else { return };
        let twins: Vec<(CallId, u16)> = self
            .trunks
            .iter()
            .filter(|t| t.idx != c.system && t.site_key().is_some_and(|k| k.twin(&key)))
            .flat_map(|t| t.calls.calls.iter())
            .filter(|o| o.talkgroup == c.talkgroup && (o.start_s - c.start_s).abs() <= multisite::TWIN_WINDOW_S)
            .map(|o| (o.id, o.system))
            .collect();
        self.multisite.link(c.id, c.system, &twins);
    }

    /// The other sites' copies of call `id` still going or held, as (call, system).
    pub fn twins(&self, id: CallId) -> Vec<(CallId, u16)> {
        self.multisite.twins(id)
    }

    fn conclude(&mut self, call: &Call) {
        let held = self.radio.recordings.remove(&call.id).map(|rec| {
            self.radio.free_nums.push(rec.recorder_num);
            Held { call: call.clone(), audio: rec.audio, frames: rec.frames, recorder_num: rec.recorder_num, tx: rec.tx, reception: rec.reception, freq_error_hz: (rec.freq_error.1 > 0).then(|| rec.freq_error.0 / rec.freq_error.1 as f64) }
        });
        let mut copies = match self.multisite.end(call.id, held) {
            Ended::Alone(None) | Ended::Waiting => return,
            Ended::Alone(Some(h)) => vec![h],
            Ended::Done(v) => v,
        };
        // The talkgroup's preferred site, by any site's talkgroup file.
        let prefs: Vec<&super::talkgroups::Talkgroup> = copies.iter().filter_map(|h| h.call.talkgroup_info.as_ref()).collect();
        let preferred: Vec<bool> =
            copies.iter().map(|h| self.trunks.get(h.call.system as usize).is_some_and(|t| prefs.iter().any(|tg| t.preferred_for(tg)))).collect();
        let Some(k) = multisite::pick(&copies, &preferred) else { return };
        let kept = copies.swap_remove(k);
        let kept_call = kept.call.clone();
        if self.write_call(kept) {
            for h in copies {
                self.events.push(Event::Duplicate { call: h.call, kept: kept_call.clone() });
            }
        }
    }

    /// A finished call's record and audio, as [`Event::Concluded`]; false
    /// when not kept (silent, or shorter than its system's minimum).
    fn write_call(&mut self, h: Held) -> bool {
        let Held { call, audio, frames, recorder_num, tx, reception, freq_error_hz } = h;
        let call = &call;
        let rules = self.trunks.get(call.system as usize).map_or(self.cfg.conv_save, |t| t.cfg.save);
        // An encrypted call's "audio" is at most a few frames vocoded before
        // the cipher was known: noise. Trunk Recorder keeps none either.
        let mut audio = if call.encrypted { Vec::new() } else { audio };
        tx.drop_short(&mut audio, rules.min_transmission_s, mbe::SAMPLE_RATE);
        let short = (audio.len() as f64) < rules.min_call_s * mbe::SAMPLE_RATE as f64 && !audio.is_empty();
        if (audio.is_empty() && !rules.keep_silent) || short {
            self.events.push(Event::NotSaved(call.clone()));
            return false;
        }
        if rules.normalize {
            loudness::normalize(&mut audio, mbe::SAMPLE_RATE);
        }
        let gain_db = if call.analog { rules.analog_gain_db } else { rules.digital_gain_db };
        if gain_db != 0.0 {
            let g = 10f32.powf(gain_db / 20.0);
            audio.iter_mut().for_each(|x| *x = (*x * g).clamp(-1.0, 1.0));
        }
        let short_name = match self.trunks.get_mut(call.system as usize) {
            Some(t) => {
                t.concluded += 1;
                t.cfg.short_name.clone()
            }
            None => {
                self.conv_concluded += 1;
                self.cfg.conv_short_name.clone()
            }
        };
        let (json, base_name) = call_record(
            call,
            &ConcludeInfo {
                short_name: &short_name,
                epoch_ms_at_zero: self.cfg.epoch_ms_at_zero,
                audio_seconds: audio.len() as f64 / mbe::SAMPLE_RATE as f64,
                errors: &frames.errors,
                recorder_num,
                end_s: call.last_audio_s,
                units: self.units(call.system),
                unit_tags: Some(self.trunks.get(call.system as usize).map_or(&self.cfg.conv_unit_tags, |t| &t.cfg.unit_tags)).filter(|t| !t.is_empty() || t.mode != Default::default()),
                reception,
                freq_error_hz: freq_error_hz.map_or(0, |e| e.round() as i32),
            },
        );
        let frames = frames.captured.filter(|_| !audio.is_empty()).map(|f| frames_jsonl(&f));
        self.events.push(Event::Concluded(Concluded { call: call.clone(), json, short_name, base_name, audio, frames }));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_conflict_and_confirmation() {
        let expect = Identity { nac: Some(0x443), site: Some(4), ..Default::default() };
        let mut heard = Identity { nac: Some(0x443), ..Default::default() };
        assert_eq!(heard.conflict(&expect), None);
        assert!(!heard.confirms(&expect), "site not heard yet");
        heard.site = Some(3);
        heard.rfss = Some(1);
        assert_eq!(heard.conflict(&expect).as_deref(), Some("site 3 (expected 4)"));
        heard.site = Some(4);
        assert!(heard.confirms(&expect) && heard.conflict(&expect).is_none());
        heard.nac = Some(0x1a);
        assert_eq!(heard.conflict(&expect).as_deref(), Some("NAC 1A (expected 443)"));
        assert!(Identity::default().confirms(&Identity::default()));
    }

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
