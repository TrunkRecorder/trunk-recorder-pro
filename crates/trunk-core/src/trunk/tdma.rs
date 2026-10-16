//! One Phase 2 TDMA voice channel's tracker: slot packets → descramble →
//! burst type → voice codewords (AMBE+2, per logical channel) or MAC PDUs
//! (who is talking, which cipher, where the voice frames fall). Two logical
//! channels (TDMA slots 0 and 1) share the frequency; each has its own
//! vocoder and burst counting. After op25 p25p2_tdma.cc handle_packet,
//! checked against TIA-102.BBAC / BBAC-1. With the system's keys, encrypted
//! voice is decrypted (AMBE frame f of a superframe: 4 per 4V burst, then
//! the 2V's two).

use super::frames::{Codec, VoiceFrame};
use super::calls::Flags;
use super::voice::{CipherId, Encryption, TrackerOut};
use crate::ambe::decode_vcw;
use crate::mbe::{self, Kind, FRAME_SAMPLES};
use crate::p25::alias::{is_alias_msg, AliasLc, MacAliases};
use crate::p25::crypt::{next_mi, prev_mi, Keyring, Keystream, Phase, SyncCheck};
use crate::p25::gps;
use crate::p25::mac::{parse_mac_end_ptt, parse_mac_ptt, Mac, MacPdu, MAC_END_PTT, MAC_HANGTIME, MAC_IDLE, MAC_PTT};
use crate::p25::phase2::{
    decode_acch, decode_ess, duid_decode, duid_decode_soft, read_ess_a, read_ess_b, xor_mask, Packet, BURST_2V, BURST_4V, BURST_DIBITS, BURST_FACCH_S,
    BURST_FACCH_U, BURST_LCCH_S, BURST_SACCH_S, BURST_SACCH_U, SLOT_CHANNEL, SLOT_DIBITS,
};
use crate::p25::voice::{EncryptionSync, ALGID_CLEAR};
use std::sync::Arc;

/// A logical channel's voice bursts more than this far apart (s) have a
/// whole superframe, and so a 2V burst, between them.
const SUPERFRAME_GAP_S: f64 = 0.3;

/// A unit address that names a radio: not "no one" or "everyone" (BAAC-C §2.4).
fn radio(id: u32) -> Option<u32> {
    (id != 0 && id != 0xff_ffff).then_some(id)
}

struct Slot {
    dec: mbe::Decoder,
    active: bool,
    voice: Encryption,
    /// Where the slot's voice frames fall: the position (0..5 among its
    /// non-SACCH bursts) of the superframe's first 4V, once a MAC PDU's
    /// offset or a 2V said.
    first4v: Option<usize>,
    /// The last voice burst's place in its superframe (0..3 4V, 4 the 2V).
    burst_id: i32,
    /// This superframe's ESS-B segments as they came (none: not heard).
    ess_b: [Option<([u8; 4], [f32; 4])>; 4],
    /// The encryption sync the current superframe's voice is under (the
    /// MAC_PTT's, then each 2V burst's ESS for the superframe after it), and
    /// the one the last ESS brought.
    es: Option<EncryptionSync>,
    es_next: Option<EncryptionSync>,
    /// Each ESS checked against the MI LFSR's prediction.
    chain: SyncCheck,
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
    /// Voice held until the superframe's ESS (frame of the superframe,
    /// bits, errors): of unknown encryption (joined with no MAC_PTT), or
    /// encrypted with no sync (joined late, or the last one lost). The ESS
    /// gives the next superframe's MI, and the LFSR run back the one this
    /// voice is under.
    held: Vec<(Option<usize>, [u8; 49], u32)>,
    end_s: f64,
    /// Who is talking (MAC_PTT, voice channel user messages), and what its
    /// messages said of the transmission, until it ends.
    talker: Option<u32>,
    flags: Flags,
    /// The group the MAC_PTT named, and the one the slot was granted for: a
    /// key by talkgroup is found with them.
    talkgroup: Option<u32>,
    granted: Option<u32>,
    /// Someone records this slot: its voice is vocoded.
    listened: bool,
    aliases: MacAliases,
}

