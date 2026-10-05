//! A frame's functional channels: where each sits in the body, decoded
//! (de-interleaved, Viterbi, CRC) into SR information and layer 3 octets.

use super::fec::{self, bytes_of, Coding};
use super::frame::Frame;

/// Body bit offsets (after the FSW).
pub const SACCH_AT: usize = 16;
pub const HALF_AT: [usize; 2] = [76, 220];
pub const CAC_AT: usize = 16;
pub const UDCH_AT: usize = 16;

/// The SR octet (§6.3): structure (2 bits) and RAN (6 bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sr {
    pub structure: u8,
    pub ran: u8,
}

impl Sr {
    fn of(bits: &[u8]) -> Sr {
        let b = bytes_of(&bits[..8])[0];
        Sr { structure: b >> 6, ran: b & 0x3f }
    }
}

fn decode_at(f: &Frame, c: &Coding, at: usize) -> fec::Decoded {
    fec::decode(c, &f.soft_range(at, at + c.air()))
}

/// A SACCH: its SR and 18 data bits (a quarter of a superframe message, or
/// a whole short one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sacch {
    pub sr: Sr,
    pub data: u32,
    pub errs: u32,
}

pub fn sacch(f: &Frame) -> Option<Sacch> {
    let d = decode_at(f, &fec::SACCH, SACCH_AT);
    d.crc_ok.then(|| Sacch { sr: Sr::of(&d.bits), data: d.bits[8..26].iter().fold(0, |w, &b| w << 1 | b as u32), errs: d.errs })
}

/// FACCH1 in half `h` (0, 1): 10 octets of layer 3.
pub fn facch1(f: &Frame, h: usize) -> Option<([u8; 10], u32)> {
    let d = decode_at(f, &fec::FACCH1, HALF_AT[h]);
    d.crc_ok.then(|| (bytes_of(&d.bits).try_into().unwrap(), d.errs))
}

/// UDCH / FACCH2: SR and 22 octets.
pub fn udch(f: &Frame) -> Option<(Sr, [u8; 22], u32)> {
    let d = decode_at(f, &fec::UDCH, UDCH_AT);
    d.crc_ok.then(|| (Sr::of(&d.bits), bytes_of(&d.bits[8..184]).try_into().unwrap(), d.errs))
}

/// Outbound CAC: SR and 18 octets (one message, or two of 9: SR structure bit 0).
pub fn cac(f: &Frame) -> Option<(Sr, [u8; 18], u32)> {
    let d = decode_at(f, &fec::CAC, CAC_AT);
    d.crc_ok.then(|| (Sr::of(&d.bits), bytes_of(&d.bits[8..152]).try_into().unwrap(), d.errs))
}

/// A Type-D SCCH (TS 1-E §6.5): structure (which of INFO1–4 it is, by its
/// place in the superframe: 3 → INFO1 … 0 → INFO4), the area bit and 22
/// bits whose meaning depends on the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scch {
    pub structure: u8,
    pub area: bool,
    pub data: u32,
    pub errs: u32,
}

impl Scch {
    /// INFO1 (3) … INFO4 (0) → 1 … 4.
    pub fn info(&self) -> u8 {
        4 - self.structure
    }
    /// The first 5-bit field: repeater in use / go to repeater / free repeater 1.
    pub fn repeater(&self) -> u8 {
        (self.data >> 17) as u8 & 0x1f
    }
    /// INFO2 / INFO3 / INFO4: the 16-bit ID (home repeater or prefix, then
    /// the 11-bit unit or group), as VCALL carries it.
    pub fn id(&self) -> u16 {
        (self.data >> 1) as u16
    }
    /// The 11-bit unit / group part (2041–2047: special messages).
    pub fn short_id(&self) -> u16 {
        self.id() & 0x7ff
    }
    /// INFO2 / INFO4: the destination is a group (G/U bit 0) or a unit.
    pub fn group(&self) -> bool {
        self.data & 1 == 0
    }
    /// INFO1: cipher type (0 clear).
    pub fn cipher(&self) -> u8 {
        (self.data >> 7) as u8 & 3
    }
}

pub fn scch(f: &Frame) -> Option<Scch> {
    let d = decode_at(f, &fec::SCCH, SACCH_AT);
    let w = |a: usize, n: usize| d.bits[a..a + n].iter().fold(0u32, |v, &b| v << 1 | b as u32);
    d.crc_ok.then(|| Scch { structure: w(0, 2) as u8, area: d.bits[2] != 0, data: w(3, 22), errs: d.errs })
}

/// Gathers a superframe's four SACCH quarters (SR structure 3, 2, 1, 0)
/// into its 72-bit message (9 octets).
#[derive(Clone, Debug, Default)]
pub struct SacchAssembler {
    parts: [Option<u32>; 4],
}

