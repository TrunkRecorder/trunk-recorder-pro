//! PDU frames → Alternate Multiple Block Trunking (AMBTC) messages: the
//! control channel's multi-block form of a TSBK, which carries both channels
//! of an explicit grant or broadcast and full (WACN, System ID, ID) unit
//! addresses. Some sites send their RFSS and network status only this way,
//! and without it their WACN and System ID are never heard.
//!
//! A PDU is a header block and the number of data blocks it names, each 196
//! bits of the same interleave and 1/2-rate trellis as a TSBK (a confirmed
//! packet's blocks are 3/4-rate, but trunking never is). The header ends in a
//! CRC-CCITT like a TSBK's; the data blocks end, together, in a CRC-32.
//! Field positions are SDRTrunk's (`pdu/ambtc/osp`), which agree with op25's
//! and Trunk Recorder's (`decode_mbt_data`) where those read a field.
//!
//! Opcodes without a TSBK form here are counted, not acted on: data and
//! telephone grants, status and message updates, roaming, authentication,
//! and the uncoordinated-band-plan adjacent status (0x3e). IDEN_UP_TDMA
//! (0x33) names the WACN and System ID its band plan is for, and a site may
//! send its neighbouring systems' too (CLMRN, ~8 a second): only one for the
//! site's own system, once its network status has said which that is, is
//! passed on.

use super::tsbk::{trellis_viterbi, tsbk_ok, Tsbk};

