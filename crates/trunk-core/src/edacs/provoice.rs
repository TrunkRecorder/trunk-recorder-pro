//! ProVoice: EDACS's digital voice. 9600 baud 2FSK carrying IMBE 7100×4400
//! (4400 bit/s of speech, 2700 of FEC), four 20 ms vocoder frames in each
//! 80 ms frame (DSD's provoice.c; mbelib's imbe7100x4400.c for the codec).
//!
//! ```text
//! sync 0x57123333 │ 144 bits │ IMBE 1+2 interleaved (286) │ 2 │ 16 │ IMBE 3+4 (286) │ 2
//! ```
//!
//! Each 142-bit IMBE 7100 frame is seven words: c0 a (24,12) Golay word
//! shortened to 7 data bits, c1–c3 (23,12) Golay, c4–c5 (15,11) Hamming (its
//! own generator), c6 23 bits unprotected. c1–c5 are whitened by the P25
//! generator seeded from c0. The 88 information bits are those of P25's IMBE
//! in another order, so P25's vocoder speaks them.

use crate::p25::fec::{golay23_decode, golay23_encode};

/// The frame sync, first bit sent highest; EA systems send their own.
pub const SYNC: u32 = 0x5712_3333;
pub const SYNC_EA: u32 = 0x94DC_0D4D;
pub const SYNC_BITS: usize = 32;
/// Bits after the sync.
pub const BODY_BITS: usize = 736;
/// Seconds per frame (four vocoder frames).
pub const FRAME_S: f64 = 0.08;
pub const BAUD: f64 = 9600.0;

/// The interleave: bit `i` of a frame's 142 (in the order sent) is word
/// `W[i]`'s bit `X[i]` (DSD's pW / pX).
const W: [u8; 142] = [
    0, 1, 2, 3, 4, 6, 0, 1, 2, 3, 4, 6, 0, 1, 2, 3, 4, 6, 0, 1, 2, 3, 5, 6, 0, 1, 2, 3, 5, 6, 0, 1, 2, 3, 5, 6, 0, 1, 3, 4, 5, 6, 1, 2, 3, 4, 5, 6, 0, 1, 2, 3, 4, 6, 0, 1, 2, 3, 4,
    6, 0, 1, 2, 3, 4, 6, 0, 1, 2, 3, 5, 6, 0, 1, 2, 3, 5, 6, 0, 1, 2, 3, 5, 6, 1, 2, 3, 4, 5, 6, 1, 2, 3, 4, 5, 0, 1, 2, 3, 4, 6, 0, 1, 2, 3, 4, 6, 0, 1, 2, 3, 5, 6, 0, 1, 2,
    3, 5, 6, 0, 1, 2, 3, 5, 6, 0, 1, 2, 4, 5, 6, 1, 2, 3, 4, 5, 6, 1, 2, 3, 4, 6,
];
const X: [u8; 142] = [
    18, 18, 17, 16, 7, 21, 15, 15, 14, 13, 4, 18, 12, 12, 11, 10, 1, 15, 9, 9, 8, 7, 13, 12, 6, 6, 5, 4, 10, 9, 3, 3, 2, 1, 7, 6, 0, 0, 22, 13, 4, 3, 21, 20, 19, 10, 1, 0, 17, 17, 16,
    15, 6, 20, 14, 14, 13, 12, 3, 17, 11, 11, 10, 9, 0, 14, 8, 8, 7, 6, 12, 11, 5, 5, 4, 3, 9, 8, 2, 2, 1, 0, 6, 5, 23, 22, 21, 12, 3, 2, 20, 19, 18, 9, 0, 16, 16, 15, 14, 5, 19, 13,
    13, 12, 11, 2, 16, 10, 10, 9, 8, 14, 13, 7, 7, 6, 5, 11, 10, 4, 4, 3, 2, 8, 7, 1, 1, 0, 14, 5, 4, 22, 21, 20, 11, 2, 1, 19, 18, 17, 8, 22,
];

/// A frame's seven words, bit j of word i at `[i][j]` (mbelib's imbe_fr).
pub type Words = [[u8; 24]; 7];

