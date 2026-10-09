//! An NXDN traffic (or conventional) channel's voice: decided symbols →
//! frames → who is talking (VCALL in FACCH1 and the SACCH superframe; on a
//! Type-D traffic channel the SCCH's INFO1–4), the cipher, the RAN, and
//! audio — the same outputs as the P25 and DMR trackers, so the engine
//! records it the same way.

use super::alias::Aliases;
use super::channel::{self, SacchAssembler};
use super::frame::{Body, Frame, Framer, RfChannel};
use super::layer3::{Context, Message};
use crate::ambe::decode_vcw;
use crate::dsp::Symbol;
use crate::mbe::{self, Kind, FRAME_SAMPLES};
use crate::p25::alias::Alias;
use crate::trunk::frames::{Codec, VoiceFrame};
use crate::trunk::voice::TrackerOut;

/// A voice codeword carries 20 ms of speech.
pub const VCH_S: f64 = 0.02;

/// What the channel said, at air time `t` (s).
pub struct Out {
    pub t: f64,
    pub out: TrackerOut,
}

pub struct NxdnVoice {
    framer: Framer,
    frames: Vec<Frame>,
    sf: SacchAssembler,
    dec: mbe::Decoder,
    talkgroup: Option<u32>,
    source: Option<u32>,
    group: bool,
    encrypted: bool,
    emergency: bool,
    /// Full-rate voice (not decoded).
    efr: bool,
    ran: Option<u8>,
    /// Kenwood talker alias segments heard in this transmission.
    aliases: Aliases,
    /// The alias heard before a VCALL named its radio.
    alias: Option<String>,
    /// Layer 3 messages heard on the channel since the last [`NxdnVoice::take_messages`] (the trunk follower's).
    pub messages: Vec<(f64, Message)>,
    /// Keep `messages` (a trunked voice channel, for late entry and other calls' DUPs).
    pub keep_messages: bool,
    /// Voice codewords / those not synthesized as sent, over the carrier's life.
    pub vch: u64,
    pub bad_vch: u64,
    /// Vocode (a monitored-only channel can skip it).
    pub vocode: bool,
}

impl NxdnVoice {
    pub fn new(seed: u32) -> Self {
        NxdnVoice {
            framer: Framer::new(),
            frames: Vec::new(),
            sf: SacchAssembler::default(),
            dec: mbe::Decoder::new(mbe::lcg(seed), mbe::Profile::Enhanced),
            talkgroup: None,
            source: None,
            group: true,
            encrypted: false,
            emergency: false,
            efr: false,
            ran: None,
            aliases: Aliases::default(),
            alias: None,
            messages: Vec::new(),
            keep_messages: false,
            vch: 0,
            bad_vch: 0,
            vocode: true,
        }
    }

    /// The group (or, unit to unit, the radio) being talked to, once a VCALL named it.
    pub fn talkgroup(&self) -> Option<u32> {
        self.talkgroup
    }
    pub fn private(&self) -> bool {
        !self.group
    }
    pub fn ran(&self) -> Option<u8> {
        self.ran
    }
    /// Symbols arrive sign-flipped (spectrally inverted IQ).
    pub fn inverted(&self) -> bool {
        self.framer.inverted
    }

    /// Symbols from the receiver; `t0` + sample / `rate` is a symbol's air time.
    pub fn push(&mut self, syms: &[Symbol], t0: f64, rate: f64, out: &mut Vec<Out>) {
        self.frames.clear();
        for s in syms {
            self.framer.push(s, &mut self.frames);
        }
        for f in std::mem::take(&mut self.frames) {
            let t = t0 + f.sample / rate;
            self.frame(&f, t, out);
        }
    }

    fn frame(&mut self, f: &Frame, t: f64, out: &mut Vec<Out>) {
        let Some((lich, _)) = f.lich() else {
            self.sf.miss();
            return;
        };
        if lich.is_cac() {
            return;
        }
        let mut msgs: Vec<Message> = Vec::new();
        match lich.body() {
            Body::Voice { superframe, facch, idle } => {
                if lich.rf() == RfChannel::Composite {
                    // Type-D: the SCCH says who in pieces, one per frame.
                    if let Some(s) = channel::scch(f) {
                        self.scch(&s, t, out);
                    }
                } else {
                    match channel::sacch(f) {
                        Some(s) => {
                            self.ran = Some(s.sr.ran);
                            if superframe {
                                if let Some(o) = self.sf.push(&s) {
                                    msgs.push(Message::parse(&o, Context::Traffic, false));
                                }
                            }
                        }
                        None => self.sf.miss(),
                    }
                }
                // Messages first (a VCALL in the first half names the voice after it).
                for h in 0..2 {
                    if facch[h] {
                        if let Some((o, _)) = channel::facch1(f, h) {
                            msgs.push(Message::parse(&o, Context::Traffic, false));
                        }
                    }
                }
                for m in msgs.drain(..) {
                    self.message(m, t, out);
                }
                for h in 0..2 {
                    if !facch[h] && !idle {
                        for k in 0..2 {
                            let (d, r) = f.voice_frame(2 * h + k);
                            self.voice(&d, &r, t + VCH_S * (2 * h + k) as f64, out);
                        }
                    }
                }
            }
            Body::Facch2 => {
                if let Some((_, o, _)) = channel::udch(f) {
                    self.message(Message::parse(&o, Context::Traffic, false), t, out);
                }
            }
            Body::Udch => {}
        }
    }

