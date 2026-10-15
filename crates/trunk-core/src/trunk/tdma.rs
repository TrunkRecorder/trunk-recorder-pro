//! One Phase 2 TDMA voice channel's tracker: slot packets → descramble →
//! burst type → voice codewords (AMBE+2, per logical channel) or MAC PDUs
//! (PTT / END_PTT: who is talking, which cipher). Two logical channels
//! (TDMA slots 0 and 1) share the frequency; each has its own vocoder and
//! burst counting. After op25 p25p2_tdma.cc handle_packet. With the
//! system's keys, encrypted voice is decrypted (AMBE frame f of a
//! superframe: 4 per 4V burst, then the 2V's two).

use super::frames::{Codec, VoiceFrame};
use super::voice::{CipherId, TrackerOut};
use crate::ambe::decode_vcw;
use crate::mbe::{self, Kind, FRAME_SAMPLES};
use crate::p25::alias::{is_alias_msg, mac_messages, mac_talker, AliasLc, MacAliases};
use crate::p25::gps;
use crate::p25::phase2::{
    decode_acch, decode_ess, duid_decode, isch_lookup, parse_mac_ptt, read_ess_a, read_ess_b, xor_mask, Packet, BURST_2V, BURST_4V,
    BURST_FACCH_S, BURST_FACCH_U, BURST_LCCH_S, BURST_SACCH_S, BURST_SACCH_U, BURST_DIBITS, MAC_ACTIVE, MAC_END_PTT, MAC_HANGTIME, MAC_IDLE, MAC_PTT,
    MAC_SIGNAL, SLOT_CHANNEL, SLOT_DIBITS,
};
use crate::p25::crypt::{next_mi, prev_mi, Keyring, Keystream, Phase};
use crate::p25::voice::{EncryptionSync, ALGID_CLEAR};
use std::sync::Arc;

/// A logical channel's voice bursts more than this far apart (s) have a
/// whole superframe, and so a 2V burst, between them.
const SUPERFRAME_GAP_S: f64 = 0.3;

struct Slot {
    dec: mbe::Decoder,
    active: bool,
    encrypted: bool,
    algid: u8,
    burst_id: i32,
    first4v: i32,
    ess_b: [u8; 16],
    next_algid: u8,
    /// The encryption sync the current superframe's voice is under (the
    /// MAC_PTT's, then each 2V burst's ESS for the superframe after it), and
    /// the one the last ESS brought.
    es: Option<EncryptionSync>,
    es_next: Option<EncryptionSync>,
    /// The cipher last reported for this transmission.
    cipher: Option<CipherId>,
    /// The keystream of the superframe `es` is for, when the system has the key.
    ks: Option<Keystream>,
    /// Some voice of this transmission was decrypted: voice that can't be
    /// now (its sync lost) is muted, not dropped.
    decrypting: bool,
    /// The last voice burst heard (burst 0..4 of its superframe) and when,
    /// to notice a 2V that never arrived.
    last_vb: Option<(i32, f64)>,
    /// Joined mid-transmission with no PTT heard: whether the voice is
    /// encrypted is unknown until the first ESS.
    unsure: bool,
    /// Voice held until the superframe's ESS (frame of the superframe,
    /// bits, errors): while `unsure`, or encrypted with no sync (joined late,
    /// or the last one lost). The ESS gives the next superframe's MI, and
    /// the LFSR run back the one this voice is under.
    held: Vec<(Option<usize>, [u8; 49], u32)>,
    end_s: f64,
    /// Who is talking (MAC_PTT, Group Voice Channel User), until the transmission ends.
    talker: Option<u32>,
    talkgroup: Option<u32>,
    aliases: MacAliases,
}

impl Slot {
    /// The voice from now on is under `es`: its keystream, when the system
    /// has the key.
    fn set_es(&mut self, es: Option<EncryptionSync>, keys: &Option<Arc<Keyring>>) {
        self.es = es;
        self.ks = match (keys, &es) {
            (Some(keys), Some(es)) => keys.keystream(es, self.talkgroup, Phase::Two),
            _ => None,
        };
    }

    /// Whether a 2V burst went missing since the last voice burst: burst
    /// `b` (now, at `t`) starts a superframe the last 2V didn't end, or a
    /// whole superframe passed unheard.
    fn lost_2v(&self, b: i32, t: f64) -> bool {
        let Some((last, lt)) = self.last_vb else { return false };
        t - lt > SUPERFRAME_GAP_S || (b <= last && last != 4)
    }