impl Slot {
    fn new(rng: mbe::Rng) -> Self {
        Slot {
            dec: mbe::Decoder::new(rng, mbe::Profile::Enhanced),
            active: false,
            voice: Encryption::Unknown,
            first4v: None,
            burst_id: -1,
            ess_b: [None; 4],
            es: None,
            es_next: None,
            chain: SyncCheck::default(),
            cipher: None,
            ks: None,
            decrypting: false,
            last_vb: None,
            held: Vec::new(),
            end_s: 0.0,
            talker: None,
            flags: Flags::default(),
            talkgroup: None,
            granted: None,
            listened: true,
            aliases: MacAliases::default(),
        }
    }

    /// A new transmission: everything of the last one forgotten (but where
    /// the voice frames fall, which is the channel's).
    fn start(&mut self, t: f64, keys: &Option<Arc<Keyring>>) {
        self.dec.reset();
        self.active = true;
        self.voice = Encryption::Unknown;
        self.end_s = t;
        self.ess_b = [None; 4];
        self.es_next = None;
        self.chain.reset();
        self.cipher = None;
        self.decrypting = false;
        self.last_vb = None;
        self.held.clear();
        self.talker = None;
        self.flags = Flags::default();
        self.talkgroup = None;
        self.set_es(None, keys);
    }

