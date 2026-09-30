//! P25 Phase 1 voice framing (op25 p25p1_fdma.cc process_HDU / process_LLDU /
//! process_LDU2 / process_TDU15 / process_LCW, and op25_imbe_frame.h
//! imbe_header_decode).
//!
//! Frame bodies are op25's: bits from the first bit of the sync with the
//! status symbols left in place ([`Frame::raw`]), because every table here
//! indexes that layout.

use super::fec::{golay23_decode, golay23_decode_soft, golay24_decode, hamming1063_decode, hamming15_decode, hamming15_decode_soft, rs_decode, Fec};
use super::frame::Frame;
use crate::tables::{HDU_CODEWORD_BITS, LDU_LS_DATA_BITS, VOICE_CODEWORD_BITS};

pub const ALGID_CLEAR: u8 = 0x80;

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
    let mut p = ImbeParams::default();
    let r0 = golay(extract(cw, 0, 23), 0);
    p.u[0] = r0.data;
    p.e0 = count(&r0);
    p.errs = p.e0;
    p.cost0 = r0.cost;
    p.cost = r0.cost;
    if let Some(s) = soft {
        p.mean_rel = s.iter().sum::<f32>() / 144.0;
    }
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
pub struct LinkControl {
    pub lco: u8,
    pub mfid: u8,
    pub protected: bool,
    pub svc_opts: Option<u8>,
    pub tgid: Option<u32>,
    pub target: Option<u32>,
    pub source: Option<u32>,
    /// The word itself (manufacturer-specific formats: talker aliases).
    pub raw: [u8; 9],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncryptionSync {
    pub algid: u8,
    pub keyid: u16,
    pub mi: [u8; 9],
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

fn hexbits_to_bytes(hb: &[u8; 63], from: usize) -> [u8; 9] {
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

fn ldu_hexbits(fb: &[u8]) -> [u8; 63] {
    let mut hb = [0u8; 63];
    let mut k = 0;
    for i in 0..24 {
        let mut cw = 0u32;
        for _ in 0..10 {
            cw = (cw << 1) | bit(fb, LDU_LS_DATA_BITS[k] as usize);
            k += 1;
        }
        hb[39 + i] = hamming1063_decode(cw >> 4, cw & 0x0f);
    }
    hb
}

pub fn parse_lcw(w: &[u8; 9]) -> LinkControl {
    let mut lc = LinkControl { lco: w[0] & 0x3f, mfid: w[1], protected: w[0] & 0x80 != 0, raw: *w, ..Default::default() };
    if lc.protected {
        return lc;
    }
    let u24 = |a: usize| ((w[a] as u32) << 16) | ((w[a + 1] as u32) << 8) | w[a + 2] as u32;
    match lc.lco {
        0x00 => {
            lc.svc_opts = Some(w[2]);
            lc.tgid = Some(((w[4] as u32) << 8) | w[5] as u32);
            lc.source = Some(u24(6));
        }
        0x03 => {
            lc.svc_opts = Some(w[2]);
            lc.target = Some(u24(3));
            lc.source = Some(u24(6));
        }
        _ => {}
    }
    lc
}

/// LDU1 link control: RS(24,12,13), ≤ 6 corrections.
pub fn decode_ldu1_lc(fb: &[u8]) -> Option<LinkControl> {
    let mut hb = ldu_hexbits(fb);
    let ec = rs_decode(&mut hb, 12, 39);
    (0..=6).contains(&ec).then(|| parse_lcw(&hexbits_to_bytes(&hb, 39)))
}

/// LDU2 encryption sync: RS(24,16,9), ≤ 4 corrections.
pub fn decode_ldu2_es(fb: &[u8]) -> Option<EncryptionSync> {
    let mut hb = ldu_hexbits(fb);
    let ec = rs_decode(&mut hb, 8, 39);
    if !(0..=4).contains(&ec) {
        return None;
    }
    let j = 51;
    Some(EncryptionSync {
        mi: hexbits_to_bytes(&hb, 39),
        algid: ((hb[j] << 2) | (hb[j + 1] >> 4)) as u8,
        keyid: (((hb[j + 1] & 0x0f) as u16) << 12) | ((hb[j + 2] as u16) << 6) | hb[j + 3] as u16,
    })
}

/// HDU: 36 Golay(18,6) hexbits → RS(36,20,17), ≤ 8 corrections.
pub fn decode_hdu(fb: &[u8]) -> Option<HduInfo> {
    let mut hb = [0u8; 63];
    let mut k = 0;
    for i in 0..36 {
        let mut cw = 0u32;
        for _ in 0..18 {
            cw = (cw << 1) | bit(fb, HDU_CODEWORD_BITS[k] as usize);
            k += 1;
        }
        hb[27 + i] = (golay24_decode(cw).data & 63) as u8;
    }
    let ec = rs_decode(&mut hb, 16, 27);
    if !(0..=8).contains(&ec) {
        return None;
    }
    let j = 39;
    let h = |i: usize| hb[j + i] as u16;
    Some(HduInfo {
        es: EncryptionSync {
            mi: hexbits_to_bytes(&hb, 27),
            algid: (((h(1) & 0x0f) << 4) | (h(2) >> 2)) as u8,
            keyid: ((h(2) & 0x03) << 14) | (h(3) << 8) | (h(4) << 2) | (h(5) >> 4),
        },
        mfid: ((h(0) << 2) | (h(1) >> 4)) as u8,
        tgid: ((h(5) & 0x0f) << 12) | (h(6) << 6) | h(7),
    })
}

/// TDULC: 12 Golay(24,12) words → link control.
pub fn decode_tdulc(fb: &[u8]) -> Option<LinkControl> {
    let mut hb = [0u8; 63];
    let mut k = 0;
    for i in (0..=22).step_by(2) {
        let mut cw = 0u32;
        for _ in 0..24 {
            cw = (cw << 1) | bit(fb, HDU_CODEWORD_BITS[k] as usize);
            k += 1;
        }
        let d = golay24_decode(cw).data;
        hb[39 + i] = (d >> 6) as u8;
        hb[40 + i] = (d & 63) as u8;
    }
    let ec = rs_decode(&mut hb, 12, 39);
    (0..=6).contains(&ec).then(|| parse_lcw(&hexbits_to_bytes(&hb, 39)))
}