    /// One AMBE frame (`f`: its frame of the superframe, if known):
    /// decrypted when it must be and can be, then vocoded. Returns whether
    /// the vocoder took it for something other than voice.
    fn voice(&mut self, c: usize, f: Option<usize>, mut bits: [u8; 49], mut errs: u32, out: &mut Vec<(usize, TrackerOut)>) -> bool {
        let mut erased = false;
        if self.encrypted {
            match (&self.ks, f) {
                (Some(ks), Some(f)) => {
                    ks.apply_p2(f, &mut bits);
                    if !std::mem::replace(&mut self.decrypting, true) {
                        out.push((c, TrackerOut::Decrypted));
                    }
                }
                // Too many errors to be voice: repeats, then mutes.
                _ if self.decrypting => (bits, errs, erased) = ([0; 49], 99, true),
                _ => return false,
            }
        }
        let mut buf = [0f32; FRAME_SAMPLES];
        let kind = self.dec.ambe(&bits, errs, &mut buf);
        mbe::to_limited(&mut buf);
        let frame = VoiceFrame { codec: Codec::Ambe, bits: bits.to_vec(), e0: 0, errs, erased, kind };
        out.push((c, TrackerOut::Audio(buf.to_vec(), frame)));
        kind != Kind::Voice
    }

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
            es: None,
            es_next: None,
            cipher: None,
            ks: None,
            decrypting: false,
            last_vb: None,
            unsure: false,
            held: Vec::new(),
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
    /// The system's keys, to decrypt the voice it has keys for.
    keys: Option<Arc<Keyring>>,
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
    /// The system's keys changed (a container unlocked, or locked): each slot's
    /// voice from now on is decrypted with these (none, or no keys: not).
    pub fn set_keys(&mut self, keys: Option<Arc<Keyring>>) {
        self.keys = keys.filter(|k| !k.is_empty());
        for s in &mut self.slots {
            let es = s.es.take();
            s.set_es(es, &self.keys);
        }
    }

    /// `keys`: the system's, to decrypt the voice it has keys for.
    pub fn new(seed: u32, keys: Option<Arc<Keyring>>) -> Self {
        TdmaTracker {
            slots: [Slot::new(mbe::lcg(seed)), Slot::new(mbe::lcg(seed ^ 0x5a5a_5a5a))],
            mask: None,
            key: None,
            keys,
            soft: true,
            frames: 0,
            bad_frames: 0,
            undecodable: 0,
            mac_pdus: 0,
        }
    }