/// Where each vocoder frame's 142 bits are in the body: `[frame][i]` is the
/// body bit carrying interleave position i.
pub fn layout() -> [[usize; 142]; 4] {
    let mut l = [[0usize; 142]; 4];
    let mut pos = 144;
    for half in 0..2 {
        let (a, b) = l.split_at_mut(2 * half + 1);
        let (a, b) = (&mut a[2 * half], &mut b[0]);
        let take = |w: &mut [usize; 142], at: usize, n: usize, pos: &mut usize| {
            for j in 0..n {
                w[at + j] = *pos;
                *pos += 1;
            }
        };
        let mut idx = 0;
        for _ in 0..11 {
            take(a, idx, 6, &mut pos);
            take(b, idx, 6, &mut pos);
            idx += 6;
        }
        take(a, 66, 6, &mut pos);
        take(b, 66, 4, &mut pos);
        pos += 2;
        take(b, 70, 2, &mut pos);
        idx = 72;
        for (reps, n) in [(3, 6), (1, 5), (7, 6), (1, 5)] {
            for _ in 0..reps {
                take(a, idx, n, &mut pos);
                take(b, idx, n, &mut pos);
                idx += n;
            }
        }
        pos += 2;
        if half == 0 {
            pos += 16;
        }
    }
    debug_assert_eq!(pos, BODY_BITS);
    l
}

/// The Hamming (15,11) code of IMBE 7100: its parity check rows (mbelib's).
/// mbelib corrects a syndrome s by flipping bit s − 1, which is the bit in
/// error only for some bits; here each syndrome flips the bit whose column
/// it is.
const HAMMING_ROWS: [u16; 4] = [0x7ac8, 0x3d64, 0x1eb2, 0x7591];

fn hamming_syndrome(block: u16) -> u16 {
    HAMMING_ROWS.iter().fold(0, |s, &r| s << 1 | ((block & r).count_ones() & 1) as u16)
}

/// A 15-bit word (bit 14 first) → its 11 data bits and the bits corrected
/// (`mbelib`: correct as mbelib does).
fn hamming_decode(block: u16, mbelib: bool) -> (u16, u32) {
    let s = hamming_syndrome(block);
    if s == 0 {
        return (block >> 4, 0);
    }
    let bit = if mbelib { s - 1 } else { (0..15).find(|&b| hamming_syndrome(1 << b) == s).unwrap_or(0) };
    ((block ^ 1 << bit) >> 4 & 0x7ff, 1)
}

fn hamming_encode(data: u16) -> u16 {
    // The rows' low four bits are the identity: each parity bit is its row's parity of the data.
    let d = (data & 0x7ff) << 4;
    HAMMING_ROWS.iter().enumerate().fold(d, |w, (i, &r)| w | (((d & r).count_ones() & 1) as u16) << (3 - i))
}

/// The whitening: 100 bits from the P25 generator seeded with `seed` (7 bits).
fn whitening(seed: u32) -> [u8; 101] {
    let mut pr = [0u8; 101];
    let mut v = 16 * seed;
    for b in pr.iter_mut().skip(1) {
        v = (173 * v + 13849) & 0xffff;
        *b = (v >> 15) as u8;
    }
    pr
}

/// The whitened bits, in mbelib's order: c1 23..0, c2 and c3 22..0, c4 and c5 14..0.
fn whitened() -> impl Iterator<Item = (usize, usize)> {
    (0..24).rev().map(|j| (1, j)).chain([2, 3].into_iter().flat_map(|i| (0..23).rev().map(move |j| (i, j)))).chain([4, 5].into_iter().flat_map(|i| (0..15).rev().map(move |j| (i, j))))
}

fn word(fr: &[u8; 24], from: usize, n: usize) -> u32 {
    (0..n).rev().fold(0, |v, j| v << 1 | fr[from + j] as u32)
}

/// mbelib's imbe7100 → imbe7200 bit order (`d` in 7100 order).
fn to_7200(d: &[u8; 88]) -> [u8; 88] {
    let k = k_of(d);
    let mut t = [0u8; 88];
    t[87] = d[0];
    t[48 + k] = d[42];
    t[49 + k] = d[43];
    t[48..48 + k].copy_from_slice(&d[44..44 + k]);
    let (mut j, mut s) = (0usize, 1usize);
    while j < 87 {
        t[j] = d[s];
        j += 1;
        if j == 48 {
            j += k + 2;
        }
        s += 1;
        if s == 42 {
            s += k + 2;
        }
    }
    t
}

/// K (the number of b1 bits) from b0, as mbelib reads it from the 7100 order.
fn k_of(d: &[u8; 88]) -> usize {
    let b0 = [d[1], d[2], d[3], d[4], d[5], d[6], d[86], d[87]].iter().fold(0u32, |v, &b| v << 1 | b as u32);
    let w0 = (4.0 * std::f32::consts::PI) / (b0 as f32 + 39.5);
    let l = (0.9254 * ((std::f32::consts::PI / w0) + 0.25).trunc()) as i32;
    if l < 37 { ((l + 2) as f32 / 3.0) as usize } else { 12 }
}

