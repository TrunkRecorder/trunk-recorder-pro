//! One trunked system above the radio(s) — Trunk Recorder's
//! monitor_messages() loop plus its recorders:
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
//! Several sources (dongles) can feed one system: the control channel runs on
//! the source that covers it, each voice channel on the source that covers its
//! frequency. Time is the control channel's sample clock (the first source's
//! when there is no trunked system).
//!
//! Conventional channels (analog FM, P25) ride the same channelizers: see
//! [`super::conventional`]. A config may have a trunked system, conventional
//! channels, or both. Everything that happens is reported as [`Event`]s; the
//! engine does no I/O.

use std::collections::HashMap;

use num_complex::Complex32;

use super::calls::{Call, CallConfig, CallEvent, CallId, CallManager, Reason, RecorderHost};
use super::conventional::{CallRules, ConvChannel, ConvConfig, ConvOut, Conventional};
use super::message::{Message, MessageType, TsbkParser};
use super::record::{call_record, ConcludeInfo};
use super::talkgroups::Talkgroups;
use super::tdma::TdmaTracker;
use super::frames::{frames_jsonl, CallFrames};
use super::tracker::{TrackerOut, VoiceTracker};
use crate::dsp::cqpsk::{self, Cqpsk};
use crate::dsp::{Channelizer, HeadId, Receiver, Symbol};
use crate::mbe;
use crate::p25::diversity::{best_frame, best_tsbks, Bank, BankConfig, Group};
use crate::p25::frame::TSDU;
use crate::p25::phase2::{self, Packet};
use crate::dsp::fm::{ChannelFilter, Nbfm};
use crate::smartnet::{self, Bandplan};

/// A SmartNet system (the control channels are SmartNet, not P25).
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

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub short_name: String,
    pub control_channels: Vec<f64>,
    pub sources: Vec<SourceConfig>,
    /// Seconds of air a voice channel replays from before its grant.
    pub preroll_s: f64,
    pub max_recorders: usize,
    /// Keep calls with no decoded audio (encrypted, lost).
    pub keep_silent_calls: bool,
    pub calls: CallConfig,
    /// Wall-clock epoch ms at sample-clock time 0.
    pub epoch_ms_at_zero: f64,
    pub bank: BankConfig,
    /// Conventional channels, energy-detected on whichever source covers them.
    pub conventional: Vec<ConvChannel>,
    pub conv: ConvConfig,
    /// Keep each call's vocoder frames ([`Concluded::frames`]).
    pub capture_frames: bool,
    /// SmartNet instead of P25 on the control channels.
    pub smartnet: Option<SmartnetConfig>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            short_name: "sys1".into(),
            control_channels: vec![],
            sources: vec![],
            preroll_s: 1.0,
            max_recorders: 32,
            keep_silent_calls: false,
            calls: CallConfig::default(),
            epoch_ms_at_zero: 0.0,
            bank: BankConfig::default(),
            conventional: vec![],
            conv: ConvConfig::default(),
            capture_frames: false,
            smartnet: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Concluded {
    pub call: Call,
    /// Trunk Recorder's call JSON.
    pub json: String,
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
    ControlChannel { freq_hz: u64 },
    Message(Message),
    CallStart(Call),
    CallUpdate(Call),
    CallEnd(Call),
    /// Live audio for a recording call.
    Audio { call_id: CallId, talkgroup: u32, samples: Vec<f32> },
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

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub now_s: f64,
    pub control_channel_hz: Option<u64>,
    pub identity: Identity,
    pub good: u64,
    pub bad: u64,
    pub modulation: &'static str,
    pub active_calls: usize,
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
}

impl Voice {
}

struct Channel {
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

/// Everything the call manager asks for (the RecorderHost), kept apart from
/// it so both can be borrowed at once.
struct Radio {
    sources: Vec<Source>,
    channels: HashMap<u64, Channel>,
    recordings: HashMap<CallId, Recording>,
    free_nums: Vec<u32>,
    next_num: u32,
    max_recorders: usize,
    preroll_s: f64,
    capture_frames: bool,
    bank_cfg: BankConfig,
    groups: Vec<Group>,
    tout: Vec<(usize, TrackerOut)>,
    /// Phase 2 scrambler seed (NAC, System ID, WACN), once the control channel gave it.
    tdma_key: Option<(u32, u32, u32)>,
    /// Audio / info produced inside start_recording (pre-roll), applied after.
    pending: Vec<(CallId, TrackerOut)>,
}

impl Radio {
    fn source_for(&self, hz: f64) -> Option<usize> {
        self.sources.iter().position(|s| (hz - s.cfg.center_hz).abs() <= s.cfg.rate_hz / 2.0 * USABLE)
    }

