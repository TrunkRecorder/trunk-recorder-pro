//! TETRA speech as it's sent (EN 300 395-2 clause 5): a traffic burst's bits to the codec's
//! frames, 137 bits each 30 ms. The codec itself (ACELP) isn't here.
//!
//! A full slot carries two frames, their bits sorted into three classes: class 0 sent as it is,
//! class 1 at rate 8/12 and class 2, with a CRC over it, at 8/18 (one trellis); then interleaved
//! and scrambled. A half slot, its other half stolen for signalling, carries one, class 2 at 8/17.

use super::burst::{Burst, Kind};
use super::fec::{self, descramble, SCH_HD};

/// A frame's bits, B1 first.
pub const FRAME_BITS: usize = 137;
pub type Frame = [u8; FRAME_BITS];

/// Frame bit numbers (from 1) of each class, in the order they're sent (tables 4 to 6).
const CLASS0: [u8; 51] = [
    35, 36, 37, 38, 39, 40, 41, 42, 43, 47, 48, 56, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 74, 75, 83, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 101, 102, 110, 115, 116, 117, 118, 119, 120, 121, 122,
    123, 124, 128, 129, 137,
];
const CLASS1: [u8; 56] = [
    58, 85, 112, 54, 81, 108, 135, 50, 77, 104, 131, 45, 72, 99, 126, 55, 82, 109, 136, 5, 13, 34, 8, 16, 17, 22, 23, 24, 25, 26, 6, 14, 7, 15, 60, 87, 114, 46, 73, 100, 127, 44, 71, 98, 125, 33, 49,
    76, 103, 130, 59, 86, 113, 57, 84, 111,
];
const CLASS2: [u8; 30] = [18, 19, 20, 21, 31, 32, 53, 80, 107, 134, 1, 2, 3, 4, 9, 10, 11, 12, 27, 28, 29, 30, 52, 79, 106, 133, 51, 78, 105, 132];

/// Puncturing (of each `.0` mother code bits, these are sent): class 1, and class 2 of a full and a half slot.
const P_8_12: (usize, &[usize]) = (6, &[0, 1, 3]);
const P_8_18: (usize, &[usize]) = (12, &[0, 1, 2, 3, 4, 6, 7, 9, 10]);
const P_8_17: (usize, &[usize]) = (24, &[0, 1, 2, 3, 4, 6, 7, 9, 10, 12, 13, 15, 16, 18, 19, 21, 22]);

/// The three code bits for input `b` from encoder state `st` (bit k: the input k + 1 steps ago):
/// G1 = 1+D+D²+D³+D⁴, G2 = 1+D+D³+D⁴, G3 = 1+D²+D⁴ (clause 5.4.3.1).
#[inline]
fn code_bits(st: u8, b: u8) -> [u8; 3] {
    let d = |k: u32| st >> (k - 1) & 1;
    [b ^ d(1) ^ d(2) ^ d(3) ^ d(4), b ^ d(1) ^ d(3) ^ d(4), b ^ d(2) ^ d(4)]
}

/// The remainder of Xᴺ·I(X) by a generator (its terms below Xᴺ), I's first bit the constant term;
/// bit i of it is the (i + 1)th sent (equations 43 to 45).
fn remainder(bits: &[u8], n: u32, low: u32) -> u32 {
    bits.iter().rev().fold(0, |r, &b| {
        let fb = (r >> (n - 1) & 1) ^ b as u32;
        (r << 1 & ((1 << n) - 1)) ^ if fb != 0 { low } else { 0 }
    })
}

