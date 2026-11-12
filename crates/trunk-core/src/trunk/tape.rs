//! What a channel received, kept to replay through its decoder alone
//! (research/iq-corpus.md): a voice channel's whole life, or a control
//! channel's last minute. Off unless [`super::engine::EngineConfig::tapes`].

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use num_complex::Complex32;

use super::voice::{VoiceParams, VoiceSpec};

/// The longest a voice channel is kept, s: a call that long is rare, and more isn't a better test.
const VOICE_MAX_S: f64 = 120.0;
/// How much of a control channel is kept, s.
const CONTROL_S: f64 = 60.0;

/// Whether channels are taped, and the voice channels' tapes that have
/// closed, for the host to take. Shared by the engine's parts, and switched
/// on and off as it runs ([`Tapes::set`]): a capture asked for by hand needs
/// them only until it's made.
#[derive(Clone, Default)]
pub struct Tapes(Arc<TapesShared>);

#[derive(Default)]
struct TapesShared {
    on: AtomicBool,
    done: Mutex<Vec<VoiceTape>>,
}

impl Tapes {
    pub fn on(&self) -> bool {
        self.0.on.load(Ordering::Relaxed)
    }

    /// Tape from now on, or stop (what's waiting goes).
    pub fn set(&self, on: bool) {
        self.0.on.store(on, Ordering::Relaxed);
        if !on {
            self.0.done.lock().unwrap().clear();
        }
    }

    /// A channel closed with tape `t`.
    pub fn keep(&self, t: Option<VoiceTape>) {
        if let Some(t) = t.filter(|_| self.on()) {
            self.0.done.lock().unwrap().push(t);
        }
    }

    /// The tapes of the channels closed since last asked.
    pub fn take(&self) -> Vec<VoiceTape> {
        std::mem::take(&mut *self.0.done.lock().unwrap())
    }
}

/// What a voice channel's decoder was told, from sample `sample` on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TapeMark {
    pub sample: u64,
    pub params: VoiceParams,
    /// Which slots were being listened to.
    pub slots: [bool; 2],
    pub squelch: f32,
}

/// A voice channel, from its pre-roll to when it closed.
#[derive(Clone, Debug)]
pub struct VoiceTape {
    pub freq_hz: f64,
    pub spec: VoiceSpec,
    /// Samples in its decoder's first push, and each later one.
    pub first: usize,
    pub block: usize,
    pub marks: Vec<TapeMark>,
    pub iq: Vec<Complex32>,
    /// It went on past the longest kept.
    pub truncated: bool,
}

impl VoiceTape {
    /// A tape of a channel decoded by `spec` (keys never go on a tape).
    pub fn new(mut spec: VoiceSpec, freq_hz: f64) -> VoiceTape {
        spec.keys = None;
        VoiceTape { freq_hz, spec, first: 0, block: 0, marks: Vec::new(), iq: Vec::new(), truncated: false }
    }

    /// The decoder was given `iq`, told this.
    pub fn run(&mut self, iq: &[Complex32], params: VoiceParams, slots: [bool; 2], squelch: f32) {
        if iq.is_empty() {
            return;
        }
        let room = (VOICE_MAX_S * self.spec.rate) as usize;
        if self.iq.len() >= room {
            self.truncated = true;
            return;
        }
        if self.first == 0 {
            self.first = iq.len();
        } else if self.block == 0 {
            self.block = iq.len();
        }
        if self.marks.last().is_none_or(|m| (m.params, m.slots, m.squelch) != (params, slots, squelch)) {
            self.marks.push(TapeMark { sample: self.iq.len() as u64, params, slots, squelch });
        }
        self.iq.extend_from_slice(iq);
    }
}

/// A control channel carrier's latest samples.
#[derive(Clone, Debug)]
pub struct ControlTape {
    pub site: u16,
    pub carrier: usize,
    /// The carrier's frequency.
    pub freq_hz: f64,
    pub rate: f64,
    /// Samples in each push.
    pub block: usize,
    /// When its first sample was, s on the engine's clock.
    pub t0: f64,
    pub iq: VecDeque<Complex32>,
}