    /// Run a channel's receivers over `iq`, collecting what its tracker
    /// produced as (slot, output).
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
    fn route(ch: &Channel, tout: &mut Vec<(usize, TrackerOut)>, pending: &mut Vec<(CallId, TrackerOut)>) {
        for (slot, o) in tout.drain(..) {
            if let Some(id) = ch.calls[slot & 1] {
                pending.push((id, o));
            }
        }
    }
}

impl RecorderHost for Radio {
    fn start_recording(&mut self, call: &Call) -> Result<(), Reason> {
        let Some(src) = self.source_for(call.freq_hz as f64) else { return Err(Reason::NoSource) };
        if self.recordings.len() >= self.max_recorders {
            return Err(Reason::NoRecorder);
        }
        let recorder_num = self.free_nums.pop().unwrap_or_else(|| {
            self.next_num += 1;
            self.next_num - 1
        });
        self.recordings.insert(call.id, Recording { audio: Vec::new(), frames: CallFrames::new(self.capture_frames), recorder_num });
        let slot = if call.phase2_tdma { call.tdma_slot as usize & 1 } else { 0 };
        if let Some(ch) = self.channels.get_mut(&call.freq_hz) {
            // A newer call on the same channel (slot) takes it over, as in Trunk Recorder.
            ch.calls[slot] = Some(call.id);
            return Ok(());
        }
        let s = &mut self.sources[src];
        let rate = s.chz.output_rate();
        let (head, pre, start_sample) = s.chz.add_head(call.freq_hz as f64 - s.cfg.center_hz, CHANNEL_CUTOFF_HZ, self.preroll_s);
        let seed = call.freq_hz as u32;
        let voice = if call.analog {
            // Squelch: the noise floor under the channel, from the source's spectrum.
            let mut prof = vec![0.0f64; 64];
            s.chz.noise_profile(&mut prof);
            let off = call.freq_hz as f64 - s.cfg.center_hz;
            let slice = (((off + s.cfg.rate_hz / 2.0) / s.cfg.rate_hz * 64.0) as usize).min(63);
            let noise = s.chz.noise_in_band(prof[slice].max(1e-30), ChannelFilter::noise_bandwidth());
            Voice::Analog { fm: Nbfm::new(rate), open: (noise * 10f64.powf(ANALOG_SQUELCH_DB / 10.0)) as f32 }
        } else if call.phase2_tdma {
            let mut tracker = TdmaTracker::new(seed);
            tracker.soft = self.bank_cfg.soft;
            let rx = Cqpsk::new(rate, cqpsk::Options { baud: phase2::SYMBOL_RATE, ..Default::default() });
            Voice::Tdma { rx, framer: phase2::Framer::default(), tracker, syms: Vec::new(), pkts: Vec::new() }
        } else {
            Voice::Fdma { bank: Bank::new(rate, self.bank_cfg), tracker: VoiceTracker::new(mbe::lcg(seed)) }
        };
        let mut calls = [None, None];
        calls[slot] = Some(call.id);
        let mut ch = Channel { source: src, head, start_sample, calls, voice };
        // Pre-roll: decode the replayed air now.
        self.tout.clear();
        Self::run_channel(&mut ch, &pre, rate, s.cfg.rate_hz, self.tdma_key, &mut self.groups, &mut self.tout, false);
        Self::route(&ch, &mut self.tout, &mut self.pending);
        self.channels.insert(call.freq_hz, ch);
        Ok(())
    }

