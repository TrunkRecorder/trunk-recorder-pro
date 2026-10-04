//! Voice channels: one decoder per kind of voice, behind [`VoiceDecoder`],
//! so trunked calls and conventional channels run the same code and a new
//! kind of voice is one implementation here plus a line in [`build`].
//!
//! Which decoder a channel gets depends on the call, not on the system's
//! control channel: a P25 system grants Phase 1 and Phase 2 calls, a
//! SmartNet system analog and P25 ones ([`VoiceKind::of`]).
//!
//! ```text
//! channel IQ ─► VoiceDecoder::push ─► VoiceOut { slot, air time, TrackerOut }
//!   P25 Phase 1: receiver bank → framer → VoiceTracker (IMBE)
//!   P25 Phase 2: H-DQPSK → slot framer → TdmaTracker (AMBE+2, both slots)
//!   DMR:         4FSK → burst framer → DmrVoice (AMBE+2, both slots)
//!   analog FM:   discriminator, squelch, MDC1200 / FleetSync unit IDs
//! ```

use num_complex::Complex32;

use super::calls::Call;
use super::frames::VoiceFrame;
use super::tdma::TdmaTracker;
use super::tracker::VoiceTracker;
use crate::dmr::voice::{DmrVoice, VOICE_BURST_S};
use crate::dsp::c4fm::C4fm;
use crate::dsp::cqpsk::{self, Cqpsk};
use crate::dsp::fm::Nbfm;
use crate::dsp::signalling::Signalling;
use crate::dsp::{Receiver, Symbol};
use crate::mbe;
use crate::metrics::{Instrumented, Sink};
use crate::p25::alias::{Alias, AliasLc};
use crate::p25::diversity::{best_frame, Bank, BankConfig, Group};
use crate::p25::phase2::{self, Packet};

/// The kinds of voice channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceKind {
    /// P25 Phase 1 (C4FM or CQPSK, IMBE).
    Fdma,
    /// P25 Phase 2 TDMA (H-DQPSK, AMBE+2, two slots).
    Tdma,
    /// Analog narrowband FM.
    Analog,
    /// DMR (4FSK, AMBE+2, two slots).
    Dmr,
}

impl VoiceKind {
    /// The kind of voice a granted call carries.
    pub fn of(call: &Call) -> VoiceKind {
        if call.analog {
            VoiceKind::Analog
        } else if call.color_code.is_some() {
            VoiceKind::Dmr
        } else if call.phase2_tdma {
            VoiceKind::Tdma
        } else {
            VoiceKind::Fdma
        }
    }

    /// Whether its calls are on one of two slots.
    pub fn slotted(self) -> bool {
        matches!(self, VoiceKind::Tdma | VoiceKind::Dmr)
    }
}

/// What a voice tracker heard, for the call on its slot.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackerOut {
    /// 20 ms of 8 kHz audio in [−1, 1], and the vocoder frame it came from.
    Audio(Vec<f32>, VoiceFrame),
    /// Link control named a source / emergency, or the call turned out encrypted.
    Info { source: Option<u32>, emergency: bool, encrypted: bool },
    /// Analog FM voice (a SmartNet analog call, a conventional FM channel): 8 kHz audio, squelched.
    AnalogAudio(Vec<f32>),
    /// Analog FM: the same audio before the 300 Hz high-pass (CTCSS / DCS
    /// still in it), when asked for ([`VoiceSpec::subaudible`]).
    Subaudible(Vec<f32>),
    /// A radio's talker alias, heard during the call.
    Alias(Alias),
    /// What a terminator / MAC message showed of talker aliases (counted).
    AliasLc(AliasLc),
}

/// What one decoder produced: an output for the call on `slot`, from air
/// time `t` (s, the source's sample clock).
#[derive(Debug)]
pub struct VoiceOut {
    pub slot: u8,
    pub t: f64,
    pub out: TrackerOut,
}

/// What the air said about a slot (conventional channels match their rows
/// by it): the talkgroup, the P25 NAC or DMR colour code, and the span of
/// air the last [`VoiceDecoder::push`] heard voice in.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AirInfo {
    pub talkgroup: Option<u32>,
    pub nac: Option<u16>,
    pub color_code: Option<u8>,
    pub air: Option<(f64, f64)>,
}

/// What a system's control channel tells its voice channels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VoiceParams {
    /// P25 Phase 2 scrambling: (NAC, System ID, WACN).
    pub tdma_key: Option<(u32, u32, u32)>,
}

/// How to make a decoder.
#[derive(Clone, Copy, Debug)]
pub struct VoiceSpec {
    pub kind: VoiceKind,
    /// The channel's sample rate, Hz.
    pub rate: f64,
    /// Air time of the channel's first sample, s.
    pub t0: f64,
    /// Seeds the vocoders' noise (the frequency, as Trunk Recorder does).
    pub seed: u32,
    pub vocoder: mbe::Profile,
    /// The P25 receivers to run.
    pub bank: BankConfig,
    /// Analog: the squelch (carrier power that opens it); see
    /// [`VoiceDecoder::set_squelch`].
    pub squelch: f32,
    /// Analog: also hand out the audio below 300 Hz (CTCSS / DCS) as
    /// [`TrackerOut::Subaudible`].
    pub subaudible: bool,
}

