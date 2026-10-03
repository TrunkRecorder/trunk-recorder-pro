//! Forward error correction for the DMR layer 2 (ETSI TS 102 361-1 annex B):
//!
//! * Hamming(7,4) — CACH TACT
//! * Golay(20,8) — slot type (colour code + data type)
//! * QR(16,7) — EMB (colour code, PI, LCSS)
//! * BPTC(196,96) — CSBK, voice LC header / terminator, PI header, data headers:
//!   Hamming(15,11) rows × Hamming(13,9) columns
//! * BPTC(128,72) — embedded LC over voice bursts B–E: Hamming(16,11) rows,
//!   even-parity row, 5-bit checksum
//! * Reed-Solomon(12,9) over GF(2⁸) — full LC's check bytes
//! * CRC-CCITT — CSBK and the other BPTC data, masked by data type
//!
//! Codewords are held MSB-first: bit n−1 of the word is the first bit on
//! the air. The three small codes are decoded by trying every codeword (at
//! most 256), which is maximum-likelihood and takes the receiver's bit
//! reliabilities when given them.

use std::sync::OnceLock;

use crate::bits::crc_ccitt;

/// A small systematic code: data bits, then each data bit's parity row.
struct Small {
    n: u32,
    k: u32,
    parity: &'static [u32],
}

/// Hamming(7,4): data AT, TC, LCSS(2).
const HAMMING7: Small = Small { n: 7, k: 4, parity: &[0b101, 0b111, 0b110, 0b011] };
const GOLAY20: Small = Small {
    n: 20,
    k: 8,
    parity: &[0x3da, 0xd99, 0x6cd, 0x367, 0xdc6, 0xa97, 0x93e, 0x8eb],
};
const QR16: Small = Small {
    n: 16,
    k: 7,
    parity: &[0b001001111, 0b100011110, 0b110110111, 0b111100010, 0b111001001, 0b011100101, 0b001110011],
};

impl Small {
    fn encode(&self, data: u32) -> u32 {
        let p = (0..self.k).filter(|i| data >> (self.k - 1 - i) & 1 != 0).fold(0, |p, i| p ^ self.parity[i as usize]);
        data << (self.n - self.k) | p
    }

    /// The nearest codeword to `cw` (with `rel`, one reliability per bit in
    /// air order: the cheapest to reach). → (data, bits changed).
    fn decode(&self, cw: u32, rel: Option<&[f32]>) -> (u32, u32) {
        let (mut best, mut best_cost, mut best_errs) = (0, f32::MAX, 0);
        for d in 0..1u32 << self.k {
            let diff = self.encode(d) ^ cw;
            let cost = match rel {
                Some(r) => (0..self.n).filter(|b| diff >> b & 1 != 0).map(|b| r[(self.n - 1 - b) as usize]).sum(),
                None => diff.count_ones() as f32,
            };
            if cost < best_cost {
                (best, best_cost, best_errs) = (d, cost, diff.count_ones());
            }
        }
        (best, best_errs)
    }
}

pub fn hamming7_encode(data: u32) -> u32 {
    HAMMING7.encode(data)
}
/// TACT: → (4 data bits, bits changed); more than 1 changed is unreliable.
pub fn hamming7_decode(cw: u32) -> (u32, u32) {
    HAMMING7.decode(cw, None)
}
pub fn golay20_encode(data: u32) -> u32 {
    GOLAY20.encode(data)
}
/// Slot type: → (colour code << 4 | data type, bits changed). Distance 8 (3 correctable).
pub fn golay20_decode(cw: u32, rel: Option<&[f32]>) -> (u32, u32) {
    GOLAY20.decode(cw, rel)
}
pub fn qr16_encode(data: u32) -> u32 {
    QR16.encode(data)
}
/// EMB: → (CC(4) PI(1) LCSS(2), bits changed). Distance 6.
pub fn qr16_decode(cw: u32, rel: Option<&[f32]>) -> (u32, u32) {
    QR16.decode(cw, rel)
}

