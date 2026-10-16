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
/// AES-128's ALGID in TIA-102.BAAC-C's table ($85).
pub const ALGID_AES_128: u8 = 0x85;
/// The AES-128 ALGID this decoder read before, kept so keys files that name it still work.
pub const ALGID_AES_128_ALT: u8 = 0x89;
pub const ALGID_ADP: u8 = 0xAA;

/// What an ALGID is called (TIA-102.BAAC's reserved values, and the
/// manufacturers' that are seen on the air).
pub fn algid_name(algid: u8) -> &'static str {
    match algid {
        0x00..=0x05 | 0x41 => "Type 1",
        0x80 => "clear",
        0x81 => "DES-OFB",
        0x82 => "2-key 3DES",
        0x83 => "3DES",
        0x84 => "AES-256",
        0x85 => "AES-128",
        0x88 => "AES-CBC",
        0x89 => "AES-128 (0x89)",
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
            ALGID_AES_128 | ALGID_AES_128_ALT => Some(Algorithm::Aes128),
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
    /// From the first voice byte (the discard dropped).
    ks: Vec<u8>,
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
        Some(Keystream { phase, ks })
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

    /// Decrypt (or encrypt) AMBE+2 frame `f` (0–17) of the superframe: its
    /// 49 bits (decode_vcw).
    pub fn apply_p2(&self, f: usize, bits: &mut [u8; 49]) {
        debug_assert_eq!(self.phase, Phase::Two);
        self.xor(7 * f, bits);
    }
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
        self.keys.push(Key { alg, keyid, key: Zeroizing::new(key), talkgroups });
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// The key for voice under `algid` / `keyid` on talkgroup `tg`: first a
    /// key listing the talkgroup (and, if it gives one, the key ID), then
    /// one by key ID alone.
    pub fn lookup(&self, algid: u8, keyid: u16, tg: Option<u32>) -> Lookup<'_> {
        let Some(alg) = Algorithm::from_algid(algid) else { return Lookup::Unsupported };
        let by_tg = tg.and_then(|tg| self.keys.iter().find(|k| k.alg == alg && k.talkgroups.contains(&tg) && k.keyid.is_none_or(|id| id == keyid)));
        let by_kid = || self.keys.iter().find(|k| k.alg == alg && k.talkgroups.is_empty() && k.keyid == Some(keyid));
        match by_tg.or_else(by_kid) {
            Some(k) => Lookup::Key(alg, &k.key),
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

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Bits → bytes, MSB first (the last byte zero-padded).
    fn pack(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |v, (i, &b)| v | b << (7 - i))).collect()
    }

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
        assert_eq!(&aes_iv(&mi)[..8], &mi[..8]);
        assert_eq!(&aes_iv(&mi)[8..], &next_mi(&mi)[..8]);
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
    fn tdes_with_one_key_three_times_is_des() {
        let k = hex("0123456789abcdef");
        let k3 = [k.clone(), k.clone(), k.clone()].concat();
        let iv = *b"\x12\x34\x56\x78\x90\xab\xcd\xef";
        assert_eq!(block_cipher::<des::TdesEde3, 8>(&k3, iv, 4), block_cipher::<des::Des, 8>(&k, iv, 4));
    }

    #[test]
    fn phase1_layout() {
        // The voice bytes of a superframe: contiguous per LDU, 11 a frame,
        // the LSD's 2 before frame 8, LDU2 101 bytes on; ADP's first voice
        // byte is keystream byte 267.
        let mi = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let key = hex("1234567890");
        let ks = Keystream::new(Algorithm::Adp, &key, &mi, Phase::One).unwrap();
        let mut k13 = key.clone();
        k13.extend_from_slice(&mi[..8]);
        let raw = rc4(&k13, 469);
        for ldu in 0..2 {
            for k in 0..9 {
                let mut bits = [0u8; 88];
                ks.apply_p1(ldu, k, &mut bits);
                let at = 267 + 101 * ldu + 11 * k + if k == 8 { 2 } else { 0 };
                assert_eq!(pack(&bits), &raw[at..at + 11], "LDU{} frame {k}", ldu + 1);
            }
        }
    }

    #[test]
    fn phase2_layout() {
        let mi = [9, 8, 7, 6, 5, 4, 3, 2, 1];
        let key = hex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let ks = Keystream::new(Algorithm::Aes256, &key, &mi, Phase::Two).unwrap();
        let raw = block_cipher::<aes::Aes256, 16>(&key, aes_iv(&mi), 9);
        for f in 0..18 {
            let mut bits = [0u8; 49];
            ks.apply_p2(f, &mut bits);
            let mut want = raw[16 + 7 * f..16 + 7 * f + 7].to_vec();
            want[6] &= 0x80;
            assert_eq!(pack(&bits), want, "frame {f}");
        }
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

    #[test]
    fn aes_128_is_algid_0x85_and_0x89_is_read_as_the_same() {
        assert_eq!(Algorithm::from_algid(0x85), Some(Algorithm::Aes128));
        assert_eq!(Algorithm::from_algid(0x89), Some(Algorithm::Aes128));
        assert_eq!(Algorithm::Aes128.algid(), 0x85);
        assert_eq!(algid_name(0x85), "AES-128");
        let mut ring = Keyring::default();
        ring.add(0x89, Some(1), vec![7; 16], vec![]).unwrap();
        for id in [0x85, 0x89] {
            assert!(matches!(ring.lookup(id, 1, None), Lookup::Key(Algorithm::Aes128, _)), "{id:#04X}");
        }
    }
}