/// A frame's words → the 88 information bits in P25's (imbe7200) order, the
/// errors corrected in c0, and in all.
pub fn decode(fr: &Words) -> ([u8; 88], u32, u32) {
    decode_as(fr, false)
}

fn decode_as(fr: &Words, mbelib: bool) -> ([u8; 88], u32, u32) {
    let mut fr = *fr;
    // c0: 7 data bits, the Golay word's other five data bits zero.
    let c0 = word(&fr[0], 1, 18);
    let r0 = golay23_decode(c0);
    let e0 = r0.errs.max(0) as u32;
    for j in 11..18 {
        fr[0][j + 1] = (r0.data >> (j - 11) & 1) as u8;
    }
    let seed = (0..7).rev().fold(0, |v, j| v << 1 | fr[0][12 + j] as u32);
    let pr = whitening(seed);
    for (k, (i, j)) in whitened().enumerate() {
        fr[i][j] ^= pr[k + 1];
    }
    let mut d = [0u8; 88];
    let mut n = 0;
    let mut put = |v: u32, bits: usize| {
        for b in (0..bits).rev() {
            d[n] = (v >> b & 1) as u8;
            n += 1;
        }
    };
    put(seed, 7);
    let mut errs = e0;
    for (i, from) in [(1usize, 1usize), (2, 0), (3, 0)] {
        let r = golay23_decode(word(&fr[i], from, 23));
        errs += r.errs.max(0) as u32;
        put(r.data, 12);
    }
    for i in [4, 5] {
        let (v, e) = hamming_decode(word(&fr[i], 0, 15) as u16, mbelib);
        errs += e;
        put(v as u32, 11);
    }
    put(word(&fr[6], 0, 23), 23);
    (to_7200(&d), e0, errs)
}

/// 88 information bits (P25's order) → a frame's words: [`decode`] backwards.
pub fn encode(bits: &[u8; 88]) -> Words {
    // The 7100 order whose 7200 order is `bits` (K depends on the bits; try each).
    let d = (1..=12)
        .find_map(|k| {
            let mut d = [0u8; 88];
            // to_7200's positions for this K, turned around.
            let mut src = [0usize; 88];
            let mut t = [usize::MAX; 88];
            t[87] = 0;
            t[48 + k] = 42;
            t[49 + k] = 43;
            for i in 0..k {
                t[48 + i] = 44 + i;
            }
            let (mut j, mut s) = (0usize, 1usize);
            while j < 87 {
                t[j] = s;
                j += 1;
                if j == 48 {
                    j += k + 2;
                }
                s += 1;
                if s == 42 {
                    s += k + 2;
                }
            }
            for (j, &s) in t.iter().enumerate() {
                src[s] = j;
            }
            for (s, &j) in src.iter().enumerate() {
                d[s] = bits[j];
            }
            (k_of(&d) == k && to_7200(&d) == *bits).then_some(d)
        })
        .unwrap_or([0; 88]);
    let val = |from: usize, n: usize| d[from..from + n].iter().fold(0u32, |v, &b| v << 1 | b as u32);
    let mut fr: Words = [[0; 24]; 7];
    let mut set = |i: usize, from: usize, n: usize, v: u32| {
        for j in 0..n {
            fr[i][from + j] = (v >> j & 1) as u8;
        }
    };
    let seed = val(0, 7);
    set(0, 1, 18, golay23_encode(seed) & 0x3ffff);
    set(1, 1, 23, golay23_encode(val(7, 12)));
    set(2, 0, 23, golay23_encode(val(19, 12)));
    set(3, 0, 23, golay23_encode(val(31, 12)));
    set(4, 0, 15, hamming_encode(val(43, 11) as u16) as u32);
    set(5, 0, 15, hamming_encode(val(54, 11) as u16) as u32);
    set(6, 0, 23, val(65, 23));
    let pr = whitening(seed);
    for (k, (i, j)) in whitened().enumerate() {
        fr[i][j] ^= pr[k + 1];
    }
    fr
}

/// A frame's words out of its 142 bits in the order sent.
pub fn deinterleave(bits: impl Fn(usize) -> u8) -> Words {
    let mut fr: Words = [[0; 24]; 7];
    for i in 0..142 {
        fr[W[i] as usize][X[i] as usize] = bits(i);
    }
    fr
}