// ── Hamming codes of the product codes ──────────────────────────────────────

const H15: [u32; 11] = [0b1001, 0b1101, 0b1111, 0b1110, 0b0111, 0b1010, 0b0101, 0b1011, 0b1100, 0b0110, 0b0011];
const H13: [u32; 9] = [0b1111, 0b1110, 0b0111, 0b1010, 0b0101, 0b1011, 0b1100, 0b0110, 0b0011];
const H16: [u32; 11] = [0b10011, 0b11010, 0b11111, 0b11100, 0b01110, 0b10101, 0b01011, 0b10110, 0b11001, 0b01101, 0b00111];

/// Parity of `bits` (bits[i] = data bit i) under the parity rows.
fn parity_of(rows: &[u32], bits: &[u8]) -> u32 {
    rows.iter().zip(bits).filter(|(_, &b)| b != 0).fold(0, |p, (r, _)| p ^ r)
}

/// Correct a single error in place in `cw` (data then parity bits, one per
/// u8). → Some(bits changed), None when the syndrome is no single error.
fn hamming_fix(rows: &[u32], pbits: usize, cw: &mut [u8]) -> Option<u32> {
    let k = rows.len();
    let got = cw[k..k + pbits].iter().fold(0, |p, &b| p << 1 | b as u32);
    let syn = parity_of(rows, &cw[..k]) ^ got;
    if syn == 0 {
        return Some(0);
    }
    // A single error in a data bit (syndrome = its row) or a parity bit (one bit set).
    if let Some(i) = rows.iter().position(|&r| r == syn) {
        cw[i] ^= 1;
        return Some(1);
    }
    if syn.count_ones() == 1 {
        cw[k + pbits - 1 - syn.trailing_zeros() as usize] ^= 1;
        return Some(1);
    }
    None
}

// ── BPTC(196,96) ────────────────────────────────────────────────────────────

/// The 96 data bits of a BPTC(196,96) block, and how it went.
#[derive(Clone, Copy, Debug)]
pub struct Bptc {
    pub bits: [u8; 96],
    /// Bits corrected; None when some row or column is still wrong.
    pub errs: Option<u32>,
}

/// Deinterleave, then correct rows and columns in turn. `raw`: the 196 bits
/// in air order (98 before the slot type and sync, 98 after).
pub fn bptc196_decode(raw: &[u8; 196]) -> Bptc {
    let mut m = [0u8; 196];
    for (i, &b) in raw.iter().enumerate() {
        m[i * 13 % 196] = b & 1;
    }
    let received = m;
    // m[1..] is the 13 × 15 matrix; m[0] is R(3).
    let at = |r: usize, c: usize| 1 + r * 15 + c;
    // Rows then columns, until a pass finds nothing to fix (a row's second
    // error is often a column's only one).
    let mut clean = false;
    for _ in 0..4 {
        let (mut fixed, mut failed) = (0, 0);
        for r in 0..9 {
            let mut row: [u8; 15] = std::array::from_fn(|c| m[at(r, c)]);
            match hamming_fix(&H15, 4, &mut row) {
                Some(n) => fixed += n,
                None => failed += 1,
            }
            for c in 0..15 {
                m[at(r, c)] = row[c];
            }
        }
        for c in 0..15 {
            let mut col: [u8; 13] = std::array::from_fn(|r| m[at(r, c)]);
            match hamming_fix(&H13, 4, &mut col) {
                Some(n) => fixed += n,
                None => failed += 1,
            }
            for r in 0..13 {
                m[at(r, c)] = col[r];
            }
        }
        if fixed == 0 {
            clean = failed == 0;
            break;
        }
    }
    let mut bits = [0u8; 96];
    let mut p = 0;
    for r in 0..9 {
        for c in if r == 0 { 3 } else { 0 }..11 {
            bits[p] = m[at(r, c)];
            p += 1;
        }
    }
    let errs = m.iter().zip(&received).filter(|(a, b)| a != b).count() as u32;
    Bptc { bits, errs: clean.then_some(errs) }
}

