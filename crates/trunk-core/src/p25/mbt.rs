//! PDU frames → Alternate Multiple Block Trunking (AMBTC) messages: the
//! control channel's multi-block form of a TSBK, which carries both channels
//! of an explicit grant or broadcast and full (WACN, System ID, ID) unit
//! addresses. Some sites send their RFSS and network status only this way,
//! and without it their WACN and System ID are never heard. What one says is
//! [`super::osp::from_mbt`]'s to read.
//!
//! A PDU is a header block and the number of data blocks it names, each 196
//! bits of the same interleave and 1/2-rate trellis as a TSBK (a confirmed
//! packet's blocks are 3/4-rate, but trunking never is). The header ends in a
//! CRC-CCITT like a TSBK's; the data blocks end, together, in a CRC-32. An
//! outbound MBT is always the alternate format (AABC-D §2.2: the unconfirmed
//! one, %10101, is inbound only).

use super::tsbk::{trellis_viterbi, tsbk_ok, Tsbk};

/// Header format: unconfirmed alternate multiple block trunking.
const AMBTC: u8 = 0x17;
/// Header SAP: trunking control, and protected (encrypted) trunking control.
const SAP_TRUNKING: u8 = 0x3d;
const SAP_PROTECTED_TRUNKING: u8 = 0x3f;

/// The longest PDU the framer follows, in blocks (header included): a frame
/// may be no longer than an LDU, which the receiver bank holds for.
pub const MAX_BLOCKS: usize = 8;

/// The header block, if it decodes with a good CRC.
pub fn decode_header(bits: &[u8], soft: Option<&[f32]>) -> Option<Tsbk> {
    let h = trellis_viterbi(bits, soft);
    tsbk_ok(&h).then_some(h)
}

/// Data blocks following the header.
pub fn blocks_to_follow(h: &Tsbk) -> usize {
    (h[6] & 0x7f) as usize
}

/// CRC-32 (x³²+x²⁶+x²³+x²²+x¹⁶+x¹²+x¹¹+x¹⁰+x⁸+x⁷+x⁵+x⁴+x²+x+1, init 0,
/// inverted) of `data`, MSB first: a packet's CRC.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { crc << 1 ^ 0x04c1_1db7 } else { crc << 1 };
        }
    }
    !crc
}

/// One AMBTC: the header block and the data blocks' octets, less their CRC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mbt {
    pub header: Tsbk,
    pub data: Vec<u8>,
}

impl Mbt {
    pub fn opcode(&self) -> u8 {
        self.header[7] & 0x3f
    }
    pub fn mfid(&self) -> u8 {
        self.header[2]
    }

    /// Blocks on air, the header included.
    pub fn blocks(&self) -> usize {
        1 + blocks_to_follow(&self.header)
    }
}

/// What a PDU frame (or a group of copies) held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pdu {
    Mbt(Mbt),
    /// A good header, but not trunking (a data packet): the header.
    Other(Tsbk),
    /// No copy decoded.
    Lost,
}

