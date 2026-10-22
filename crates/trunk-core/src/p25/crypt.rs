//! P25 voice encryption: the ALGID table, the message indicator (MI) LFSR
//! and the keystreams that decrypt voice for users who hold the keys.
//!
//! Every cipher here runs as a keystream generator, one keystream per
//! superframe from (key, MI). The keystream is XORed onto the voice frames'
//! information bits after FEC: the 88 IMBE bits u0..u7 (Phase 1) or the 49
//! AMBE+2 bits (Phase 2), packed MSB first. Layout (TIA-102.AAAD; as OP25
//! and dsd-fme apply it):
//!
//! - the first block (ADP: 256 bytes) is thrown away;
//! - Phase 1: IMBE frame k (0–8) of LDU n (0: LDU1, 1: LDU2) takes 11 bytes
//!   from `discard + 101·n + 11 + 11·k`, 2 more for k = 8 (the low-speed
//!   data sits between frames 7 and 8);
//! - Phase 2: AMBE frame f (0–17: four per 4V burst, then the 2V's two)
//!   takes 7 bytes from `discard + 7·f`, the last byte's top bit only.
//!
//! See research/p25-encryption.md.

use aes::cipher::{BlockCipherEncrypt, KeyInit};

use zeroize::Zeroizing;

use super::voice::EncryptionSync;

pub const ALGID_CLEAR: u8 = 0x80;
pub const ALGID_DES_OFB: u8 = 0x81;
pub const ALGID_TDES: u8 = 0x83;
pub const ALGID_AES_256: u8 = 0x84;
pub const ALGID_AES_128: u8 = 0x89;
pub const ALGID_ADP: u8 = 0xAA;

/// What an ALGID is called (TIA-102.BAAC-C §2.8's values, and the
/// manufacturers' that are seen on the air).
///
/// $85 is the standard's AES-128, but no TIA document defines voice under
/// it: AAAD-B (2015) has voice annexes for $81, $83 and $84 only, and $85 is
/// the algorithm of link layer authentication's key (AACD-A), used there in
/// ECB mode (AACE-A). AES-128 voice on the air is Motorola's $89, in AAAD-B
/// §4's OFB (as OP25 and dsd-fme decrypt it). So $85 isn't decrypted.
pub fn algid_name(algid: u8) -> &'static str {
    match algid {
        0x00..=0x04 | 0x41 => "Type 1",
        0x80 => "clear",
        0x81 => "DES-OFB",
        0x82 => "2-key 3DES",
        0x83 => "3DES",
        0x84 => "AES-256",
        0x85 => "AES-128",
        0x88 => "AES-CBC",
        0x89 => "AES-128 (Motorola)",
        0x9F => "DES-XL",
        0xA0 => "DVI-XL",
        0xA1 => "DVP-XL",
        0xA2 => "DVP-SPFL",
        0xA3 => "Haystack",
        0xAA => "ADP",
        0xAB => "CFX-256",
        0xAF => "AES-256 GCM",
        _ => "unknown",
    }
}

/// The ciphers that can be decrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Algorithm {
    /// Motorola ADP: RC4 with a 40-bit key.
    Adp,
    DesOfb,
    /// Three-key triple DES (EDE), OFB.
    Tdes,
    Aes128,
    Aes256,
}

impl Algorithm {
    /// None: clear, or a cipher this can't decrypt.
    pub fn from_algid(algid: u8) -> Option<Self> {
        match algid {
            ALGID_ADP => Some(Algorithm::Adp),
            ALGID_DES_OFB => Some(Algorithm::DesOfb),
            ALGID_TDES => Some(Algorithm::Tdes),
            ALGID_AES_128 => Some(Algorithm::Aes128),
            ALGID_AES_256 => Some(Algorithm::Aes256),
            _ => None,
        }
    }

    pub fn algid(self) -> u8 {
        match self {
            Algorithm::Adp => ALGID_ADP,
            Algorithm::DesOfb => ALGID_DES_OFB,
            Algorithm::Tdes => ALGID_TDES,
            Algorithm::Aes128 => ALGID_AES_128,
            Algorithm::Aes256 => ALGID_AES_256,
        }
    }

    /// Key length in bytes.
    pub fn key_len(self) -> usize {
        match self {
            Algorithm::Adp => 5,
            Algorithm::DesOfb => 8,
            Algorithm::Tdes => 24,
            Algorithm::Aes128 => 16,
            Algorithm::Aes256 => 32,
        }
    }