/// Chase decoding of one Hamming row / column: the received bits and their
/// reliabilities → the codeword cheapest to reach among the hard decode and
/// those after flipping each subset of the 3 least reliable bits. None when
/// no candidate is a codeword.
fn hamming_chase(rows: &[u32], pbits: usize, bits: &[u8], rel: &[f32]) -> Option<Vec<u8>> {
    let n = bits.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| rel[a].total_cmp(&rel[b]));
    let lrb = &idx[..3];
    let mut best: Option<(f32, Vec<u8>)> = None;
    for m in 0..8u32 {
        let mut t = bits.to_vec();
        for (k, &i) in lrb.iter().enumerate() {
            if m & (1 << k) != 0 {
                t[i] ^= 1;
            }
        }
        if hamming_fix(rows, pbits, &mut t).is_none() {
            continue;
        }
        let cost: f32 = (0..n).filter(|&i| t[i] != bits[i]).map(|i| rel[i]).sum();
        if best.as_ref().is_none_or(|b| cost < b.0) {
            best = Some((cost, t));
        }
    }
    best.map(|b| b.1)
}

/// BPTC(196,96) from soft bits (sign = bit, magnitude = reliability, air
/// order): rows and columns Chase-decoded in turn against the received
/// reliabilities, until every row and column is a codeword. The hard
/// decoder's result when that doesn't settle.
pub fn bptc196_decode_soft(soft: &[f32; 196]) -> Bptc {
    let mut m = [0u8; 196];
    let mut rel = [0f32; 196];
    for (i, &v) in soft.iter().enumerate() {
        m[i * 13 % 196] = (v > 0.0) as u8;
        rel[i * 13 % 196] = v.abs();
    }
    let received = m;
    let at = |r: usize, c: usize| 1 + r * 15 + c;
    let mut clean = false;
    for _ in 0..4 {
        let mut changed = false;
        for r in 0..9 {
            let bits: Vec<u8> = (0..15).map(|c| m[at(r, c)]).collect();
            let w: Vec<f32> = (0..15).map(|c| rel[at(r, c)]).collect();
            if let Some(t) = hamming_chase(&H15, 4, &bits, &w) {
                for c in 0..15 {
                    changed |= m[at(r, c)] != t[c];
                    m[at(r, c)] = t[c];
                }
            }
        }
        for c in 0..15 {
            let bits: Vec<u8> = (0..13).map(|r| m[at(r, c)]).collect();
            let w: Vec<f32> = (0..13).map(|r| rel[at(r, c)]).collect();
            if let Some(t) = hamming_chase(&H13, 4, &bits, &w) {
                for r in 0..13 {
                    changed |= m[at(r, c)] != t[r];
                    m[at(r, c)] = t[r];
                }
            }
        }
        let ok_rows = (0..9).all(|r| {
            let mut row: [u8; 15] = std::array::from_fn(|c| m[at(r, c)]);
            hamming_fix(&H15, 4, &mut row) == Some(0)
        });
        let ok_cols = (0..15).all(|c| {
            let mut col: [u8; 13] = std::array::from_fn(|r| m[at(r, c)]);
            hamming_fix(&H13, 4, &mut col) == Some(0)
        });
        if ok_rows && ok_cols {
            clean = true;
            break;
        }
        if !changed {
            break;
        }
    }
    if !clean {
        let raw: [u8; 196] = std::array::from_fn(|i| (soft[i] > 0.0) as u8);
        return bptc196_decode(&raw);
    }
    let mut bits = [0u8; 96];
    let mut p = 0;
    for r in 0..9 {
        for c in if r == 0 { 3 } else { 0 }..11 {
            bits[p] = m[at(r, c)];
            p += 1;
        }
    }
    let errs = m.iter().zip(&received).filter(|(a, b)| a != b).count() as u32;
    Bptc { bits, errs: Some(errs) }
}

