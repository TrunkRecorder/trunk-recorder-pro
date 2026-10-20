//! One voice channel's call tracker (Phase 1): HDU / LDU1 / LDU2 / TDU(LC)
//! frames → encryption and link-control state → IMBE vocoder → audio.
//! Takes receiver-diversity groups: header fields from the best NID, link
//! control / encryption sync from whichever copy decodes, each codeword from
//! the receiver the soft decoder trusted most.

use super::frames::{Codec, VoiceFrame};
use crate::mbe::{self, Kind, FRAME_SAMPLES};
use crate::p25::alias::{is_alias_lcw, AliasLc, LcAliases};
use crate::p25::crypt::{imbe_voice, next_mi, prev_mi, KeyCheck, Keyring, Keystream, Phase, SyncCheck, Verdict};
use crate::p25::diversity::{best_es, best_frame, best_hdu, best_imbe, best_lc, Group};
use crate::p25::frame::{HDU, LDU1, LDU2, TDU, TDULC};
use crate::p25::gps::LcPositions;
use crate::p25::lc::{self, Lc, LinkControl};
use crate::p25::voice::{decode_tdulc, imbe_bits_to_params, imbe_params_to_bits, ldu_codeword_end_bit, EncryptionSync, ALGID_CLEAR};
use super::calls::Flags;
use super::voice::{CipherId, Encryption, TrackerOut};
use std::sync::Arc;

/// An LDU's length, s (1728 symbols at 4800 baud).
const LDU_S: f64 = 0.18;

/// A unit address that names a radio: not "no one" or "everyone" (BAAC-C §2.4).
fn radio(id: u32) -> Option<u32> {
    (id != 0 && id != 0xff_ffff).then_some(id)
}

pub struct VoiceTracker {
    dec: mbe::Decoder,
    active: bool,
    voice: Encryption,
    /// The encryption sync the current superframe's voice is under (the
    /// HDU's, then each LDU2's for the superframe after it), and the one the
    /// last LDU2 brought.
    es: Option<EncryptionSync>,
    es_next: Option<EncryptionSync>,
    /// Each LDU2's sync checked against the MI LFSR's prediction.
    chain: SyncCheck,
    /// The cipher last reported for this transmission.
    cipher: Option<CipherId>,
    /// The system's keys, and the keystream of the superframe `es` is for.
    keys: Option<Arc<Keyring>>,
    ks: Option<Keystream>,
    /// Some voice of this transmission was decrypted: voice that can't be
    /// now (its sync lost to a fade) is muted, not dropped.
    decrypting: bool,
    /// Whether the key decrypts this transmission to voice, and whether
    /// that was reported; a wrong key's voice isn't decrypted.
    check: KeyCheck,
    reported: bool,
    wrong_key: bool,
    /// The last LDU's DUID and time, to notice LDUs that never arrived.
    last_ldu: Option<(u8, f64)>,
    /// Voice held for an LDU2's sync: encrypted with no sync (joined late,
    /// or the last LDU2 lost), its sync the next superframe's and the LFSR
    /// run back this one's; or of unknown encryption (joined late), until a
    /// sync says. (LDU, frame, u, e0, errors, erased.)
    held: Vec<(usize, usize, [u32; 8], u32, u32, bool)>,
    /// The group link control named, and whether it was Motorola's patch
    /// word (a supergroup, where the standard's word names a member).
    tgid: Option<u32>,
    patch_word: bool,
    /// The talkgroup the channel was granted for: a key by talkgroup is
    /// found with it before link control names the group.
    granted: Option<u32>,
    /// A unit-to-unit call's target.
    target: Option<u32>,
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
            voice: Encryption::Unknown,
            es: None,
            es_next: None,
            chain: SyncCheck::default(),
            cipher: None,
            keys,
            ks: None,
            decrypting: false,
            check: KeyCheck::default(),
            reported: false,
            wrong_key: false,
            last_ldu: None,
            held: Vec::new(),
            tgid: None,
            patch_word: false,
            granted: None,
            target: None,
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

    /// A unit-to-unit call's target, as link control named it.
    pub fn target(&self) -> Option<u32> {
        self.target
    }