/// The 142 bits in the order sent.
pub fn interleave(fr: &Words) -> [u8; 142] {
    std::array::from_fn(|i| fr[W[i] as usize][X[i] as usize])
}

/// A whole frame's bits (sync and body) for four vocoder frames (test signals).
pub fn frame_bits(frames: &[[u8; 88]; 4]) -> Vec<u8> {
    let mut bits = vec![0u8; SYNC_BITS + BODY_BITS];
    for (i, b) in bits.iter_mut().take(SYNC_BITS).enumerate() {
        *b = (SYNC >> (31 - i) & 1) as u8;
    }
    // The 16 bits between the halves are usually 0x2175 (DSD); the rest of the header, zeros.
    for i in 0..16 {
        bits[SYNC_BITS + 432 + i] = (0x2175u16 >> (15 - i) & 1) as u8;
    }
    let l = layout();
    for (f, d) in frames.iter().enumerate() {
        let sent = interleave(&encode(d));
        for i in 0..142 {
            bits[SYNC_BITS + l[f][i]] = sent[i];
        }
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits88(seed: &mut u32) -> [u8; 88] {
        std::array::from_fn(|_| {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 17;
            *seed ^= *seed << 5;
            (*seed & 1) as u8
        })
    }

    #[test]
    fn the_layout_covers_the_body_once() {
        let l = layout();
        let mut seen = vec![0u8; BODY_BITS];
        for f in &l {
            for &p in f.iter() {
                seen[p] += 1;
            }
        }
        // Header 144, two spacers between halves' frames, 16 bits between halves, a spacer at the end.
        let unused: Vec<usize> = (0..BODY_BITS).filter(|&p| seen[p] == 0).collect();
        assert_eq!(unused.len(), 144 + 2 + 2 + 16 + 2 + 2);
        assert!(seen.iter().all(|&n| n <= 1));
    }

    #[test]
    fn interleave_is_a_permutation() {
        let mut seen = [[false; 24]; 7];
        for i in 0..142 {
            assert!(!seen[W[i] as usize][X[i] as usize]);
            seen[W[i] as usize][X[i] as usize] = true;
        }
        let words = [19, 24, 23, 23, 15, 15, 23];
        for (i, &n) in words.iter().enumerate() {
            assert_eq!(seen[i].iter().filter(|&&b| b).count(), n, "word {i}");
        }
    }

    #[test]
    fn hamming_corrects_one_error() {
        for d in (0..2048u16).step_by(37) {
            let w = hamming_encode(d);
            assert_eq!(hamming_decode(w, false), (d, 0));
            for b in 0..15 {
                assert_eq!(hamming_decode(w ^ 1 << b, false), (d, 1), "data {d} bit {b}");
            }
        }
    }

    #[test]
    fn round_trip_and_errors_corrected() {
        let mut s = 0x2468_ace1u32;
        for n in 0..300 {
            let b = bits88(&mut s);
            let fr = encode(&b);
            let (d, e0, et) = decode(&fr);
            assert_eq!((d, e0, et), (b, 0, 0), "frame {n}");
            // One error in each protected word.
            let mut bad = fr;
            for (i, j) in [(0, 13), (1, 5), (2, 20), (3, 0), (4, 9), (5, 14)] {
                bad[i][j] ^= 1;
            }
            let (d, e0, et) = decode(&bad);
            assert_eq!(d, b, "frame {n}");
            assert_eq!((e0, et), (1, 6));
        }
    }

    /// Frames from mbelib (imbe7100x4400.c behind DSD's interleave): the
    /// 142 bits sent (random: errors everywhere), as hex of 144 bits (two
    /// zero bits first), and the 88 bits it decoded, as hex. Decoded with
    /// mbelib's Hamming correction, every bit matches.
    #[test]
    fn matches_mbelib() {
        let hex = |s: &str| -> Vec<u8> { s.chars().flat_map(|c| { let v = c.to_digit(16).unwrap(); (0..4).rev().map(move |b| (v >> b & 1) as u8) }).collect() };
        for (n, (sent, want)) in super::super::provoice_vectors::VECTORS.iter().enumerate() {
            let sent = hex(sent);
            let want = hex(want);
            let fr = deinterleave(|i| sent[2 + i]);
            let (d, _, _) = decode_as(&fr, true);
            assert_eq!(d.to_vec(), want, "vector {n}");
        }
    }
}