/// How a slot's speech is coded.
struct Layout {
    frames: usize,
    air: usize,
    class2: (usize, &'static [usize]),
    /// CRC bits, the generator's low terms, and whether a parity bit over class 2 and the CRC follows.
    crc: (u32, u32, bool),
    /// The most coded bits that may have been wrong: speech leaves a handful, anything else (a half
    /// slot's 4 CRC bits pass one in 16) many more.
    max_errs: u32,
}

const FULL: Layout = Layout { frames: 2, air: 432, class2: P_8_18, crc: (7, 0x09, true), max_errs: 28 };
const HALF: Layout = Layout { frames: 1, air: 216, class2: P_8_17, crc: (4, 0x03, false), max_errs: 16 };

impl Layout {
    /// Air position of type-3 bit `i`.
    fn air_pos(&self, i: usize) -> usize {
        if self.frames == 2 {
            // Written in 24 lines of 18, read by columns.
            i % 18 * 24 + i / 18
        } else {
            101 * (i + 1) % self.air
        }
    }
    /// Bits into the trellis: class 1, class 2, the CRC, the tail.
    fn inputs(&self) -> usize {
        self.frames * (56 + 30) + self.crc.0 as usize + self.crc.2 as usize + 4
    }
    /// Mother code positions of the coded bits, in the order they're sent.
    fn sent(&self) -> impl Iterator<Item = usize> {
        let (c1, all) = (self.frames * 56 * 3, self.inputs() * 3);
        let of = |from: usize, to: usize, (period, kept): (usize, &'static [usize])| (from..to).filter(move |k| kept.contains(&((k - from) % period)));
        of(0, c1, P_8_12).chain(of(c1, all, self.class2)).take(self.air - self.frames * 51)
    }
    fn check_ok(&self, class2: &[u8], check: &[u8]) -> bool {
        let (n, low, parity) = self.crc;
        let r = remainder(class2, n, low);
        let sum = || class2.iter().chain(&check[..=n as usize]).fold(0, |x, b| x ^ b) == 0;
        (0..n).all(|i| check[i as usize] as u32 == r >> i & 1) && (!parity || sum())
    }

    fn decode(&self, air: &[f32], seed: u32) -> Option<(Vec<Frame>, u32)> {
        let mut soft = air.to_vec();
        descramble(&mut soft, seed);
        let type3: Vec<f32> = (0..self.air).map(|i| soft[self.air_pos(i)]).collect();
        let (n0, n1, n2) = (self.frames * 51, self.frames * 56, self.frames * 30);
        let mut mother = vec![0f32; self.inputs() * 3];
        for (k, &v) in self.sent().zip(&type3[n0..]) {
            mother[k] = v;
        }
        let coded = fec::viterbi(&mother, code_bits);
        let (class1, rest) = coded.split_at(n1);
        let (class2, check) = rest.split_at(n2);
        if !self.check_ok(class2, check) {
            return None;
        }
        let mut st = 0u8;
        let sent: Vec<u8> = coded.iter().flat_map(|&b| { let c = code_bits(st, b); st = (st << 1 | b) & 15; c }).collect();
        let errs = self.sent().zip(&type3[n0..]).filter(|&(k, &v)| (v > 0.0) != (sent[k] != 0)).count() as u32;
        if errs > self.max_errs {
            return None;
        }
        let mut out = vec![[0u8; FRAME_BITS]; self.frames];
        let class0: Vec<u8> = type3[..n0].iter().map(|&v| (v > 0.0) as u8).collect();
        for (bits, places) in [(&class0[..], &CLASS0[..]), (class1, &CLASS1[..]), (class2, &CLASS2[..])] {
            // Two frames' bits alternate.
            for (i, &b) in bits.iter().enumerate() {
                out[i % self.frames][places[i / self.frames] as usize - 1] = b;
            }
        }
        Some((out, errs))
    }

    fn encode(&self, frames: &[Frame], seed: u32) -> Vec<u8> {
        let class = |places: &[u8]| -> Vec<u8> { (0..places.len() * self.frames).map(|i| frames[i % self.frames][places[i / self.frames] as usize - 1]).collect() };
        let (mut type3, mut inputs, class2) = (class(&CLASS0), class(&CLASS1), class(&CLASS2));
        let (n, low, parity) = self.crc;
        let r = remainder(&class2, n, low);
        inputs.extend(&class2);
        inputs.extend((0..n).map(|i| (r >> i & 1) as u8));
        if parity {
            inputs.push(inputs[self.frames * 56..].iter().fold(0, |x, b| x ^ b));
        }
        inputs.extend([0; 4]);
        let mut st = 0u8;
        let mut mother = Vec::new();
        for &b in &inputs {
            mother.extend(code_bits(st, b));
            st = (st << 1 | b) & 15;
        }
        type3.extend(self.sent().map(|k| mother[k]));
        let mut air = vec![0u8; self.air];
        for (i, &b) in type3.iter().enumerate() {
            air[self.air_pos(i)] = b;
        }
        air.iter().zip(fec::scrambling(seed)).map(|(b, p)| b ^ p).collect()
    }
}

/// A full slot's two frames (432 bits), when their CRC passes, and the bits corrected in them.
pub fn full(air: &[f32], seed: u32) -> Option<([Frame; 2], u32)> {
    let (f, errs) = FULL.decode(air, seed)?;
    Some((f.try_into().ok()?, errs))
}

/// A half slot's frame (216 bits), likewise.
pub fn half(air: &[f32], seed: u32) -> Option<(Frame, u32)> {
    let (f, errs) = HALF.decode(air, seed)?;
    Some((*f.first()?, errs))
}

/// A traffic slot's two frames, each with the bits corrected in it; None: lost, or not speech.
pub fn frames(b: &Burst, seed: u32) -> [Option<(Frame, u32)>; 2] {
    match b.kind {
        Kind::Full => full(&b.full(), seed).map_or([None; 2], |(f, errs)| f.map(|f| Some((f, errs / 2)))),
        // The first half is stolen for signalling; the second may be too.
        Kind::Halves if fec::decode(&SCH_HD, b.block2(), seed).is_none() => [None, half(b.block2(), seed)],
        _ => [None; 2],
    }
}

/// Encoders (the synthesizer, tests).
pub fn encode_full(frames: &[Frame; 2], seed: u32) -> Vec<u8> {
    FULL.encode(frames, seed)
}
pub fn encode_half(frame: &Frame, seed: u32) -> Vec<u8> {
    HALF.encode(std::slice::from_ref(frame), seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(x: &mut u64) -> u64 {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        *x
    }
    fn frame(x: &mut u64) -> Frame {
        std::array::from_fn(|_| (noise(x) >> 20 & 1) as u8)
    }
    fn soft(bits: &[u8]) -> Vec<f32> {
        bits.iter().map(|&b| if b != 0 { 1.0 } else { -1.0 }).collect()
    }

    #[test]
    fn the_classes_are_every_bit_once() {
        let mut all: Vec<u8> = CLASS0.iter().chain(&CLASS1).chain(&CLASS2).copied().collect();
        all.sort();
        assert_eq!(all, (1..=137).collect::<Vec<u8>>());
    }

    /// Lengths as clauses 5.5 and 5.6 give them.
    #[test]
    fn lengths() {
        assert_eq!((FULL.inputs(), FULL.sent().count(), HALF.inputs(), HALF.sent().count()), (184, 330, 94, 165));
    }

    #[test]
    fn round_trips() {
        let mut x = 5;
        for _ in 0..50 {
            let f = [frame(&mut x), frame(&mut x)];
            assert_eq!(full(&soft(&encode_full(&f, 77)), 77), Some((f, 0)));
            assert_eq!(half(&soft(&encode_half(&f[0], 77)), 77), Some((f[0], 0)));
        }
    }

    #[test]
    fn anything_else_isnt_speech() {
        let mut x = 9;
        let f = [frame(&mut x), frame(&mut x)];
        let mut passed = 0;
        for _ in 0..2000 {
            let s: Vec<f32> = (0..432).map(|_| if noise(&mut x) >> 20 & 1 != 0 { 1.0 } else { -1.0 }).collect();
            passed += full(&s, 77).is_some() as u32;
        }
        assert!(passed < 30, "{passed}");
        let halves = (0..2000).filter(|_| { let s: Vec<f32> = (0..216).map(|_| if noise(&mut x) >> 20 & 1 != 0 { 1.0 } else { -1.0 }).collect(); half(&s, 77).is_some() }).count();
        assert!(halves < 30, "{halves}");
        assert_eq!(full(&soft(&encode_full(&f, 78)), 77), None);
    }
}
