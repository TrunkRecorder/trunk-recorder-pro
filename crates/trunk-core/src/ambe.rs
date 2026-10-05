//! The AMBE+2 (3600x2450) voice codeword as P25 Phase 2 and DMR carry it:
//! 72 bits (36 dibits) in four words, c0 Golay(24,12), c1 Golay(23,12)
//! whitened by a PN sequence seeded from c0's data, c2 and c3 uncoded. After
//! op25 p25p2_vf.cc; the vocoder itself is [`crate::mbe`].

use crate::p25::fec::{golay23_decode, golay23_decode_soft, golay23_encode, golay24_decode, golay24_encode};

/// extract_vcw: vf bit k ← (which of c0..c3, bit index).
pub const VCW_MAP: [(u8, u8); 72] = [
    (0, 23), (0, 5), (1, 10), (2, 3), (0, 22), (0, 4), (1, 9), (2, 2), (0, 21), (0, 3), (1, 8), (2, 1),
    (0, 20), (0, 2), (1, 7), (2, 0), (0, 19), (0, 1), (1, 6), (3, 13), (0, 18), (0, 0), (1, 5), (3, 12),
    (0, 17), (1, 22), (1, 4), (3, 11), (0, 16), (1, 21), (1, 3), (3, 10), (0, 15), (1, 20), (1, 2), (3, 9),
    (0, 14), (1, 19), (1, 1), (3, 8), (0, 13), (1, 18), (1, 0), (3, 7), (0, 12), (1, 17), (2, 10), (3, 6),
    (0, 11), (1, 16), (2, 9), (3, 5), (0, 10), (1, 15), (2, 8), (3, 4), (0, 9), (1, 14), (2, 7), (3, 3),
    (0, 8), (1, 13), (2, 6), (3, 2), (0, 7), (1, 12), (2, 5), (3, 1), (0, 6), (1, 11), (2, 4), (3, 0),
];

/// op25 extract_vcw: 36 dibits → c0 (24 bits), c1 (23), c2 (11), c3 (14),
/// and each bit's reliability (index = bit position, MSB first, per word).
pub fn extract_vcw(d: &[u8], rel: Option<&[f32]>, w1: &mut [f32; 23]) -> [u32; 4] {
    let mut c = [0u32; 4];
    for (k, &(w, i)) in VCW_MAP.iter().enumerate() {
        let dib = d[k >> 1];
        let b = if k & 1 != 0 { dib & 1 } else { dib >> 1 & 1 };
        if b != 0 {
            c[w as usize] |= 1 << i;
        }
        if let (1, Some(r)) = (w, rel) {
            w1[22 - i as usize] = r[k];
        }
    }
    c
}

/// The AMBE c1 modulator seeded from u0 (mbe_demodulateAmbe3600x2450Data):
/// 23 bits, first PN bit in the MSB.
pub fn ambe_pn23(u0: u32) -> u32 {
    let mut pr = 16 * u0;
    let mut m1 = 0;
    for _ in 1..24 {
        pr = (173 * pr + 13849) % 65536;
        m1 = m1 << 1 | (pr >> 15 & 1);
    }
    m1
}

#[derive(Clone, Copy, Debug)]
pub struct AmbeFrame {
    /// mbelib's ambe_d[49]: u0 (12) u1 (12) u2 (11) u3 (14), MSB first.
    pub bits: [u8; 49],
    /// Bits corrected in c0 + c1 (uncorrectable c0 counts 4).
    pub errs: u32,
}

/// op25 process_vcw: FEC-decode one 36-dibit voice codeword. With `rel`
/// (72 bit reliabilities), c1 is decoded soft (Chase-II).
pub fn decode_vcw(d: &[u8], rel: Option<&[f32]>) -> AmbeFrame {
    let mut w1 = [0f32; 23];
    let [c0, c1, c2, c3] = extract_vcw(d, rel, &mut w1);
    let r0 = golay24_decode(c0);
    let u0 = r0.data;
    let c1 = c1 ^ ambe_pn23(u0);
    let r1 = if rel.is_some() { golay23_decode_soft(c1, &w1) } else { golay23_decode(c1) };
    let u = [(u0, 12), (r1.data, 12), (c2, 11), (c3, 14)];
    let mut bits = [0u8; 49];
    let mut p = 0;
    for (v, w) in u {
        for b in (0..w).rev() {
            bits[p] = (v >> b & 1) as u8;
            p += 1;
        }
    }
    AmbeFrame { bits, errs: (if r0.errs < 0 { 4 } else { r0.errs as u32 }) + r1.errs.max(0) as u32 }
}

/// 49 AMBE bits → the 36 dibits of a 72-bit codeword (the inverse of
/// [`decode_vcw`]; for synthesizers and tests).
pub fn encode_vcw(u: &[u8; 49]) -> [u8; 36] {
    let word = |a: usize, n: usize| u[a..a + n].iter().fold(0u32, |v, &b| v << 1 | b as u32);
    let (u0, u1, u2, u3) = (word(0, 12), word(12, 12), word(24, 11), word(35, 14));
    let c = [golay24_encode(u0), golay23_encode(u1) ^ ambe_pn23(u0), u2, u3];
    let mut bits = [0u8; 72];
    for (k, &(w, i)) in VCW_MAP.iter().enumerate() {
        bits[k] = (c[w as usize] >> i & 1) as u8;
    }
    std::array::from_fn(|i| bits[2 * i] << 1 | bits[2 * i + 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pn_first_bit() {
        assert_eq!(ambe_pn23(0) >> 22, 0);
    }
}