    /// Keystream bytes thrown away before the voice's.
    fn discard(self) -> usize {
        match self {
            Algorithm::Adp => 256,
            Algorithm::DesOfb | Algorithm::Tdes => 8,
            Algorithm::Aes128 | Algorithm::Aes256 => 16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// FDMA: an LDU1 + LDU2 superframe, 18 IMBE frames.
    One,
    /// TDMA: one slot's 4 × 4V + 2V, 18 AMBE+2 frames.
    Two,
}

/// Voice keystream bytes a superframe uses, past the discard.
const P1_LEN: usize = 101 + 11 + 11 * 8 + 2 + 11;
const P2_LEN: usize = 7 * 18;

// ── Message indicator ────────────────────────────────────────────────────────

/// One step of the MI LFSR, C(x) = x^64 + x^62 + x^46 + x^38 + x^27 + x^15 + 1:
/// the feedback goes in at the bottom; returns the bit shifted out the top.
fn lfsr_step(r: &mut u64) -> u64 {
    let out = *r >> 63;
    let fb = (*r >> 63 ^ *r >> 61 ^ *r >> 45 ^ *r >> 37 ^ *r >> 26 ^ *r >> 14) & 1;
    *r = *r << 1 | fb;
    out
}

fn mi64(mi: &[u8; 9]) -> u64 {
    u64::from_be_bytes(mi[..8].try_into().unwrap())
}

/// The MI of the next superframe (64 LFSR steps; the 9th byte is 0): what an
/// encryption sync lost to errors would have said.
pub fn next_mi(mi: &[u8; 9]) -> [u8; 9] {
    let mut r = mi64(mi);
    for _ in 0..64 {
        lfsr_step(&mut r);
    }
    let mut out = [0u8; 9];
    out[..8].copy_from_slice(&r.to_be_bytes());
    out
}

/// The MI of the superframe before (64 LFSR steps back; the LFSR can be run
/// backwards, its polynomial having an x⁰ term): what a receiver that joined
/// late, and heard only the sync for the next superframe, needs for this one.
pub fn prev_mi(mi: &[u8; 9]) -> [u8; 9] {
    let mut r = mi64(mi);
    for _ in 0..64 {
        // The bit shifted out was the feedback's only unknown term.
        let top = (r ^ r >> 62 ^ r >> 46 ^ r >> 38 ^ r >> 27 ^ r >> 15) & 1;
        r = r >> 1 | top << 63;
    }
    let mut out = [0u8; 9];
    out[..8].copy_from_slice(&r.to_be_bytes());
    out
}

/// AES's 128-bit IV: the 64 bits the LFSR shifts out over 64 steps (the MI
/// itself), then its state (the next MI).
pub fn aes_iv(mi: &[u8; 9]) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&mi[..8]);
    iv[8..].copy_from_slice(&next_mi(mi)[..8]);
    iv
}

// ── Keystreams ───────────────────────────────────────────────────────────────

/// Output feedback: `n` blocks of `encrypt` run on its own output, from `iv`.
fn ofb<const B: usize>(iv: [u8; B], n: usize, mut encrypt: impl FnMut(&mut [u8; B])) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * B);
    let mut b = iv;
    for _ in 0..n {
        encrypt(&mut b);
        out.extend_from_slice(&b);
    }
    out
}

fn block_cipher<C: BlockCipherEncrypt + KeyInit, const B: usize>(key: &[u8], iv: [u8; B], n: usize) -> Vec<u8> {
    let c = C::new_from_slice(key).expect("key length checked");
    ofb(iv, n, |b| {
        let mut blk = b.as_slice().try_into().expect("block size");
        c.encrypt_block(&mut blk);
        b.copy_from_slice(&blk);
    })
}

