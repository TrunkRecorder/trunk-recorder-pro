//! One voice channel's call tracker (Phase 1): HDU / LDU1 / LDU2 / TDU(LC)
//! frames → encryption and link-control state → IMBE vocoder → audio.
//! Takes receiver-diversity groups: header fields from the best NID, link
//! control / encryption sync from whichever copy decodes, each codeword from
//! the receiver the soft decoder trusted most.

use super::frames::{Codec, VoiceFrame};
use crate::mbe::{self, Kind, FRAME_SAMPLES};
use crate::p25::alias::{is_alias_lcw, AliasLc, LcAliases};
use crate::p25::diversity::{best_es, best_frame, best_imbe, best_lc, Group};
use crate::p25::frame::{HDU, LDU1, LDU2, TDU, TDULC};
use crate::p25::crypt::{next_mi, prev_mi, Keyring, Keystream, Phase};
use crate::p25::gps::LcPositions;
use crate::p25::voice::{decode_hdu, decode_tdulc, imbe_params_to_bits, ldu_codeword_end_bit, EncryptionSync, ALGID_CLEAR};
use super::voice::{CipherId, TrackerOut};
use std::sync::Arc;

/// Algorithm id when encryption is known but not which cipher.
const ALGID_UNKNOWN: i32 = -1;

/// An LDU's length, s (1728 symbols at 4800 baud).
const LDU_S: f64 = 0.18;

pub struct VoiceTracker {
    dec: mbe::Decoder,
    active: bool,
    encrypted: bool,
    algid: i32,
    /// The encryption sync the current superframe's voice is under (the
    /// HDU's, then each LDU2's for the superframe after it), and the one the
    /// last LDU2 brought.
    es: Option<EncryptionSync>,
    es_next: Option<EncryptionSync>,
    /// The cipher last reported for this transmission.
    cipher: Option<CipherId>,
    /// The system's keys, and the keystream of the superframe `es` is for.
    keys: Option<Arc<Keyring>>,
    ks: Option<Keystream>,
    /// Some voice of this transmission was decrypted: voice that can't be
    /// now (its sync lost to a fade) is muted, not dropped.
    decrypting: bool,
    /// The last LDU's DUID and time, to notice LDUs that never arrived.
    last_ldu: Option<(u8, f64)>,
    /// Encrypted voice with no sync (joined late, or the last LDU2 lost),
    /// held for the LDU2: its sync is the next superframe's, and the LFSR
    /// run back gives this one's. (LDU, frame, u, e0, errors, erased.)
    held: Vec<(usize, usize, [u32; 8], u32, u32, bool)>,
    tgid: Option<u32>,
    /// The unit link control named for the current transmission.
    talker: Option<u32>,
    aliases: LcAliases,
    positions: LcPositions,
    call_frames: u64,
    end_s: f64,
    /// Vocoder frames / repeated or muted, over the channel's life.
    pub frames: u64,
    pub bad_frames: u64,
}

impl VoiceTracker {
    /// `keys`: the system's, to decrypt the voice it has keys for.
    pub fn new(rng: mbe::Rng, vocoder: mbe::Profile, keys: Option<Arc<Keyring>>) -> Self {
        VoiceTracker {
            dec: mbe::Decoder::new(rng, vocoder),
            active: false,
            encrypted: false,
            algid: ALGID_CLEAR as i32,
            es: None,
            es_next: None,
            cipher: None,
            keys,
            ks: None,
            decrypting: false,
            last_ldu: None,
            held: Vec::new(),
            tgid: None,
            talker: None,
            aliases: LcAliases::default(),
            positions: LcPositions::default(),
            call_frames: 0,
            end_s: 0.0,
            frames: 0,
            bad_frames: 0,
        }
    }

    /// The talkgroup link control named for the current transmission.
    pub fn talkgroup(&self) -> Option<u32> {
        self.tgid
    }

