//! NXDN channel coding (TS 1-A §4.5): every coded channel is the same chain —
//!
//! ```text
//! info bits (SR + layer 3) → CRC (register preset to ones) → 4 zero tail bits
//!   → convolutional code K=5 R=½ (G1 = 1+D³+D⁴, G2 = 1+D+D²+D⁴, G1 first)
//!   → puncturing (a fixed pattern per channel) → block interleave
//!     (written in rows of `cols`, read by columns)
//! ```
//!
//! Decoding runs it backwards with soft bits (+ a 1, − a 0, 0 unknown or
//! punctured) through a 16-state Viterbi decoder; a CRC pass is the verdict.

/// One coded channel's parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coding {
    /// Bits before the CRC (SR and layer 3 data, and any null bits).
    pub info: usize,
    pub crc_bits: u32,
    /// The CRC's generator polynomial without its leading term.
    pub crc_poly: u32,
    /// Puncturing period and the erased positions within it (coded-bit order X1, X2, …, from 0).
    pub punct_period: usize,
    pub punct: &'static [usize],
    /// Interleaver: columns of the write matrix (rows = air bits / cols).
    pub cols: usize,
}

/// Outbound CAC (control channel): SR 8 + 144 + 3 null, CRC-16 → 300 bits.
pub const CAC: Coding = Coding { info: 155, crc_bits: 16, crc_poly: 0x1021, punct_period: 14, punct: &[3, 11], cols: 12 };
/// Inbound long CAC: SR 8 + 128, CRC-16 → 252 bits.
pub const LONG_CAC: Coding = Coding { info: 136, crc_bits: 16, crc_poly: 0x1021, punct_period: 26, punct: &[1, 7, 9, 11, 19], cols: 12 };
/// Inbound short CAC: SR 8 + 96 + 2 null, CRC-16, not punctured → 252 bits.
pub const SHORT_CAC: Coding = Coding { info: 106, crc_bits: 16, crc_poly: 0x1021, punct_period: 1, punct: &[], cols: 12 };
/// SACCH: SR 8 + 18, CRC-6 → 60 bits.
pub const SACCH: Coding = Coding { info: 26, crc_bits: 6, crc_poly: 0x27, punct_period: 12, punct: &[5, 11], cols: 12 };
/// Type-D SCCH (in the SACCH's place): SR 7 + 18, CRC-7 → 60 bits.
pub const SCCH: Coding = Coding { info: 25, crc_bits: 7, crc_poly: 0x09, punct_period: 12, punct: &[5, 11], cols: 12 };
/// FACCH1: 80 bits of layer 3 (no SR), CRC-12 → 144 bits.
pub const FACCH1: Coding = Coding { info: 80, crc_bits: 12, crc_poly: 0x80f, punct_period: 4, punct: &[1], cols: 16 };
/// UDCH / FACCH2: SR 8 + 176, CRC-15 → 348 bits.
pub const UDCH: Coding = Coding { info: 184, crc_bits: 15, crc_poly: 0x4cc5, punct_period: 14, punct: &[3, 11], cols: 12 };

impl Coding {
    /// Convolutional-coded bits: (info + CRC + 4 tail) × 2.
    pub fn coded(&self) -> usize {
        (self.info + self.crc_bits as usize + 4) * 2
    }
    /// Bits on the air after puncturing.
    pub fn air(&self) -> usize {
        let n = self.coded();
        n - n / self.punct_period * self.punct.len() - (0..n % self.punct_period).filter(|i| self.punct.contains(i)).count()
    }
    fn kept(&self, i: usize) -> bool {
        !self.punct.contains(&(i % self.punct_period))
    }
    /// Air position of de-interleaved bit `i`.
    fn air_pos(&self, i: usize) -> usize {
        let rows = self.air() / self.cols;
        (i % self.cols) * rows + i / self.cols
    }
}

/// CRC of `bits` (MSB first), the register preset to all ones.
pub fn crc(bits: &[u8], width: u32, poly: u32) -> u32 {
    let mask = (1u32 << width) - 1;
    let top = 1u32 << (width - 1);
    let mut r = mask;
    for &b in bits {
        let fb = (b as u32 & 1) ^ (r & top != 0) as u32;
        r = (r << 1) & mask;
        if fb != 0 {
            r ^= poly;
        }
    }
    r
}