/// RC4: `n` bytes of output for `key`.
fn rc4(key: &[u8], n: usize) -> Vec<u8> {
    // (The state is the key, scrambled: wiped too.)
    let mut s = Zeroizing::new(std::array::from_fn::<u8, 256, _>(|i| i as u8));
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    (0..n)
        .map(|_| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

/// One superframe's keystream.
#[derive(Clone)]
pub struct Keystream {
    phase: Phase,
    /// From the first voice byte (the discard dropped). Wiped when dropped,
    /// as the keys are: with the MI it would give the voice of anything
    /// else sent under the same key and MI.
    ks: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for Keystream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keystream").field("phase", &self.phase).finish_non_exhaustive()
    }
}

impl Keystream {
    /// None: the key isn't `alg`'s length.
    pub fn new(alg: Algorithm, key: &[u8], mi: &[u8; 9], phase: Phase) -> Option<Self> {
        if key.len() != alg.key_len() {
            return None;
        }
        let need = alg.discard() + if phase == Phase::One { P1_LEN } else { P2_LEN };
        let blocks = |b: usize| need.div_ceil(b);
        let iv8: [u8; 8] = mi[..8].try_into().unwrap();
        let mut ks = match alg {
            Algorithm::Adp => {
                let mut k = Zeroizing::new([0u8; 13]);
                k[..5].copy_from_slice(key);
                k[5..].copy_from_slice(&mi[..8]);
                rc4(&k[..], need)
            }
            Algorithm::DesOfb => block_cipher::<des::Des, 8>(key, iv8, blocks(8)),
            Algorithm::Tdes => block_cipher::<des::TdesEde3, 8>(key, iv8, blocks(8)),
            Algorithm::Aes128 => block_cipher::<aes::Aes128, 16>(key, aes_iv(mi), blocks(16)),
            Algorithm::Aes256 => block_cipher::<aes::Aes256, 16>(key, aes_iv(mi), blocks(16)),
        };
        ks.drain(..alg.discard());
        Some(Keystream { phase, ks: Zeroizing::new(ks) })
    }

    /// Where Phase 1 frame `k` of LDU `ldu` (0: LDU1, 1: LDU2) starts.
    fn p1_offset(ldu: usize, k: usize) -> usize {
        101 * ldu + 11 + 11 * k + if k == 8 { 2 } else { 0 }
    }

    fn xor(&self, at: usize, bits: &mut [u8]) {
        for (i, b) in bits.iter_mut().enumerate() {
            *b ^= self.ks[at + i / 8] >> (7 - i % 8) & 1;
        }
    }

    /// Decrypt (or encrypt) IMBE frame `k` (0–8) of LDU `ldu` (0: LDU1,
    /// 1: LDU2): its 88 bits u0..u7 (imbe_params_to_bits).
    pub fn apply_p1(&self, ldu: usize, k: usize, bits: &mut [u8; 88]) {
        debug_assert_eq!(self.phase, Phase::One);
        self.xor(Self::p1_offset(ldu, k), bits);
    }

    /// Decrypt (or encrypt) LDU1's link control word: its octets L1..L8
    /// under keystream octets 4..11 (AAAD-B Tables 5-2, 5-5; 1..3 are
    /// reserved), less an explicit MFID's, which is sent clear. Its P bit
    /// stays as it was.
    pub fn apply_lc(&self, w: &mut [u8; 9]) {
        debug_assert_eq!(self.phase, Phase::One);
        for i in super::lc::encrypted_octets(w) {
            w[i] ^= self.ks[2 + i];
        }
    }

    /// Decrypt (or encrypt) AMBE+2 frame `f` (0–17) of the superframe: its
    /// 49 bits (decode_vcw).
    pub fn apply_p2(&self, f: usize, bits: &mut [u8; 49]) {
        debug_assert_eq!(self.phase, Phase::Two);
        self.xor(7 * f, bits);
    }
}

// ── Encryption syncs from one superframe to the next ─────────────────────────

/// Each superframe's encryption sync from the one the air brought for it
/// (Phase 1's LDU2, Phase 2's ESS). One that doesn't run on from the last
/// (the MI LFSR, the same cipher and key; AAAD-B §3: the LFSR lets a
/// receiver check) is doubted, a likely miscorrection: the LFSR's
/// prediction stands until the next one runs on from it.
#[derive(Clone, Copy, Debug, Default)]
pub struct SyncCheck {
    doubt: Option<EncryptionSync>,
}

impl SyncCheck {
    /// The next superframe's sync: `heard` checked against `cur`, this one's.
    pub fn next(&mut self, cur: Option<EncryptionSync>, heard: Option<EncryptionSync>) -> Option<EncryptionSync> {
        let follows = |a: &EncryptionSync, b: &EncryptionSync| a.algid == b.algid && a.keyid == b.keyid && next_mi(&a.mi) == b.mi;
        let heard = heard?;
        let Some(cur) = cur else {
            self.doubt = None;
            return Some(heard);
        };
        if follows(&cur, &heard) || self.doubt.is_some_and(|d| follows(&d, &heard)) {
            self.doubt = None;
            return Some(heard);
        }
        self.doubt = Some(heard);
        Some(EncryptionSync { mi: next_mi(&cur.mi), ..cur })
    }

    pub fn reset(&mut self) {
        self.doubt = None;
    }
}

// ── Whether a key is the right one ───────────────────────────────────────────

/// Whether a transmission's key decrypts it to voice (Phase 1). A wrong
/// key's output is noise, and full-rate IMBE reserves the pitch values b0
/// 208..=255 (BABA-A): 3 frames in 16 of noise name one. Real voice almost
/// never does (a gateway passes an erasure on now and then). So a key is
/// taken as right once two superframes (36 frames) have come with at most
/// one such frame (a wrong key passes 1 time in 190), and wrong as soon as
/// three have and they are a tenth of the frames or more. (Half-rate AMBE+2
/// has no such tell: noise lands on its erasure and tone codes 1 time in 30,
/// too seldom to judge a key by in a few superframes.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyCheck {
    frames: u32,
    invalid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Unsure,
    Right,
    Wrong,
}