    fn stop_recording(&mut self, call: &Call) {
        let Some(ch) = self.channels.get_mut(&call.freq_hz) else { return };
        for c in ch.calls.iter_mut() {
            if *c == Some(call.id) {
                *c = None;
            }
        }
        if ch.calls.iter().all(Option::is_none) {
            let ch = self.channels.remove(&call.freq_hz).unwrap();
            self.sources[ch.source].chz.remove_head(ch.head);
        }
    }
}

pub struct Engine {
    cfg: EngineConfig,
    radio: Radio,
    calls: CallManager,
    conv: Conventional,
    conv_out: Vec<ConvOut>,
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
    events: Vec<Event>,
    call_events: Vec<CallEvent>,
}

impl Engine {
    pub fn new(cfg: EngineConfig, talkgroups: Talkgroups) -> Result<Self, String> {
        if cfg.sources.is_empty() {
            return Err("no sources configured".into());
        }
        if cfg.control_channels.is_empty() && cfg.conventional.is_empty() {
            return Err("Add a control channel or a conventional channel.".into());
        }
        let history = cfg.preroll_s.max(cfg.conv.preroll_s).max(0.1);
        let spans: Vec<(f64, f64)> = cfg.sources.iter().map(|s| (s.center_hz, s.rate_hz)).collect();
        let conv = Conventional::new(&cfg.conventional, &spans, cfg.conv, cfg.bank, USABLE)?;
        let sources: Vec<Source> =
            cfg.sources.iter().map(|s| Source { cfg: s.clone(), chz: Channelizer::new(s.rate_hz, MIN_CHANNEL_RATE, history) }).collect();
        let rate = sources[0].chz.output_rate();
        let mut e = Engine {
            radio: Radio {
                sources,
                channels: HashMap::new(),
                recordings: HashMap::new(),
                free_nums: Vec::new(),
                next_num: 0,
                max_recorders: cfg.max_recorders,
                preroll_s: cfg.preroll_s,
                capture_frames: cfg.capture_frames,
                bank_cfg: cfg.bank,
                groups: Vec::new(),
                tout: Vec::new(),
                tdma_key: None,
                pending: Vec::new(),
            },
            calls: CallManager::new(cfg.calls, talkgroups),
            conv,
            conv_out: Vec::new(),
            parser: TsbkParser::default(),
            cc_source: 0,
            cc_head: None,
            cc_bank: Bank::new(rate, cfg.bank),
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
            events: Vec::new(),
            call_events: Vec::new(),
            cfg,
        };
        if !e.cfg.control_channels.is_empty() {
            e.tune_control(0)?;
        }
        Ok(e)
    }

    /// Preload a band plan saved by [`Engine::bandplan`] (a grant heard before
    /// the next IDEN broadcast can then be followed at once).
    pub fn load_bandplan(&mut self, s: &str) {
        self.parser.bandplan_from_str(s);
    }
    pub fn bandplan(&self) -> String {
        self.parser.bandplan_to_string()
    }

    pub fn sources(&self) -> &[SourceConfig] {
        &self.cfg.sources
    }

    /// Calls in progress (recording or monitoring), trunked then conventional.
    pub fn active_calls(&self) -> Vec<Call> {
        self.calls.calls.iter().chain(self.conv.calls()).cloned().collect()
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
        let (q, c) = self.cc_bank.frames_per_rx().iter().enumerate().fold((0, 0), |(q, c), (i, &n)| if i < 2 { (q + n, c) } else { (q, c + n) });
        Status {
            now_s: self.now_s,
            control_channel_hz: self.cc_hz,
            identity: self.identity.clone(),
            good: self.good,
            bad: self.bad,
            modulation: if self.cfg.smartnet.is_some() { "2FSK" } else if q + c < 8 { "" } else if q >= c { "CQPSK" } else { "C4FM" },
            active_calls: self.calls.calls.len() + self.conv.calls().count(),
            // Trunked recorders only: conventional channels don't use the pool.
            recording: self.radio.recordings.len(),
            channels_open: self.radio.channels.len() + self.conv.open_count(),
            conventional_open: self.conv.open_count(),
            calls_concluded: self.concluded,
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
        let mut groups = Vec::new();
        self.cc_bank.flush(&mut groups);
        self.on_control_groups(groups);
        let freqs: Vec<u64> = self.radio.channels.keys().copied().collect();
        for f in freqs {
            let ch = self.radio.channels.get_mut(&f).unwrap();
            let src_rate = self.radio.sources[ch.source].cfg.rate_hz;
            let rate = self.radio.sources[ch.source].chz.output_rate();
            self.radio.tout.clear();
            Radio::run_channel(ch, &[], rate, src_rate, self.radio.tdma_key, &mut self.radio.groups, &mut self.radio.tout, true);
            Radio::route(ch, &mut self.radio.tout, &mut self.radio.pending);
        }
        self.apply_pending();
        self.calls.end_all(&mut self.radio, &mut self.call_events);
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
                ConvOut::Audio { call_id, talkgroup, samples } => self.events.push(Event::Audio { call_id, talkgroup, samples }),
                ConvOut::End { call, audio, frames, recorder_num } => {
                    self.write_call(&call, audio, frames, recorder_num);
                    self.events.push(Event::CallEnd(call));
                }
            }
        }
        self.conv_out = out;
    }