pub fn bptc196_encode(data: &[u8; 96]) -> [u8; 196] {
    let mut m = [0u8; 196];
    let at = |r: usize, c: usize| 1 + r * 15 + c;
    let mut p = 0;
    for r in 0..9 {
        for c in if r == 0 { 3 } else { 0 }..11 {
            m[at(r, c)] = data[p];
            p += 1;
        }
        let row: [u8; 11] = std::array::from_fn(|c| m[at(r, c)]);
        let par = parity_of(&H15, &row);
        for j in 0..4 {
            m[at(r, 11 + j)] = (par >> (3 - j) & 1) as u8;
        }
    }
    for c in 0..15 {
        let col: [u8; 9] = std::array::from_fn(|r| m[at(r, c)]);
        let par = parity_of(&H13, &col);
        for j in 0..4 {
            m[at(9 + j, c)] = (par >> (3 - j) & 1) as u8;
        }
    }
    let mut raw = [0u8; 196];
    for i in 0..196 {
        raw[i] = m[i * 13 % 196];
    }
    raw
}

// ── Embedded LC: BPTC(128,72) over bursts B–E ───────────────────────────────

/// 72 LC bits from the 128 embedded bits of bursts B–E (air order, 32 per
/// burst), bits corrected, and whether the 5-bit checksum agrees (a keyed
/// system's never does); None when a row or the column parity fails.
pub fn embedded_lc_decode(raw: &[u8; 128]) -> Option<([u8; 72], u32, bool)> {
    let mut m = [0u8; 128];
    for i in 0..127 {
        m[i] = raw[i * 8 % 127] & 1;
    }
    m[127] = raw[127] & 1;
    let mut errs = 0;
    for r in 0..7 {
        let row = &mut m[r * 16..r * 16 + 16];
        errs += hamming_fix(&H16, 5, row)?;
    }
    // Row 7 is the even parity of each column.
    if (0..16).any(|c| (0..8).fold(0, |p, r| p ^ m[r * 16 + c]) != 0) {
        return None;
    }
    let mut lc = [0u8; 72];
    let mut p = 0;
    let mut sum = 0u32;
    for r in 0..7 {
        let w = if r < 2 { 11 } else { 10 };
        for c in 0..w {
            lc[p] = m[r * 16 + c];
            p += 1;
        }
        if r >= 2 {
            sum = sum << 1 | m[r * 16 + 10] as u32;
        }
    }
    Some((lc, errs, checksum5(&lc) == sum))
}

pub fn embedded_lc_encode(lc: &[u8; 72]) -> [u8; 128] {
    let mut m = [0u8; 128];
    let cs = checksum5(lc);
    let mut p = 0;
    for r in 0..7 {
        let w = if r < 2 { 11 } else { 10 };
        for c in 0..w {
            m[r * 16 + c] = lc[p];
            p += 1;
        }
        if r >= 2 {
            m[r * 16 + 10] = (cs >> (6 - r) & 1) as u8;
        }
        let par = parity_of(&H16, &m[r * 16..r * 16 + 11]);
        for j in 0..5 {
            m[r * 16 + 11 + j] = (par >> (4 - j) & 1) as u8;
        }
    }
    for c in 0..16 {
        m[7 * 16 + c] = (0..7).fold(0, |p, r| p ^ m[r * 16 + c]);
    }
    let mut raw = [0u8; 128];
    for i in 0..127 {
        raw[i * 8 % 127] = m[i];
    }
    raw[127] = m[127];
    raw
}

/// The embedded LC checksum: the nine LC bytes summed, mod 31.
pub fn checksum5(lc: &[u8; 72]) -> u32 {
    lc.chunks(8).map(|b| b.iter().fold(0u32, |v, &x| v << 1 | x as u32)).sum::<u32>() % 31
}