impl KeyCheck {
    /// One decrypted frame, and whether it could be voice: the verdict so far.
    pub fn frame(&mut self, voice: bool) -> Verdict {
        self.frames += 1;
        self.invalid += !voice as u32;
        self.verdict()
    }
    pub fn verdict(&self) -> Verdict {
        if self.invalid >= 3 && self.invalid * 10 >= self.frames {
            Verdict::Wrong
        } else if self.frames >= 36 && self.invalid <= 1 {
            Verdict::Right
        } else {
            Verdict::Unsure
        }
    }
    /// The verdict when the transmission ends before one came: right if a
    /// superframe's worth came with none invalid (a wrong key: 1 in 40).
    pub fn at_end(&self) -> Verdict {
        match self.verdict() {
            Verdict::Unsure if self.frames >= 18 && self.invalid == 0 => Verdict::Right,
            v => v,
        }
    }
}

/// Whether full-rate IMBE frame `u` (u0..u7) could be voice: its pitch b0
/// (u0's top eight bits) a value the encoder sends (BABA-A: 0..=207).
pub fn imbe_voice(u: &[u32; 8]) -> bool {
    u[0] >> 4 < 208
}

// ── Keyring ──────────────────────────────────────────────────────────────────

/// One key of a [`Keyring`].
#[derive(Clone)]
pub struct Key {
    pub alg: Algorithm,
    /// None: any key ID (only with `talkgroups`).
    pub keyid: Option<u16>,
    /// Wiped when the key is dropped.
    key: Zeroizing<Vec<u8>>,
    /// Used for these talkgroups, under any key ID or the one given (empty:
    /// by key ID, on any talkgroup).
    pub talkgroups: Vec<u32>,
}

impl Key {
    /// Whether it decrypts voice under `alg`, as which cipher and key: its
    /// own, and DES and 3DES for each other (AAAD-B B.1: 3DES with K1 = K2 =
    /// K3 is DES under K1, and is sent as $81; a DES key is 3DES's that way).
    fn serves(&self, alg: Algorithm) -> Option<(Algorithm, &[u8])> {
        let k = &self.key[..];
        match (self.alg, alg) {
            (a, b) if a == b => Some((a, k)),
            (Algorithm::Tdes, Algorithm::DesOfb) if k[..8] == k[8..16] && k[8..16] == k[16..] => Some((Algorithm::DesOfb, &k[..8])),
            (Algorithm::DesOfb, Algorithm::Tdes) => Some((Algorithm::DesOfb, k)),
            _ => None,
        }
    }

    /// Something worth telling whoever loaded it: a DES key byte whose parity
    /// bit (the LSB; odd parity) is wrong, which a key fill device or the
    /// KMF would refuse (AACD-A, AACA-A), likely a mistyped key. Decryption
    /// doesn't use those bits.
    pub fn warning(&self) -> Option<String> {
        if !matches!(self.alg, Algorithm::DesOfb | Algorithm::Tdes) || self.key.iter().all(|b| b.count_ones() % 2 == 1) {
            return None;
        }
        let kid = self.keyid.map_or("A key without a key ID".to_string(), |k| format!("Key ID {k:#06X}"));
        Some(format!("{kid}: a DES key byte has the wrong parity (each byte's bit count should be odd): check it was typed right"))
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key").field("alg", &self.alg).field("keyid", &self.keyid).field("talkgroups", &self.talkgroups).finish_non_exhaustive()
    }
}

/// What a keyring has for an encryption sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup<'a> {
    Key(Algorithm, &'a [u8]),
    /// A cipher that can be decrypted, but no key for it.
    NoKey,
    /// Clear, or a cipher this can't decrypt.
    Unsupported,
}

/// A system's keys. They are never printed, logged or sent anywhere
/// ([`Key`]'s Debug leaves the key out), and are wiped from memory when dropped.
#[derive(Clone, Debug, Default)]
pub struct Keyring {
    keys: Vec<Key>,
}