    /// The encryption sync the current superframe's voice is under (None:
    /// clear, or not known yet).
    pub fn encryption_sync(&self) -> Option<EncryptionSync> {
        self.es
    }

    /// A frame group at time `t` (s, sample clock).
    pub fn group(&mut self, g: &Group, t: f64, out: &mut Vec<TrackerOut>) {
        let best = best_frame(g);
        match best.nid.duid {
            HDU => {
                let h = decode_hdu(&best.raw);
                self.algid = h.map_or(ALGID_CLEAR as i32, |h| h.es.algid as i32);
                self.start(t, h.map(|h| h.tgid as u32));
                self.set_es(h.map(|h| h.es).filter(|es| es.algid != ALGID_CLEAR));
                if let Some(es) = self.es {
                    self.report_cipher(&es, out);
                }
            }
            LDU1 | LDU2 => {
                self.touch(t);
                if self.lost_ldu2(best.nid.duid, t) {
                    // Its sync is gone with it: wait for the next good one.
                    // Voice held for it can't be decrypted now.
                    self.es_next = None;
                    self.set_es(None);
                    self.release_held(out);
                }
                self.last_ldu = Some((best.nid.duid, t));
                if best.nid.duid == LDU1 {
                    if let Some(lc) = best_lc(g) {
                        if lc.protected {
                            let a = if self.algid == ALGID_CLEAR as i32 { ALGID_UNKNOWN } else { self.algid };
                            self.set_encryption(a, out);
                        } else if lc.voice_user() {
                            if let (Some(cur), Some(tg)) = (self.tgid, lc.tgid) {
                                if cur != tg && self.call_frames > 0 {
                                    self.start(t, None);
                                }
                            }
                            if lc.tgid.is_some() {
                                self.tgid = lc.tgid;
                            }
                            if lc.source.is_some_and(|s| s != 0) {
                                self.talker = lc.source;
                            }
                            let svc = lc.svc_opts.unwrap_or(0);
                            if svc & 0x40 != 0 && self.algid == ALGID_CLEAR as i32 {
                                self.set_encryption(ALGID_UNKNOWN, out);
                            }
                            out.push(TrackerOut::Info { source: lc.source, emergency: svc & 0x80 != 0, encrypted: self.encrypted });
                        } else {
                            if let Some(a) = self.aliases.lcw(&lc.raw, self.talker, self.tgid) {
                                out.push(TrackerOut::Alias(a));
                            }
                            if let Some(p) = self.positions.lcw(&lc.raw, lc.mfid, self.talker) {
                                out.push(TrackerOut::Position(p));
                            }
                        }
                    }
                } else {
                    self.es_next = best_es(g);
                    if let Some(es) = self.es_next {
                        self.set_encryption(es.algid as i32, out);
                        if es.algid != ALGID_CLEAR {
                            self.report_cipher(&es, out);
                            // No sync for this superframe: the LDU2's, run back.
                            if self.es.is_none() {
                                self.set_es(Some(EncryptionSync { mi: prev_mi(&es.mi), ..es }));
                            }
                        }
                    }
                    self.release_held(out);
                }
                // A cut frame (the next sync came early, in every receiver):
                // from the first codeword that fails its FEC or runs past the
                // cut, nothing is trusted — erasures.
                let complete = g.iter().any(|f| f.complete);
                let cut_at = (!complete).then(|| best.raw.len());
                let chosen = best_imbe(g);
                let ldu = (best.nid.duid == LDU2) as usize;
                let mut erased = false;
                for (k, p) in chosen.iter().enumerate() {
                    if cut_at.is_some_and(|c| p.errs > 2 || ldu_codeword_end_bit(k) >= c) {
                        erased = true;
                    }
                    self.voice_frame(ldu, k, p.u, p.e0, p.errs, erased, out);
                }
                if best.nid.duid == LDU2 {
                    self.next_superframe();
                }
            }
            TDU | TDULC => {
                // Link control from whichever receiver's copy decodes. Motorola
                // sends talker aliases here, in the terminators after the voice.
                let lc = g.iter().filter(|f| f.nid.duid == TDULC).find_map(|f| decode_tdulc(&f.raw));
                if let Some(lc) = lc {
                    let sign = if lc.protected {
                        AliasLc::Protected
                    } else if is_alias_lcw(&lc.raw) {
                        AliasLc::Fragment
                    } else {
                        AliasLc::Clear
                    };
                    out.push(TrackerOut::AliasLc(sign));
                }
                if let Some(lc) = lc.filter(|lc| !lc.protected) {
                    if self.active && self.tgid.is_none() {
                        self.tgid = lc.tgid;
                    }
                    if let Some(a) = self.aliases.lcw(&lc.raw, self.talker, self.tgid) {
                        out.push(TrackerOut::Alias(a));
                    }
                    if let Some(p) = self.positions.lcw(&lc.raw, lc.mfid, self.talker) {
                        out.push(TrackerOut::Position(p));
                    }
                }
                self.active = false;
                self.algid = ALGID_CLEAR as i32;
                self.set_es(None);
                // Voice still waiting for a sync that won't come.
                self.release_held(out);
                self.last_ldu = None;
            }
            _ => {}
        }
    }

