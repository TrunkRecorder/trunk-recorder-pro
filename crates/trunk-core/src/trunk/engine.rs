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
//! system is a system of its own (see [`SystemConfig::expect`]); calls heard
//! on two sites are recorded by both.
//!
//! Conventional channels (analog FM, P25) ride the same channelizers: see
//! [`super::conventional`]. A config may have trunked systems, conventional
//! channels, or both. Everything that happens is reported as [`Event`]s; the
//! engine does no I/O.

use std::collections::HashMap;

use num_complex::Complex32;

use super::calls::{Call, CallConfig, CallEvent, CallId, CallIds, CallManager, Reason, RecorderHost, CONVENTIONAL};
use super::conventional::{CallRules, ConvChannel, ConvConfig, ConvOut, Conventional};
use super::message::{Message, MessageType, TsbkParser};
use super::patches;
use super::record::{call_record, ConcludeInfo};
use super::talkgroups::Talkgroups;
use super::tdma::TdmaTracker;
use super::frames::{frames_jsonl, CallFrames};
use super::tracker::{TrackerOut, VoiceTracker};
use super::units::{UnitAlias, UnitAliases};
use crate::dsp::cqpsk::{self, Cqpsk};
use crate::dsp::{Channelizer, HeadId, Receiver, Symbol};
use crate::mbe;
use crate::p25::alias::Alias;
use crate::p25::diversity::{best_frame, best_tsbks, Bank, BankConfig, Group};
use crate::p25::frame::TSDU;
use crate::p25::phase2::{self, Packet};
use crate::dsp::fm::{ChannelFilter, Nbfm};
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

#[derive(Clone, Debug)]
pub struct SourceConfig {
    pub center_hz: f64,
    pub rate_hz: f64,
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
    /// Keep calls with no decoded audio (encrypted, lost).
    pub keep_silent_calls: bool,
    /// Conventional channels' call rules (timeout, encrypted).
    pub calls: CallConfig,
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
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            systems: vec![],
            sources: vec![],
            preroll_s: 1.0,
            max_recorders: 32,
            keep_silent_calls: false,
            calls: CallConfig::default(),
            epoch_ms_at_zero: 0.0,
            bank: BankConfig::default(),
            conventional: vec![],
            conv: ConvConfig::default(),
            conv_short_name: "conv".into(),
            conv_talkgroups: Talkgroups::default(),
            capture_frames: false,
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
}

struct Source {
    cfg: SourceConfig,
    chz: Channelizer,
}

enum Voice {
    /// Phase 1: receiver bank → IMBE tracker.
    Fdma { bank: Bank, tracker: VoiceTracker },
    /// Phase 2: H-DQPSK receiver → slot framer → TDMA tracker (both slots).
    Tdma { rx: Cqpsk, framer: phase2::Framer, tracker: TdmaTracker, syms: Vec<Symbol>, pkts: Vec<Packet> },
    /// Analog FM (SmartNet analog grants), squelched at `open` carrier power.
    Analog { fm: Nbfm, open: f32 },
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
}