    /// The talkgroup the channel was granted for (None: not a trunked call).
    pub fn set_granted(&mut self, tg: Option<u32>) {
        if self.granted != tg {
            self.granted = tg;
            let es = self.es;
            self.set_es(es);
        }
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
                self.end(out);
                let h = best_hdu(g);
                self.start(t, h.map(|h| h.tgid as u32).filter(|&tg| tg != 0));
                // The HDU's sync is the first superframe's; without it, the
                // first LDU2 says what the voice is.
                if let Some(h) = h {
                    self.set_voice(Encryption::from_sync(&h.es), out);
                    if h.es.algid != ALGID_CLEAR {
                        self.set_es(Some(h.es));
                        self.report_cipher(&h.es, out);
                    }
                }
            }
            LDU1 | LDU2 => {
                self.touch(t, out);
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
                        self.link_control(lc, t, out);
                    }
                } else {
                    self.es_next = self.chain.next(self.es, best_es(g));
                    if let Some(es) = self.es_next {
                        self.set_voice(Encryption::from_sync(&es), out);
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
                let lc = g.iter().filter(|f| f.nid.duid == TDULC).find_map(|f| decode_tdulc(f, true));
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
                        self.tgid = lc.group().filter(|&g| g != 0);
                    }
                    // (A call termination, AABF-D §7.3.8, ends no call here:
                    // Motorola sends one after every transmission, the channel
                    // released between talkers, and a call is the talkgroup's
                    // conversation until it has been quiet for the timeout.)
                    self.other_lc(&lc, out);
                }
                // Voice still waiting for a sync that won't come.
                self.release_held(out);
                self.end(out);
                self.active = false;
                self.set_es(None);
                self.last_ldu = None;
            }
            _ => {}
        }
    }

    /// LDU1's link control. Encrypted (P), it is readable with the
    /// superframe's keystream (AAAD-B Table 5-2); what it says then fills in
    /// what clear words would, but doesn't start a new transmission.
    fn link_control(&mut self, lc: LinkControl, t: f64, out: &mut Vec<TrackerOut>) {
        let mut lc = lc;
        let mut clear = true;
        if lc.protected {
            match self.ks.as_ref().filter(|_| !self.wrong_key) {
                Some(ks) => {
                    let mut w = lc.raw;
                    ks.apply_lc(&mut w);
                    w[0] &= 0x7f;
                    lc = lc::parse(&w);
                    clear = false;
                }
                None => return self.hint(out),
            }
        }
        match lc.msg {
            Lc::GroupVoiceUser { .. } | Lc::PatchVoiceUser { .. } | Lc::UnitVoiceUser { .. } | Lc::PhoneVoiceUser { .. } => self.voice_user(&lc, clear, t, out),
            // The full ID of a talker the voice user word couldn't address.
            Lc::SourceIdExtension { id, .. } => {
                if let (None, Some(id)) = (self.talker, radio(id)) {
                    self.talker = Some(id);
                    out.push(TrackerOut::Info { source: Some(id), flags: Flags { encrypted: self.voice.encrypted(), ..Default::default() } });
                }
            }
            _ => self.other_lc(&lc, out),
        }
    }

    /// A voice channel user word: the call's group (or target) and talker.
    fn voice_user(&mut self, lc: &LinkControl, clear: bool, t: f64, out: &mut Vec<TrackerOut>) {
        let group = lc.group().filter(|&g| g != 0);
        let patch = matches!(lc.msg, Lc::PatchVoiceUser { .. });
        if let (Some(cur), Some(g)) = (self.tgid, group) {
            // Another group mid-transmission: a new one, its HDU lost. (Not
            // a patch's supergroup word between its member's: one call.)
            if clear && cur != g && patch == self.patch_word && self.call_frames > 0 {
                self.end(out);
                self.start(t, None);
            }
        }
        if group.is_some() && (clear || self.tgid.is_none()) {
            (self.tgid, self.patch_word) = (group, patch);
        }
        if let Some(target) = lc.target() {
            self.target = radio(target);
        }
        let source = lc.source().and_then(radio);
        if source.is_some() {
            self.talker = source;
        }
        let opts = lc.opts().unwrap_or_default();
        if opts.protected() {
            self.hint(out);
        }
        // (Encrypted as the voice is: a sync overrules the word's protected bit.)
        let flags = Flags { encrypted: self.voice.encrypted(), ..opts.into() };
        out.push(TrackerOut::Info { source, flags });
    }

    /// Words of the manufacturers' own: talker aliases and positions.
    fn other_lc(&mut self, lc: &LinkControl, out: &mut Vec<TrackerOut>) {
        if let Some(a) = self.aliases.lcw(&lc.raw, self.talker, self.tgid) {
            out.push(TrackerOut::Alias(a));
        }
        if let Some(p) = self.positions.lcw(&lc.raw, lc.mfid, self.talker) {
            out.push(TrackerOut::Position(p));
        }
    }

    /// Link control says the voice is encrypted: until a sync says.
    fn hint(&mut self, out: &mut Vec<TrackerOut>) {
        let v = self.voice.hinted();
        self.set_voice(v, out);
    }

    fn set_voice(&mut self, v: Encryption, out: &mut Vec<TrackerOut>) {
        let was = self.voice.encrypted();
        self.voice = v;
        if self.active && v.encrypted() && !was {
            out.push(TrackerOut::Info { source: None, flags: Flags { encrypted: true, ..Default::default() } });
        }
    }

    fn start(&mut self, t: f64, tgid: Option<u32>) {
        self.dec.reset();
        self.active = true;
        self.voice = Encryption::Unknown;
        self.tgid = tgid;
        self.patch_word = false;
        self.target = None;
        self.talker = None;
        self.set_es(None);
        self.es_next = None;
        self.chain.reset();
        self.cipher = None;
        self.decrypting = false;
        self.check = KeyCheck::default();
        self.reported = false;
        self.wrong_key = false;
        self.last_ldu = None;
        self.held.clear();
        self.end_s = t;
        self.call_frames = 0;
    }

    /// The transmission is over: its key's verdict, if none came yet.
    fn end(&mut self, out: &mut Vec<TrackerOut>) {
        if self.decrypting && !self.reported && self.check.at_end() == Verdict::Right {
            self.reported = true;
            out.push(TrackerOut::Decrypted);
        }
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
    /// has the key (by talkgroup: link control's, or the grant's).
    fn set_es(&mut self, es: Option<EncryptionSync>) {
        self.es = es;
        self.ks = match (&self.keys, &es) {
            (Some(keys), Some(es)) => keys.keystream(es, self.tgid.or(self.granted), Phase::One),
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

    fn touch(&mut self, t: f64, out: &mut Vec<TrackerOut>) {
        if self.active && t - self.end_s > 1.0 {
            // Quiet with no terminator: the next transmission re-learns its cipher.
            self.end(out);
            self.active = false;
        }
        if !self.active {
            self.start(t, None);
        }
        self.end_s = t;
    }

    /// Vocode the voice held for a sync: decrypted if one came, clear if
    /// none said otherwise.
    fn release_held(&mut self, out: &mut Vec<TrackerOut>) {
        for (ldu, k, u, e0, errs, erased) in std::mem::take(&mut self.held) {
            self.vocode(ldu, k, u, e0, errs, erased, out);
        }
    }

    /// IMBE frame `k` of an LDU (`ldu` 0: LDU1, 1: LDU2): vocoded, or held
    /// for a sync: when it's encrypted under none yet and there are keys, or
    /// nothing has said yet whether it is.
    #[allow(clippy::too_many_arguments)]
    fn voice_frame(&mut self, ldu: usize, k: usize, u: [u32; 8], e0: u32, errs: u32, erased: bool, out: &mut Vec<TrackerOut>) {
        self.frames += 1;
        self.call_frames += 1;
        let unsure = self.voice == Encryption::Unknown;
        let waiting = self.voice.encrypted() && self.ks.is_none() && self.keys.is_some();
        if ldu == 0 && (unsure || waiting) {
            self.held.push((ldu, k, u, e0, errs, erased));
            return;
        }
        self.vocode(ldu, k, u, e0, errs, erased, out);
    }

    #[allow(clippy::too_many_arguments)]
    fn vocode(&mut self, ldu: usize, k: usize, u: [u32; 8], e0: u32, errs: u32, mut erased: bool, out: &mut Vec<TrackerOut>) {
        let mut bits = imbe_params_to_bits(&u);
        if self.voice.encrypted() {
            match self.ks.as_ref().filter(|_| !self.wrong_key) {
                Some(ks) => {
                    ks.apply_p1(ldu, k, &mut bits);
                    self.decrypting = true;
                    if !self.key_check(imbe_voice(&imbe_bits_to_params(&bits)), out) {
                        return;
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

    /// One decrypted frame for the key check. Its verdict reported once;
    /// false: the key is wrong, and this transmission isn't decrypted.
    fn key_check(&mut self, voice: bool, out: &mut Vec<TrackerOut>) -> bool {
        match self.check.frame(voice) {
            Verdict::Right if !self.reported => {
                self.reported = true;
                out.push(TrackerOut::Decrypted);
            }
            Verdict::Wrong => {
                (self.wrong_key, self.decrypting) = (true, false);
                if let Some(c) = self.cipher {
                    out.push(TrackerOut::WrongKey(c));
                }
                return false;
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::crypt::ALGID_ADP;

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

    /// One LDU's nine frames of `clear` voice under `ks` (as the air carries them).
    fn superframe(t: &mut VoiceTracker, ks: &Keystream, clear: &[u8; 88]) -> Vec<TrackerOut> {
        let mut out = Vec::new();
        for ldu in 0..2 {
            for k in 0..9 {
                let mut bits = *clear;
                ks.apply_p1(ldu, k, &mut bits);
                t.voice_frame(ldu, k, imbe_bits_to_params(&bits), 0, 0, false, &mut out);
            }
        }
        out
    }

    fn adp() -> (Keyring, EncryptionSync, Keystream) {
        let mut keys = Keyring::default();
        keys.add(ALGID_ADP, Some(0x1B50), vec![0x12, 0x34, 0x56, 0x78, 0x90], vec![]).unwrap();
        let es = EncryptionSync { algid: ALGID_ADP, keyid: 0x1B50, mi: [1, 2, 3, 4, 5, 6, 7, 8, 0] };
        let ks = keys.keystream(&es, None, Phase::One).unwrap();
        (keys, es, ks)
    }

    /// Voice whose pitch (b0, the first 8 bits) is a real one.
    fn clear_voice() -> [u8; 88] {
        std::array::from_fn(|i| (i * 5 % 7 < 3) as u8)
    }

    fn begin(t: &mut VoiceTracker, es: EncryptionSync) {
        t.start(0.0, Some(100));
        t.voice = Encryption::Encrypted(es.algid);
        t.set_es(Some(es));
    }

    fn audio(out: &[TrackerOut]) -> Vec<&VoiceFrame> {
        out.iter().filter_map(|o| if let TrackerOut::Audio(_, f) = o { Some(f) } else { None }).collect()
    }

    #[test]
    fn decrypts_phase1_voice_with_the_key() {
        let (keys, es, ks) = adp();
        let clear = clear_voice();
        let mut t = tracker(Some(keys.clone()));
        begin(&mut t, es);
        // Decrypted, and said so once two superframes of it are voice.
        let first = superframe(&mut t, &ks, &clear);
        assert!(audio(&first).iter().all(|f| f.bits == clear && !f.erased));
        assert!(!first.iter().any(|o| matches!(o, TrackerOut::Decrypted)));
        let second = superframe(&mut t, &ks, &clear);
        assert_eq!(second.iter().filter(|o| matches!(o, TrackerOut::Decrypted)).count(), 1);
        // The sync lost to a fade: LDU1 voice is held for the next one…
        t.set_es(None);
        let mut out = Vec::new();
        t.voice_frame(0, 0, imbe_bits_to_params(&clear), 0, 0, false, &mut out);
        assert!(out.is_empty());
        // …and muted, not dropped, if none comes; so is LDU2 voice.
        t.release_held(&mut out);
        t.voice_frame(1, 0, imbe_bits_to_params(&clear), 0, 0, false, &mut out);
        assert!(audio(&out).len() == 2 && audio(&out).iter().all(|f| f.erased && f.bits == [0; 88]), "{out:?}");

        // No key for it: dropped.
        for keys in [Some(Keyring::default()), None] {
            let mut t = tracker(keys);
            begin(&mut t, es);
            assert!(audio(&superframe(&mut t, &ks, &clear)).is_empty());
        }

        // Unlocked in the middle of the call: decrypted from the next frame. Locked again: not, at once.
        let mut t = tracker(None);
        begin(&mut t, es);
        t.set_keys(Some(Arc::new(keys.clone())));
        assert!(audio(&superframe(&mut t, &ks, &clear)).iter().all(|f| f.bits == clear));
        t.set_keys(None);
        let out = superframe(&mut t, &ks, &clear);
        assert!(audio(&out).iter().all(|f| f.erased && f.bits == [0; 88]));
    }

    /// A wrong key's "voice" is noise, much of it pitches no encoder sends:
    /// found out within a superframe, never reported decrypted, the rest dropped.
    #[test]
    fn a_wrong_key_is_found_out() {
        let (_, es, ks) = adp();
        let mut wrong = Keyring::default();
        wrong.add(ALGID_ADP, Some(0x1B50), vec![9, 9, 9, 9, 9], vec![]).unwrap();
        let mut t = tracker(Some(wrong));
        begin(&mut t, es);
        t.cipher = Some(CipherId { algid: ALGID_ADP, keyid: 0x1B50 });
        let mut out = superframe(&mut t, &ks, &clear_voice());
        out.extend(superframe(&mut t, &ks, &clear_voice()));
        assert!(out.iter().any(|o| matches!(o, TrackerOut::WrongKey(CipherId { keyid: 0x1B50, .. }))));
        assert!(!out.iter().any(|o| matches!(o, TrackerOut::Decrypted)));
        let after = out.iter().rposition(|o| matches!(o, TrackerOut::WrongKey(_))).unwrap();
        assert!(audio(&out[after..]).is_empty());
    }

    /// Link control's hint (P) is overruled by a sync that says clear (AAAD-B §5.1).
    #[test]
    fn a_clear_sync_overrules_link_control() {
        let mut t = tracker(None);
        let mut out = Vec::new();
        t.start(0.0, None);
        t.hint(&mut out);
        assert!(t.voice.encrypted());
        t.set_voice(Encryption::from_sync(&EncryptionSync { algid: ALGID_CLEAR, keyid: 0, mi: [0; 9] }), &mut out);
        assert_eq!(t.voice, Encryption::Clear);
        // From then on a hint changes nothing.
        t.hint(&mut out);
        assert_eq!(t.voice, Encryption::Clear);
    }

    /// An LDU2 sync off the LFSR's chain (AAAD-B §3) is doubted: the
    /// prediction stands until the next LDU2 runs on from the new one.
    #[test]
    fn a_sync_off_the_chain_is_doubted() {
        let (_, es, _) = adp();
        let mut t = tracker(None);
        begin(&mut t, es);
        let next = EncryptionSync { mi: next_mi(&es.mi), ..es };
        assert_eq!(t.chain.next(t.es, Some(next)), Some(next));
        let odd = EncryptionSync { mi: [9, 9, 9, 9, 9, 9, 9, 9, 0], ..es };
        assert_eq!(t.chain.next(t.es, Some(odd)), Some(next), "the prediction");
        // The next superframe's runs on from the odd one: it was right.
        t.set_es(Some(next));
        let after = EncryptionSync { mi: next_mi(&odd.mi), ..es };
        assert_eq!(t.chain.next(t.es, Some(after)), Some(after));
    }

    /// Protected link control is read with the key (AAAD-B Table 5-2): the talker.
    #[test]
    fn protected_link_control_is_read_with_the_key() {
        let (keys, es, ks) = adp();
        let mut t = tracker(Some(keys));
        begin(&mut t, es);
        let mut w = [0x80, 0x00, 0x40, 0x00, 0x00, 100, 0x12, 0x34, 0x56];
        ks.apply_lc(&mut w);
        let mut out = Vec::new();
        t.link_control(lc::parse(&w), 0.0, &mut out);
        assert_eq!(t.talker, Some(0x123456));
        assert!(out.iter().any(|o| matches!(o, TrackerOut::Info { source: Some(0x123456), .. })));
    }

    /// AABF-D §7.3.32: a talker with no address here (S set, source 0) is named by the extension.
    #[test]
    fn a_source_id_extension_names_the_talker() {
        let mut t = tracker(None);
        t.start(0.0, None);
        let mut out = Vec::new();
        t.link_control(lc::parse(&[0x00, 0x00, 0x00, 0x01, 0x00, 100, 0, 0, 0]), 0.0, &mut out);
        assert_eq!(t.talker, None);
        t.link_control(lc::parse(&[0x49, 0, 0xbe, 0xe0, 0x03, 0xa9, 0x01, 0x02, 0x03]), 0.0, &mut out);
        assert_eq!(t.talker, Some(0x010203));
    }
}
