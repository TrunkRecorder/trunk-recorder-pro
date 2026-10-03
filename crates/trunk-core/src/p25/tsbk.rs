//! TSDU → TSBKs: block de-interleave + 1/2-rate trellis decode and the TSBK
//! CRC-16 (op25 p25p1_fdma.cc, after wireshark packet-p25cai.c).
//!
//! [`trellis_viterbi`] replaces op25's greedy walk, which gives up on a tie
//! (and then drops the rest of the TSDU): with soft bits it takes simulcast
//! control channels from 62 % to 98–99 % of TSBKs.

use super::frame::{Frame, TSDU};
use crate::bits::crc_ccitt_bytes;

pub type Tsbk = [u8; 12];

/// op25's 196-entry table: 12 rows of four 4-bit groups at 4g, 52+4g, 100+4g,
/// 148+4g, then the flush group 48..51.
static TB: [u8; 196] = {
    let mut t = [0u8; 196];
    let mut k = 0;
    let mut g = 0;
    while g < 12 {
        let bases = [0u8, 52, 100, 148];
        let mut bi = 0;
        while bi < 4 {
            let mut e = 0;
            while e < 4 {
                t[k] = bases[bi] + 4 * g as u8 + e as u8;
                k += 1;
                e += 1;
            }
            bi += 1;
        }
        g += 1;
    }
    let mut e = 0;
    while e < 4 {
        t[k] = 48 + e as u8;
        k += 1;
        e += 1;
    }
    t
};
static NEXT: [[u8; 4]; 4] = [[0x2, 0xc, 0x1, 0xf], [0xe, 0x0, 0xd, 0x3], [0x9, 0x7, 0xa, 0x4], [0x5, 0xb, 0x6, 0x8]];

/// op25's greedy decoder: `None` on a tie.
pub fn trellis_greedy(bits: &[u8]) -> Option<Tsbk> {
    let mut out = [0u8; 12];
    let mut state = 0usize;
    for b in (0..196).step_by(4) {
        let cw = (bits[TB[b] as usize] << 3) | (bits[TB[b + 1] as usize] << 2) | (bits[TB[b + 2] as usize] << 1) | bits[TB[b + 3] as usize];
        let (mut best, mut min, mut unique) = (0usize, 99u32, true);
        for j in 0..4 {
            let hd = (cw ^ NEXT[state][j]).count_ones();
            if hd < min {
                min = hd;
                best = j;
                unique = true;
            } else if hd == min {
                unique = false;
            }
        }
        if !unique {
            return None;
        }
        state = best;
        let d = b >> 2;
        if d < 48 {
            out[d >> 2] |= (state << (6 - (d % 4) * 2)) as u8;
        }
    }
    Some(out)
}

/// Viterbi decode over the 4-state trellis (49 steps, the last a flush to 0).
/// `soft` (optional, parallel to `bits`) weights each bit; none = Hamming.
pub fn trellis_viterbi(bits: &[u8], soft: Option<&[f32]>) -> Tsbk {
    const INF: f32 = f32::INFINITY;
    let mut pm = [0.0, INF, INF, INF];
    let mut from = [[0u8; 4]; 49];
    for d in 0..49 {
        let (mut c0, mut c1) = ([0f32; 4], [0f32; 4]);
        for k in 0..4 {
            let idx = TB[d * 4 + k] as usize;
            let w = soft.map_or(1.0, |s| s[idx]);
            if bits[idx] != 0 {
                c0[k] = w;
            } else {
                c1[k] = w;
            }
        }
        let mut nm = [INF; 4];
        for s in 0..4 {
            if pm[s] == INF {
                continue;
            }
            for j in 0..4 {
                if d == 48 && j != 0 {
                    continue;
                }
                let cw = NEXT[s][j];
                let m = pm[s] + (0..4).map(|k| if (cw >> (3 - k)) & 1 != 0 { c1[k] } else { c0[k] }).sum::<f32>();
                if m < nm[j] {
                    nm[j] = m;
                    from[d][j] = s as u8;
                }
            }
        }
        pm = nm;
    }
    let mut out = [0u8; 12];
    let mut state = 0usize;
    for d in (0..49).rev() {
        if d < 48 {
            out[d >> 2] |= (state << (6 - (d % 4) * 2)) as u8;
        }
        state = from[d][state] as usize;
    }
    out
}

/// The block's last 16 bits are the CRC-CCITT of the 80 before them.
pub fn tsbk_ok(t: &Tsbk) -> bool {
    crc_ccitt_bytes(&t[..10]) == u16::from_be_bytes([t[10], t[11]])
}
pub fn tsbk_last(t: &Tsbk) -> bool {
    t[0] >> 7 != 0
}
pub fn tsbk_opcode(t: &Tsbk) -> u8 {
    t[0] & 0x3f
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trellis {
    Greedy,
    Viterbi,
}

#[derive(Default, Debug)]
pub struct TsduResult {
    pub good: Vec<Tsbk>,
    pub bad: usize,
    pub trellis_fail: usize,
    pub crc_fail: usize,
    /// Blocks never tried after a greedy trellis failure.
    pub blocks_skipped: usize,
}

/// Every block of a TSDU frame. Greedy: a trellis failure ends the frame.
/// Viterbi: every block is decoded and the CRC alone judges. Either way the
/// last-block flag stops.
pub fn decode_tsdu(f: &Frame, mode: Trellis) -> TsduResult {
    let mut r = TsduResult::default();
    if f.nid.duid != TSDU || f.bits.len() < 112 {
        return r;
    }
    let blocks = ((f.bits.len() - 112) / 196).min(3);
    for b in 0..blocks {
        let at = 112 + b * 196;
        let bits = &f.bits[at..at + 196];
        let t = match mode {
            Trellis::Viterbi => trellis_viterbi(bits, (!f.soft.is_empty()).then(|| &f.soft[at..at + 196])),
            Trellis::Greedy => match trellis_greedy(bits) {
                Some(t) => t,
                None => {
                    r.bad += 1;
                    r.trellis_fail += 1;
                    r.blocks_skipped = blocks - b - 1;
                    break;
                }
            },
        };
        if !tsbk_ok(&t) {
            r.bad += 1;
            r.crc_fail += 1;
            continue;
        }
        r.good.push(t);
        if tsbk_last(&t) {
            break;
        }
    }
    r
}

