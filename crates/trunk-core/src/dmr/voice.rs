//! A DMR carrier's voice, both slots: decided symbols → bursts → per slot,
//! who is talking (link control), the cipher, and audio — the same outputs
//! as the P25 trackers, tagged with the slot, so a call can be recorded on
//! each slot independently.

use crate::trunk::Flags;
use super::alias::{self, Assembler};
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
    /// The talker alias being put together, and when voice LC last named the talker for alias/GPS attribution.
    alias: Assembler,
    lc_s: f64,
}

impl Slot {
    fn new(seed: u32) -> Self {
        Slot { dec: mbe::Decoder::new(mbe::lcg(seed), mbe::Profile::Enhanced), talkgroup: None, source: None, group: true, encrypted: false, emergency: false, alias: Assembler::default(), lc_s: f64::NEG_INFINITY }
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
            self.chan.burst_at(&b, t, &mut self.ev);
            for (slot, e) in std::mem::take(&mut self.ev) {
                self.event(slot, e, t, out);
            }
        }
    }

    /// The talker alias slot `slot`'s transmission has sent so far, whole or not (`tool dmr`).
    pub fn alias_so_far(&self, slot: u8) -> Option<&alias::Decoded> {
        self.slots[slot as usize & 1].alias.so_far()
    }

    /// One of [`Self::chan`]'s events, at air time `t`: what [`Self::push`] does with each.
    pub fn event(&mut self, slot: u8, e: SlotEvent, t: f64, out: &mut Vec<Out>) {
        let s = &mut self.slots[slot as usize];
        let mut push = |o: TrackerOut| out.push(Out { slot, t, out: o });
        match e {
            SlotEvent::Packet(p) => {
                if let Some(p) = super::hytera::packet(&p).position { push(TrackerOut::Position(p)); }
            }
            SlotEvent::Lc { lc, from } => {
                if from == LcFrom::Embedded {
                    let talker = s.source.filter(|_| t - s.lc_s < super::gps::HOLD_S);
                    if let Some(p) = super::gps::lc(&lc, talker) {
                        push(TrackerOut::Position(p));
                    }
                    // A talker alias block: whole, it names the radio voice LC named lately.
                    let fresh = t - s.lc_s < alias::HOLD_S;
                    let name = s.alias.push(lc, t).and_then(|d| d.alias(s.source.filter(|&u| u != 0 && fresh)?, s.talkgroup));
                    if let Some(a) = name {
                        push(TrackerOut::Alias(a));
                    }
                }
                if from == LcFrom::Terminator && lc.ends_voice() {
                    // End of the transmission (repeated through the hang time): not a call of its own.
                    s.alias.reset();
                    s.source = None;
                    s.encrypted = false;
                    s.emergency = false;
                    return;
                }
                let Some(group) = lc.voice_user() else { return };
                let (tg, src) = (lc.target(), lc.source());
                let fresh = s.talkgroup != Some(tg) || s.source != Some(src) || s.encrypted != lc.encrypted() || s.emergency != lc.emergency();
                if s.source != Some(src) {
                    // Another talker: the alias heard so far isn't its.
                    s.alias.reset();
                }
                s.lc_s = t;
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
                    push(TrackerOut::Info { source: (src != 0).then_some(src), flags: Flags { emergency: s.emergency, encrypted: s.encrypted, ..Default::default() } });
                }
            }
            SlotEvent::Privacy { .. } => {
                if !s.encrypted {
                    s.encrypted = true;
                    push(TrackerOut::Info { source: None, flags: Flags { emergency: s.emergency, encrypted: true, ..Default::default() } });
                }
            }
            SlotEvent::Voice { frame, pos } => {
                self.frames += 1;
                if pos == Some(0) && s.source.is_none() {
                    // A new superframe with no LC yet: a fresh transmission's vocoder state.
                    s.dec.reset();
                }
                if s.encrypted {
                    return;
                }
                let mut buf = [0f32; FRAME_SAMPLES];
                let kind = s.dec.ambe(&frame.bits, frame.errs, &mut buf);
                if !matches!(kind, Kind::Voice | Kind::Tone) {
                    self.bad_frames += 1;
                }
                mbe::to_limited(&mut buf);
                let vf = VoiceFrame { codec: Codec::Ambe, bits: frame.bits.to_vec(), e0: 0, errs: frame.errs, erased: false, kind, tone: s.dec.tone_played(kind) };
                push(TrackerOut::Audio(buf.to_vec(), vf));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmr::slot::{Lc, FID_MOTOROLA};

    /// Voice link control `flco` (feature set `fid`) to 201 from radio 5;
    /// `privacy`: the service options' privacy bit.
    fn lc(flco: u8, fid: u8, privacy: bool) -> Lc {
        Lc([flco, fid, if privacy { 0x40 } else { 0 }, 0, 0, 201, 0, 0, 5])
    }

    /// An encrypted transmission whose terminator is lost (a fade), then a
    /// clear one on the same slot: the clear one is not taken for encrypted.
    #[test]
    fn a_new_transmission_starts_clear() {
        let mut v = DmrVoice::new(1);
        let mut out = Vec::new();
        v.event(0, SlotEvent::Lc { lc: lc(0, 0, true), from: LcFrom::Header }, 0.0, &mut out);
        assert!(v.slots[0].encrypted);
        v.event(0, SlotEvent::Lc { lc: lc(0, 0, false), from: LcFrom::Header }, 1.0, &mut out);
        assert!(!v.slots[0].encrypted);
        // Embedded link control saying so mid-transmission still counts.
        v.event(0, SlotEvent::Lc { lc: lc(0, 0, true), from: LcFrom::Embedded }, 1.2, &mut out);
        assert!(v.slots[0].encrypted);
    }

    #[test]
    fn gps_belongs_to_a_recent_talker_on_its_slot() {
        let mut v = DmrVoice::new(1);
        let mut out = Vec::new();
        let gps = Lc([8, 0x68, 1, 0x80, 0, 0, 0x40, 0, 0]); // 45 N, 90 W
        let report = SlotEvent::Lc { lc: gps, from: LcFrom::Embedded };
        v.event(0, report.clone(), 0.0, &mut out);
        assert!(out.is_empty(), "GPS cannot name its own talker");
        v.event(0, SlotEvent::Lc { lc: lc(0, 0, true), from: LcFrom::Header }, 0.1, &mut out);
        out.clear();
        v.event(1, report.clone(), 0.2, &mut out);
        assert!(out.is_empty(), "the other slot has no talker");
        v.event(0, report.clone(), 0.3, &mut out);
        assert_eq!(out.len(), 1, "encrypted voice still has clear GPS metadata");
        assert_eq!((out[0].slot, out[0].t), (0, 0.3));
        let TrackerOut::Position(p) = out[0].out else { panic!("position output") };
        assert_eq!((p.unit, p.lat, p.lon, p.source), (5, 45.0, -90.0, "HyteraDMR"));
        assert_eq!(v.talkgroup(0), Some(201));
        assert!(v.slots[0].encrypted, "coordinates are not service options");
        out.clear();
        v.event(0, SlotEvent::Lc { lc: lc(0, 0, true), from: LcFrom::Terminator }, 0.4, &mut out);
        v.event(0, report.clone(), 0.5, &mut out);
        assert!(out.is_empty(), "the transmission ended");
        let mut next = lc(0, 0, false);
        next.0[8] = 7;
        v.event(0, SlotEvent::Lc { lc: next, from: LcFrom::Header }, 0.6, &mut out);
        out.clear();
        v.event(0, report.clone(), 0.7, &mut out);
        assert!(matches!(out[0].out, TrackerOut::Position(p) if p.unit == 7));
        out.clear();
        v.event(0, SlotEvent::Lc { lc: gps, from: LcFrom::Header }, 0.8, &mut out);
        v.event(0, SlotEvent::Lc { lc: gps, from: LcFrom::Terminator }, 0.9, &mut out);
        assert!(out.is_empty(), "only embedded GPS describes the active talker");
        let terminator = Lc([0x30, 0x68, 0, 0, 0, 201, 0, 0, 7]);
        v.event(0, SlotEvent::Lc { lc: terminator, from: LcFrom::Terminator }, 1.0, &mut out);
        v.event(0, report.clone(), 1.1, &mut out);
        assert!(out.is_empty(), "Hytera's own terminator clears its talker");
        v.event(0, SlotEvent::Lc { lc: next, from: LcFrom::Header }, 2.0, &mut out);
        out.clear();
        v.event(0, report.clone(), 2.0 + super::super::gps::HOLD_S, &mut out);
        assert!(out.is_empty(), "a missed terminator cannot keep the talker forever");
        v.event(0, SlotEvent::Lc { lc: next, from: LcFrom::Embedded }, 6.1, &mut out);
        v.event(0, report, 6.2, &mut out);
        assert!(matches!(out[0].out, TrackerOut::Position(p) if p.unit == 7), "repeated voice LC refreshes the talker");
    }

    /// Motorola restricted access (RAS): a call's embedded LC sets the FLCO's
    /// 0x20 bit. It is still a clear call, group (0x20) or private (0x23).
    #[test]
    fn restricted_access_link_control_is_a_clear_call() {
        let mut v = DmrVoice::new(1);
        let mut out = Vec::new();
        v.event(0, SlotEvent::Lc { lc: lc(0, FID_MOTOROLA, false), from: LcFrom::Header }, 0.0, &mut out);
        v.event(0, SlotEvent::Lc { lc: lc(0x20, FID_MOTOROLA, false), from: LcFrom::Embedded }, 0.4, &mut out);
        assert!(!v.slots[0].encrypted && v.slots[0].group);
        assert_eq!((v.slots[0].talkgroup, v.slots[0].source), (Some(201), Some(5)));
        v.event(1, SlotEvent::Lc { lc: lc(0x23, FID_MOTOROLA, false), from: LcFrom::Embedded }, 0.4, &mut out);
        assert!(!v.slots[1].encrypted && !v.slots[1].group);
        // Encrypted is the privacy bit, restricted access or not.
        v.event(0, SlotEvent::Lc { lc: lc(0x20, FID_MOTOROLA, true), from: LcFrom::Embedded }, 0.8, &mut out);
        assert!(v.slots[0].encrypted);
    }

    #[test]
    fn complete_alias_reaches_existing_unit_name_path_without_changing_privacy() {
        let mut v = DmrVoice::new(1);
        let mut out = Vec::new();
        // XPT voice LC as the channel hands it on: 16-bit IDs.
        v.event(0, SlotEvent::Lc { lc: crate::dmr::Lc([0, 0x68, 0, 0, 0, 149, 0, 0x13, 0x8e]), from: LcFrom::Header }, 0.0, &mut out);
        // The captured UTF-16 payload, with its declared length set to the six
        // units actually supplied. The original declares 13 and stays partial.
        v.event(0, SlotEvent::Lc { lc: crate::dmr::Lc([4, 0x68, 0xcc, 0, 0x48, 0, 0x79, 0, 0x74]), from: LcFrom::Embedded }, 0.2, &mut out);
        v.event(0, SlotEvent::Lc { lc: crate::dmr::Lc([5, 0x68, 0, 0x65, 0, 0x72, 0, 0x61, 0]), from: LcFrom::Embedded }, 0.9, &mut out);
        let aliases: Vec<_> = out.iter().filter_map(|o| match &o.out { TrackerOut::Alias(a) => Some((o.slot, a)), _ => None }).collect();
        assert_eq!(aliases.len(), 1);
        assert_eq!((aliases[0].0, aliases[0].1.unit, aliases[0].1.alias.as_str(), aliases[0].1.talkgroup), (0, 5006, "Hytera", Some(149)));
        assert!(!v.slots[0].encrypted && !v.slots[0].emergency, "alias format byte is not service options");
        assert!(v.slots[1].source.is_none());
    }

    #[test]
    fn gps_interleaved_with_an_alias_preserves_both() {
        let mut v = DmrVoice::new(1);
        let mut out = Vec::new();
        v.event(0, SlotEvent::Lc { lc: lc(0, 0x68, true), from: LcFrom::Header }, 0.0, &mut out);
        for (i, raw) in [
            [4, 0x68, 0x50, 0x44, 0x45, 0x53, 0x4b, 0x20, 0x31], // "DESK 123"
            [8, 0x68, 1, 0x80, 0, 0, 0x40, 0, 0], // GPS, 45 N, 90 W
            [5, 0x68, 0x32, 0x33, 0, 0, 0, 0, 0],
        ].into_iter().enumerate() {
            v.event(0, SlotEvent::Lc { lc: Lc(raw), from: LcFrom::Embedded }, 0.2 + i as f64 * 0.36, &mut out);
        }
        assert_eq!(out.iter().filter(|o| matches!(&o.out, TrackerOut::Alias(a) if a.unit == 5 && a.alias == "DESK 123")).count(), 1);
        assert_eq!(out.iter().filter(|o| matches!(o.out, TrackerOut::Position(p) if (p.unit, p.lat, p.lon) == (5, 45.0, -90.0))).count(), 1);
        assert!(v.slots[0].encrypted && v.slots[0].source == Some(5));
    }

    #[test]
    fn alias_names_the_slots_talker_until_it_changes_ends_or_goes_stale() {
        use crate::dmr::{slot::FID_HYTERA, Lc};
        // "DESK 123", Latin-1, Hytera FID: a header block and one more.
        let ms = [Lc([4, FID_HYTERA, 0x50, 0x44, 0x45, 0x53, 0x4b, 0x20, 0x31]), Lc([5, FID_HYTERA, 0x32, 0x33, 0, 0, 0, 0, 0])];
        let voice = |src: u8, enc: bool| Lc([0, FID_HYTERA, if enc { 0x40 } else { 0 }, 0, 0, 149, 0, 0x13, src]);
        let names = |out: &[Out]| out.iter().filter_map(|o| match &o.out { TrackerOut::Alias(a) => Some((o.slot, a.unit, a.alias.clone())), _ => None }).collect::<Vec<_>>();
        let mut v = DmrVoice::new(1);
        let mut out = Vec::new();
        v.event(0, SlotEvent::Lc { lc: voice(0x8e, true), from: LcFrom::Header }, 0.0, &mut out);
        v.event(1, SlotEvent::Lc { lc: voice(0x8f, false), from: LcFrom::Header }, 0.0, &mut out);
        v.event(0, SlotEvent::Lc { lc: ms[0], from: LcFrom::Embedded }, 0.2, &mut out);
        v.event(1, SlotEvent::Lc { lc: ms[1], from: LcFrom::Embedded }, 0.3, &mut out);
        v.event(0, SlotEvent::Lc { lc: ms[1], from: LcFrom::Embedded }, 0.9, &mut out);
        assert_eq!(names(&out), [(0, 5006, "DESK 123".to_string())], "slot-local, and an encrypted call's alias is clear");
        out.clear();
        v.event(0, SlotEvent::Lc { lc: ms[0], from: LcFrom::Embedded }, 1.0, &mut out);
        v.event(0, SlotEvent::Lc { lc: voice(0x8f, false), from: LcFrom::Embedded }, 1.1, &mut out);
        v.event(0, SlotEvent::Lc { lc: ms[1], from: LcFrom::Embedded }, 1.7, &mut out);
        v.event(0, SlotEvent::Lc { lc: ms[0], from: LcFrom::Embedded }, 1.8, &mut out);
        v.event(0, SlotEvent::Lc { lc: voice(0x8f, false), from: LcFrom::Terminator }, 1.9, &mut out);
        v.event(0, SlotEvent::Lc { lc: ms[1], from: LcFrom::Embedded }, 2.0, &mut out);
        assert!(names(&out).is_empty(), "a new talker or the end of the transmission starts over");
        v.event(1, SlotEvent::Lc { lc: ms[0], from: LcFrom::Embedded }, 4.1, &mut out);
        v.event(1, SlotEvent::Lc { lc: ms[1], from: LcFrom::Embedded }, 4.5, &mut out);
        assert!(names(&out).is_empty(), "no name for a talker last named long ago");
    }
    #[test]
    fn xpt_private_encrypted_voice_is_never_vocoded() {
        let mut v = DmrVoice::new(1);
        let lc = Lc([3, 0x68, 0x40, 0, 0, 25, 0, 0x12, 0x34]);
        let mut out = Vec::new();
        v.event(0, SlotEvent::Lc { lc, from: LcFrom::Embedded }, 0.0, &mut out);
        assert_eq!(v.talkgroup(0), Some(25));
        assert_eq!(v.slots[0].source, Some(0x1234));
        assert!(v.private(0) && v.slots[0].encrypted);
        v.event(0, SlotEvent::Voice { frame: crate::ambe::AmbeFrame { bits: [0; 49], errs: 0 }, pos: Some(0) }, 0.1, &mut out);
        assert!(out.iter().all(|v| !matches!(v.out, TrackerOut::Audio(..))), "encrypted frames are never vocoded");
    }

}
