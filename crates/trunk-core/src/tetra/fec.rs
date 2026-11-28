//! TETRA channel coding for signalling (EN 300 392-2 clause 8.2): every coded channel but the
//! AACH is the same chain —
//!
//! ```text
//! type-1 bits → CRC-16 → 4 zero tail bits → convolutional code K=5 R=¼, punctured to ⅔
//!   → block interleave (bit i, from 1, goes to 1 + a·i mod K) → scramble
//! ```
//!
//! Decoding runs it backwards with soft bits (+ a 1, − a 0, 0 unknown). The AACH is 14 bits in a
//! shortened (30,14) Reed-Muller code, scrambled.

/// One coded channel's parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coding {
    /// Type-1 (MAC) bits.
    pub info: usize,
    /// Bits on the air.
    pub air: usize,
    /// The interleaver's `a`.
    pub a: usize,
}

/// The broadcast synchronisation channel (SYNC PDU).
pub const BSCH: Coding = Coding { info: 60, air: 120, a: 11 };
/// Half a slot of signalling: SCH/HD, BNCH, STCH.
pub const SCH_HD: Coding = Coding { info: 124, air: 216, a: 101 };
/// A full slot of signalling.
pub const SCH_F: Coding = Coding { info: 268, air: 432, a: 103 };

/// The scrambling register for the BSCH, and everything else before a cell's identity is known.
pub const BSCH_SEED: u32 = 3;

/// A cell's scrambling register (clause 8.2.5.2): its MCC, MNC and colour code. Colour code 0
/// scrambles as the BSCH does, whatever the network (table 21.76 note 1).
pub fn seed(mcc: u16, mnc: u16, colour: u8) -> u32 {
    if colour & 0x3f == 0 {
        return BSCH_SEED;
    }
    (colour as u32 & 0x3f | (mnc as u32 & 0x3fff) << 6 | (mcc as u32 & 0x3ff) << 20) << 2 | BSCH_SEED
}

/// The scrambling sequence from a register (equation 8.42): p(k − 32) in bit 0, p(k − 1) in bit 31.
pub fn scrambling(seed: u32) -> impl Iterator<Item = u8> {
    /// p(k) is the sum of p(k − i) for these i: bit 32 − i of the register.
    const TAPS: [u32; 14] = [1, 2, 4, 5, 7, 8, 10, 11, 12, 16, 22, 23, 26, 32];
    let mask = TAPS.iter().fold(0u32, |m, t| m | 1 << (32 - t));
    let mut s = seed;
    std::iter::repeat_with(move || {
        let b = (s & mask).count_ones() & 1;
        s = s >> 1 | b << 31;
        b as u8
    })
}

/// Descramble soft bits in place.
pub(super) fn descramble(soft: &mut [f32], seed: u32) {
    for (v, p) in soft.iter_mut().zip(scrambling(seed)) {
        if p != 0 {
            *v = -*v;
        }
    }
}

/// The four code bits for input `b` from encoder state `st` (bit k: the input k + 1 steps ago):
/// G1 = 1+D+D⁴, G2 = 1+D²+D³+D⁴, G3 = 1+D+D²+D⁴, G4 = 1+D+D³+D⁴ (clause 8.2.3.1.1).
#[inline]
fn code_bits(st: u8, b: u8) -> [u8; 4] {
    let d = |k: u32| st >> (k - 1) & 1;
    [b ^ d(1) ^ d(4), b ^ d(2) ^ d(3) ^ d(4), b ^ d(1) ^ d(2) ^ d(4), b ^ d(1) ^ d(3) ^ d(4)]
}

/// Puncturing to rate ⅔ (clause 8.2.3.1.3): of each two inputs' eight code bits, these are sent.
const KEPT: [usize; 3] = [0, 1, 4];