impl Keyring {
    /// Add a key for `algid`. Refused, with what to fix: an ALGID that
    /// can't be decrypted, a key of the wrong length (keys are never
    /// padded), neither a key ID nor talkgroups, or a key ID given twice.
    pub fn add(&mut self, algid: u8, keyid: Option<u16>, key: Vec<u8>, talkgroups: Vec<u32>) -> Result<(), String> {
        let kid = keyid.map_or("Key without a key ID".to_string(), |k| format!("Key ID {k:#06X}"));
        let name = algid_name(algid);
        let Some(alg) = Algorithm::from_algid(algid) else {
            return Err(format!("{kid}: ALGID {algid:#04X} ({name}) can't be decrypted"));
        };
        if key.len() != alg.key_len() {
            let (need, have) = (alg.key_len(), key.len());
            return Err(format!("{kid}: {name} ({algid:#04X}) needs a {}-bit key ({} hex digits); this one is {} bits ({} hex digits)", need * 8, need * 2, have * 8, have * 2));
        }
        if keyid.is_none() && talkgroups.is_empty() {
            return Err(format!("{kid}: {name} ({algid:#04X}): give a key ID, talkgroups, or both"));
        }
        if keyid.is_some() && talkgroups.is_empty() && self.keys.iter().any(|o| o.alg == alg && o.keyid == keyid && o.talkgroups.is_empty()) {
            return Err(format!("{kid}: {name} ({algid:#04X}) is given twice"));
        }
        // Two keys for one talkgroup (and key ID, or any): only the first would ever be used.
        if let Some(tg) = talkgroups.iter().find(|tg| self.keys.iter().any(|o| o.alg == alg && o.keyid == keyid && o.talkgroups.contains(tg))) {
            return Err(format!("{kid}: {name} ({algid:#04X}) is given twice for talkgroup {tg}"));
        }
        self.keys.push(Key { alg, keyid, key: Zeroizing::new(key), talkgroups });
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// What is worth telling whoever loaded the keys ([`Key::warning`]).
    pub fn warnings(&self) -> impl Iterator<Item = String> + '_ {
        self.keys.iter().filter_map(Key::warning)
    }

