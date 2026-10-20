//! P25 Phase 1 voice framing (op25 p25p1_fdma.cc process_HDU / process_LLDU /
//! process_LDU2 / process_TDU15 / process_LCW, and op25_imbe_frame.h
//! imbe_header_decode).
//!
//! Frame bodies are op25's: bits from the first bit of the sync with the
//! status symbols left in place ([`Frame::raw`]), because every table here
//! indexes that layout.

use super::fec::{
    golay18_codewords, golay23_decode, golay23_decode_soft, golay23_decode_soft2, golay23_encode, golay24_codewords, golay24_decode, hamming1063_codewords,
    hamming1063_decode, hamming15_decode, hamming15_decode_soft, hamming15_encode, ml_decode, rs_decode_erasures, rs_decode_gmd, Fec,
};
use super::frame::Frame;
use super::lc::{self, LinkControl};
use crate::tables::{HDU_CODEWORD_BITS, LDU_LS_DATA_BITS, VOICE_CODEWORD_BITS};

pub use super::crypt::ALGID_CLEAR;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImbeParams {
    /// u0..u6, and u7 stored <<1 (op25).
    pub u: [u32; 8],
    /// Bits corrected; an uncorrectable word counts 4.
    pub errs: u32,
    /// Errors in u0 (the pitch).
    pub e0: u32,
    /// Soft decoding: reliability overridden, all words / u0's word.
    pub cost: f32,
    pub cost0: f32,
    /// Mean bit reliability of the codeword (soft only).
    pub mean_rel: f32,
    /// Its bits with no reliability at all (soft only): symbols the receiver
    /// heard nothing in, e.g. a fade. The soft decoder flips them for free.
    pub erased: u32,
}

#[inline]
fn extract(cw: &[u8], begin: usize, end: usize) -> u32 {
    cw[begin..end].iter().fold(0, |v, &b| (v << 1) | (b & 1) as u32)
}
#[inline]
fn pngen(pr: &mut u32, nbits: u32) -> u32 {
    let mut n = 0;
    for i in (0..nbits).rev() {
        *pr = (173 * *pr + 13849) & 0xffff;
        if *pr & 32768 != 0 {
            n += 1 << i;
        }
    }
    n
}

/// 144-bit IMBE codeword → u0..u7. With `soft` (144 reliabilities, parallel
/// to `cw`) the Golay words are Chase-decoded and the Hamming words decoded by
/// maximum likelihood.
///
/// u0 seeds the PN mask over u1..u6, so a wrong u0 unscrambles the rest
/// wrong too. Soft, when u0 wasn't clean, its Chase runner-up is tried as
/// well, and whichever pairing costs least over all seven words wins.
pub fn imbe_header_decode(cw: &[u8; 144], soft: Option<&[f32; 144]>) -> ImbeParams {
    let count = |r: &Fec| if r.errs < 0 { 4 } else { r.errs as u32 };
    let golay = |v: u32, at: usize| match soft {
        Some(s) => golay23_decode_soft(v, &s[at..at + 23]),
        None => golay23_decode(v),
    };
    let hamming = |v: u32, at: usize| match soft {
        Some(s) => hamming15_decode_soft(v, &s[at..at + 15]),
        None => hamming15_decode(v),
    };
    let (r0, alt) = match soft {
        Some(s) => golay23_decode_soft2(extract(cw, 0, 23), &s[..23]),
        None => (golay23_decode(extract(cw, 0, 23)), None),
    };
    let decode = |r0: Fec| {
        let mut p = ImbeParams::default();
        p.u[0] = r0.data;
        p.e0 = count(&r0);
        p.errs = p.e0;
        p.cost0 = r0.cost;
        p.cost = r0.cost;
        let mut pr = p.u[0] << 4;
        for k in 1..=3 {
            let m = pngen(&mut pr, 23);
            let r = golay(extract(cw, 23 * k, 23 * (k + 1)) ^ m, 23 * k);
            p.u[k] = r.data;
            p.errs += count(&r);
            p.cost += r.cost;
        }
        for k in 4..=6 {
            let m = pngen(&mut pr, 15);
            let s = 92 + (k - 4) * 15;
            let r = hamming(extract(cw, s, s + 15) ^ m, s);
            p.u[k] = r.data;
            p.errs += count(&r);
            p.cost += r.cost;
        }
        p.u[7] = extract(cw, 137, 144) << 1;
        p
    };
    let mut p = decode(r0);
    if let Some(alt) = alt.filter(|a| U0_RUNNER_UP.load(std::sync::atomic::Ordering::Relaxed) && a.cost < p.cost) {
        let q = decode(alt);
        if q.cost < p.cost {
            p = q;
        }
    }
    if let Some(s) = soft {
        p.mean_rel = s.iter().sum::<f32>() / 144.0;
        p.erased = s.iter().filter(|&&r| r == 0.0).count() as u32;
    }
    p
}