    fn start(&mut self, t: f64, tgid: Option<u32>) {
        self.dec.reset();
        self.active = true;
        self.tgid = tgid;
        self.talker = None;
        self.encrypted = self.algid != ALGID_CLEAR as i32;
        self.set_es(None);
        self.es_next = None;
        self.cipher = None;
        self.decrypting = false;
        self.last_ldu = None;
        self.held.clear();
        self.end_s = t;
        self.call_frames = 0;
    }

    /// An LDU2's voice is done: the next superframe is under the sync it
    /// brought. Lost to errors, the MI it would have said follows from the
    /// last one (the MI LFSR).
    fn next_superframe(&mut self) {
        let es = match self.es_next.take() {
            Some(es) => Some(es).filter(|es| es.algid != ALGID_CLEAR),
            None => self.es.map(|es| EncryptionSync { mi: next_mi(&es.mi), ..es }),
        };
        self.set_es(es);
    }

    /// The system's keys changed (a container unlocked, or locked): the voice
    /// from now on, a call in progress too, is decrypted with these (none, or
    /// no keys: not).
    pub fn set_keys(&mut self, keys: Option<Arc<Keyring>>) {
        self.keys = keys.filter(|k| !k.is_empty());
        let es = self.es;
        self.set_es(es);
    }

    /// The voice from now on is under `es`: its keystream, when the system
    /// has the key.
    fn set_es(&mut self, es: Option<EncryptionSync>) {
        self.es = es;
        self.ks = match (&self.keys, &es) {
            (Some(keys), Some(es)) => keys.keystream(es, self.tgid, Phase::One),
            _ => None,
        };
    }

    /// Whether an LDU2 went missing since the last LDU (an LDU1 after an
    /// LDU1, or a gap with an LDU2 in it): the sync it carried is lost.
    fn lost_ldu2(&self, duid: u8, t: f64) -> bool {
        let Some((last, lt)) = self.last_ldu else { return false };
        // LDUs since the last one, the one heard now included.
        let n = ((t - lt) / LDU_S).round().max(1.0) as u32;
        let expected = if n % 2 == 1 { if last == LDU1 { LDU2 } else { LDU1 } } else { last };
        let missed = n - 1;
        duid != expected || missed >= if last == LDU1 { 1 } else { 2 }
    }

    fn report_cipher(&mut self, es: &EncryptionSync, out: &mut Vec<TrackerOut>) {
        let c = CipherId { algid: es.algid, keyid: es.keyid };
        if self.active && self.cipher != Some(c) {
            self.cipher = Some(c);
            out.push(TrackerOut::Cipher(c));
        }
    }