    /// The key for voice under `algid` / `keyid` on talkgroup `tg`: first a
    /// key listing the talkgroup (and, if it gives one, the key ID), then
    /// one by key ID alone.
    pub fn lookup(&self, algid: u8, keyid: u16, tg: Option<u32>) -> Lookup<'_> {
        let Some(alg) = Algorithm::from_algid(algid) else { return Lookup::Unsupported };
        let by_tg = tg.and_then(|tg| self.keys.iter().find_map(|k| k.serves(alg).filter(|_| k.talkgroups.contains(&tg) && k.keyid.is_none_or(|id| id == keyid))));
        let by_kid = || self.keys.iter().find_map(|k| k.serves(alg).filter(|_| k.talkgroups.is_empty() && k.keyid == Some(keyid)));
        match by_tg.or_else(by_kid) {
            Some((alg, key)) => Lookup::Key(alg, key),
            None => Lookup::NoKey,
        }
    }

    /// The keystream for a superframe under `es` on talkgroup `tg` (None:
    /// clear, no key, or a cipher this can't decrypt).
    pub fn keystream(&self, es: &EncryptionSync, tg: Option<u32>, phase: Phase) -> Option<Keystream> {
        match self.lookup(es.algid, es.keyid, tg) {
            Lookup::Key(alg, key) => Keystream::new(alg, key, &es.mi, phase),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::crypt_vectors as v;

    /// Hex → bytes (spaces ignored).
    fn hex(s: &str) -> Vec<u8> {
        let s: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
        s.chunks(2).map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap()).collect()
    }

    fn mi9(s: &str) -> [u8; 9] {
        let mut mi = [0u8; 9];
        mi[..8].copy_from_slice(&hex(s));
        mi
    }

    /// Bits → bytes, MSB first (the last byte zero-padded).
    fn pack(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |v, (i, &b)| v | b << (7 - i))).collect()
    }

    /// Bytes → their first `n` bits, MSB first.
    fn unpack(bytes: &[u8], n: usize) -> Vec<u8> {
        (0..n).map(|i| bytes[i / 8] >> (7 - i % 8) & 1).collect()
    }

    // ── TIA-102.AAAD-B's examples ────────────────────────────────────────────

    /// The keystream for an example (MI: Table 3-1's header), checked
    /// against its output registers: B# 1 thrown away, B# 2..=`blocks` the
    /// voice's (B# 1 is then the only block that encrypts to B# 2).
    fn spec_keystream(alg: Algorithm, key: &str, out: &[&str], blocks: usize, phase: Phase) -> Keystream {
        let ks = Keystream::new(alg, &hex(key), &mi9(v::MI), phase).unwrap();
        let out: Vec<u8> = out[..blocks].iter().flat_map(|r| hex(r)).collect();
        assert_eq!(ks.ks[..], out[alg.discard()..], "{alg:?} {phase:?}: output registers");
        ks
    }

    /// An FDMA example's 18 voice frames, and the LSD (00 00 in the clear)
    /// before each LDU's last frame.
    fn fdma(ks: &Keystream, cipher: &[&str; 18], lsd: &[&str; 2]) {
        for f in 0..18 {
            let mut bits: [u8; 88] = unpack(&hex(v::FDMA_PLAIN[f]), 88).try_into().unwrap();
            ks.apply_p1(f / 9, f % 9, &mut bits);
            assert_eq!(pack(&bits), hex(cipher[f]), "voice frame {}", f + 1);
        }
        for (ldu, lsd) in lsd.iter().enumerate() {
            let at = Keystream::p1_offset(ldu, 8) - 2;
            assert_eq!(ks.ks[at..at + 2], hex(lsd)[..], "LDU{} LSD", ldu + 1);
        }
    }

    /// A TDMA example's 18 voice frames.
    fn tdma(ks: &Keystream, cipher: &[&str; 18]) {
        for f in 0..18 {
            let mut bits: [u8; 49] = unpack(&hex(v::TDMA_PLAIN[f]), 49).try_into().unwrap();
            ks.apply_p2(f, &mut bits);
            let mut want = hex(cipher[f]);
            want[6] &= 0x80; // past the 49th bit: not voice
            assert_eq!(pack(&bits), want, "voice frame {}", f + 1);
        }
    }

    /// Table 3-1: each superframe's MI from the last's, and back.
    #[test]
    fn table_3_1_mi_chain() {
        let mut mi = mi9(v::MI);
        for (i, want) in v::MI_CHAIN.iter().enumerate() {
            let next = next_mi(&mi);
            assert_eq!(next, mi9(want), "superframe {}", i + 1);
            assert_eq!(prev_mi(&next), mi, "superframe {} back", i + 1);
            mi = next;
        }
    }

    /// Table 4-1: AES's 128-bit IV, row by row (each row is Table 3-1's MI
    /// and the one after).
    #[test]
    fn table_4_1_aes_iv() {
        let mut mi = mi9(v::MI);
        for (row, next) in v::MI_CHAIN.iter().enumerate() {
            assert_eq!(aes_iv(&mi)[..], [&mi[..8], &hex(next)[..]].concat()[..], "row {row}");
            mi = mi9(next);
        }
    }

    /// A.4: DES-OFB, FDMA.
    #[test]
    fn a4_des_fdma() {
        fdma(&spec_keystream(Algorithm::DesOfb, v::DES_KEY, &v::DES_OUT, 28, Phase::One), &v::DES_FDMA, &v::DES_FDMA_LSD);
    }

    /// A.5: DES-OFB, TDMA.
    #[test]
    fn a5_des_tdma() {
        tdma(&spec_keystream(Algorithm::DesOfb, v::DES_KEY, &v::DES_OUT, 17, Phase::Two), &v::DES_TDMA);
    }

    /// B.4: 3DES-OFB, FDMA. The key is K1 K2 K3 in that order: swapping K1
    /// and K3 gives another keystream.
    #[test]
    fn b4_tdes_fdma() {
        fdma(&spec_keystream(Algorithm::Tdes, v::TDES_KEY, &v::TDES_OUT, 28, Phase::One), &v::TDES_FDMA, &v::TDES_FDMA_LSD);
        let k = hex(v::TDES_KEY);
        let swapped = [&k[16..], &k[8..16], &k[..8]].concat();
        let ks = Keystream::new(Algorithm::Tdes, &swapped, &mi9(v::MI), Phase::One).unwrap();
        assert_ne!(ks.ks[..8], hex(v::TDES_OUT[1])[..]);
    }

    /// B.5: 3DES-OFB, TDMA.
    #[test]
    fn b5_tdes_tdma() {
        tdma(&spec_keystream(Algorithm::Tdes, v::TDES_KEY, &v::TDES_OUT, 17, Phase::Two), &v::TDES_TDMA);
    }

    /// C.4: AES-256-OFB, FDMA.
    #[test]
    fn c4_aes_256_fdma() {
        fdma(&spec_keystream(Algorithm::Aes256, v::AES_256_KEY, &v::AES_256_OUT, 15, Phase::One), &v::AES_256_FDMA, &v::AES_256_FDMA_LSD);
    }

    /// C.5: AES-256-OFB, TDMA.
    #[test]
    fn c5_aes_256_tdma() {
        tdma(&spec_keystream(Algorithm::Aes256, v::AES_256_KEY, &v::AES_256_OUT, 9, Phase::Two), &v::AES_256_TDMA);
    }

    /// A.4 and B.4 with every key byte's DES parity bit (its LSB) flipped:
    /// the same ciphertext.
    #[test]
    fn des_parity_bits_ignored() {
        let flip = |k: &str| hex(k).iter().map(|b| format!("{:02X}", b ^ 1)).collect::<String>();
        fdma(&spec_keystream(Algorithm::DesOfb, &flip(v::DES_KEY), &v::DES_OUT, 28, Phase::One), &v::DES_FDMA, &v::DES_FDMA_LSD);
        fdma(&spec_keystream(Algorithm::Tdes, &flip(v::TDES_KEY), &v::TDES_OUT, 28, Phase::One), &v::TDES_FDMA, &v::TDES_FDMA_LSD);
    }

    // ── Other references ─────────────────────────────────────────────────────

    #[test]
    fn lfsr_matches_bitwise_polynomial() {
        // An independent statement of the register: a Fibonacci LFSR over
        // bits 1..64 (bit 64 the MSB), taps 64, 62, 46, 38, 27, 15.
        let mi = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x77];
        let mut bits: Vec<u8> = (0..64).map(|i| (mi64(&mi) >> (63 - i) & 1) as u8).collect();
        for _ in 0..64 {
            let fb = bits[0] ^ bits[2] ^ bits[18] ^ bits[26] ^ bits[37] ^ bits[49];
            bits.remove(0);
            bits.push(fb);
        }
        let want = bits.iter().fold(0u64, |v, &b| v << 1 | b as u64);
        let next = next_mi(&mi);
        assert_eq!(mi64(&next), want);
        assert_eq!(next[8], 0);
    }

    #[test]
    fn lfsr_runs_backwards() {
        let mut x = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..1000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let mut mi = [0u8; 9];
            mi[..8].copy_from_slice(&x.to_be_bytes());
            assert_eq!(next_mi(&prev_mi(&mi)), mi);
            assert_eq!(prev_mi(&next_mi(&mi)), mi);
        }
    }

    #[test]
    fn lfsr_predicts_the_air() {
        // Consecutive superframes' MIs from DCFD's AES-256 traffic
        // (captures/p2-*.cu8, 2026-10-03): Phase 1 LDU2s, then Phase 2 ESS.
        // Every one of the 233 transitions in the capture matched.
        let pairs = [
            ("339236983f24ce1b", "3fe93fe615cf7a70"),
            ("814a25fd3828a71c", "ac5b4b142b08d3bb"),
            ("e9dcc55b5960e123", "d8fca3b36ffd5d47"),
            ("830c0bbe9ebaddcd", "377d63848f1dec28"),
        ];
        for (a, b) in pairs {
            let mut mi = [0u8; 9];
            mi[..8].copy_from_slice(&hex(a));
            assert_eq!(&next_mi(&mi)[..8], &hex(b)[..], "after {a}");
            let mut nx = [0u8; 9];
            nx[..8].copy_from_slice(&hex(b));
            assert_eq!(&prev_mi(&nx)[..8], &hex(a)[..], "before {b}");
        }
    }

    #[test]
    fn lfsr_is_not_trivial() {
        assert_eq!(next_mi(&[0; 9]), [0; 9]);
        let mi = [0, 0, 0, 0, 0, 0, 0, 1, 0];
        assert_ne!(next_mi(&mi), mi);
    }

    #[test]
    fn rc4_rfc6229() {
        // RFC 6229, 40-bit key 0102030405: output bytes 0 and 256.
        let ks = rc4(&hex("0102030405"), 272);
        assert_eq!(&ks[..16], &hex("b2396305f03dc027ccc3524a0a1118a8")[..]);
        assert_eq!(&ks[256..272], &hex("1cfcf62b03eddb641d77dfcf7f8d8c93")[..]);
    }

    #[test]
    fn des_ofb_fips81() {
        // DES ECB known answer (FIPS 81 / the classic "Now is t" vector),
        // then OFB = the cipher on its own output.
        let key = hex("0123456789abcdef");
        let ks = block_cipher::<des::Des, 8>(&key, *b"Now is t", 2);
        assert_eq!(&ks[..8], &hex("3fa40e8a984d4815")[..]);
        let again = block_cipher::<des::Des, 8>(&key, ks[..8].try_into().unwrap(), 1);
        assert_eq!(&ks[8..], &again[..]);
    }

    #[test]
    fn aes_ofb_sp800_38a() {
        // SP 800-38A F.4.1 (OFB-AES128) and F.4.5 (OFB-AES256): keystream =
        // plaintext XOR ciphertext.
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let p = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        let c128 = hex("3b3fd92eb72dad20333449f8e83cfb4a7789508d16918f03f53c52dac54ed825");
        let ks = block_cipher::<aes::Aes128, 16>(&hex("2b7e151628aed2a6abf7158809cf4f3c"), iv, 2);
        assert_eq!(ks, p.iter().zip(&c128).map(|(a, b)| a ^ b).collect::<Vec<_>>());
        let c256 = hex("dc7e84bfda79164b7ecd8486985d38604febdc6740d20b3ac88f6ad82a4fb08d");
        let key = hex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let ks = block_cipher::<aes::Aes256, 16>(&key, iv, 2);
        assert_eq!(ks, p.iter().zip(&c256).map(|(a, b)| a ^ b).collect::<Vec<_>>());
    }

    #[test]
    fn adp_discards_256_bytes() {
        // Not in TIA-102.AAAD (ADP is Motorola's); the layout past the
        // discard is the spec's, as the DES/AES examples check.
        let mi = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let key = hex("1234567890");
        let ks = Keystream::new(Algorithm::Adp, &key, &mi, Phase::One).unwrap();
        let raw = rc4(&[&key[..], &mi[..8]].concat(), 256 + 22);
        let mut bits = [0u8; 88];
        ks.apply_p1(0, 0, &mut bits);
        assert_eq!(pack(&bits), &raw[256 + 11..]);
    }

    #[test]
    fn every_cipher_round_trips_and_checks_key_length() {
        let mi = [0xde, 0xad, 0xbe, 0xef, 0, 1, 2, 3, 4];
        for alg in [Algorithm::Adp, Algorithm::DesOfb, Algorithm::Tdes, Algorithm::Aes128, Algorithm::Aes256] {
            assert_eq!(Algorithm::from_algid(alg.algid()), Some(alg));
            assert!(Keystream::new(alg, &vec![7; alg.key_len() + 1], &mi, Phase::One).is_none());
            let key: Vec<u8> = (0..alg.key_len() as u8).collect();
            for phase in [Phase::One, Phase::Two] {
                let ks = Keystream::new(alg, &key, &mi, phase).unwrap();
                let clear: [u8; 88] = std::array::from_fn(|i| (i * 7 % 3 == 0) as u8);
                let (mut a, mut b) = (clear, [0u8; 49]);
                match phase {
                    Phase::One => {
                        ks.apply_p1(1, 8, &mut a);
                        assert_ne!(a, clear);
                        ks.apply_p1(1, 8, &mut a);
                        assert_eq!(a, clear);
                    }
                    Phase::Two => {
                        ks.apply_p2(17, &mut b);
                        assert_ne!(b, [0; 49]);
                    }
                }
            }
        }
        assert_eq!(Algorithm::from_algid(ALGID_CLEAR), None);
        assert_eq!(algid_name(0xAA), "ADP");
    }

    #[test]
    fn keyring_refuses_bad_keys_and_finds_good_ones() {
        let mut r = Keyring::default();
        let e = r.add(ALGID_AES_256, Some(0x0002), vec![1; 16], vec![]).unwrap_err();
        assert!(e.contains("0x0002") && e.contains("AES-256") && e.contains("256-bit") && e.contains("this one is 128 bits"), "{e}");
        assert!(r.add(0x9F, Some(1), vec![1; 8], vec![]).unwrap_err().contains("DES-XL"));
        assert!(r.add(ALGID_ADP, None, vec![1; 5], vec![]).is_err());
        r.add(ALGID_ADP, Some(0x1B50), vec![1; 5], vec![]).unwrap();
        assert!(r.add(ALGID_ADP, Some(0x1B50), vec![2; 5], vec![]).unwrap_err().contains("twice"));
        r.add(ALGID_ADP, None, vec![3; 5], vec![1201]).unwrap();
        assert!(r.add(ALGID_ADP, None, vec![5; 5], vec![1201, 1202]).unwrap_err().contains("talkgroup 1201"));
        r.add(ALGID_AES_256, Some(0x0002), vec![4; 32], vec![]).unwrap();
        assert_eq!(r.lookup(ALGID_ADP, 0x1B50, Some(7)), Lookup::Key(Algorithm::Adp, &[1; 5]));
        // The talkgroup's key, whatever the key ID.
        assert_eq!(r.lookup(ALGID_ADP, 0x1B50, Some(1201)), Lookup::Key(Algorithm::Adp, &[3; 5]));
        assert_eq!(r.lookup(ALGID_ADP, 0x0001, Some(1201)), Lookup::Key(Algorithm::Adp, &[3; 5]));
        assert_eq!(r.lookup(ALGID_ADP, 0x0001, None), Lookup::NoKey);
        assert_eq!(r.lookup(ALGID_AES_256, 0x1B50, None), Lookup::NoKey);
        assert_eq!(r.lookup(ALGID_AES_256, 0x0002, None), Lookup::Key(Algorithm::Aes256, &[4; 32]));
        assert_eq!(r.lookup(0xA0, 0x0002, None), Lookup::Unsupported);
        assert!(!format!("{r:?}").contains("[4, 4"));
    }
}