// ── Reed-Solomon(12,9), GF(2⁸) x⁸+x⁴+x³+x²+1, roots α¹..α³ ────────────────

struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
}

fn gf() -> &'static Gf {
    static G: OnceLock<Gf> = OnceLock::new();
    G.get_or_init(|| {
        let (mut exp, mut log) = ([0u8; 512], [0u8; 256]);
        let mut x = 1u32;
        for i in 0..255 {
            exp[i] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11d;
            }
        }
        for i in 255..512 {
            exp[i] = exp[i - 255];
        }
        Gf { exp, log }
    })
}

fn gmul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let g = gf();
    g.exp[g.log[a as usize] as usize + g.log[b as usize] as usize]
}

fn syndromes(cw: &[u8; 12]) -> [u8; 3] {
    let g = gf();
    std::array::from_fn(|j| cw.iter().fold(0u8, |s, &c| gmul(s, g.exp[j + 1]) ^ c))
}

/// The three check bytes of 9 LC bytes (before the data type's CRC mask).
pub fn rs129_parity(data: &[u8; 9]) -> [u8; 3] {
    // Generator (x − α)(x − α²)(x − α³) = x³ + g2 x² + g1 x + g0; systematic division.
    let g = gf();
    let (a1, a2, a3) = (g.exp[1], g.exp[2], g.exp[3]);
    let g2 = a1 ^ a2 ^ a3;
    let g1 = gmul(a1, a2) ^ gmul(a1, a3) ^ gmul(a2, a3);
    let g0 = gmul(gmul(a1, a2), a3);
    let mut r = [0u8; 3];
    for &d in data {
        let f = d ^ r[0];
        r = [r[1] ^ gmul(f, g2), r[2] ^ gmul(f, g1), gmul(f, g0)];
    }
    r
}

/// Check (and correct one byte of) a full LC's 12 bytes, the data type's
/// CRC mask already removed from the last three. → bytes corrected, or None.
pub fn rs129_decode(cw: &mut [u8; 12]) -> Option<u32> {
    let s = syndromes(cw);
    if s == [0, 0, 0] {
        return Some(0);
    }
    // One error e at degree p: S1 = e·αᵖ, S2 = e·α²ᵖ, S3 = e·α³ᵖ.
    if s[0] == 0 || s[1] == 0 {
        return None;
    }
    let g = gf();
    let (l1, l2) = (g.log[s[0] as usize] as usize, g.log[s[1] as usize] as usize);
    let p = (l2 + 255 - l1) % 255;
    if p > 11 {
        return None;
    }
    let e = g.exp[(2 * l1 + 255 - l2) % 255];
    if gmul(e, g.exp[3 * p % 255]) != s[2] {
        return None;
    }
    cw[11 - p] ^= e;
    Some(1)
}

// ── CRCs ───────────────────────────────────────────────────────────────────

/// CRC masks by data type (TS 102 361-1 B.3.12).
pub const MASK_PI: u16 = 0x6969;
pub const MASK_VOICE_LC_HEADER: u32 = 0x969696;
pub const MASK_TERMINATOR_LC: u32 = 0x999999;
pub const MASK_CSBK: u16 = 0xa5a5;
pub const MASK_MBC_HEADER: u16 = 0xaaaa;
pub const MASK_DATA_HEADER: u16 = 0xcccc;
pub const MASK_USBD: u16 = 0x3333;

/// 96 BPTC bits whose last 16 are a CRC-CCITT under `mask`: do they check?
pub fn crc16_ok(bits: &[u8; 96], mask: u16) -> bool {
    let got = bits[80..].iter().fold(0u16, |v, &b| v << 1 | b as u16);
    crc_ccitt(&bits[..80]) ^ mask == got
}