    /// A Type-D SCCH: INFO1 the cipher, INFO2 / INFO4 the destination,
    /// INFO3 the source (special IDs 2041–2046 are the repeater's own news).
    fn scch(&mut self, s: &channel::Scch, t: f64, out: &mut Vec<Out>) {
        let special = (2041..=2046).contains(&s.short_id());
        match s.info() {
            1 => {
                let encrypted = s.cipher() != 0;
                if encrypted != self.encrypted {
                    self.encrypted = encrypted;
                    out.push(Out { t, out: TrackerOut::Info { source: self.source, emergency: self.emergency, encrypted } });
                }
            }
            2 | 4 if !special && s.short_id() != 0 => {
                if s.repeater() == 31 {
                    // The end of a transmission ("go to repeater" 31).
                    self.source = None;
                    self.encrypted = false;
                    return;
                }
                self.talkgroup = Some(s.id() as u32);
                self.group = s.group();
            }
            3 if s.short_id() != 0 => {
                let src = Some(s.id() as u32);
                if self.source != src {
                    self.source = src;
                    out.push(Out { t, out: TrackerOut::Info { source: src, emergency: self.emergency, encrypted: self.encrypted } });
                }
            }
            _ => {}
        }
    }

    fn message(&mut self, m: Message, t: f64, out: &mut Vec<Out>) {
        match &m {
            Message::VCall { head, cipher, .. } => {
                let tg = (head.destination != 0).then_some(head.destination as u32);
                let src = (head.source != 0).then_some(head.source as u32);
                let encrypted = *cipher != 0;
                let fresh = self.source != src || self.encrypted != encrypted || self.emergency != head.emergency() || (tg.is_some() && self.talkgroup != tg);
                if tg.is_some() {
                    self.talkgroup = tg;
                }
                if self.source.is_some() && self.source != src {
                    // Another radio took over mid-call: the alias heard so far isn't its.
                    self.aliases.reset();
                    self.alias = None;
                }
                self.group = head.group();
                self.source = src;
                self.encrypted = encrypted;
                self.emergency = head.emergency();
                self.efr = head.efr();
                if fresh {
                    out.push(Out { t, out: TrackerOut::Info { source: src, emergency: self.emergency, encrypted } });
                }
                self.name_source(t, out);
            }
            Message::TalkerAlias(seg) => {
                // Kenwood radios name themselves in the SACCH and FACCH1, whole once its checksum is good.
                if let Some(alias) = self.aliases.push(*seg) {
                    self.alias = Some(alias);
                    self.name_source(t, out);
                }
            }
            Message::TxRel { .. } | Message::TxRelEx { .. } | Message::Disc { .. } => {
                // End of the transmission: what it was doesn't carry over to the next.
                self.source = None;
                self.encrypted = false;
                self.emergency = false;
                self.efr = false;
                self.aliases.reset();
                self.alias = None;
                self.dec.reset();
            }
            _ => {}
        }
        if self.keep_messages {
            self.messages.push((t, m));
        }
    }

    /// Tell the engine the talker's alias, once the VCALL has said who that is.
    fn name_source(&mut self, t: f64, out: &mut Vec<Out>) {
        if let Some(unit) = self.source {
            let Some(alias) = self.alias.take() else { return };
            out.push(Out { t, out: TrackerOut::Alias(Alias { unit, alias, source: "KenwoodNXDN", talkgroup: self.talkgroup }) });
        }
    }

    fn voice(&mut self, d: &[u8; 36], r: &[f32; 72], t: f64, out: &mut Vec<Out>) {
        self.vch += 1;
        if self.encrypted || self.efr || !self.vocode {
            return;
        }
        let v = decode_vcw(d, Some(r));
        let mut buf = [0f32; FRAME_SAMPLES];
        let kind = self.dec.ambe(&v.bits, v.errs, &mut buf);
        if kind != Kind::Voice {
            self.bad_vch += 1;
        }
        mbe::to_limited(&mut buf);
        let vf = VoiceFrame { codec: Codec::Ambe, bits: v.bits.to_vec(), e0: 0, errs: v.errs, erased: false, kind };
        out.push(Out { t, out: TrackerOut::Audio(buf.to_vec(), vf) });
    }