/// Research switch for `tool snr` (the u0 runner-up on or off).
pub static U0_RUNNER_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// u0..u7 → the 144-bit codeword: [`imbe_header_decode`] backwards (for
/// test signals).
pub fn imbe_header_encode(u: &[u32; 8]) -> [u8; 144] {
    let mut cw = [0u8; 144];
    let mut put = |at: usize, n: usize, v: u32| {
        for i in 0..n {
            cw[at + i] = ((v >> (n - 1 - i)) & 1) as u8;
        }
    };
    put(0, 23, golay23_encode(u[0]));
    let mut pr = u[0] << 4;
    for k in 1..=3 {
        let m = pngen(&mut pr, 23);
        put(23 * k, 23, golay23_encode(u[k]) ^ m);
    }
    for k in 4..=6 {
        let m = pngen(&mut pr, 15);
        put(92 + (k - 4) * 15, 15, hamming15_encode(u[k]) ^ m);
    }
    put(137, 7, u[7] >> 1);
    cw
}

/// mbelib's imbe_d[88] → u0..u7 ([`imbe_params_to_bits`] backwards).
pub fn imbe_bits_to_params(bits: &[u8; 88]) -> [u32; 8] {
    const W: [usize; 8] = [12, 12, 12, 12, 11, 11, 11, 7];
    let mut u = [0u32; 8];
    let mut p = 0;
    for k in 0..8 {
        u[k] = bits[p..p + W[k]].iter().fold(0, |v, &b| v << 1 | b as u32);
        p += W[k];
    }
    u[7] <<= 1;
    u
}

/// u0..u7 → mbelib's imbe_d[88].
pub fn imbe_params_to_bits(u: &[u32; 8]) -> [u8; 88] {
    const W: [u32; 8] = [12, 12, 12, 12, 11, 11, 11, 7];
    let mut d = [0u8; 88];
    let mut p = 0;
    for k in 0..8 {
        let v = if k == 7 { u[7] >> 1 } else { u[k] };
        for b in (0..W[k]).rev() {
            d[p] = ((v >> b) & 1) as u8;
            p += 1;
        }
    }
    d
}

/// IMBE codeword `f` (0..8) of an LDU and its bit reliabilities (missing: 0).
pub fn ldu_codeword(fr: &Frame, f: usize) -> ([u8; 144], [f32; 144]) {
    let (mut cw, mut soft) = ([0u8; 144], [0f32; 144]);
    for j in 0..144 {
        let k = VOICE_CODEWORD_BITS[f * 144 + j] as usize;
        cw[j] = fr.raw.get(k).map_or(0, |b| b & 1);
        soft[j] = fr.raw_soft.get(k).copied().unwrap_or(0.0);
    }
    (cw, soft)
}

/// The last frame-body bit IMBE codeword `f` occupies.
pub fn ldu_codeword_end_bit(f: usize) -> usize {
    VOICE_CODEWORD_BITS[f * 144..(f + 1) * 144].iter().copied().max().unwrap_or(0) as usize
}