/// The best of several copies of one PDU (one per receiver): the first good
/// header, then each data block's distinct candidates, until a combination
/// passes the CRC-32. `copies` are each frame's bits and soft bits.
pub fn decode<'a>(copies: impl Iterator<Item = (&'a [u8], Option<&'a [f32]>)> + Clone) -> Pdu {
    let block = |bits: &'a [u8], soft: Option<&'a [f32]>, k: usize| {
        let at = 112 + k * 196;
        (bits.len() >= at + 196).then(|| (&bits[at..at + 196], soft.filter(|s| s.len() >= at + 196).map(|s| &s[at..at + 196])))
    };
    let Some(h) = copies.clone().find_map(|(b, s)| block(b, s, 0).and_then(|(b, s)| decode_header(b, s))) else {
        return Pdu::Lost;
    };
    if h[0] & 0x1f != AMBTC || !matches!(h[1] & 0x3f, SAP_TRUNKING | SAP_PROTECTED_TRUNKING) {
        return Pdu::Other(h);
    }
    let n = blocks_to_follow(&h);
    if n == 0 || n + 1 > MAX_BLOCKS {
        return Pdu::Lost;
    }
    let mut cands: Vec<Vec<Tsbk>> = vec![Vec::new(); n];
    for (b, s) in copies {
        for (k, c) in cands.iter_mut().enumerate() {
            if let Some((bits, soft)) = block(b, s, k + 1) {
                let t = trellis_viterbi(bits, soft);
                if !c.contains(&t) {
                    c.push(t);
                }
            }
        }
    }
    if cands.iter().any(|c| c.is_empty()) {
        return Pdu::Lost;
    }
    // Every combination (a few receivers, a block or two: a handful).
    let mut pick = vec![0usize; n];
    loop {
        let data: Vec<u8> = pick.iter().enumerate().flat_map(|(k, &i)| cands[k][i]).collect();
        let (body, crc) = data.split_at(data.len() - 4);
        if crc32(body) == u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]]) {
            return Pdu::Mbt(Mbt { header: h, data: body.to_vec() });
        }
        let mut k = 0;
        while k < n {
            pick[k] += 1;
            if pick[k] < cands[k].len() {
                break;
            }
            pick[k] = 0;
            k += 1;
        }
        if k == n {
            return Pdu::Lost;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The 1/2-rate trellis encoder and interleave (the inverse of
    /// `trellis_viterbi`): 12 octets → 196 bits.
    pub fn encode_block(t: &Tsbk) -> Vec<u8> {
        const NEXT: [[u8; 4]; 4] = [[0x2, 0xc, 0x1, 0xf], [0xe, 0x0, 0xd, 0x3], [0x9, 0x7, 0xa, 0x4], [0x5, 0xb, 0x6, 0x8]];
        let mut dibits: Vec<usize> = (0..48).map(|d| ((t[d / 4] >> (6 - (d % 4) * 2)) & 3) as usize).collect();
        dibits.push(0);
        let mut seq = Vec::with_capacity(196);
        let mut state = 0;
        for &d in &dibits {
            let cw = NEXT[state][d];
            seq.extend((0..4).map(|k| (cw >> (3 - k)) & 1));
            state = d;
        }
        // trellis_viterbi reads step d's four bits at TB[4d..4d+4].
        let mut bits = vec![0u8; 196];
        let mut k = 0;
        for g in 0..12 {
            for base in [0usize, 52, 100, 148] {
                for e in 0..4 {
                    bits[base + 4 * g + e] = seq[k];
                    k += 1;
                }
            }
        }
        for e in 0..4 {
            bits[48 + e] = seq[k];
            k += 1;
        }
        bits
    }

    fn with_crc16(mut t: Tsbk) -> Tsbk {
        let c = crate::bits::crc_ccitt_bytes(&t[..10]);
        t[10..12].copy_from_slice(&c.to_be_bytes());
        t
    }

    /// A PDU frame's bits (status symbols removed) carrying `header` and
    /// `body` (padded to whole blocks, CRC-32 appended).
    pub fn pdu_bits(header: Tsbk, body: &[u8]) -> Vec<u8> {
        let mut data = body.to_vec();
        let blocks = (data.len() + 4).div_ceil(12);
        data.resize(blocks * 12 - 4, 0);
        data.extend(crc32(&data).to_be_bytes());
        let mut h = header;
        h[6] = 0x80 | blocks as u8;
        let mut bits = vec![0u8; 112];
        bits.extend(encode_block(&with_crc16(h)));
        for c in data.chunks(12) {
            bits.extend(encode_block(c.try_into().unwrap()));
        }
        bits
    }

    pub fn ambtc_header(opcode: u8, addr: [u8; 3], tail: [u8; 2]) -> Tsbk {
        let mut h = [0u8; 12];
        h[0] = 0x20 | AMBTC; // outbound
        h[1] = 0xc0 | SAP_TRUNKING;
        h[3..6].copy_from_slice(&addr);
        h[7] = opcode;
        h[8..10].copy_from_slice(&tail);
        h
    }

    #[test]
    fn the_encoder_is_the_decoders_inverse() {
        let t = with_crc16([0x3b, 0, 1, 2, 3, 4, 5, 6, 7, 8, 0, 0]);
        assert_eq!(trellis_viterbi(&encode_block(&t), None), t);
    }

    #[test]
    fn crc32_matches_the_reference() {
        // This is CRC-32/CKSUM (cksum appends the length; the check value doesn't).
        assert_eq!(crc32(b"123456789"), 0x765e_7680);
    }

    #[test]
    fn a_bad_block_on_one_receiver_is_taken_from_another() {
        let h = ambtc_header(0x3a, [0x12, 0x03, 0xa9], [0, 0]);
        let good = pdu_bits(h, &[1, 7, 0x12, 0x34, 0x56, 0x78, 0x70, 0]);
        let mut bad = good.clone();
        for i in (112 + 196..112 + 392).step_by(5) {
            bad[i] ^= 1;
        }
        // The header from the damaged copy, the data block from the clean one.
        let mut mixed = good.clone();
        mixed[112 + 196..].copy_from_slice(&bad[112 + 196..]);
        assert_eq!(decode([(&mixed[..], None)].into_iter()), Pdu::Lost);
        let Pdu::Mbt(m) = decode([(&mixed[..], None), (&good[..], None)].into_iter()) else { panic!("no MBT") };
        assert_eq!(&m.data[..2], &[1, 7]);
    }

    #[test]
    fn a_data_packet_is_not_trunking() {
        let mut h = ambtc_header(0, [0; 3], [0, 0]);
        h[0] = 0x55; // unconfirmed data
        h[1] = 0x80; // SAP 0
        assert!(matches!(decode(std::iter::once((&pdu_bits(h, &[0; 8])[..], None))), Pdu::Other(_)));
    }
}