impl ControlTape {
    pub fn new(site: u16, carrier: usize, freq_hz: f64, rate: f64) -> ControlTape {
        ControlTape { site, carrier, freq_hz, rate, block: 0, t0: 0.0, iq: VecDeque::new() }
    }

    /// Samples whose first was at `t0`.
    pub fn push(&mut self, iq: &[Complex32], t0: f64) {
        if iq.is_empty() {
            return;
        }
        if self.iq.is_empty() {
            self.t0 = t0;
        }
        self.block = self.block.max(iq.len());
        self.iq.extend(iq.iter().copied());
        let room = (CONTROL_S * self.rate) as usize;
        if self.iq.len() > room {
            let over = self.iq.len() - room;
            // Whole blocks, so a replay pushes what the decoder was pushed.
            let drop = over.div_ceil(self.block) * self.block;
            self.iq.drain(..drop.min(self.iq.len()));
            self.t0 += drop as f64 / self.rate;
        }
    }

    /// Only (about) its last `seconds`: whole blocks, at least that much.
    pub fn keep_last(&mut self, seconds: f64) {
        let room = (seconds * self.rate) as usize;
        if self.block > 0 && self.iq.len() > room {
            let drop = (self.iq.len() - room) / self.block * self.block;
            self.iq.drain(..drop);
            self.t0 += drop as f64 / self.rate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::AnalogTuning;
    use crate::mbe;
    use crate::p25::diversity::BankConfig;
    use crate::trunk::voice::VoiceKind;

    fn spec(rate: f64) -> VoiceSpec {
        VoiceSpec { kind: VoiceKind::Fdma, rate, t0: 0.0, seed: 1, vocoder: mbe::Profile::Fixed, bank: BankConfig::default(), squelch: 0.5, subaudible: false, analog: AnalogTuning::default(), ignore_hz: Vec::new(), keys: None }
    }

    #[test]
    fn a_voice_tape_notes_what_its_decoder_was_told_when_it_changes() {
        let mut t = VoiceTape::new(spec(1000.0), 851e6);
        let iq = [Complex32::new(1.0, 0.0); 10];
        let key = VoiceParams { tdma_key: Some((1, 2, 3)), nac: Some(1) };
        t.run(&iq, VoiceParams::default(), [true, false], 0.5);
        t.run(&iq[..4], VoiceParams::default(), [true, false], 0.5);
        t.run(&iq[..4], key, [true, false], 0.5);
        t.run(&iq[..4], key, [true, true], 0.5);
        t.run(&[], key, [false, false], 0.5);
        assert_eq!((t.first, t.block, t.iq.len()), (10, 4, 22));
        assert_eq!(t.marks.iter().map(|m| m.sample).collect::<Vec<_>>(), [0, 14, 18]);
    }

    #[test]
    fn a_voice_tape_stops_at_the_longest_kept() {
        let mut t = VoiceTape::new(spec(10.0), 851e6);
        let iq = vec![Complex32::default(); 700];
        t.run(&iq, VoiceParams::default(), [true, false], 0.5);
        t.run(&iq, VoiceParams::default(), [true, false], 0.5);
        assert!(t.iq.len() == 1400 && !t.truncated);
        t.run(&iq, VoiceParams::default(), [true, false], 0.5);
        assert!(t.truncated && t.iq.len() == 1400);
    }

    #[test]
    fn a_control_tape_keeps_the_latest_whole_blocks() {
        let mut t = ControlTape::new(0, 0, 851e6, 10.0);
        let block = vec![Complex32::default(); 100];
        for b in 0..8 {
            t.push(&block, b as f64 * 10.0);
        }
        assert_eq!((t.iq.len(), t.t0), (600, 20.0));
        t.keep_last(25.0);
        assert_eq!((t.iq.len(), t.t0), (300, 50.0), "whole blocks, at least that much");
    }

    #[test]
    fn tapes_are_kept_only_while_on() {
        let tapes = Tapes::default();
        tapes.keep(Some(VoiceTape::new(spec(10.0), 851e6)));
        assert!(tapes.take().is_empty());
        tapes.set(true);
        tapes.keep(Some(VoiceTape::new(spec(10.0), 851e6)));
        tapes.set(false);
        assert!(tapes.take().is_empty(), "switched off: what was waiting goes");
    }
}