/// Decode all nine codewords of an LDU (soft when the frame carries reliabilities).
pub fn ldu_imbe(fr: &Frame, soft: bool) -> [ImbeParams; 9] {
    std::array::from_fn(|k| {
        let (cw, s) = ldu_codeword(fr, k);
        imbe_header_decode(&cw, (soft && !fr.raw_soft.is_empty()).then_some(&s))
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncryptionSync {
    pub algid: u8,
    pub keyid: u16,
    pub mi: [u8; 9],
}

impl EncryptionSync {
    /// Whether it can be what was sent; if not, the RS decoder corrected to
    /// the wrong word. Clear voice's MI is null (AAAD-B Table 5-1);
    /// encrypted voice's never is (BAAC-C §2.6, AAAD-B §3.1), and is the
    /// LFSR's 64 bits with 8 zeros after them (AAAD-B §5.2).
    pub fn plausible(&self) -> bool {
        if self.algid == ALGID_CLEAR {
            self.mi == [0; 9]
        } else {
            self.mi[..8] != [0; 8] && self.mi[8] == 0
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HduInfo {
    pub es: EncryptionSync,
    pub mfid: u8,
    pub tgid: u16,
}

#[inline]
fn bit(fb: &[u8], k: usize) -> u32 {
    fb.get(k).map_or(0, |b| (b & 1) as u32)
}

/// Nine bytes from the hexbits at `from` on (three bytes per four hexbits).
pub(crate) fn hexbits_to_bytes(hb: &[u8; 63], from: usize) -> [u8; 9] {
    let mut out = [0u8; 9];
    let (mut n, mut j) = (0, from);
    while n < 9 {
        out[n] = ((hb[j] << 2) | (hb[j + 1] >> 4)) as u8;
        n += 1;
        if n < 9 {
            out[n] = (((hb[j + 1] & 0x0f) << 4) | (hb[j + 2] >> 2)) as u8;
            n += 1;
        }
        if n < 9 {
            out[n] = (((hb[j + 2] & 0x03) << 6) | hb[j + 3]) as u8;
            n += 1;
        }
        j += 4;
    }
    out
}

/// Bytes → hexbits (MSB first; the last one zero-filled): [`hexbits_to_bytes`]
/// backwards.
pub(crate) fn bytes_to_hexbits(bytes: &[u8]) -> Vec<u8> {
    let n = (bytes.len() * 8).div_ceil(6);
    (0..n)
        .map(|h| (0..6).fold(0u8, |v, i| {
            let b = 6 * h + i;
            v << 1 | bytes.get(b / 8).map_or(0, |x| (x >> (7 - b % 8)) & 1)
        }))
        .collect()
}

/// Parity symbols the soft RS decoders keep spare on a try with erasures
/// (see [`rs_decode_gmd`]), so a word too damaged to decode is refused
/// rather than rewritten to the nearest codeword. LC has no other check
/// behind it.
const LC_SPARE: usize = 4;
const ES_SPARE: usize = 4;
const HDU_SPARE: usize = 6;

/// A small code's word read from frame-body bits `at` (MSB first) and,
/// when soft, their reliabilities.
fn word(f: &Frame, at: &[u16], soft: bool) -> (u32, Option<Vec<f32>>) {
    let cw = at.iter().fold(0, |cw, &k| (cw << 1) | bit(&f.raw, k as usize));
    let w = (soft && !f.raw_soft.is_empty()).then(|| at.iter().map(|&k| f.raw_soft.get(k as usize).copied().unwrap_or(0.0)).collect());
    (cw, w)
}

/// An LDU's 24 Hamming(10,6) hexbits (from 39, as RS(63,·) shortened), the
/// erasures (hard: words with more errors than the code corrects) and each
/// hexbit's reliability (soft: maximum likelihood's margin).
fn ldu_hexbits(f: &Frame, soft: bool) -> ([u8; 63], Vec<usize>, Option<[f32; 63]>) {
    let mut hb = [0u8; 63];
    let mut rel = [f32::INFINITY; 63];
    let mut erasures = Vec::new();
    let mut is_soft = false;
    for i in 0..24 {
        let (cw, w) = word(f, &LDU_LS_DATA_BITS[10 * i..10 * i + 10], soft);
        if let Some(w) = w {
            let r = ml_decode(cw, &w, 10, hamming1063_codewords());
            hb[39 + i] = r.data as u8;
            rel[39 + i] = r.margin;
            is_soft = true;
            continue;
        }
        match hamming1063_decode(cw >> 4, cw & 0x0f) {
            Some(h) => hb[39 + i] = h,
            None => {
                hb[39 + i] = (cw >> 4) as u8;
                erasures.push(39 + i);
            }
        }
    }
    (hb, erasures, is_soft.then_some(rel))
}

/// RS errors-and-erasures (hard: the words found too damaged erased), or
/// GMD when there are reliabilities.
fn rs(hb: &mut [u8; 63], nroots: usize, erasures: &[usize], first: usize, rel: Option<[f32; 63]>, spare: usize) -> bool {
    match rel {
        Some(rel) => rs_decode_gmd(hb, nroots, first, &rel, spare) >= 0,
        None => rs_decode_erasures(hb, nroots, erasures, first) >= 0,
    }
}

/// LDU1 link control: RS(24,12,13). `soft`: from the frame's bit
/// reliabilities, when it has them.
pub fn decode_ldu1_lc(f: &Frame, soft: bool) -> Option<LinkControl> {
    let (mut hb, erasures, rel) = ldu_hexbits(f, soft);
    rs(&mut hb, 12, &erasures, 39, rel, LC_SPARE).then(|| lc::parse(&hexbits_to_bytes(&hb, 39)))
}

/// LDU2 encryption sync: RS(24,16,9).
pub fn decode_ldu2_es(f: &Frame, soft: bool) -> Option<EncryptionSync> {
    let (mut hb, erasures, rel) = ldu_hexbits(f, soft);
    if !rs(&mut hb, 8, &erasures, 39, rel, ES_SPARE) {
        return None;
    }
    let j = 51;
    let es = EncryptionSync {
        mi: hexbits_to_bytes(&hb, 39),
        algid: ((hb[j] << 2) | (hb[j + 1] >> 4)) as u8,
        keyid: (((hb[j + 1] & 0x0f) as u16) << 12) | ((hb[j + 2] as u16) << 6) | hb[j + 3] as u16,
    };
    es.plausible().then_some(es)
}

/// HDU: 36 Golay(18,6) hexbits → RS(36,20,17). The (18,6) code is the
/// (24,12) with its first six bits deleted (BAAA-A §5.7): hard, a
/// correction there means the word was too damaged, an erasure.
pub fn decode_hdu(f: &Frame, soft: bool) -> Option<HduInfo> {
    let mut hb = [0u8; 63];
    let mut rel = [f32::INFINITY; 63];
    let mut erasures = Vec::new();
    let mut is_soft = false;
    for i in 0..36 {
        let (cw, w) = word(f, &HDU_CODEWORD_BITS[18 * i..18 * i + 18], soft);
        if let Some(w) = w {
            let r = ml_decode(cw, &w, 18, golay18_codewords());
            hb[27 + i] = r.data as u8;
            rel[27 + i] = r.margin;
            is_soft = true;
            continue;
        }
        let r = golay24_decode(cw);
        hb[27 + i] = (r.data & 63) as u8;
        if r.errs < 0 || r.data >> 6 != 0 {
            erasures.push(27 + i);
        }
    }
    if !rs(&mut hb, 16, &erasures, 27, is_soft.then_some(rel), HDU_SPARE) {
        return None;
    }
    let j = 39;
    let h = |i: usize| hb[j + i] as u16;
    let hdu = HduInfo {
        es: EncryptionSync {
            mi: hexbits_to_bytes(&hb, 27),
            algid: (((h(1) & 0x0f) << 4) | (h(2) >> 2)) as u8,
            keyid: ((h(2) & 0x03) << 14) | (h(3) << 8) | (h(4) << 2) | (h(5) >> 4),
        },
        mfid: ((h(0) << 2) | (h(1) >> 4)) as u8,
        tgid: ((h(5) & 0x0f) << 12) | (h(6) << 6) | h(7),
    };
    hdu.es.plausible().then_some(hdu)
}

/// TDULC: 12 Golay(24,12) words, two hexbits each (hard: a word it can't
/// correct, both erasures) → RS(24,12,13) → link control.
pub fn decode_tdulc(f: &Frame, soft: bool) -> Option<LinkControl> {
    let mut hb = [0u8; 63];
    let mut rel = [f32::INFINITY; 63];
    let mut erasures = Vec::new();
    let mut is_soft = false;
    for i in (0..=22).step_by(2) {
        let (cw, w) = word(f, &HDU_CODEWORD_BITS[12 * i..12 * i + 24], soft);
        let r = match w {
            Some(w) => {
                is_soft = true;
                ml_decode(cw, &w, 24, golay24_codewords())
            }
            None => golay24_decode(cw),
        };
        hb[39 + i] = (r.data >> 6) as u8;
        hb[40 + i] = (r.data & 63) as u8;
        (rel[39 + i], rel[40 + i]) = (r.margin, r.margin);
        if r.errs < 0 {
            erasures.extend([39 + i, 40 + i]);
        }
    }
    rs(&mut hb, 12, &erasures, 39, is_soft.then_some(rel), LC_SPARE).then(|| lc::parse(&hexbits_to_bytes(&hb, 39)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AAAD-B Table 5-1, §5.2, BAAC-C §2.6: clear's MI is null; encrypted's never is, and ends in $00.
    #[test]
    fn an_implausible_sync_is_refused() {
        let es = |algid: u8, mi: [u8; 9]| EncryptionSync { algid, keyid: 1, mi };
        assert!(es(0x84, [1, 2, 3, 4, 5, 6, 7, 8, 0]).plausible());
        assert!(!es(0x84, [0; 9]).plausible());
        assert!(!es(0x84, [1, 2, 3, 4, 5, 6, 7, 8, 9]).plausible());
        assert!(es(ALGID_CLEAR, [0; 9]).plausible());
        assert!(!es(ALGID_CLEAR, [1, 2, 3, 4, 5, 6, 7, 8, 0]).plausible());
    }
}