/// One voice channel's decoding: channel IQ in, outputs for the calls on
/// its slots out.
pub trait VoiceDecoder: Send {
    fn kind(&self) -> VoiceKind;
    /// Decode `iq`; outputs go to `out` in the order they were heard.
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<VoiceOut>);
    /// End of input: decode what the receivers still hold.
    fn flush(&mut self, _out: &mut Vec<VoiceOut>) {}
    /// Which slots are being recorded (DMR vocodes only those).
    fn listen(&mut self, _slots: [bool; 2]) {}
    /// What the control channel says voice needs (Phase 2: the scrambler key).
    fn set_params(&mut self, _p: &VoiceParams) {}
    /// Analog: the carrier power that opens the squelch.
    fn set_squelch(&mut self, _open_power: f32) {}
    /// What the air said about `slot`, as of the last push.
    fn air(&self, _slot: u8) -> AirInfo {
        AirInfo::default()
    }
    /// How far above the channel's centre the carrier is, Hz, when known.
    fn offset_hz(&self) -> Option<f32> {
        None
    }
    /// Analog: whether the carrier was up during the last push (digital
    /// decoders don't tell: None).
    fn carrier(&self) -> Option<bool> {
        None
    }
    /// Its receivers' measurements for the dashboard (the eye opening as
    /// `sep`, CQPSK's phase error as `phaseErr`; see [`crate::metrics`]).
    /// Default: none.
    fn report(&self, _sink: &mut dyn Sink) {}
}

/// Make the decoder for `spec.kind`.
pub fn build(spec: &VoiceSpec) -> Box<dyn VoiceDecoder> {
    match spec.kind {
        VoiceKind::Fdma => Box::new(P25Fdma {
            bank: Bank::new(spec.rate, spec.bank),
            tracker: VoiceTracker::new(mbe::lcg(spec.seed), spec.vocoder),
            groups: Vec::new(),
            t0: spec.t0,
            rate: spec.rate,
            nac: None,
            air: None,
        }),
        VoiceKind::Tdma => {
            let mut tracker = TdmaTracker::new(spec.seed);
            tracker.soft = spec.bank.soft;
            // Decision-feedback differential detection: ~1 dB in noise on Phase 2
            // voice (tool snr); not used on Phase 1, where simulcast didn't like it.
            let rx = Cqpsk::new(spec.rate, cqpsk::Options { baud: phase2::SYMBOL_RATE, df_beta: 0.5, ..Default::default() });
            Box::new(P25Tdma { rx, framer: phase2::Framer::new(), tracker, syms: Vec::new(), pkts: Vec::new(), tout: Vec::new(), t0: spec.t0, rate: spec.rate })
        }
        VoiceKind::Dmr => Box::new(Dmr { rx: C4fm::dmr(spec.rate), voice: Box::new(DmrVoice::new(spec.seed)), syms: Vec::new(), t0: spec.t0, rate: spec.rate, air: [None; 2] }),
        VoiceKind::Analog => {
            Box::new(Analog { fm: Nbfm::new(spec.rate), ids: Signalling::default(), squelch: spec.squelch, subaudible: spec.subaudible, carrier: false })
        }
    }
}

/// P25 Phase 1: a receiver bank (best of several receivers each frame), IMBE.
struct P25Fdma {
    bank: Bank,
    tracker: VoiceTracker,
    groups: Vec<Group>,
    t0: f64,
    rate: f64,
    /// The last push's NAC, and the air it heard voice in.
    nac: Option<u16>,
    air: Option<(f64, f64)>,
}

impl P25Fdma {
    /// Run the tracker over `self.groups`.
    fn track(&mut self, out: &mut Vec<VoiceOut>) {
        let mut tout = Vec::new();
        for g in &self.groups {
            let best = best_frame(g);
            self.nac = Some(best.nid.nac);
            let t = self.t0 + best.sample / self.rate;
            tout.clear();
            self.tracker.group(g, t, &mut tout);
            if !tout.is_empty() {
                // An LDU is 180 ms of voice.
                self.air = Some((self.air.map_or(t, |a| a.0), t + 0.18));
            }
            out.extend(tout.drain(..).map(|o| VoiceOut { slot: 0, t, out: o }));
        }
    }
}

