//! One Phase 2 TDMA voice channel's tracker: slot packets → descramble →
//! burst type → voice codewords (AMBE+2, per logical channel) or MAC PDUs
//! (PTT / END_PTT: who is talking, which cipher). Two logical channels
//! (TDMA slots 0 and 1) share the frequency; each has its own vocoder and
//! burst counting. After op25 p25p2_tdma.cc handle_packet.

use super::frames::{Codec, VoiceFrame};
use super::voice::TrackerOut;
use crate::ambe::decode_vcw;
use crate::mbe::{self, Kind, FRAME_SAMPLES};
use crate::p25::alias::{is_alias_msg, mac_messages, mac_talker, AliasLc, MacAliases};
use crate::p25::phase2::{
    decode_acch, decode_ess, duid_decode, isch_lookup, parse_mac_ptt, read_ess_a, read_ess_b, xor_mask, Packet, BURST_2V, BURST_4V,
    BURST_FACCH_S, BURST_FACCH_U, BURST_LCCH_S, BURST_SACCH_S, BURST_SACCH_U, BURST_DIBITS, MAC_ACTIVE, MAC_END_PTT, MAC_HANGTIME, MAC_IDLE, MAC_PTT,
    MAC_SIGNAL, SLOT_CHANNEL, SLOT_DIBITS,
};
use crate::p25::voice::ALGID_CLEAR;

struct Slot {
    dec: mbe::Decoder,
    active: bool,
    encrypted: bool,
    algid: u8,
    burst_id: i32,
    first4v: i32,
    ess_b: [u8; 16],
    next_algid: u8,
    end_s: f64,
    /// Who is talking (MAC_PTT, Group Voice Channel User), until the transmission ends.
    talker: Option<u32>,
    talkgroup: Option<u32>,
    aliases: MacAliases,
}

impl Slot {
    fn new(rng: mbe::Rng) -> Self {
        Slot {
            dec: mbe::Decoder::new(rng, mbe::Profile::Enhanced),
            active: false,
            encrypted: false,
            algid: ALGID_CLEAR,
            burst_id: -1,
            first4v: -1,
            ess_b: [0; 16],
            next_algid: ALGID_CLEAR,
            end_s: 0.0,
            talker: None,
            talkgroup: None,
            aliases: MacAliases::default(),
        }
    }
}

pub struct TdmaTracker {
    slots: [Slot; 2],
    mask: Option<Vec<u8>>,
    key: Option<(u32, u32, u32)>,
    /// Use bit reliabilities for the AMBE c1 word.
    pub soft: bool,
    /// Vocoder frames / repeated, muted or erased, over the channel's life.
    pub frames: u64,
    pub bad_frames: u64,
    /// Scrambled bursts seen before the key (WACN / System ID / NAC) was known.
    pub undecodable: u64,
    /// MAC PDUs decoded (PTT, END_PTT, …).
    pub mac_pdus: u64,
}

impl TdmaTracker {
    pub fn new(seed: u32) -> Self {
        TdmaTracker {
            slots: [Slot::new(mbe::lcg(seed)), Slot::new(mbe::lcg(seed ^ 0x5a5a_5a5a))],
            mask: None,
            key: None,
            soft: true,
            frames: 0,
            bad_frames: 0,
            undecodable: 0,
            mac_pdus: 0,
        }
    }

    /// The scrambler seed: NAC, System ID, WACN (from the control channel).
    pub fn set_key(&mut self, nac: u32, sys_id: u32, wacn: u32) {
        if self.key != Some((nac, sys_id, wacn)) {
            self.key = Some((nac, sys_id, wacn));
            self.mask = Some(xor_mask(nac, sys_id, wacn));
        }
    }