    fn tune_control(&mut self, index: usize) -> Result<(), String> {
        let list: Vec<(f64, usize)> =
            self.cfg.control_channels.iter().filter_map(|&f| self.radio.source_for(f).map(|s| (f, s))).collect();
        if list.is_empty() {
            return Err("No control channel falls inside a source's bandwidth — move the center frequency.".into());
        }
        self.cc_index = index % list.len();
        let (hz, src) = list[self.cc_index];
        if let Some(h) = self.cc_head.take() {
            self.radio.sources[self.cc_source].chz.remove_head(h);
        }
        self.cc_source = src;
        let s = &mut self.radio.sources[src];
        let cutoff = if self.cfg.smartnet.is_some() { smartnet::CHANNEL_CUTOFF_HZ } else { CHANNEL_CUTOFF_HZ };
        let (head, _, _) = s.chz.add_head(hz - s.cfg.center_hz, cutoff, 0.0);
        self.cc_head = Some(head);
        self.cc_hz = Some(hz.round() as u64);
        self.cc_start_s = self.now_s;
        self.cc_samples = 0;
        self.last_good_s = self.now_s;
        self.cc_bank = Bank::new(s.chz.output_rate(), self.cfg.bank);
        if let Some(sn) = &self.cfg.smartnet {
            // A new parser per channel (its queue is per stream); what it learned carries over.
            let mut p = smartnet::Parser::new(sn.bandplan.clone());
            p.analog_default = sn.analog_default;
            if let Some(old) = self.cc_sn.take() {
                p.sys_id = old.parser.sys_id;
                p.site = old.parser.site;
            }
            self.cc_sn = Some(smartnet::ControlChannel::new(s.chz.output_rate(), p));
        }
        self.events.push(Event::ControlChannel { freq_hz: hz.round() as u64 });
        Ok(())
    }

    fn on_block(&mut self, source: usize) {
        // Voice channels on this source.
        let src_rate = self.radio.sources[source].cfg.rate_hz;
        let rate = self.radio.sources[source].chz.output_rate();
        let freqs: Vec<u64> = self.radio.channels.iter().filter(|(_, c)| c.source == source).map(|(&f, _)| f).collect();
        for f in freqs {
            let radio = &mut self.radio;
            let Some(ch) = radio.channels.get_mut(&f) else { continue };
            let Some(iq) = radio.sources[source].chz.output(ch.head) else { continue };
            radio.tout.clear();
            Radio::run_channel(ch, iq, rate, src_rate, radio.tdma_key, &mut radio.groups, &mut radio.tout, false);
            Radio::route(ch, &mut radio.tout, &mut radio.pending);
        }
        // The control channel.
        if source == self.cc_source {
            if let Some(head) = self.cc_head {
                let iq = self.radio.sources[source].chz.output(head).map(|v| v.to_vec()).unwrap_or_default();
                if self.cc_sn.is_some() {
                    self.cc_samples += iq.len() as u64;
                    self.now_s = self.cc_start_s + self.cc_samples as f64 / rate;
                    self.on_smartnet(&iq);
                } else {
                    let mut groups = Vec::new();
                    self.cc_bank.push(&iq, &mut groups);
                    self.cc_samples += iq.len() as u64;
                    self.now_s = self.cc_start_s + self.cc_samples as f64 / rate;
                    self.on_control_groups(groups);
                }
                self.apply_pending();
                if self.now_s - self.last_good_s > CC_HUNT_S && self.cfg.control_channels.len() > 1 {
                    let _ = self.tune_control(self.cc_index + 1);
                }
                self.calls.tick(self.now_s, &mut self.radio, &mut self.call_events);
            }
        }
        // No trunked system: the first source's sample clock is the time.
        if self.cc_head.is_none() && source == 0 {
            let c = &self.radio.sources[0].chz;
            self.now_s = c.sample_position() as f64 / c.fs();
        }
        if !self.conv.is_empty() {
            let c = &self.radio.sources[source].chz;
            let t = c.sample_position() as f64 / c.fs();
            let rules = self.call_rules();
            let mut out = std::mem::take(&mut self.conv_out);
            self.conv.on_block(source, &mut self.radio.sources[source].chz, t, &mut self.calls, &rules, &mut out);
            self.emit_conv(out);
        }
        self.apply_pending();
        self.emit_call_events();
    }