impl VoiceDecoder for P25Fdma {
    fn kind(&self) -> VoiceKind {
        VoiceKind::Fdma
    }
    fn report(&self, sink: &mut dyn Sink) {
        self.bank.report(sink);
    }
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<VoiceOut>) {
        (self.nac, self.air) = (None, None);
        self.groups.clear();
        self.bank.push(iq, &mut self.groups);
        self.track(out);
    }
    fn flush(&mut self, out: &mut Vec<VoiceOut>) {
        self.groups.clear();
        self.bank.flush(&mut self.groups);
        self.track(out);
    }
    fn air(&self, slot: u8) -> AirInfo {
        if slot != 0 {
            return AirInfo::default();
        }
        AirInfo { talkgroup: self.tracker.talkgroup(), nac: self.nac, color_code: None, air: self.air }
    }
    fn offset_hz(&self) -> Option<f32> {
        self.bank.offset_hz()
    }
}

/// P25 Phase 2: H-DQPSK, the slot framer, both slots' AMBE+2.
struct P25Tdma {
    rx: Cqpsk,
    framer: phase2::Framer,
    tracker: TdmaTracker,
    syms: Vec<Symbol>,
    pkts: Vec<Packet>,
    tout: Vec<(usize, TrackerOut)>,
    t0: f64,
    rate: f64,
}

impl VoiceDecoder for P25Tdma {
    fn kind(&self) -> VoiceKind {
        VoiceKind::Tdma
    }
    fn report(&self, sink: &mut dyn Sink) {
        if let Some(e) = self.rx.phase_error_deg() {
            sink.gauge("phaseErr", e as f64);
        }
    }
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<VoiceOut>) {
        self.syms.clear();
        self.pkts.clear();
        self.rx.push(iq, &mut self.syms);
        for s in &self.syms {
            self.framer.push(s, &mut self.pkts);
        }
        for p in &self.pkts {
            let t = self.t0 + p.sample / self.rate;
            self.tracker.packet(p, t, &mut self.tout);
            out.extend(self.tout.drain(..).map(|(slot, o)| VoiceOut { slot: slot as u8, t, out: o }));
        }
    }
    fn set_params(&mut self, p: &VoiceParams) {
        if let Some((nac, sys, wacn)) = p.tdma_key {
            self.tracker.set_key(nac, sys, wacn);
        }
    }
}

/// DMR: 4FSK, the burst framer, both slots.
struct Dmr {
    rx: C4fm,
    voice: Box<DmrVoice>,
    syms: Vec<Symbol>,
    t0: f64,
    rate: f64,
    /// Each slot's air with voice in the last push.
    air: [Option<(f64, f64)>; 2],
}

impl VoiceDecoder for Dmr {
    fn kind(&self) -> VoiceKind {
        VoiceKind::Dmr
    }
    fn report(&self, sink: &mut dyn Sink) {
        if let Some(q) = self.rx.quality() {
            sink.gauge("sep", q as f64);
        }
    }
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<VoiceOut>) {
        self.air = [None; 2];
        self.syms.clear();
        self.rx.push(iq, &mut self.syms);
        let mut vout = Vec::new();
        self.voice.push(&self.syms, self.t0, self.rate, &mut vout);
        for v in vout {
            if matches!(v.out, TrackerOut::Audio(..)) {
                let a = &mut self.air[v.slot as usize & 1];
                *a = Some((a.map_or(v.t, |a| a.0), v.t + VOICE_BURST_S));
            }
            out.push(VoiceOut { slot: v.slot, t: v.t, out: v.out });
        }
    }
    fn listen(&mut self, slots: [bool; 2]) {
        self.voice.vocode = slots;
    }
    fn air(&self, slot: u8) -> AirInfo {
        AirInfo { talkgroup: self.voice.talkgroup(slot), nac: None, color_code: self.voice.color_code(slot), air: self.air[slot as usize & 1] }
    }
}

/// Analog FM, squelched; unit IDs from MDC1200 / FleetSync bursts.
struct Analog {
    fm: Nbfm,
    ids: Signalling,
    squelch: f32,
    subaudible: bool,
    carrier: bool,
}

impl VoiceDecoder for Analog {
    fn kind(&self) -> VoiceKind {
        VoiceKind::Analog
    }
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<VoiceOut>) {
        let mut audio = Vec::new();
        let mut low = Vec::new();
        self.carrier = self.fm.push_low(iq, self.squelch, &mut audio, self.subaudible.then_some(&mut low));
        let mut found = Vec::new();
        self.ids.push(&audio, &mut found);
        for u in found {
            out.push(VoiceOut { slot: 0, t: 0.0, out: TrackerOut::Info { source: Some(u.unit), emergency: u.emergency, encrypted: false } });
        }
        if !audio.is_empty() {
            out.push(VoiceOut { slot: 0, t: 0.0, out: TrackerOut::AnalogAudio(audio) });
        }
        if !low.is_empty() {
            out.push(VoiceOut { slot: 0, t: 0.0, out: TrackerOut::Subaudible(low) });
        }
    }
    fn set_squelch(&mut self, open_power: f32) {
        self.squelch = open_power;
    }
    fn carrier(&self) -> Option<bool> {
        Some(self.carrier)
    }
}