    /// One slot packet at time `t` (s); what it produced goes to `out` as
    /// (logical channel, output).
    pub fn packet(&mut self, p: &Packet, t: f64, out: &mut Vec<(usize, TrackerOut)>) {
        let c = SLOT_CHANNEL[p.sf_slot];
        let burstp = &p.dibits[10..];
        let kind = duid_decode(burstp);
        if kind < 0 {
            return;
        }
        let scrambled = matches!(kind, BURST_4V | BURST_2V | BURST_SACCH_S | BURST_LCCH_S | BURST_FACCH_S);
        let mut x = [0u8; BURST_DIBITS];
        x.copy_from_slice(&burstp[..BURST_DIBITS]);
        if scrambled {
            let Some(m) = self.mask.as_ref() else {
                self.undecodable += 1;
                return;
            };
            let base = p.sf_slot * SLOT_DIBITS;
            for (i, v) in x.iter_mut().enumerate() {
                *v ^= m[base + i];
            }
        }
        let rel = |a: usize| &p.rel[2 * (10 + a)..2 * (10 + a) + 72];
        let s = &mut self.slots[c];

        if kind == BURST_4V || kind == BURST_2V {
            // op25 track_vb
            let current = (p.sf_slot >> 1) as i32;
            s.burst_id += 1;
            s.burst_id = if kind == BURST_4V { s.burst_id % 5 } else { 4 };
            let last_rc = isch_lookup(&p.dibits);
            if kind == BURST_2V && last_rc >= 0 {
                s.first4v = (current + 1) % 5;
            }
            if s.first4v >= 0 && last_rc >= 0 {
                let mut cs = current;
                if cs < s.first4v {
                    cs += 5;
                }
                cs -= s.first4v;
                if cs != s.burst_id && cs > s.burst_id {
                    s.burst_id = cs;
                }
            }
            if !s.active || t - s.end_s > 1.0 {
                // Joined mid-transmission (no PTT heard): cipher unknown until the ESS.
                s.dec.reset();
                s.active = true;
                s.encrypted = false;
                s.algid = ALGID_CLEAR;
            }
            s.end_s = t;
            // ESS (op25 handle_4V2V_ess)
            if s.burst_id < 4 {
                let hb = read_ess_b(&x);
                let b = 4 * s.burst_id as usize;
                s.ess_b[b..b + 4].copy_from_slice(&hb);
            } else if let Some(ess) = decode_ess(&s.ess_b, &read_ess_a(&x)) {
                s.next_algid = ess.algid;
                if ess.algid != ALGID_CLEAR {
                    Self::set_encryption(s, c, ess.algid, out);
                }
            }
            let at: &[usize] = if kind == BURST_4V { &[11, 48, 96, 133] } else { &[11, 48] };
            for &a in at {
                let f = decode_vcw(&x[a..a + 36], self.soft.then(|| rel(a)));
                self.frames += 1;
                if s.encrypted {
                    continue;
                }
                let mut buf = [0f32; FRAME_SAMPLES];
                let kind = s.dec.ambe(&f.bits, f.errs, &mut buf);
                if kind != Kind::Voice {
                    self.bad_frames += 1;
                }
                mbe::to_limited(&mut buf);
                let frame = VoiceFrame { codec: Codec::Ambe, bits: f.bits.to_vec(), e0: 0, errs: f.errs, erased: false, kind };
                out.push((c, TrackerOut::Audio(buf.to_vec(), frame)));
            }
            if kind == BURST_2V {
                let a = s.next_algid;
                Self::set_encryption(s, c, a, out);
            }
            return;
        }

        if matches!(kind, BURST_SACCH_S | BURST_SACCH_U | BURST_FACCH_S | BURST_FACCH_U) {
            let Some(pdu) = decode_acch(&x, kind == BURST_FACCH_S || kind == BURST_FACCH_U) else { return };
            self.mac_pdus += 1;
            match pdu.opcode {
                MAC_PTT => {
                    let ptt = parse_mac_ptt(&pdu.bytes);
                    s.dec.reset();
                    s.active = true;
                    s.end_s = t;
                    s.algid = ptt.algid;
                    s.encrypted = ptt.algid != ALGID_CLEAR;
                    s.first4v = ((p.sf_slot >> 1) as i32 + pdu.offset as i32 + 1) % 5;
                    s.burst_id = -1;
                    s.talker = (ptt.source != 0).then_some(ptt.source);
                    s.talkgroup = (ptt.group != 0).then_some(ptt.group);
                    out.push((c, TrackerOut::Info { source: (ptt.source != 0).then_some(ptt.source), emergency: false, encrypted: s.encrypted }));
                }
                MAC_END_PTT => {
                    s.active = false;
                    s.algid = ALGID_CLEAR;
                    s.talker = None;
                }
                MAC_ACTIVE => s.first4v = if pdu.offset > 4 { 0 } else { pdu.offset as i32 },
                _ => {}
            }
            // The messages a SIGNAL / IDLE / ACTIVE / HANGTIME PDU carries (op25 decode_mac_msg).
            if matches!(pdu.opcode, MAC_SIGNAL | MAC_IDLE | MAC_ACTIVE | MAC_HANGTIME) {
                let bytes = if pdu.opcode == MAC_SIGNAL { &pdu.bytes[..pdu.bytes.len().min(18)] } else { &pdu.bytes[..] };
                for m in mac_messages(bytes) {
                    if let Some(u) = mac_talker(&m) {
                        s.talker = Some(u);
                    }
                    if is_alias_msg(&m) {
                        out.push((c, TrackerOut::AliasLc(AliasLc::MacFragment)));
                    }
                    if let Some(a) = s.aliases.msg(&m, s.talker, s.talkgroup) {
                        out.push((c, TrackerOut::Alias(a)));
                    }
                }
                if pdu.opcode == MAC_HANGTIME {
                    s.talker = None;
                }
            }
        }
    }

    fn set_encryption(s: &mut Slot, c: usize, algid: u8, out: &mut Vec<(usize, TrackerOut)>) {
        s.algid = algid;
        if s.active && algid != ALGID_CLEAR && !s.encrypted {
            s.encrypted = true;
            out.push((c, TrackerOut::Info { source: None, emergency: false, encrypted: true }));
        }
    }
}