    /// The messages kept since the last call.
    pub fn take_messages(&mut self) -> Vec<(f64, Message)> {
        std::mem::take(&mut self.messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Receiver;
    use crate::nxdn::layer3::{CallHead, CALL_CONFERENCE};
    use crate::nxdn::synth::{dibits, modulate, Tx};
    use crate::nxdn::Rate;

    fn run(tx: &Tx) -> (NxdnVoice, Vec<Out>) {
        let mut d = vec![0u8, 1, 2, 3].repeat(200);
        d.extend(dibits(&tx.frames()));
        d.extend(vec![3u8, 2, 1, 0].repeat(400));
        let fs = 24_000.0;
        let mut ph = 0.0;
        let iq = modulate(&d, None, tx.rate, fs, -200.0, 1.0, &mut ph);
        let mut rx = tx.rate.receiver(fs);
        let mut syms = Vec::new();
        rx.push(&iq, &mut syms);
        let mut v = NxdnVoice::new(1);
        let mut out = Vec::new();
        v.push(&syms, 0.0, fs, &mut out);
        (v, out)
    }

    fn tx(rate: Rate, cipher: u8) -> Tx {
        Tx { rate, ran: 3, head: CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source: 501, destination: 3100 }, cipher, superframes: 4, rf: 2, outbound: true, alias: None }
    }

    #[test]
    fn a_call_names_its_group_source_and_ran_and_has_audio() {
        for rate in [Rate::N48, Rate::N96] {
            let (v, out) = run(&tx(rate, 0));
            assert_eq!(v.talkgroup(), Some(3100), "{rate:?}");
            assert_eq!(v.ran(), Some(3));
            assert!(out.iter().any(|o| matches!(o.out, TrackerOut::Info { source: Some(501), encrypted: false, .. })));
            let audio = out.iter().filter(|o| matches!(o.out, TrackerOut::Audio(..))).count();
            // 4 superframes: 16 frames × 4 codewords (at 9600 every other frame is FACCH1).
            let want = if rate == Rate::N96 { 32 } else { 64 };
            assert_eq!(audio, want, "{rate:?}");
        }
    }

    fn heard(v: &mut NxdnVoice, o: &[u8], out: &mut Vec<Out>) {
        v.message(Message::parse(o, Context::Traffic, false), 0.0, out);
    }

    fn aliases(out: &[Out]) -> Vec<(u32, String, Option<u32>)> {
        out.iter()
            .filter_map(|o| match &o.out {
                TrackerOut::Alias(a) => Some((a.unit, a.alias.clone(), a.talkgroup)),
                _ => None,
            })
            .collect()
    }

    fn vcall(source: u16) -> Vec<u8> {
        let head = CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source, destination: 3100 };
        crate::nxdn::layer3::build::vcall(&head, 0, 0)
    }

    #[test]
    fn a_kenwood_alias_names_the_radio_that_called() {
        let mut v = NxdnVoice::new(1);
        let mut out = Vec::new();
        heard(&mut v, &vcall(501), &mut out);
        for _ in 0..3 {
            for s in crate::nxdn::alias::build::segments("E12 CAPT") {
                heard(&mut v, &s, &mut out);
            }
        }
        assert_eq!(aliases(&out), [(501, "E12 CAPT".to_string(), Some(3100))]);
    }

    #[test]
    fn an_alias_heard_before_the_call_names_its_radio_afterwards() {
        let mut v = NxdnVoice::new(1);
        let mut out = Vec::new();
        for s in crate::nxdn::alias::build::segments("MEDIC 7") {
            heard(&mut v, &s, &mut out);
        }
        assert!(aliases(&out).is_empty());
        heard(&mut v, &vcall(77), &mut out);
        assert_eq!(aliases(&out), [(77, "MEDIC 7".to_string(), Some(3100))]);
    }

    #[test]
    fn the_next_radio_gets_its_own_alias_not_the_last_ones() {
        let mut v = NxdnVoice::new(1);
        let mut out = Vec::new();
        heard(&mut v, &vcall(1), &mut out);
        for s in crate::nxdn::alias::build::segments("ALPHA") {
            heard(&mut v, &s, &mut out);
        }
        // Another radio keys up without an alias of its own: it isn't called ALPHA.
        heard(&mut v, &vcall(2), &mut out);
        for _ in 0..2 {
            heard(&mut v, &vcall(2), &mut out);
        }
        assert_eq!(aliases(&out), [(1, "ALPHA".to_string(), Some(3100))]);
        for s in crate::nxdn::alias::build::segments("BRAVO") {
            heard(&mut v, &s, &mut out);
        }
        assert_eq!(aliases(&out).last(), Some(&(2, "BRAVO".to_string(), Some(3100))));
    }

    #[test]
    fn an_encrypted_call_is_flagged_and_not_vocoded() {
        let (_, out) = run(&tx(Rate::N48, 3));
        assert!(out.iter().any(|o| matches!(o.out, TrackerOut::Info { encrypted: true, .. })));
        assert!(!out.iter().any(|o| matches!(o.out, TrackerOut::Audio(..))));
    }
}