    fn touch(&mut self, t: f64) {
        if self.active && t - self.end_s > 1.0 {
            // Quiet with no terminator: the next transmission re-learns its cipher.
            self.active = false;
            self.algid = ALGID_CLEAR as i32;
        }
        if !self.active {
            self.start(t, None);
        }
        self.end_s = t;
    }

    fn set_encryption(&mut self, algid: i32, out: &mut Vec<TrackerOut>) {
        self.algid = algid;
        if self.active && algid != ALGID_CLEAR as i32 && !self.encrypted {
            self.encrypted = true;
            out.push(TrackerOut::Info { source: None, emergency: false, encrypted: true });
        }
    }

    /// Vocode the voice held for a sync: decrypted if one came.
    fn release_held(&mut self, out: &mut Vec<TrackerOut>) {
        for (ldu, k, u, e0, errs, erased) in std::mem::take(&mut self.held) {
            self.vocode(ldu, k, u, e0, errs, erased, out);
        }
    }

    /// IMBE frame `k` of an LDU (`ldu` 0: LDU1, 1: LDU2): vocoded, or held
    /// for a sync when it is encrypted under none yet and there are keys.
    #[allow(clippy::too_many_arguments)]
    fn voice_frame(&mut self, ldu: usize, k: usize, u: [u32; 8], e0: u32, errs: u32, erased: bool, out: &mut Vec<TrackerOut>) {
        self.frames += 1;
        self.call_frames += 1;
        if self.encrypted && self.ks.is_none() && self.keys.is_some() && ldu == 0 {
            self.held.push((ldu, k, u, e0, errs, erased));
            return;
        }
        self.vocode(ldu, k, u, e0, errs, erased, out);
    }