impl SacchAssembler {
    /// A decoded superframe SACCH; the message once its last quarter is in
    /// and all four were.
    pub fn push(&mut self, s: &Sacch) -> Option<[u8; 9]> {
        let k = 3 - s.sr.structure as usize;
        if k == 0 {
            self.parts = [None; 4];
        }
        self.parts[k] = Some(s.data);
        if k != 3 {
            return None;
        }
        let parts = std::mem::take(&mut self.parts);
        let all = parts.iter().try_fold(0u128, |w, p| p.map(|p| w << 18 | p as u128))?;
        Some(std::array::from_fn(|i| (all >> (64 - 8 * i)) as u8))
    }

    /// A frame went by without a SACCH: the message in progress is lost.
    pub fn miss(&mut self) {
        self.parts = [None; 4];
    }
}

/// Encoders (the synthesizer, tests): channel air bits from SR and data.
pub mod build {
    use super::*;
    use crate::nxdn::fec::{bits_of, encode};

    fn sr_bits(sr: Sr) -> Vec<u8> {
        bits_of(&[sr.structure << 6 | sr.ran & 0x3f], 8)
    }

    pub fn sacch(sr: Sr, data: u32) -> Vec<u8> {
        let mut b = sr_bits(sr);
        b.extend((0..18).rev().map(|k| (data >> k & 1) as u8));
        encode(&fec::SACCH, &b)
    }

    pub fn facch1(octets: &[u8]) -> Vec<u8> {
        let mut o = octets.to_vec();
        o.resize(10, 0);
        encode(&fec::FACCH1, &bits_of(&o, 80))
    }

    pub fn udch(sr: Sr, octets: &[u8]) -> Vec<u8> {
        let mut o = octets.to_vec();
        o.resize(22, 0);
        let mut b = sr_bits(sr);
        b.extend(bits_of(&o, 176));
        encode(&fec::UDCH, &b)
    }

    /// A Type-D SCCH from its structure, area bit and 22 data bits.
    pub fn scch(structure: u8, area: bool, data: u32) -> Vec<u8> {
        let mut b: Vec<u8> = vec![structure >> 1 & 1, structure & 1, area as u8];
        b.extend((0..22).rev().map(|k| (data >> k & 1) as u8));
        encode(&fec::SCCH, &b)
    }

    /// SCCH data for INFO2 / INFO4 (and INFO3, `group` false): repeater, 16-bit ID, G/U.
    pub fn scch_id(repeater: u8, id: u16, group: bool) -> u32 {
        (repeater as u32 & 0x1f) << 17 | (id as u32) << 1 | !group as u32
    }

    /// SCCH data for INFO1: free repeaters, call option, cipher, key.
    pub fn scch_info1(rep1: u8, rep2: u8, option: u8, cipher: u8, key: u8) -> u32 {
        (rep1 as u32 & 0x1f) << 17 | (rep2 as u32 & 0x1f) << 12 | (option as u32 & 7) << 9 | (cipher as u32 & 3) << 7 | (key as u32 & 0x3f) << 1
    }

    pub fn cac(sr: Sr, octets: &[u8]) -> Vec<u8> {
        let mut o = octets.to_vec();
        o.resize(18, 0);
        let mut b = sr_bits(sr);
        b.extend(bits_of(&o, 144));
        b.extend([0, 0, 0]);
        encode(&fec::CAC, &b)
    }

    /// A 72-bit message split into its four superframe SACCHs' data.
    pub fn sacch_quarters(octets: &[u8; 9]) -> [u32; 4] {
        let all = octets.iter().fold(0u128, |w, &b| w << 8 | b as u128);
        std::array::from_fn(|k| (all >> (54 - 18 * k) & 0x3ffff) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sacch_superframe_reassembles() {
        let msg = [0x01, 0x00, 0x20, 0x03, 0x85, 0x07, 0xd1, 0x00, 0x00];
        let q = build::sacch_quarters(&msg);
        let mut a = SacchAssembler::default();
        for k in 0..4 {
            let s = Sacch { sr: Sr { structure: 3 - k as u8, ran: 1 }, data: q[k], errs: 0 };
            let got = a.push(&s);
            assert_eq!(got, (k == 3).then_some(msg));
        }
        // A quarter missing: nothing.
        for k in [0, 1, 3] {
            a.push(&Sacch { sr: Sr { structure: 3 - k as u8, ran: 1 }, data: q[k], errs: 0 });
        }
        assert_eq!(a.push(&Sacch { sr: Sr { structure: 0, ran: 1 }, data: q[3], errs: 0 }), None);
    }
}