/// Viterbi over a 16-state mother code's soft bits (N a step), from state 0 to state 0: the input
/// bits, tail included. `code`: its bits for an input from a state (bit k: the input k + 1 steps ago).
pub(super) fn viterbi<const N: usize>(soft: &[f32], code: impl Fn(u8, u8) -> [u8; N]) -> Vec<u8> {
    let n = soft.len() / N;
    let mut metric = [f32::NEG_INFINITY; 16];
    metric[0] = 0.0;
    // The survivor's oldest input, which left the state.
    let mut from = vec![[0u8; 16]; n];
    for (t, s) in soft.as_chunks::<N>().0.iter().enumerate() {
        let mut next = [f32::NEG_INFINITY; 16];
        for st in 0..16u8 {
            let m0 = metric[st as usize];
            if m0 == f32::NEG_INFINITY {
                continue;
            }
            for b in 0..2u8 {
                let m = m0 + code(st, b).iter().zip(s).map(|(&c, &v)| if c != 0 { v } else { -v }).sum::<f32>();
                let ns = ((st << 1 | b) & 15) as usize;
                if m > next[ns] {
                    next[ns] = m;
                    from[t][ns] = st >> 3;
                }
            }
        }
        metric = next;
    }
    let mut st = 0u8;
    let mut out = vec![0u8; n];
    for t in (0..n).rev() {
        out[t] = st & 1;
        st = st >> 1 | from[t][st as usize] << 3;
    }
    out
}

/// The 16 check bits over `bits` (clause 8.2.3.3): CRC-CCITT preset to ones, inverted.
fn crc16(bits: &[u8]) -> u32 {
    crate::bits::crc(bits, 16, 0x1021, u32::MAX) ^ 0xffff
}

/// A block's type-1 bits from its air bits, when the CRC passes.
pub fn decode(c: &Coding, air: &[f32], seed: u32) -> Option<Vec<u8>> {
    debug_assert_eq!(air.len(), c.air);
    let mut soft = air.to_vec();
    descramble(&mut soft, seed);
    let mut mother = vec![0f32; (c.info + 20) * 4];
    for j in 0..c.air {
        // Coded bit j (from 0) is bit j + 1 of the standard's, sent at a·(j + 1) mod K.
        mother[j / 3 * 8 + KEPT[j % 3]] = soft[c.a * (j + 1) % c.air];
    }
    let mut all = viterbi(&mother, code_bits);
    let rx = all[c.info..c.info + 16].iter().fold(0u32, |w, &b| w << 1 | b as u32);
    if crc16(&all[..c.info]) != rx || all[c.info + 16..].iter().any(|&b| b != 0) {
        return None;
    }
    all.truncate(c.info);
    Some(all)
}

/// Encode a block's type-1 bits into its air bits.
pub fn encode(c: &Coding, info: &[u8], seed: u32) -> Vec<u8> {
    debug_assert_eq!(info.len(), c.info);
    let mut bits = info.to_vec();
    let r = crc16(info);
    bits.extend((0..16).rev().map(|k| (r >> k & 1) as u8));
    bits.extend([0; 4]);
    let mut st = 0u8;
    let mut mother = Vec::with_capacity(bits.len() * 4);
    for &b in &bits {
        mother.extend(code_bits(st, b));
        st = (st << 1 | b) & 15;
    }
    let mut air = vec![0u8; c.air];
    for j in 0..c.air {
        air[c.a * (j + 1) % c.air] = mother[j / 3 * 8 + KEPT[j % 3]];
    }
    air.iter().zip(scrambling(seed)).map(|(b, p)| b ^ p).collect()
}

/// The parity each of the AACH's 14 bits (first bit first) adds (clause 8.2.3.2).
const RM_PARITY: [u16; 14] =
    [0x9b60, 0x2de0, 0xfc20, 0xe03c, 0x983a, 0x5436, 0x2c2e, 0xffdf, 0x8339, 0x42b5, 0x21ad, 0x1273, 0x096b, 0x04e7];

/// The (30,14) Reed-Muller codeword of 14 bits: them, then 16 parity bits.
pub fn rm_encode(info: u16) -> u32 {
    let parity = RM_PARITY.iter().enumerate().filter(|(i, _)| info >> (13 - i) & 1 != 0).fold(0, |p, (_, &r)| p ^ r);
    (info as u32 & 0x3fff) << 16 | parity as u32
}