    fn on_smartnet(&mut self, iq: &[Complex32]) {
        let Some(cc) = self.cc_sn.as_mut() else { return };
        let (good0, bad0) = cc.counts();
        let mut msgs = Vec::new();
        cc.push(iq, self.cc_start_s, &mut msgs);
        let (good, bad) = cc.counts();
        self.good += good - good0;
        self.bad += bad - bad0;
        if good > good0 {
            self.last_good_s = self.now_s;
        }
        self.identity.sys_id = cc.parser.sys_id;
        self.identity.site = cc.parser.site;
        if !msgs.is_empty() {
            self.calls.handle(&msgs, &mut self.radio, &mut self.call_events);
            self.events.extend(msgs.into_iter().map(Event::Message));
        }
    }

    fn on_control_groups(&mut self, groups: Vec<Group>) {
        let rate = self.radio.sources[self.cc_source].chz.output_rate();
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
                self.last_good_s = self.now_s;
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
                        _ => {}
                    }
                }
                if let (Some(nac), Some(sys), Some(wacn)) = (self.identity.nac, self.identity.sys_id, self.identity.wacn) {
                    self.radio.tdma_key = Some((nac as u32, sys, wacn));
                }
                self.calls.handle(&msgs, &mut self.radio, &mut self.call_events);
                self.events.extend(msgs.into_iter().map(Event::Message));
            }
        }
    }

    /// Route what the voice trackers produced to their calls.
    fn apply_pending(&mut self) {
        for (id, o) in std::mem::take(&mut self.radio.pending) {
            match o {
                TrackerOut::Audio(samples, frame) => {
                    let Some(rec) = self.radio.recordings.get_mut(&id) else { continue };
                    rec.audio.extend_from_slice(&samples);
                    rec.frames.push(frame);
                    self.calls.note_audio(id, self.now_s);
                    let tg = self.calls.calls.iter().find(|c| c.id == id).map_or(0, |c| c.talkgroup);
                    self.events.push(Event::Audio { call_id: id, talkgroup: tg, samples });
                }
                TrackerOut::AnalogAudio(samples) => {
                    let Some(rec) = self.radio.recordings.get_mut(&id) else { continue };
                    rec.audio.extend_from_slice(&samples);
                    self.calls.note_audio(id, self.now_s);
                    let tg = self.calls.calls.iter().find(|c| c.id == id).map_or(0, |c| c.talkgroup);
                    self.events.push(Event::Audio { call_id: id, talkgroup: tg, samples });
                }
                TrackerOut::Info { source, emergency, encrypted } => {
                    let now = self.now_s;
                    if let Some(c) = self.calls.call_mut(id) {
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
        let (json, base_name) = call_record(
            call,
            &ConcludeInfo {
                short_name: &self.cfg.short_name,
                epoch_ms_at_zero: self.cfg.epoch_ms_at_zero,
                audio_seconds: audio.len() as f64 / mbe::SAMPLE_RATE as f64,
                errors: &frames.errors,
                recorder_num,
                end_s: call.last_audio_s,
            },
        );
        self.concluded += 1;
        let frames = frames.captured.filter(|_| !audio.is_empty()).map(|f| frames_jsonl(&f));
        self.events.push(Event::Concluded(Concluded { call: call.clone(), json, base_name, audio, frames }));
    }
}