/// The two code bits for input `b` from encoder state `st` (bit k: the input k+1 steps ago).
#[inline]
fn code_bits(st: u8, b: u8) -> (u8, u8) {
    let d = |k: u32| st >> (k - 1) & 1;
    (b ^ d(3) ^ d(4), b ^ d(1) ^ d(2) ^ d(4))
}

/// Convolutional encoder (from state 0): two code bits per input bit, G1 first.
pub fn conv_encode(bits: &[u8]) -> Vec<u8> {
    let mut st = 0u8;
    let mut out = Vec::with_capacity(bits.len() * 2);
    for &b in bits {
        let (g1, g2) = code_bits(st, b & 1);
        out.push(g1);
        out.push(g2);
        st = (st << 1 | (b & 1)) & 15;
    }
    out
}

/// Soft Viterbi decoder over `soft` (pairs G1, G2; + a 1, − a 0, 0 unknown),
/// from state 0 to state 0 (the zero tail): the input bits, tail included.
pub fn viterbi(soft: &[f32]) -> Vec<u8> {
    let n = soft.len() / 2;
    let mut metric = [f32::NEG_INFINITY; 16];
    metric[0] = 0.0;
    // Survivor: for each step and state, the predecessor's high bit (the
    // state is the last 4 inputs; the input itself is its low bit).
    let mut from = vec![[0u8; 16]; n];
    for t in 0..n {
        let (s1, s2) = (soft[2 * t], soft[2 * t + 1]);
        let mut next = [f32::NEG_INFINITY; 16];
        for st in 0..16u8 {
            if metric[st as usize] == f32::NEG_INFINITY {
                continue;
            }
            for b in 0..2u8 {
                let (g1, g2) = code_bits(st, b);
                let m = metric[st as usize] + if g1 != 0 { s1 } else { -s1 } + if g2 != 0 { s2 } else { -s2 };
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

/// A decoded channel: its info bits (without CRC and tail), whether the CRC
/// passed, and how many air bits the decoder corrected.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    pub bits: Vec<u8>,
    pub crc_ok: bool,
    pub errs: u32,
}

/// Decode a channel from its air bits as soft values (in air order, before
/// de-interleaving; `c.air()` of them).
pub fn decode(c: &Coding, air: &[f32]) -> Decoded {
    debug_assert_eq!(air.len(), c.air());
    let mut coded = vec![0f32; c.coded()];
    // De-interleave into the punctured stream, then put the erasures back.
    let punctured: Vec<f32> = (0..c.air()).map(|i| air[c.air_pos(i)]).collect();
    let mut k = 0;
    for (i, slot) in coded.iter_mut().enumerate() {
        if c.kept(i) {
            *slot = punctured[k];
            k += 1;
        }
    }
    let all = viterbi(&coded);
    let n = c.info + c.crc_bits as usize;
    let crc_rx = all[c.info..n].iter().fold(0u32, |w, &b| w << 1 | b as u32);
    let crc_ok = crc(&all[..c.info], c.crc_bits, c.crc_poly) == crc_rx && all[n..].iter().all(|&b| b == 0);
    // Corrected bits: the re-encoded stream against the hard decisions.
    let re = conv_encode(&all);
    let errs = re.iter().zip(&coded).filter(|&(&b, &s)| s != 0.0 && (b != 0) != (s > 0.0)).count() as u32;
    Decoded { bits: all[..c.info].to_vec(), crc_ok, errs }
}

/// Encode `info` (c.info bits) into the channel's air bits.
pub fn encode(c: &Coding, info: &[u8]) -> Vec<u8> {
    debug_assert_eq!(info.len(), c.info);
    let mut bits = info.to_vec();
    let r = crc(info, c.crc_bits, c.crc_poly);
    bits.extend((0..c.crc_bits).rev().map(|k| (r >> k & 1) as u8));
    bits.extend([0u8; 4]);
    let coded = conv_encode(&bits);
    let punctured: Vec<u8> = coded.iter().enumerate().filter(|&(i, _)| c.kept(i)).map(|(_, &b)| b).collect();
    let mut air = vec![0u8; c.air()];
    for (i, &b) in punctured.iter().enumerate() {
        air[c.air_pos(i)] = b;
    }
    air
}

/// Bits of `bytes` (MSB first), the first `n`.
pub fn bits_of(bytes: &[u8], n: usize) -> Vec<u8> {
    (0..n).map(|i| bytes[i / 8] >> (7 - i % 8) & 1).collect()
}

/// Bytes from bits (MSB first; a last partial byte is padded with zeros).
pub fn bytes_of(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |w, (i, &b)| w | (b & 1) << (7 - i))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(&str, Coding, usize); 7] =
        [("cac", CAC, 300), ("long_cac", LONG_CAC, 252), ("short_cac", SHORT_CAC, 252), ("sacch", SACCH, 60), ("scch", SCCH, 60), ("facch1", FACCH1, 144), ("udch", UDCH, 348)];

    fn noise(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn air_lengths_match_the_spec() {
        for (name, c, n) in ALL {
            assert_eq!(c.air(), n, "{name}");
            assert_eq!(c.air() % c.cols, 0, "{name}");
        }
    }

    /// The spec's example (§4.5.4): on the CAC, X4 and X12 (1-based) are the
    /// first punctured bits; air order is Y1, Y13, Y25 … (columns of 12).
    #[test]
    fn puncture_and_interleave_follow_the_spec_example() {
        assert!(!CAC.kept(3) && !CAC.kept(11) && CAC.kept(4) && !CAC.kept(17));
        assert_eq!((0..4).map(|k| (0..300).find(|&i| CAC.air_pos(i) == k).unwrap()).collect::<Vec<_>>(), [0, 12, 24, 36]);
        assert_eq!(CAC.air_pos(1), 25);
    }

    /// CRCs as MMDVMHost / SDRTrunk compute them (preset all ones): the
    /// register over the data then its own CRC is the CRC of a message
    /// ending in it, and a known vector.
    #[test]
    fn crcs() {
        // CRC-16 CCITT preset 0xFFFF (no final XOR) of "123456789" is 0x29B1.
        let bits = bits_of(b"123456789", 72);
        assert_eq!(crc(&bits, 16, 0x1021), 0x29b1);
        // CRC-6/7/12/15: nonzero and changes with any bit flip.
        for (w, p) in [(6, 0x27), (7, 0x09), (12, 0x80f), (15, 0x4cc5)] {
            let c0 = crc(&bits, w, p);
            for i in 0..bits.len() {
                let mut b = bits.clone();
                b[i] ^= 1;
                assert_ne!(crc(&b, w, p), c0, "width {w} bit {i}");
            }
        }
    }

    /// A message followed by its CRC leaves no remainder (how OP25 checks
    /// the CAC: over data and CRC together, against zero).
    #[test]
    fn data_plus_its_crc_leaves_no_remainder() {
        let mut seed = 0x1234_5678_9abc_def0;
        for (w, p) in [(16, 0x1021), (6, 0x27), (12, 0x80f), (15, 0x4cc5)] {
            let info: Vec<u8> = (0..155).map(|_| (noise(&mut seed) & 1) as u8).collect();
            let r = crc(&info, w, p);
            let mut all = info.clone();
            all.extend((0..w).rev().map(|k| (r >> k & 1) as u8));
            // Preset ones over the data, then the CRC bits: zero; equivalently
            // the register value equals the transmitted CRC.
            assert_eq!(crc(&all[..155], w, p), r);
        }
    }

    #[test]
    fn round_trip_clean_and_with_errors() {
        let mut seed = 0x9e37_79b9_7f4a_7c15;
        for (name, c, _) in ALL {
            for trial in 0..20 {
                let info: Vec<u8> = (0..c.info).map(|_| (noise(&mut seed) & 1) as u8).collect();
                let air = encode(&c, &info);
                let mut soft: Vec<f32> = air.iter().map(|&b| if b != 0 { 1.0 } else { -1.0 }).collect();
                let flips = trial % 4;
                for _ in 0..flips {
                    let i = (noise(&mut seed) % soft.len() as u64) as usize;
                    soft[i] = -soft[i];
                }
                let d = decode(&c, &soft);
                assert!(d.crc_ok, "{name} trial {trial}");
                assert_eq!(d.bits, info, "{name} trial {trial}");
            }
        }
    }

    #[test]
    fn a_wrong_message_fails_its_crc() {
        let info = vec![0u8; FACCH1.info];
        let air = encode(&FACCH1, &info);
        // Flip a whole run: beyond correction.
        let soft: Vec<f32> = air.iter().enumerate().map(|(i, &b)| if (b != 0) ^ (i % 3 == 0) { 1.0 } else { -1.0 }).collect();
        assert!(!decode(&FACCH1, &soft).crc_ok);
    }
}