    #[allow(clippy::too_many_arguments)]
    fn vocode(&mut self, ldu: usize, k: usize, u: [u32; 8], e0: u32, errs: u32, mut erased: bool, out: &mut Vec<TrackerOut>) {
        let mut bits = imbe_params_to_bits(&u);
        if self.encrypted {
            match &self.ks {
                Some(ks) => {
                    ks.apply_p1(ldu, k, &mut bits);
                    if !std::mem::replace(&mut self.decrypting, true) {
                        out.push(TrackerOut::Decrypted);
                    }
                }
                None if self.decrypting => {
                    erased = true;
                    bits = [0; 88];
                }
                None => return,
            }
        }
        let mut buf = [0f32; FRAME_SAMPLES];
        let kind = self.dec.imbe(&bits, e0, if erased { 0 } else { errs }, erased, &mut buf);
        if kind != Kind::Voice {
            self.bad_frames += 1;
        }
        mbe::to_limited(&mut buf);
        let frame = VoiceFrame { codec: Codec::Imbe, bits: bits.to_vec(), e0, errs, erased, kind };
        out.push(TrackerOut::Audio(buf.to_vec(), frame));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::crypt::ALGID_ADP;
    use crate::p25::voice::imbe_bits_to_params;

    fn tracker(keys: Option<Keyring>) -> VoiceTracker {
        VoiceTracker::new(mbe::lcg(1), mbe::Profile::Enhanced, keys.map(Arc::new))
    }

    #[test]
    fn notices_a_lost_ldu2() {
        let mut t = tracker(None);
        let lost = |t: &mut VoiceTracker, last: u8, duid: u8, dt: f64| {
            t.last_ldu = Some((last, 10.0));
            t.lost_ldu2(duid, 10.0 + dt)
        };
        assert!(!lost(&mut t, LDU1, LDU2, 0.18));
        assert!(!lost(&mut t, LDU2, LDU1, 0.18));
        assert!(lost(&mut t, LDU1, LDU1, 0.18));
        // An LDU1 lost: the superframe's sync was still heard.
        assert!(!lost(&mut t, LDU2, LDU2, 0.36));
        assert!(lost(&mut t, LDU2, LDU1, 0.54));
        assert!(lost(&mut t, LDU1, LDU2, 0.54));
        t.last_ldu = None;
        assert!(!t.lost_ldu2(LDU1, 3.0));
    }

    #[test]
    fn decrypts_phase1_voice_with_the_key() {
        let key = [0x12, 0x34, 0x56, 0x78, 0x90];
        let mut keys = Keyring::default();
        keys.add(ALGID_ADP, Some(0x1B50), key.to_vec(), vec![]).unwrap();
        let es = EncryptionSync { algid: ALGID_ADP, keyid: 0x1B50, mi: [1, 2, 3, 4, 5, 6, 7, 8, 0] };
        let ks = keys.keystream(&es, None, Phase::One).unwrap();
        let clear: [u8; 88] = std::array::from_fn(|i| (i * 5 % 7 < 3) as u8);
        let heard = |t: &mut VoiceTracker, ldu: usize, k: usize| {
            let mut bits = clear;
            ks.apply_p1(ldu, k, &mut bits);
            let mut out = Vec::new();
            t.voice_frame(ldu, k, imbe_bits_to_params(&bits), 0, 0, false, &mut out);
            // Decrypted comes once, before the transmission's first decrypted audio.
            let first = ldu == 0 && k == 0 && t.ks.is_some();
            match out.as_slice() {
                [TrackerOut::Decrypted, TrackerOut::Audio(_, f)] if first => Some(f.clone()),
                [TrackerOut::Audio(_, f)] if !first => Some(f.clone()),
                [] => None,
                o => panic!("{o:?}"),
            }
        };
        let begin = |t: &mut VoiceTracker| {
            t.algid = ALGID_ADP as i32;
            t.start(0.0, Some(100));
            t.set_es(Some(es));
        };

        let mut t = tracker(Some(keys.clone()));
        begin(&mut t);
        for ldu in 0..2 {
            for k in 0..9 {
                let f = heard(&mut t, ldu, k).expect("decrypted");
                assert_eq!(f.bits, clear, "LDU{} frame {k}", ldu + 1);
                assert!(!f.erased);
            }
        }
        // The sync lost to a fade: LDU1 voice is held for the next one…
        t.set_es(None);
        assert!(heard(&mut t, 0, 0).is_none());
        // …and muted, not dropped, if none comes; so is LDU2 voice.
        let mut out = Vec::new();
        t.release_held(&mut out);
        assert!(matches!(out.as_slice(), [TrackerOut::Audio(_, f)] if f.erased && f.bits == [0; 88]), "{out:?}");
        let f = heard(&mut t, 1, 0).expect("muted, not dropped");
        assert!(f.erased && f.bits == [0; 88]);

        // No key for it: dropped, as before.
        let mut t = tracker(Some(Keyring::default()));
        begin(&mut t);
        assert!(heard(&mut t, 0, 0).is_none());
        let mut t = tracker(None);
        begin(&mut t);
        assert!(heard(&mut t, 0, 0).is_none());

        // Unlocked in the middle of the call: decrypted from the next frame. Locked again: not, at once.
        let frame = |t: &mut VoiceTracker, ldu: usize, k: usize| {
            let mut bits = clear;
            ks.apply_p1(ldu, k, &mut bits);
            let mut out = Vec::new();
            t.voice_frame(ldu, k, imbe_bits_to_params(&bits), 0, 0, false, &mut out);
            out
        };
        t.set_keys(Some(Arc::new(keys.clone())));
        assert!(matches!(frame(&mut t, 1, 0).as_slice(), [TrackerOut::Decrypted, TrackerOut::Audio(_, f)] if f.bits == clear));
        // (What was decrypted is muted from then on, never passed on as it was heard.)
        let muted = |out: Vec<TrackerOut>| out.is_empty() || matches!(out.as_slice(), [TrackerOut::Audio(_, f)] if f.erased && f.bits == [0; 88]);
        t.set_keys(None);
        assert!(muted(frame(&mut t, 1, 1)));
        t.set_keys(Some(Arc::new(Keyring::default())));
        assert!(muted(frame(&mut t, 1, 2)));
    }
}
