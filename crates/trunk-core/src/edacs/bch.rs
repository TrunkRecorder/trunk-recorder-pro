//! EDACS's BCH (40,28) code: each control channel message is 28 bits of
//! data and 12 of parity, the remainder of the data (shifted up 12) divided
//! by g(x) = x¹² + x¹⁰ + x⁸ + x⁵ + x⁴ + x³ + 1 — a shortened (63,51) BCH
//! code with a designed distance of 5, so two errors in a word are corrected.
//!
//! Words are 40-bit integers, the first bit sent the highest (bit 39).

use std::sync::OnceLock;

/// g(x), bit i the coefficient of xⁱ.
pub const GEN: u64 = 0x1539;
const DATA_MASK: u64 = 0xFFF_FFFF;

/// The remainder of `v` (up to 40 bits) divided by g(x).
fn rem(mut v: u64) -> u64 {
    for i in (12..40).rev() {
        if v >> i & 1 == 1 {
            v ^= GEN << (i - 12);
        }
    }
    v
}

/// 28 bits of data → the 40-bit word.
pub fn encode(data: u32) -> u64 {
    let d = (data as u64 & DATA_MASK) << 12;
    d | rem(d)
}

/// Each syndrome's error pattern of weight 1 or 2 (0: none such).
fn table() -> &'static [u64] {
    static T: OnceLock<Vec<u64>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![0u64; 1 << 12];
        for a in 0..40 {
            t[rem(1 << a) as usize] = 1 << a;
            for b in a + 1..40 {
                let e = (1u64 << a) | (1 << b);
                t[rem(e) as usize] = e;
            }
        }
        t
    })
}

/// A received word → its data and the bits corrected (up to 2); None when it
/// has more errors than that.
pub fn decode(word: u64) -> Option<(u32, u32)> {
    let w = word & 0xFF_FFFF_FFFF;
    let s = rem(w);
    if s == 0 {
        return Some(((w >> 12) as u32, 0));
    }
    let e = table()[s as usize];
    if e == 0 {
        return None;
    }
    let fixed = w ^ e;
    Some(((fixed >> 12) as u32, e.count_ones()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_words() {
        // From DSD-FME's edacs_bch (Morelos-Zaragoza's BCH encoder, m = 6, length 40).
        for (d, w) in [(0x0000001, 0x00_0000_1539u64), (0xFC00000, 0xFC_0000_0DE2), (0x1234567, 0x12_3456_7F9D), (0xABCDEF0, 0xAB_CDEF_0395), (0xFFFFFFF, 0xFF_FFFF_FA9A)] {
            assert_eq!(encode(d), w, "{d:07x}");
            assert_eq!(decode(w), Some((d, 0)));
        }
    }

    #[test]
    fn corrects_two_errors_and_refuses_three() {
        let mut s = 0x1357_9bdfu32;
        for _ in 0..2000 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let d = s & 0xFFF_FFFF;
            let w = encode(d);
            let (a, b) = ((s >> 3) % 40, (s >> 11) % 40);
            assert_eq!(decode(w ^ 1 << a), Some((d, 1)));
            if a != b {
                assert_eq!(decode(w ^ (1 << a) ^ (1 << b)), Some((d, 2)));
            }
        }
        // Three errors: never decoded as the word sent.
        let w = encode(0x0ABCDEF);
        assert_ne!(decode(w ^ 0b1011).map(|r| r.0), Some(0x0ABCDEF));
    }
}
