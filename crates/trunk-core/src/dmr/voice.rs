//! A DMR carrier's voice, both slots: decided symbols → bursts → per slot,
//! who is talking (link control), the cipher, and audio — the same outputs
//! as the P25 trackers, tagged with the slot, so a call can be recorded on
//! each slot independently.

use super::burst::{Burst, Framer};
use super::slot::{Channel, LcFrom, SlotEvent};
use crate::dsp::Symbol;
use crate::mbe::{self, Kind, FRAME_SAMPLES};
use crate::trunk::frames::{Codec, VoiceFrame};
use crate::trunk::voice::TrackerOut;

/// A voice burst carries 60 ms of one slot's speech.
pub const VOICE_BURST_S: f64 = 0.06;

struct Slot {
    dec: mbe::Decoder,
    talkgroup: Option<u32>,
    source: Option<u32>,
    group: bool,
    encrypted: bool,
    emergency: bool,
}

impl Slot {
    fn new(seed: u32) -> Self {
        Slot { dec: mbe::Decoder::new(mbe::lcg(seed), mbe::Profile::Enhanced), talkgroup: None, source: None, group: true, encrypted: false, emergency: false }
    }
}

/// What one slot produced, at air time `t` (s).
pub struct Out {
    pub slot: u8,
    pub t: f64,
    pub out: TrackerOut,
}

pub struct DmrVoice {
    framer: Framer,
    pub chan: Channel,
    slots: [Slot; 2],
    bursts: Vec<Burst>,
    ev: Vec<(u8, SlotEvent)>,
    /// Voice codewords / those not synthesized as sent, over the carrier's life.
    pub frames: u64,
    pub bad_frames: u64,
    /// Which slots to vocode (a trunked voice channel skips the slot nobody records).
    pub vocode: [bool; 2],
}

impl DmrVoice {
    pub fn new(seed: u32) -> Self {
        DmrVoice {
            framer: Framer::new(),
            chan: Channel::default(),
            slots: [Slot::new(seed), Slot::new(seed ^ 0x5a5a_5a5a)],
            bursts: Vec::new(),
            ev: Vec::new(),
            frames: 0,
            bad_frames: 0,
            vocode: [true; 2],
        }
    }

    /// The talkgroup (or, unit to unit, the radio) slot `slot` is talking to, once link control named it.
    pub fn talkgroup(&self, slot: u8) -> Option<u32> {
        self.slots[slot as usize & 1].talkgroup
    }
    /// Unit to unit rather than to a group.
    pub fn private(&self, slot: u8) -> bool {
        !self.slots[slot as usize & 1].group
    }
    pub fn color_code(&self, slot: u8) -> Option<u8> {
        self.chan.slots[slot as usize & 1].color_code
    }

    /// Symbols from the receiver; `t0` + sample / `rate` is a symbol's air time.
    pub fn push(&mut self, syms: &[Symbol], t0: f64, rate: f64, out: &mut Vec<Out>) {
        self.bursts.clear();
        for s in syms {
            self.framer.push(s, &mut self.bursts);
        }
        for b in std::mem::take(&mut self.bursts) {
            let t = t0 + b.sample / rate;
            self.ev.clear();
            self.chan.burst(&b, &mut self.ev);
            for (slot, e) in std::mem::take(&mut self.ev) {
                self.event(slot, e, t, out);
            }
        }
    }

    fn event(&mut self, slot: u8, e: SlotEvent, t: f64, out: &mut Vec<Out>) {
        let s = &mut self.slots[slot as usize];
        let mut push = |o: TrackerOut| out.push(Out { slot, t, out: o });
        match e {
            SlotEvent::Lc { lc, from } => {
                let Some(group) = lc.voice_user() else { return };
                if from == LcFrom::Terminator {
                    // End of the transmission (repeated through the hang time): not a call of its own.
                    s.source = None;
                    s.encrypted = false;
                    s.emergency = false;
                    return;
                }
                let (tg, src) = (lc.target(), lc.source());
                let fresh = s.talkgroup != Some(tg) || s.source != Some(src) || s.encrypted != lc.encrypted() || s.emergency != lc.emergency();
                s.talkgroup = Some(tg);
                s.group = group;
                s.source = Some(src);
                if from == LcFrom::Header {
                    // A new transmission: what an earlier one was (its
                    // terminator lost in a fade) doesn't carry over.
                    s.encrypted = lc.encrypted();
                    s.emergency = lc.emergency();
                } else {
                    s.encrypted |= lc.encrypted();
                    s.emergency |= lc.emergency();
                }
                if fresh {
                    push(TrackerOut::Info { source: (src != 0).then_some(src), emergency: s.emergency, encrypted: s.encrypted });
                }
            }
            SlotEvent::Privacy { .. } => {
                if !s.encrypted {
                    s.encrypted = true;
                    push(TrackerOut::Info { source: None, emergency: s.emergency, encrypted: true });
                }
            }
            SlotEvent::Voice { frame, pos } => {
                self.frames += 1;
                if pos == Some(0) && s.source.is_none() {
                    // A new superframe with no LC yet: a fresh transmission's vocoder state.
                    s.dec.reset();
                }
                if s.encrypted || !self.vocode[slot as usize] {
                    return;
                }
                let mut buf = [0f32; FRAME_SAMPLES];
                let kind = s.dec.ambe(&frame.bits, frame.errs, &mut buf);
                if kind != Kind::Voice {
                    self.bad_frames += 1;
                }
                mbe::to_limited(&mut buf);
                let vf = VoiceFrame { codec: Codec::Ambe, bits: frame.bits.to_vec(), e0: 0, errs: frame.errs, erased: false, kind };
                push(TrackerOut::Audio(buf.to_vec(), vf));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmr::slot::Lc;

    /// Group voice link control to TG 201 from radio 5; `privacy`: the
    /// service options' privacy bit.
    fn lc(privacy: bool) -> Lc {
        Lc([0, 0, if privacy { 0x40 } else { 0 }, 0, 0, 201, 0, 0, 5])
    }

    /// An encrypted transmission whose terminator is lost (a fade), then a
    /// clear one on the same slot: the clear one is not taken for encrypted.
    #[test]
    fn a_new_transmission_starts_clear() {
        let mut v = DmrVoice::new(1);
        let mut out = Vec::new();
        v.event(0, SlotEvent::Lc { lc: lc(true), from: LcFrom::Header }, 0.0, &mut out);
        assert!(v.slots[0].encrypted);
        v.event(0, SlotEvent::Lc { lc: lc(false), from: LcFrom::Header }, 1.0, &mut out);
        assert!(!v.slots[0].encrypted);
        // Embedded link control saying so mid-transmission still counts.
        v.event(0, SlotEvent::Lc { lc: lc(true), from: LcFrom::Embedded }, 1.2, &mut out);
        assert!(v.slots[0].encrypted);
    }
}