/// The 14 bits of a received codeword and how many bits were corrected: up to 3 (the code's
/// minimum distance is 8, so no other codeword is as near).
pub fn rm_decode(rx: u32) -> Option<(u16, u32)> {
    let syndrome = |w: u32| (rm_encode((w >> 16) as u16) ^ w) as u16;
    let s = syndrome(rx);
    let fixed = |e: u32, n: u32| Some((((rx ^ e) >> 16) as u16 & 0x3fff, n));
    if s == 0 {
        return fixed(0, 0);
    }
    let one: [u16; 30] = std::array::from_fn(|k| syndrome(1 << k));
    for a in 0..30 {
        if one[a] == s {
            return fixed(1 << a, 1);
        }
        for b in a + 1..30 {
            if one[a] ^ one[b] == s {
                return fixed(1 << a | 1 << b, 2);
            }
            if let Some(c) = (b + 1..30).find(|&c| one[a] ^ one[b] ^ one[c] == s) {
                return fixed(1 << a | 1 << b | 1 << c, 3);
            }
        }
    }
    None
}

/// The AACH's 14 bits from its 30 air bits.
pub fn aach(air: &[f32; 30], seed: u32) -> Option<u16> {
    let mut soft = *air;
    descramble(&mut soft, seed);
    rm_decode(soft.iter().fold(0, |w, &v| w << 1 | (v > 0.0) as u32)).map(|(i, _)| i)
}