pub fn pack(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8).map(|c| c.iter().fold(0u8, |v, &b| v << 1 | b)).collect()
}
pub fn unpack(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().flat_map(|&b| (0..8).rev().map(move |i| b >> i & 1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed;
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        }
    }

    fn min_distance(c: &Small) -> u32 {
        (1..1u32 << c.k).map(|d| c.encode(d).count_ones()).min().unwrap()
    }

    #[test]
    fn small_codes_have_their_distances_and_correct() {
        assert_eq!(min_distance(&HAMMING7), 3);
        assert_eq!(min_distance(&GOLAY20), 8);
        assert_eq!(min_distance(&QR16), 6);
        for (c, t) in [(&HAMMING7, 1), (&GOLAY20, 3), (&QR16, 2)] {
            let mut r = rng(7);
            for _ in 0..500 {
                let d = r() as u32 & ((1 << c.k) - 1);
                let mut cw = c.encode(d);
                for _ in 0..t {
                    cw ^= 1 << (r() % c.n as u64);
                }
                assert_eq!(c.decode(cw, None).0, d);
            }
        }
    }

    #[test]
    fn soft_decoding_trusts_reliable_bits() {
        // Four errors defeat hard Golay(20,8); flagged unreliable, they are found.
        let d = 0xa7;
        let cw = golay20_encode(d) ^ 0b1111 << 5;
        let mut rel = [1.0f32; 20];
        for b in 5..9 {
            rel[19 - b] = 0.05;
        }
        assert_ne!(golay20_decode(cw, None).0, d);
        assert_eq!(golay20_decode(cw, Some(&rel)), (d, 4));
    }

    #[test]
    fn bptc196_round_trip_with_errors() {
        // Any 1–2 bit errors are fixed; 3 nearly always (not two in a row plus one in its column).
        let mut r = rng(11);
        let mut fixed3 = 0;
        for trial in 0..600 {
            let data: [u8; 96] = std::array::from_fn(|_| (r() & 1) as u8);
            let mut raw = bptc196_encode(&data);
            let flips = trial % 4;
            let mut at = Vec::new();
            while at.len() < flips {
                let i = (r() % 196) as usize;
                if !at.contains(&i) {
                    at.push(i);
                    raw[i] ^= 1;
                }
            }
            let b = bptc196_decode(&raw);
            if flips < 3 {
                assert!(b.bits == data && b.errs.is_some_and(|e| e <= flips as u32), "trial {trial}"); // R(3) is not counted
            } else if b.errs.is_some() && b.bits == data {
                fixed3 += 1;
            }
        }
        assert!(fixed3 >= 140, "{fixed3} of 150 three-error blocks fixed");
    }

    #[test]
    fn embedded_lc_round_trip() {
        let mut r = rng(3);
        for _ in 0..100 {
            let lc: [u8; 72] = std::array::from_fn(|_| (r() & 1) as u8);
            let mut raw = embedded_lc_encode(&lc);
            assert_eq!(embedded_lc_decode(&raw).unwrap(), (lc, 0, true));
            raw[(r() % 128) as usize] ^= 1;
            // One flipped bit: fixed by its row, or (row 7) caught by the column parity.
            if let Some((got, _, _)) = embedded_lc_decode(&raw) {
                assert_eq!(got, lc);
            }
        }
    }

    #[test]
    fn reed_solomon_corrects_one_byte() {
        let data = [0x00, 0x00, 0x20, 0x00, 0x0c, 0x30, 0x00, 0x2f, 0x9b];
        let p = rs129_parity(&data);
        let mut cw = [0u8; 12];
        cw[..9].copy_from_slice(&data);
        cw[9..].copy_from_slice(&p);
        assert_eq!(syndromes(&cw), [0, 0, 0]);
        for pos in 0..12 {
            let mut bad = cw;
            bad[pos] ^= 0x5a;
            assert_eq!(rs129_decode(&mut bad), Some(1));
            assert_eq!(bad, cw);
        }
        let mut two = cw;
        two[1] ^= 1;
        two[7] ^= 0x80;
        assert_eq!(rs129_decode(&mut two), None);
    }
}