struct Recording {
    audio: Vec<f32>,
    frames: CallFrames,
    recorder_num: u32,
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
            Voice::Analog { fm, open } => {
                let mut audio = Vec::new();
                fm.push(iq, *open, &mut audio);
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
        Self::route(ch, &mut self.tout, &mut self.pending);
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
        let slot = if call.phase2_tdma || call.color_code.is_some() { call.tdma_slot as usize & 1 } else { 0 };
        let key = (self.system, call.freq_hz);
        if let Some(ch) = r.channels.get_mut(&key) {
            ch.calls[slot] = Some(call.id);
            return;
        }
        let s = &mut r.sources[src];
        let rate = s.chz.output_rate();
        let cutoff = if call.color_code.is_some() { dmr::CHANNEL_CUTOFF_HZ } else { CHANNEL_CUTOFF_HZ };
        let (head, pre, start_sample) = s.chz.add_head(call.freq_hz as f64 - s.cfg.center_hz, cutoff, r.preroll_s);
        let seed = call.freq_hz as u32;
        let voice = if call.analog {
            // Squelch: the noise floor under the channel, from the source's spectrum.
            let mut prof = vec![0.0f64; 64];
            s.chz.noise_profile(&mut prof);
            let off = call.freq_hz as f64 - s.cfg.center_hz;
            let slice = (((off + s.cfg.rate_hz / 2.0) / s.cfg.rate_hz * 64.0) as usize).min(63);
            let noise = s.chz.noise_in_band(prof[slice].max(1e-30), ChannelFilter::noise_bandwidth());
            Voice::Analog { fm: Nbfm::new(rate), open: (noise * 10f64.powf(ANALOG_SQUELCH_DB / 10.0)) as f32 }
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
            Voice::Fdma { bank: Bank::new(rate, self.bank), tracker: VoiceTracker::new(mbe::lcg(seed)) }
        };
        let mut calls = [None, None];
        calls[slot] = Some(call.id);
        let mut ch = Channel { system: self.system, source: src, head, start_sample, calls, voice };
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
        let (head, _, _) = s.chz.add_head(hz - s.cfg.center_hz, cutoff, 0.0);
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
        self.good += msgs.len() as u64;
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
        }
        let mut host = self.host(radio);
        self.calls.tick(self.now_s, &mut host, call_events);
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
        let conv = Conventional::new(&cfg.conventional, &spans, cfg.conv, cfg.bank, USABLE)?;
        let sources: Vec<Source> =
            cfg.sources.iter().map(|s| Source { cfg: s.clone(), chz: Channelizer::new(s.rate_hz, MIN_CHANNEL_RATE, history) }).collect();
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
                ConvOut::End { call, audio, frames, recorder_num } => {
                    self.write_call(&call, audio, frames, recorder_num);
                    self.events.push(Event::CallEnd(call));
                }
                ConvOut::Alias(a) => self.learn_alias(CONVENTIONAL, a, None),
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
                    rec.audio.extend_from_slice(&samples);
                    rec.frames.push(frame);
                    t.calls.note_audio(id, t.now_s);
                    let tg = t.calls.calls.iter().find(|c| c.id == id).map_or(0, |c| c.talkgroup);
                    self.events.push(Event::Audio { call_id: id, system: sys, talkgroup: tg, samples });
                }
                TrackerOut::AnalogAudio(samples) => {
                    let Some(rec) = self.radio.recordings.get_mut(&id) else { continue };
                    rec.audio.extend_from_slice(&samples);
                    t.calls.note_audio(id, t.now_s);
                    let tg = t.calls.calls.iter().find(|c| c.id == id).map_or(0, |c| c.talkgroup);
                    self.events.push(Event::Audio { call_id: id, system: sys, talkgroup: tg, samples });
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
                CallEvent::Start(c) => self.events.push(Event::CallStart(c)),
                CallEvent::Update(c) => self.events.push(Event::CallUpdate(c)),
                CallEvent::End(c) => {
                    self.conclude(&c);
                    self.events.push(Event::CallEnd(c));
                }
            }
        }
    }

    fn conclude(&mut self, call: &Call) {
        let Some(rec) = self.radio.recordings.remove(&call.id) else { return };
        self.radio.free_nums.push(rec.recorder_num);
        self.write_call(call, rec.audio, rec.frames, rec.recorder_num);
    }

    /// A finished call's record and audio, as [`Event::Concluded`].
    fn write_call(&mut self, call: &Call, audio: Vec<f32>, frames: CallFrames, recorder_num: u32) {
        // An encrypted call's "audio" is at most a few frames vocoded before
        // the cipher was known: noise. Trunk Recorder keeps none either.
        let audio = if call.encrypted { Vec::new() } else { audio };
        if audio.is_empty() && !self.cfg.keep_silent_calls {
            return;
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
            },
        );
        let frames = frames.captured.filter(|_| !audio.is_empty()).map(|f| frames_jsonl(&f));
        self.events.push(Event::Concluded(Concluded { call: call.clone(), json, short_name, base_name, audio, frames }));
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
        let src = vec![SourceConfig { center_hz: 851e6, rate_hz: 2.4e6 }];
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
}