    /// The voice from now on is under `es`: its keystream, when the system
    /// has the key (by talkgroup: the MAC_PTT's, or the grant's).
    fn set_es(&mut self, es: Option<EncryptionSync>, keys: &Option<Arc<Keyring>>) {
        self.es = es;
        self.ks = match (keys, &es) {
            (Some(keys), Some(es)) => keys.keystream(es, self.talkgroup.or(self.granted), Phase::Two),
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
        if self.voice.encrypted() {
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
        if !self.listened {
            return false;
        }
        let mut buf = [0f32; FRAME_SAMPLES];
        let kind = self.dec.ambe(&bits, errs, &mut buf);
        mbe::to_limited(&mut buf);
        let frame = VoiceFrame { codec: Codec::Ambe, bits: bits.to_vec(), e0: 0, errs, erased, kind };
        out.push((c, TrackerOut::Audio(buf.to_vec(), frame)));
        kind != Kind::Voice
    }

    /// Vocode the voice held for a sync (`clear`: none says it's encrypted,
    /// so it plays; else it is decrypted or muted as it can be).
    fn release_held(&mut self, c: usize, out: &mut Vec<(usize, TrackerOut)>) -> u64 {
        let mut bad = 0;
        for (f, bits, errs) in std::mem::take(&mut self.held) {
            bad += self.voice(c, f, bits, errs, out) as u64;
        }
        bad
    }

    /// What a sync says the voice is.
    fn set_voice(&mut self, v: Encryption, c: usize, out: &mut Vec<(usize, TrackerOut)>) {
        let was = self.voice.encrypted();
        self.voice = v;
        if self.active && v.encrypted() && !was {
            out.push((c, TrackerOut::Info { source: None, flags: Flags { encrypted: true, ..Default::default() } }));
        }
    }

    fn report_cipher(&mut self, c: usize, es: &EncryptionSync, out: &mut Vec<(usize, TrackerOut)>) {
        let id = CipherId { algid: es.algid, keyid: es.keyid };
        if self.active && self.cipher != Some(id) {
            self.cipher = Some(id);
            out.push((c, TrackerOut::Cipher(id)));
        }
    }

    /// A talker named (MAC_PTT, a voice channel user message) and what the
    /// message said (its service options): told when it is new, or says
    /// something new. (Encrypted as the voice is: a sync overrules a
    /// message's protected bit.)
    fn talker(&mut self, c: usize, source: Option<u32>, flags: Flags, out: &mut Vec<(usize, TrackerOut)>) {
        let (Some(src), true) = (source, self.active) else { return };
        let flags = Flags { encrypted: self.voice.encrypted(), ..flags };
        let merged = if self.talker == Some(src) { self.flags.merge(flags) } else { flags };
        if self.talker != Some(src) || merged != self.flags {
            (self.talker, self.flags) = (Some(src), merged);
            out.push((c, TrackerOut::Info { source: Some(src), flags: merged }));
        }
    }

    /// The transmission is over (MAC_END_PTT, hangtime, idle): voice still
    /// waiting for a sync that won't come goes out (or is dropped).
    fn end(&mut self, c: usize, keys: &Option<Arc<Keyring>>, out: &mut Vec<(usize, TrackerOut)>) -> u64 {
        let bad = self.release_held(c, out);
        self.active = false;
        self.set_es(None, keys);
        self.last_vb = None;
        self.talker = None;
        bad
    }
}

pub struct TdmaTracker {
    slots: [Slot; 2],
    mask: Option<Vec<u8>>,
    key: Option<(u32, u32, u32)>,
    /// The system's keys, to decrypt the voice it has keys for.
    keys: Option<Arc<Keyring>>,
    /// Use bit reliabilities (the DUID, the AMBE c1 word, the RS codes).
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

    /// The talkgroup each slot's call was granted for (None: no call there,
    /// and its voice isn't vocoded).
    pub fn listen(&mut self, slots: [Option<u32>; 2]) {
        for (s, tg) in self.slots.iter_mut().zip(slots) {
            s.listened = tg.is_some();
            if s.granted != tg {
                s.granted = tg;
                let es = s.es.take();
                s.set_es(es, &self.keys);
            }
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

    /// The scrambler seed: NAC (the color code), System ID, WACN (from the
    /// control channel, or the voice channel's network status).
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
        let brel = (self.soft && p.rel.len() >= 2 * (10 + BURST_DIBITS)).then(|| &p.rel[20..]);
        let kind = match brel {
            Some(r) => duid_decode_soft(burstp, r),
            None => duid_decode(burstp),
        };
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
        if kind == BURST_4V || kind == BURST_2V {
            self.voice_burst(c, kind, &x, brel, p, t, out);
        } else if matches!(kind, BURST_SACCH_S | BURST_SACCH_U | BURST_FACCH_S | BURST_FACCH_U) {
            let sacch = matches!(kind, BURST_SACCH_S | BURST_SACCH_U);
            if let Some(pdu) = decode_acch(&x, brel, !sacch) {
                self.mac_pdus += 1;
                self.mac(c, &pdu, sacch, p, t, out);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn voice_burst(&mut self, c: usize, kind: i8, x: &[u8; BURST_DIBITS], rel: Option<&[f32]>, p: &Packet, t: f64, out: &mut Vec<(usize, TrackerOut)>) {
        let keys = &self.keys;
        let s = &mut self.slots[c];
        // Its place among the slot's five non-SACCH bursts (the framer only
        // passes packets whose slot it knows), and so in the superframe.
        let pos = p.sf_slot >> 1;
        if kind == BURST_2V {
            s.first4v = Some((pos + 1) % 5);
        }
        s.burst_id = match s.first4v {
            _ if kind == BURST_2V => 4,
            Some(f) => ((pos + 5 - f) % 5) as i32,
            None => (s.burst_id + 1) % 5,
        };
        // A 4V where the 2V should be: the 2V before it never came, and it
        // starts the next superframe (the framing moved: anchored again).
        let skipped_2v = kind == BURST_4V && s.burst_id == 4;
        if skipped_2v {
            (s.burst_id, s.first4v) = (0, Some(pos));
        }
        if !s.active || t - s.end_s > 1.0 {
            // Joined mid-transmission (no MAC_PTT heard): cipher unknown until the ESS.
            s.start(t, keys);
        }
        s.end_s = t;
        if skipped_2v || s.lost_2v(s.burst_id, t) {
            // Its ESS, the sync for the superframe after it, is gone with it:
            // wait for the next good one. Voice held for it can't be
            // decrypted now; of unknown encryption, it isn't played.
            s.es_next = None;
            s.ess_b = [None; 4];
            s.set_es(None, keys);
            if s.voice == Encryption::Unknown {
                s.held.clear();
            }
            self.bad_frames += s.release_held(c, out);
        }
        s.last_vb = Some((s.burst_id, t));
        if s.burst_id < 4 {
            s.ess_b[s.burst_id as usize] = Some(read_ess_b(x, rel));
        } else {
            let (ess_a, ess_a_rel) = read_ess_a(x, rel);
            let heard = decode_ess(&s.ess_b, &ess_a, &ess_a_rel);
            s.ess_b = [None; 4];
            s.es_next = s.chain.next(s.es, heard);
            if let Some(es) = s.es_next {
                s.set_voice(Encryption::from_sync(&es), c, out);
                if es.algid != ALGID_CLEAR {
                    s.report_cipher(c, &es, out);
                    // No sync for this superframe: the ESS's, run back.
                    if s.es.is_none() {
                        s.set_es(Some(EncryptionSync { mi: prev_mi(&es.mi), ..es }), keys);
                    }
                }
            }
            self.bad_frames += s.release_held(c, out);
        }
        let at: &[usize] = if kind == BURST_4V { &[11, 48, 96, 133] } else { &[11, 48] };
        for (j, &a) in at.iter().enumerate() {
            let v = decode_vcw(&x[a..a + 36], rel.map(|r| &r[2 * a..2 * a + 72]));
            self.frames += 1;
            // Its frame of the superframe (none where the count is off).
            let f = match kind {
                BURST_2V => Some(16 + j),
                _ => (0..4).contains(&s.burst_id).then(|| 4 * s.burst_id as usize + j),
            };
            if s.voice == Encryption::Unknown || (s.voice.encrypted() && s.ks.is_none() && keys.is_some()) {
                s.held.push((f, v.bits, v.errs));
            } else {
                self.bad_frames += s.voice(c, f, v.bits, v.errs, out) as u64;
            }
        }
        if kind == BURST_2V {
            // The next superframe is under the ESS this one brought; lost to
            // errors, the MI it would have said follows from the last.
            let es = match s.es_next.take() {
                Some(es) => Some(es).filter(|es| es.algid != ALGID_CLEAR),
                None => s.es.map(|es| EncryptionSync { mi: next_mi(&es.mi), ..es }),
            };
            s.set_es(es, keys);
        }
    }

    fn mac(&mut self, c: usize, pdu: &MacPdu, sacch: bool, p: &Packet, t: f64, out: &mut Vec<(usize, TrackerOut)>) {
        let keys = &self.keys;
        let s = &mut self.slots[c];
        // The slot's next non-SACCH burst after this one: the superframe's
        // first after a SACCH, else the one after this FACCH's.
        let next = if sacch { 0 } else { (p.sf_slot >> 1) + 1 };
        match pdu.opcode {
            MAC_PTT => {
                let ptt = parse_mac_ptt(&pdu.bytes);
                let es = EncryptionSync { algid: ptt.algid, keyid: ptt.keyid, mi: ptt.mi };
                // A SACCH may repeat the transmission's MAC_PTT ("transitional
                // cases", BBAC-1 §7.3.2): the same one starts nothing new.
                let cipher = if ptt.algid == ALGID_CLEAR { s.voice == Encryption::Clear } else { s.cipher == Some(CipherId { algid: ptt.algid, keyid: ptt.keyid }) };
                let same = s.active && cipher && radio(ptt.source) == s.talker && (ptt.group != 0).then_some(ptt.group) == s.talkgroup;
                if !(sacch && same) {
                    s.start(t, keys);
                    s.talkgroup = (ptt.group != 0).then_some(ptt.group);
                    s.set_voice(Encryption::from_sync(&es), c, out);
                    if ptt.algid != ALGID_CLEAR {
                        s.set_es(Some(es), keys);
                        s.report_cipher(c, &es, out);
                    }
                    s.talker(c, radio(ptt.source), Flags::default(), out);
                }
                s.first4v = pdu.first_4v(next).or(s.first4v);
            }
            MAC_END_PTT => {
                // Sent unscrambled with the color code (BBAC §8.4.8): a
                // co-channel system's isn't this one's (BBAC-1 §7.3.3.1).
                let end = parse_mac_end_ptt(&pdu.bytes);
                if self.key.is_none_or(|(nac, _, _)| nac as u16 == end.color_code) {
                    self.bad_frames += s.end(c, keys, out);
                }
            }
            _ => {
                if let Some(f) = pdu.first_4v(next) {
                    s.first4v = Some(f);
                }
            }
        }
        // The messages a SIGNAL / IDLE / ACTIVE / HANGTIME PDU carries (op25 decode_mac_msg).
        for m in pdu.messages() {
            match m.decode() {
                Mac::GroupVoiceUser { opts, .. } | Mac::UnitVoiceUser { opts, .. } | Mac::PhoneVoiceUser { opts, .. } => {
                    // The 4th superframe's SACCH names the inbound talker, for
                    // it to check: listeners take the first three's (BBAC §3.2.2).
                    if !(sacch && p.ultraframe == Some(3)) {
                        s.talker(c, m.talker().and_then(radio), opts.into(), out);
                    }
                    if opts.protected() {
                        let v = s.voice.hinted();
                        s.set_voice(v, c, out);
                    }
                }
                Mac::PatchVoiceUser { .. } => {
                    if !(sacch && p.ultraframe == Some(3)) {
                        s.talker(c, m.talker().and_then(radio), Flags::default(), out);
                    }
                }
                // Unscrambled, with what scrambling is seeded from: the voice
                // channel says it before the control channel may have.
                Mac::NetworkStatus { wacn, sys_id, color_code } if self.mask.is_none() => {
                    self.key = Some((color_code as u32, sys_id as u32, wacn));
                    self.mask = Some(xor_mask(color_code as u32, sys_id as u32, wacn));
                }
                _ => {}
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
        // Between transmissions: the last is over, and the next starts afresh
        // (BBAC-1 §7.3.3.2, §7.3.4).
        if matches!(pdu.opcode, MAC_HANGTIME | MAC_IDLE) && s.active {
            self.bad_frames += s.end(c, keys, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::crypt::{ALGID_ADP, ALGID_AES_128, ALGID_AES_256, ALGID_DES_OFB, ALGID_TDES};
    use crate::p25::mac::{MAC_ACTIVE, MAC_END_PTT, MAC_HANGTIME, MAC_PTT};
    use crate::p25::phase2::{BURST_FACCH_S, BURST_FACCH_U, BURST_SACCH_S};
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
        let ess_b: [Option<([u8; 4], [f32; 4])>; 4] = std::array::from_fn(|b| Some(read_ess_b(&burst(&p[5 + b].0), None)));
        let (a, ar) = read_ess_a(&burst(&p[9].0), None);
        assert!(decode_ess(&ess_b, &a, &ar).is_none(), "the ESS is broken");
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

    /// A MAC PDU's octets: opcode, offset, then `rest`.
    fn pdu(opcode: u8, offset: u8, rest: &[u8]) -> Vec<u8> {
        let mut b = vec![opcode << 5 | offset << 2];
        b.extend_from_slice(rest);
        b
    }

    fn outs(t: &mut TdmaTracker, packets: &[(Packet, f64)]) -> Vec<TrackerOut> {
        let mut out = Vec::new();
        for (p, at) in packets {
            t.packet(p, *at, &mut out);
        }
        out.into_iter().map(|(_, o)| o).collect()
    }

    fn tracker() -> TdmaTracker {
        let mut t = TdmaTracker::new(1, None);
        t.set_key(SEED.0, SEED.1, SEED.2);
        t
    }

    /// A Group Voice Channel User (BBAC Figure 8-13) in a MAC_ACTIVE names
    /// the talker, emergency and all; the 4th superframe's SACCH (the
    /// inbound talker's, BBAC §3.2.2) doesn't.
    #[test]
    fn a_voice_channel_user_names_the_talker() {
        let tx = tx(None, 1);
        let vcu = |src: u32| pdu(MAC_ACTIVE, 7, &[0x01, 0x80, 0x08, 0x9f, (src >> 16) as u8, (src >> 8) as u8, src as u8]);
        let mut t = tracker();
        let mut p = tx.packets();
        p.push((tx.mac_packet(11, BURST_SACCH_S, &vcu(0x123456), Some(3)), 0.35));
        p.push((tx.mac_packet(11, BURST_SACCH_S, &vcu(0x654321), Some(0)), 0.36));
        let infos: Vec<(Option<u32>, bool)> = outs(&mut t, &p).into_iter().filter_map(|o| if let TrackerOut::Info { source, flags } = o { Some((source, flags.emergency)) } else { None }).collect();
        assert_eq!(infos, [(Some(0x654321), true)]);
    }

    /// BBAC-1 Table 8-7: a SACCH's MAC_PTT offset counts from the
    /// superframe's first burst.
    #[test]
    fn a_sacch_offset_counts_from_the_superframe_start() {
        let tx = tx(None, 1);
        let mut t = tracker();
        let ptt = pdu(MAC_PTT, 1, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, 0x12, 0x34, 0x56, 0x08, 0x9f]);
        outs(&mut t, &[(tx.mac_packet(11, BURST_SACCH_S, &ptt, Some(0)), 0.0)]);
        assert_eq!(t.slots[0].first4v, Some(1));
        assert_eq!((t.slots[0].talker, t.slots[0].talkgroup), (Some(0x123456), Some(0x089f)));
    }

    /// Hangtime ends the transmission (BBAC-1 §7.3.4): the next one isn't
    /// taken for its continuation.
    #[test]
    fn hangtime_ends_the_transmission() {
        let tx = tx(None, 1);
        let mut t = tracker();
        outs(&mut t, &tx.packets());
        assert!(t.slots[0].active);
        outs(&mut t, &[(tx.mac_packet(0, BURST_FACCH_S, &pdu(MAC_HANGTIME, 7, &[]), Some(0)), 0.4)]);
        assert!(!t.slots[0].active);
    }

    /// A co-channel system's MAC_END_PTT (another color code, BBAC §8.4.8)
    /// ends nothing here.
    #[test]
    fn another_systems_end_ptt_is_ignored() {
        let tx = tx(None, 1);
        let end = |cc: u16| pdu(MAC_END_PTT, 7, &[(cc >> 8) as u8, cc as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12, 0x34, 0x56, 0x08, 0x9f]);
        let mut t = tracker();
        outs(&mut t, &tx.packets());
        outs(&mut t, &[(tx.mac_packet(0, BURST_FACCH_U, &end(0x111), None), 0.4)]);
        assert!(t.slots[0].active, "another system's");
        outs(&mut t, &[(tx.mac_packet(0, BURST_FACCH_U, &end(SEED.0 as u16), None), 0.4)]);
        assert!(!t.slots[0].active);
    }

    /// The voice channel's own Network Status Broadcast (BBAC Figure 8-47,
    /// sent unscrambled) seeds the scrambling before the control channel does.
    #[test]
    fn the_voice_channel_names_its_scrambling() {
        let tx = tx(None, 1);
        let (w, sy, cc) = (SEED.2, SEED.1, SEED.0);
        let nsb = [0x7b, 0x01, (w >> 12) as u8, (w >> 4) as u8, ((w & 0xf) << 4 | sy >> 8) as u8, sy as u8, 0x10, 0x01, 0x70, (cc >> 8) as u8, cc as u8];
        let mut t = TdmaTracker::new(1, None);
        let mut p = vec![(tx.mac_packet(0, BURST_FACCH_U, &pdu(MAC_ACTIVE, 7, &nsb), None), 0.0)];
        p.extend(tx.packets());
        let heard = outs(&mut t, &p).into_iter().filter(|o| matches!(o, TrackerOut::Audio(..))).count();
        assert_eq!((t.undecodable, heard), (0, 18));
    }

    /// A slot nobody records isn't vocoded.
    #[test]
    fn an_unrecorded_slot_isnt_vocoded() {
        let tx = tx(None, 1);
        let mut t = tracker();
        t.listen([None, Some(101)]);
        assert!(!outs(&mut t, &tx.packets()).iter().any(|o| matches!(o, TrackerOut::Audio(..))));
    }
}