/// Header format: unconfirmed alternate multiple block trunking.
const AMBTC: u8 = 0x17;
/// Header SAP: trunking control.
const SAP_TRUNKING: u8 = 0x3d;

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

    /// The (WACN, System ID) an IDEN_UP_TDMA's band plan is for.
    pub fn band_plan_system(&self) -> Option<(u32, u32)> {
        let h = &self.header;
        (self.opcode() == 0x33 && self.mfid() == 0).then(|| ((h[4] as u32) << 12 | (h[5] as u32) << 4 | (h[8] as u32) >> 4, (h[8] as u32 & 0xf) << 8 | h[9] as u32))
    }

    /// The same message as a single TSBK (CRC included), for the opcodes
    /// the TSBK parser acts on: the outbound channel stands for the pair, a
    /// unit's ID for its full (WACN, System ID, ID) address. `own`: the
    /// site's (WACN, System ID), when known, which a band plan must be for.
    pub fn to_tsbk(&self, own: Option<(u32, u32)>) -> Option<Tsbk> {
        let (h, d) = (&self.header, &self.data);
        // Header: [3..6] the address (source or target), [8] service options.
        let need = |n: usize| (d.len() >= n).then_some(());
        let mut t = [0u8; 12];
        t[0] = 0x80 | self.opcode();
        t[1] = self.mfid();
        match (self.opcode(), self.mfid()) {
            // Group voice channel grant: options, channel, group, source.
            (0x00, 0) => {
                need(8)?;
                t[2] = h[8];
                t[3..5].copy_from_slice(&d[2..4]);
                t[5..7].copy_from_slice(&d[6..8]);
                t[7..10].copy_from_slice(&h[3..6]);
            }
            // Motorola regroup (patch) channel grant: options, channel, supergroup, source.
            (0x02, 0x90) => {
                need(6)?;
                t[2] = h[8];
                t[3..5].copy_from_slice(&d[0..2]);
                t[5..7].copy_from_slice(&d[4..6]);
                t[7..10].copy_from_slice(&h[3..6]);
            }
            // Unit-to-unit grant and update: channel, target, source.
            (0x04 | 0x06, 0) => {
                need(12)?;
                t[2..4].copy_from_slice(&d[10..12]);
                t[4..7].copy_from_slice(&d[7..10]);
                t[7..10].copy_from_slice(&d[4..7]);
            }
            // Unit-to-unit answer request: options, target, source.
            (0x05, 0) => {
                need(8)?;
                t[2] = h[8];
                t[4..7].copy_from_slice(&h[3..6]);
                t[7..10].copy_from_slice(&d[5..8]);
            }
            // Call alert: target, source.
            (0x1f, 0) => {
                need(5)?;
                t[4..7].copy_from_slice(&h[3..6]);
                t[7..10].copy_from_slice(&d[2..5]);
            }
            // Group affiliation response (two blocks): LG and GAV, announcement group, group, target.
            (0x28, 0) => {
                need(9)?;
                t[2] = d[8];
                t[3..7].copy_from_slice(&d[4..8]);
                t[7..10].copy_from_slice(&h[3..6]);
            }
            // Unit registration response: RV and System ID, the unit's ID, its address.
            (0x2c, 0) => {
                need(6)?;
                t[2] = (d[5] & 3) << 4 | d[0] & 0x0f;
                t[3] = d[1];
                t[4..7].copy_from_slice(&d[2..5]);
                t[7..10].copy_from_slice(&h[3..6]);
            }
            // Band plan (TDMA): identifier and type, offset and spacing, base.
            (0x33, 0) => {
                need(7)?;
                if own.is_none() || self.band_plan_system() != own {
                    return None;
                }
                t[2] = h[3];
                t[3..6].copy_from_slice(&d[4..7]);
                t[6..10].copy_from_slice(&d[0..4]);
            }
            // RFSS status: LRA, System ID, RFSS, site, channel.
            (0x3a, 0) => {
                need(4)?;
                t[2] = h[3];
                t[3..5].copy_from_slice(&h[4..6]);
                t[5..7].copy_from_slice(&d[0..2]);
                t[7..9].copy_from_slice(&d[2..4]);
            }
            // Network status: LRA, WACN, System ID, channel.
            (0x3b, 0) => {
                need(5)?;
                t[2] = h[3];
                t[3..5].copy_from_slice(&d[0..2]);
                t[5] = d[2] & 0xf0 | h[4] & 0x0f;
                t[6] = h[5];
                t[7..9].copy_from_slice(&d[3..5]);
            }
            // Adjacent site: LRA, System ID, RFSS, site, channel.
            (0x3c, 0) => {
                need(2)?;
                t[2] = h[3];
                t[3..5].copy_from_slice(&h[4..6]);
                t[5..7].copy_from_slice(&h[8..10]);
                t[7..9].copy_from_slice(&d[0..2]);
            }
            _ => return None,
        }
        let crc = crate::bits::crc_ccitt_bytes(&t[..10]);
        t[10..12].copy_from_slice(&crc.to_be_bytes());
        Some(t)
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
    if h[0] & 0x1f != AMBTC || h[1] & 0x3f != SAP_TRUNKING {
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
    use crate::p25::tsbk::{tsbk_last, tsbk_opcode};

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
    fn network_status_becomes_its_tsbk() {
        // WACN BEE00, System ID 3A9, LRA 0x12, channel 0x1234 / 0x5678, SSC 0x70.
        let h = ambtc_header(0x3b, [0x12, 0x03, 0xa9], [0, 0]);
        let bits = pdu_bits(h, &[0xbe, 0xe0, 0x00, 0x12, 0x34, 0x56, 0x78, 0x70]);
        let Pdu::Mbt(m) = decode(std::iter::once((&bits[..], None))) else { panic!("no MBT") };
        let t = m.to_tsbk(None).expect("has a TSBK form");
        assert!(tsbk_ok(&t) && tsbk_last(&t));
        assert_eq!(tsbk_opcode(&t), 0x3b);
        let v = t.iter().fold(0u128, |a, &b| (a << 8) | b as u128);
        let b = |shift: u32, mask: u64| ((v >> shift) as u64 & mask) as u32;
        assert_eq!((b(52, 0xfffff), b(40, 0xfff), b(24, 0xffff)), (0xbee00, 0x3a9, 0x1234));
    }

    #[test]
    fn a_grant_keeps_its_options_from_the_header() {
        // Source 0x123456, emergency + encrypted; channel 0x1234, group 0x0101.
        let h = ambtc_header(0x00, [0x12, 0x34, 0x56], [0xc0, 0]);
        let bits = pdu_bits(h, &[0, 0, 0x12, 0x34, 0x56, 0x78, 0x01, 0x01]);
        let Pdu::Mbt(m) = decode(std::iter::once((&bits[..], None))) else { panic!("no MBT") };
        let t = m.to_tsbk(None).expect("has a TSBK form");
        let v = t.iter().fold(0u128, |a, &b| (a << 8) | b as u128);
        let b = |shift: u32, mask: u64| ((v >> shift) as u64 & mask) as u32;
        assert_eq!((b(72, 0xff), b(56, 0xffff), b(40, 0xffff), b(16, 0xffffff)), (0xc0, 0x1234, 0x0101, 0x123456));
    }

    /// Each MBT the TSBK parser should hear, as the parser reads it.
    #[test]
    fn each_converted_mbt_parses_as_its_tsbk() {
        use crate::trunk::message::{MessageType as K, TsbkParser};
        let parse = |opcode: u8, mfid: u8, addr: [u8; 3], tail: [u8; 2], body: &[u8]| {
            let mut h = ambtc_header(opcode, addr, tail);
            h[2] = mfid;
            let Pdu::Mbt(m) = decode(std::iter::once((&pdu_bits(h, body)[..], None))) else { panic!("no MBT 0x{opcode:02x}") };
            let t = m.to_tsbk(None).unwrap_or_else(|| panic!("0x{opcode:02x} has no TSBK form"));
            assert!(tsbk_ok(&t));
            let mut p = TsbkParser::default();
            let mut msgs = p.parse(&t, 0x332, 0.0);
            msgs.pop().unwrap()
        };
        let unit = [0x0a, 0x0b, 0x0c]; // 0x0a0b0c
        // Motorola patch grant: supergroup 0x0204 on channel 0x1001, from the unit.
        let m = parse(0x02, 0x90, unit, [0x80, 0], &[0x10, 0x01, 0x10, 0x01, 0x02, 0x04, 0, 0]);
        assert_eq!((m.kind, m.talkgroup, m.source, m.emergency), (K::Grant, 0x0204, 0x0a0b0c, true));
        // Unit-to-unit grant and update: source 0x010203 → target 0x040506 on channel 0x1001.
        let uu = [0xbe, 0xe0, 0x03, 0xa9, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x10, 0x01, 0x10, 0x01, 0, 0];
        for (op, kind) in [(0x04, K::UuVGrant), (0x06, K::UuVUpdate)] {
            let m = parse(op, 0, [0; 3], [0, 0], &uu);
            assert_eq!((m.kind, m.source, m.talkgroup), (kind, 0x010203, 0x040506), "0x{op:02x}");
        }
        // Answer request: from 0x010203 to the unit.
        let m = parse(0x05, 0, unit, [0, 0], &[0, 0xbe, 0xe0, 0x03, 0xa9, 0x01, 0x02, 0x03]);
        assert_eq!((m.kind, m.source, m.talkgroup), (K::UuAnsReq, 0x010203, 0x0a0b0c));
        // Call alert: from 0x010203 to the unit.
        let m = parse(0x1f, 0, unit, [0, 0], &[0xe0, 0x3a, 0x01, 0x02, 0x03, 0, 0, 0]);
        assert_eq!((m.kind, m.source, m.talkgroup), (K::CallAlert, 0x010203, 0x0a0b0c));
        // Affiliation response: the unit to group 0x0101.
        let m = parse(0x28, 0, unit, [0, 0], &[0xe0, 0x3a, 0, 0, 0, 0, 0x01, 0x01, 0]);
        assert_eq!((m.kind, m.source, m.talkgroup), (K::Affiliation, 0x0a0b0c, 0x0101));
        // Refused (GAV 3): not an affiliation.
        let m = parse(0x28, 0, unit, [0, 0], &[0xe0, 0x3a, 0, 0, 0, 0, 0x01, 0x01, 3]);
        assert_ne!(m.kind, K::Affiliation);
        // Registration response: unit 0x010203.
        let m = parse(0x2c, 0, unit, [0, 0], &[0xe0, 0x3a, 0x01, 0x02, 0x03, 0, 0, 0]);
        assert_eq!((m.kind, m.source), (K::Registration, 0x010203));
        // Denied (RV 2): not a registration.
        let m = parse(0x2c, 0, unit, [0, 0], &[0xe0, 0x3a, 0x01, 0x02, 0x03, 2, 0, 0]);
        assert_ne!(m.kind, K::Registration);
        // RFSS status: System ID 3A9, RFSS 1, site 7.
        let m = parse(0x3a, 0, [0x12, 0x03, 0xa9], [0, 0], &[1, 7, 0x10, 0x01, 0, 0, 0x70, 0]);
        assert_eq!((m.kind, m.sys_id, m.rfss, m.site), (K::SysId, 0x3a9, 1, 7));
        // Adjacent site 1-7 of System ID 3A9.
        let m = parse(0x3c, 0, [0x12, 0x03, 0xa9], [1, 7], &[0x10, 0x01, 0x10, 0x01, 0, 0x70, 0, 0]);
        assert_eq!((m.kind, m.sys_id, m.rfss, m.site), (K::Adjacent, 0x3a9, 1, 7));
    }

    fn from_hex(hex: &str) -> Mbt {
        let b: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        let m = Mbt { header: b[..12].try_into().unwrap(), data: b[12..].to_vec() };
        assert!(tsbk_ok(&m.header));
        m
    }

    /// A band plan (CLMRN-I's sample, for WACN BEE00 System ID 9EF) is
    /// passed on only on a site of that system; a site on a band plan of its
    /// own (0x3e) not at all.
    #[test]
    fn only_the_sites_own_band_plan_is_passed_on() {
        use crate::trunk::message::TsbkParser;
        let m = from_hex("37fd0011bee0813309ef4afd09157562cb003200");
        assert_eq!(m.band_plan_system(), Some((0xbee00, 0x9ef)));
        assert_eq!(m.to_tsbk(None), None, "system not known yet");
        assert_eq!(m.to_tsbk(Some((0xbee00, 0x3a9))), None, "a neighbour's");
        let t = m.to_tsbk(Some((0xbee00, 0x9ef))).expect("its own");
        assert!(tsbk_ok(&t));
        let mut p = TsbkParser::default();
        let msg = p.parse(&t, 0x332, 0.0).pop().unwrap();
        assert!(msg.meta.contains("IDEN_UP_TDMA 1: base 762.00625 MHz, step 6250 Hz"), "{}", msg.meta);
        assert_eq!(from_hex("37fd00011097813e0101b14700a8ffff70000010").to_tsbk(Some((0xbee00, 0x9ef))), None);
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