/// Encode the AACH.
pub fn aach_encode(info: u16, seed: u32) -> Vec<u8> {
    let w = rm_encode(info);
    (0..30).zip(scrambling(seed)).map(|(i, p)| (w >> (29 - i) & 1) as u8 ^ p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(text: &str) -> Vec<u8> {
        text.bytes().map(|b| b - b'0').collect()
    }

    fn soft(bits: &[u8]) -> Vec<f32> {
        bits.iter().map(|&b| if b != 0 { 1.0 } else { -1.0 }).collect()
    }

    fn word(b: &[u8], at: usize, n: usize) -> u32 {
        b[at..at + n].iter().fold(0, |w, &x| w << 1 | x as u32)
    }

    /// dsd-neo's fixed vectors (tests/protocol/tetra/test_air_chain_vector.c), from an encoder
    /// that isn't this one.
    const VECTOR_BSCH: &str = "101110011011001001110101100100111111101011110110100100001111001101111001010100010110000000111011010011110110110000010100";
    const VECTOR_SYSINFO: &str = "100000011010001000011100001101100010101110100101100000011101010111001101100110111011000101111011010111010010100110000110110111101011000100000101100001110001100101101101010100101001100100100100100001101111000101011111";
    /// A BNCH block received off the air (MCC 250, MNC 13, colour 13), from GopherTrunk's tests
    /// (Apache-2.0) by way of dsd-neo's: one character a dibit, numbered 0 +45°, 1 +135°, 2 −135°, 3 −45°.
    const REAL_BNCH: &str = "301001001021222233210033333010313133131332333113133201133201320112011103013102212002313230110333100200111110";

    #[test]
    fn the_independent_bsch_vector_decodes() {
        let d = decode(&BSCH, &soft(&bits(VECTOR_BSCH)), BSCH_SEED).expect("CRC");
        assert_eq!((word(&d, 4, 6), word(&d, 10, 2) + 1, word(&d, 12, 5), word(&d, 17, 6)), (17, 3, 9, 37));
        assert_eq!((word(&d, 31, 10), word(&d, 41, 14)), (460, 4242));
        // And encodes back to it.
        assert_eq!(encode(&BSCH, &d, BSCH_SEED), bits(VECTOR_BSCH));
    }

    #[test]
    fn the_independent_sysinfo_vector_decodes() {
        let s = seed(460, 4242, 17);
        assert_eq!(s, 0x7310_9247);
        let d = decode(&SCH_HD, &soft(&bits(VECTOR_SYSINFO)), s).expect("CRC");
        // SYSINFO: main carrier 321, band 4, offset 1.
        assert_eq!((word(&d, 0, 4), word(&d, 4, 12), word(&d, 16, 4), word(&d, 20, 2)), (0b1000, 321, 4, 1));
        assert_eq!(encode(&SCH_HD, &d, s), bits(VECTOR_SYSINFO));
        // Not with another cell's scrambling.
        assert_eq!(decode(&SCH_HD, &soft(&bits(VECTOR_SYSINFO)), seed(460, 4242, 18)), None);
    }

    #[test]
    fn a_block_off_the_air_decodes() {
        let air: Vec<u8> = REAL_BNCH.bytes().flat_map(|c| { let d = c - b'0'; [d >> 1, (d >> 1) ^ (d & 1)] }).collect();
        assert_eq!(air.len(), 216);
        assert!(decode(&SCH_HD, &soft(&air), seed(250, 13, 13)).is_some());
    }

    /// The sequence by the standard's recurrence over its whole history (equations 8.41, 8.42).
    #[test]
    fn scrambling_is_the_recurrence() {
        for (mcc, mnc, cc) in [(0, 0, 0), (250, 13, 0), (250, 13, 13), (310, 42, 1), (1023, 16383, 63)] {
            let s = seed(mcc, mnc, cc);
            let mut p: Vec<u8> = (0..32).map(|j| (s >> j & 1) as u8).collect();
            for k in 32..300 {
                let b = [1, 2, 4, 5, 7, 8, 10, 11, 12, 16, 22, 23, 26, 32].iter().fold(0, |x, t| x ^ p[k - t]);
                p.push(b);
            }
            assert_eq!(scrambling(s).take(268).collect::<Vec<_>>(), p[32..]);
        }
    }

    #[test]
    fn round_trips_with_errors() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut noise = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for c in [BSCH, SCH_HD, SCH_F] {
            for trial in 0..20 {
                let info: Vec<u8> = (0..c.info).map(|_| (noise() & 1) as u8).collect();
                let mut s = soft(&encode(&c, &info, seed(310, 42, 5)));
                // As many as the punctured code (free distance 4) always corrects hard, and others weak.
                for k in 0..trial % 4 {
                    let i = noise() as usize % s.len();
                    s[i] *= if k == 0 { -1.0 } else { -0.2 };
                }
                assert_eq!(decode(&c, &s, seed(310, 42, 5)), Some(info), "{c:?} {trial}");
            }
        }
    }

    /// Noise must not pass: 16 CRC bits and 4 tail bits (which the Viterbi decoder always makes zero).
    #[test]
    fn noise_rarely_passes_the_crc() {
        let mut x = 1u64;
        let mut ok = 0;
        for _ in 0..3000 {
            let s: Vec<f32> = (0..120).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; if x & 1 != 0 { 1.0 } else { -1.0 } }).collect();
            ok += decode(&BSCH, &s, BSCH_SEED).is_some() as u32;
        }
        assert!(ok <= 1, "{ok}");
    }

    /// AACH words received off the air (MCC 228, MNC 8889), from dsd-neo's tests: three headers, some with bit errors.
    #[test]
    fn aach_words_off_the_air() {
        for (text, header, f1, f2, fixed) in [
            ("000010010010010100001010101111", 0, 9, 9, 0),
            ("111100111100111011010101101101", 3, 51, 51, 0),
            ("111100111100111011010101101001", 3, 51, 51, 1),
            ("101100110010010111000111000111", 2, 51, 9, 0),
            ("100000010010010101000111100101", 2, 1, 9, 2),
            ("110000011100111000010101011111", 3, 1, 51, 0),
        ] {
            let w = bits(text).iter().fold(0u32, |w, &b| w << 1 | b as u32);
            assert_eq!(rm_decode(w), Some((header << 12 | f1 << 6 | f2, fixed)), "{text}");
        }
    }

    #[test]
    fn aach_corrects_three_and_refuses_four() {
        let w = rm_encode(0x1234);
        assert_eq!(rm_decode(w ^ 0x2000_0101), Some((0x1234, 3)));
        assert_eq!(rm_decode(w ^ 0x2000_0111), None);
        let min = (1..1u16 << 14).map(|i| rm_encode(i).count_ones()).min();
        assert_eq!(min, Some(8));
    }
}