    /// The encryption sync logical channel `c`'s current superframe is under
    /// (None: clear, or not known yet).
    pub fn encryption_sync(&self, c: usize) -> Option<EncryptionSync> {
        self.slots[c].es
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
        let keys = &self.keys;
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
            // A 4V counted as the superframe's fifth burst: the 2V before it
            // never came, and it starts the next superframe.
            let skipped_2v = kind == BURST_4V && s.burst_id == 4;
            if skipped_2v {
                s.burst_id = 0;
            }
            if !s.active || t - s.end_s > 1.0 {
                // Joined mid-transmission (no PTT heard): cipher unknown until the ESS.
                s.dec.reset();
                s.active = true;
                s.encrypted = false;
                s.algid = ALGID_CLEAR;
                s.set_es(None, keys);
                s.es_next = None;
                s.cipher = None;
                s.decrypting = false;
                s.last_vb = None;
                s.unsure = true;
                s.held.clear();
            }
            s.end_s = t;
            if skipped_2v || s.lost_2v(s.burst_id, t) {
                // Its ESS, the sync for the superframe after it, is gone
                // with it: wait for the next good one. Voice held for it
                // can't be decrypted now.
                s.es_next = None;
                s.set_es(None, keys);
                if s.unsure {
                    // Not known to be clear: not played.
                    s.held.clear();
                }
                for (f, bits, errs) in std::mem::take(&mut s.held) {
                    self.bad_frames += s.voice(c, f, bits, errs, out) as u64;
                }
            }
            s.last_vb = Some((s.burst_id, t));
            // ESS (op25 handle_4V2V_ess)
            if s.burst_id < 4 {
                let hb = read_ess_b(&x);
                let b = 4 * s.burst_id as usize;
                s.ess_b[b..b + 4].copy_from_slice(&hb);
            } else {
                let ess = decode_ess(&s.ess_b, &read_ess_a(&x));
                if let Some(ess) = ess {
                    s.next_algid = ess.algid;
                    let es = EncryptionSync { algid: ess.algid, keyid: ess.keyid, mi: ess.mi };
                    s.es_next = Some(es);
                    if ess.algid != ALGID_CLEAR {
                        Self::set_encryption(s, c, ess.algid, out);
                        Self::report_cipher(s, c, &es, out);
                    }
                }
                // No sync for this superframe: the ESS's, run back.
                if let Some(es) = s.es_next.filter(|es| es.algid != ALGID_CLEAR && s.es.is_none()) {
                    s.set_es(Some(EncryptionSync { mi: prev_mi(&es.mi), ..es }), keys);
                }
                s.unsure = false;
                for (f, bits, errs) in std::mem::take(&mut s.held) {
                    self.bad_frames += s.voice(c, f, bits, errs, out) as u64;
                }
            }
            let at: &[usize] = if kind == BURST_4V { &[11, 48, 96, 133] } else { &[11, 48] };
            for (j, &a) in at.iter().enumerate() {
                let v = decode_vcw(&x[a..a + 36], self.soft.then(|| rel(a)));
                self.frames += 1;
                // Its frame of the superframe (none where the count is off).
                let f = match kind {
                    BURST_2V => Some(16 + j),
                    _ => (0..4).contains(&s.burst_id).then(|| 4 * s.burst_id as usize + j),
                };
                if s.unsure || (s.encrypted && s.ks.is_none() && keys.is_some()) {
                    s.held.push((f, v.bits, v.errs));
                } else {
                    self.bad_frames += s.voice(c, f, v.bits, v.errs, out) as u64;
                }
            }
            if kind == BURST_2V {
                let a = s.next_algid;
                Self::set_encryption(s, c, a, out);
                // The next superframe is under the ESS this one brought; lost
                // to errors, the MI it would have said follows from the last.
                let es = match s.es_next.take() {
                    Some(es) => Some(es).filter(|es| es.algid != ALGID_CLEAR),
                    None => s.es.map(|es| EncryptionSync { mi: next_mi(&es.mi), ..es }),
                };
                s.set_es(es, keys);
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
                    s.cipher = None;
                    s.es_next = None;
                    s.decrypting = false;
                    s.last_vb = None;
                    s.unsure = false;
                    s.held.clear();
                    s.set_es(s.encrypted.then_some(EncryptionSync { algid: ptt.algid, keyid: ptt.keyid, mi: ptt.mi }), keys);
                    if let Some(es) = s.es {
                        Self::report_cipher(s, c, &es, out);
                    }
                }
                MAC_END_PTT => {
                    s.active = false;
                    s.algid = ALGID_CLEAR;
                    s.set_es(None, keys);
                    s.last_vb = None;
                    s.unsure = false;
                    // Voice still waiting for a sync that won't come.
                    for (f, bits, errs) in std::mem::take(&mut s.held) {
                        self.bad_frames += s.voice(c, f, bits, errs, out) as u64;
                    }
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
                    if let Some(p) = gps::mac(&m, s.talker) {
                        out.push((c, TrackerOut::Position(p)));
                    }
                }
                if pdu.opcode == MAC_HANGTIME {
                    s.talker = None;
                }
            }
        }
    }

    fn report_cipher(s: &mut Slot, c: usize, es: &EncryptionSync, out: &mut Vec<(usize, TrackerOut)>) {
        let id = CipherId { algid: es.algid, keyid: es.keyid };
        if s.active && s.cipher != Some(id) {
            s.cipher = Some(id);
            out.push((c, TrackerOut::Cipher(id)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::crypt::{ALGID_ADP, ALGID_AES_128, ALGID_AES_256, ALGID_DES_OFB, ALGID_TDES};
    use crate::p25::synth::{ambe, Tx2};

    const SEED: (u32, u32, u32) = (0x293, 0x1a5, 0xbee00);

    fn es(algid: u8, keyid: u16) -> EncryptionSync {
        EncryptionSync { algid, keyid, mi: [0x31, 0x4f, 0x02, 0xa7, 0x9c, 0x55, 0xe0, 0x18, 0] }
    }

    fn tx(cipher: Option<(EncryptionSync, Vec<u8>)>, superframes: usize) -> Tx2 {
        Tx2 { nac: SEED.0, sys_id: SEED.1, wacn: SEED.2, cipher, voice: (0..18 * superframes).map(ambe).collect() }
    }

    fn keyring(algid: u8, keyid: u16, key: &[u8]) -> Arc<Keyring> {
        let mut k = Keyring::default();
        k.add(algid, Some(keyid), key.to_vec(), vec![]).unwrap();
        Arc::new(k)
    }

    struct Heard {
        cipher: Vec<CipherId>,
        decrypted: usize,
        voice: Vec<([u8; 49], bool)>,
    }

    fn hear(packets: &[(Packet, f64)], keys: Option<Arc<Keyring>>) -> Heard {
        let mut t = TdmaTracker::new(1, keys);
        t.set_key(SEED.0, SEED.1, SEED.2);
        let mut out = Vec::new();
        for (p, at) in packets {
            t.packet(p, *at, &mut out);
        }
        let mut h = Heard { cipher: Vec::new(), decrypted: 0, voice: Vec::new() };
        for (c, o) in out {
            assert_eq!(c, 0);
            match o {
                TrackerOut::Cipher(id) => h.cipher.push(id),
                TrackerOut::Decrypted => h.decrypted += 1,
                TrackerOut::Audio(_, f) => h.voice.push((f.bits.try_into().unwrap(), f.erased)),
                _ => {}
            }
        }
        h
    }

    /// Clear Phase 2 voice comes through as sent (the bursts are right).
    #[test]
    fn clear_bursts_decode_as_built() {
        let t = tx(None, 2);
        let h = hear(&t.packets(), None);
        assert_eq!(h.voice.len(), t.voice.len());
        assert!(h.voice.iter().zip(&t.voice).all(|((b, e), v)| !e && b == v));
    }

    /// Every cipher, all of it: no MAC_PTT, so the first superframe is held
    /// until its ESS, and decrypted with the MI before the ESS's.
    #[test]
    fn each_cipher_decrypts_phase2_voice() {
        let key = |n: usize| (0..n).map(|i| (i * 37 + 11) as u8).collect::<Vec<u8>>();
        for (algid, len) in [(ALGID_ADP, 5), (ALGID_DES_OFB, 8), (ALGID_TDES, 24), (ALGID_AES_128, 16), (ALGID_AES_256, 32)] {
            let t = tx(Some((es(algid, 0x0009), key(len))), 4);
            let h = hear(&t.packets(), Some(keyring(algid, 0x0009, &key(len))));
            assert_eq!(h.cipher, [CipherId { algid, keyid: 0x0009 }], "{algid:#x}");
            assert_eq!(h.decrypted, 1, "{algid:#x}");
            assert_eq!(h.voice.len(), t.voice.len(), "{algid:#x}");
            for (i, (bits, erased)) in h.voice.iter().enumerate() {
                assert!(!erased && *bits == t.voice[i], "{algid:#x}: frame {i} isn't the clear voice");
            }
        }
    }

    /// No key, or the wrong one.
    #[test]
    fn without_the_key_phase2_stays_secret() {
        let t = tx(Some((es(ALGID_AES_256, 0x370E), vec![1; 32])), 3);
        for keys in [None, Some(Arc::new(Keyring::default()))] {
            let h = hear(&t.packets(), keys);
            assert_eq!((h.decrypted, h.voice.len()), (0, 0));
        }
        let h = hear(&t.packets(), Some(keyring(ALGID_AES_256, 0x370E, &[2; 32])));
        assert_eq!(h.voice.len(), 54);
        assert!(h.voice.iter().zip(&t.voice).all(|((b, _), v)| b != v));
    }

    /// An ESS that fails its FEC: the next MI follows from the LFSR.
    #[test]
    fn a_damaged_ess_is_predicted() {
        let t = tx(Some((es(ALGID_ADP, 0x1B50), vec![9, 8, 7, 6, 5])), 4);
        let mut p = t.packets();
        // Superframe 1's 2V: 20 of its ESS-A hexbits wrong.
        for i in 0..20 {
            p[9].0.dibits[10 + 84 + 3 * i + (i > 15) as usize] ^= 1;
        }
        let mask = xor_mask(SEED.0, SEED.1, SEED.2);
        let burst = |p: &Packet| -> [u8; BURST_DIBITS] { std::array::from_fn(|i| p.dibits[10 + i] ^ mask[p.sf_slot * SLOT_DIBITS + i]) };
        let mut ess_b = [0u8; 16];
        for b in 0..4 {
            ess_b[4 * b..4 * b + 4].copy_from_slice(&read_ess_b(&burst(&p[5 + b].0)));
        }
        assert!(decode_ess(&ess_b, &read_ess_a(&burst(&p[9].0))).is_none(), "the ESS is broken");
        let h = hear(&p, Some(keyring(ALGID_ADP, 0x1B50, &[9, 8, 7, 6, 5])));
        assert_eq!(h.voice.len(), 72);
        assert!(h.voice.iter().zip(&t.voice).all(|((b, e), v)| !e && b == v));
    }

    /// A 2V burst lost: the superframe after it is held for its own ESS,
    /// whose MI run back is its own. Only the 2V's voice is missing.
    #[test]
    fn a_lost_2v_costs_only_its_own_voice() {
        let t = tx(Some((es(ALGID_AES_128, 0x4319), vec![3; 16])), 4);
        let mut p = t.packets();
        p.remove(9);
        let h = hear(&p, Some(keyring(ALGID_AES_128, 0x4319, &[3; 16])));
        let want: Vec<&[u8; 49]> = t.voice.iter().enumerate().filter(|(i, _)| !(34..36).contains(i)).map(|(_, v)| v).collect();
        assert_eq!(h.voice.len(), want.len());
        assert!(h.voice.iter().zip(want).all(|((b, e), v)| !e && b == v));
    }
}
