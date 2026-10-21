//! Phase 2 MAC PDUs and the messages in them, as TIA-102.BBAC §8 (and
//! BBAC-1) lay them out: the PDU header (opcode, offset), MAC_PTT and
//! MAC_END_PTT's fixed contents, and the B1/B2/MCO messages of the others.
//! Message fields are read by octet as the figures number them (the MCO
//! octet first).

use super::osp::{CallSign, ServiceOptions, MFID_MOTOROLA};

/// PDU opcodes (§8.4.1).
pub const MAC_SIGNAL: u8 = 0;
pub const MAC_PTT: u8 = 1;
pub const MAC_END_PTT: u8 = 2;
pub const MAC_IDLE: u8 = 3;
pub const MAC_ACTIVE: u8 = 4;
pub const MAC_HANGTIME: u8 = 6;

/// One MAC PDU (CRC checked): its header's opcode and offset, and its octets.
#[derive(Clone, Debug)]
pub struct MacPdu {
    pub opcode: u8,
    pub offset: u8,
    pub bytes: Vec<u8>,
}

impl MacPdu {
    pub fn new(bytes: Vec<u8>) -> Self {
        MacPdu { opcode: bytes[0] >> 5 & 7, offset: bytes[0] >> 2 & 7, bytes }
    }

    /// Where the slot's next voice starts (BBAC-1 §8.4.2, Table 8-7): the
    /// first 4V is in the (offset + 1)th non-SACCH burst after this one.
    /// None: no voice framing, or it isn't known (offsets 6 and 7).
    /// `next`: the position (0..5 in the superframe) of the slot's next
    /// non-SACCH burst.
    pub fn first_4v(&self, next: usize) -> Option<usize> {
        (self.offset <= 5).then(|| (next + self.offset as usize) % 5)
    }

    /// The messages a MAC_SIGNAL / IDLE / ACTIVE / HANGTIME PDU carries.
    pub fn messages(&self) -> Vec<MacMsg<'_>> {
        match self.opcode {
            MAC_SIGNAL | MAC_IDLE | MAC_ACTIVE | MAC_HANGTIME => messages(&self.bytes),
            _ => Vec::new(),
        }
    }
}

/// MAC_PTT (§8.2.1, Figures 8-4..8-6): the transmission's encryption sync
/// (its first superframe's MI) and who is talking to whom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacPtt {
    pub mi: [u8; 9],
    pub algid: u8,
    pub keyid: u16,
    pub source: u32,
    pub group: u32,
}

/// MAC_END_PTT (§8.2.2, Figures 8-7..8-9): sent unscrambled, with the
/// system's color code (its NAC, §8.4.8) to tell a co-channel system's apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacEndPtt {
    pub color_code: u16,
    pub source: u32,
    pub group: u32,
}

fn u24(b: &[u8], i: usize) -> u32 {
    (b[i] as u32) << 16 | (b[i + 1] as u32) << 8 | b[i + 2] as u32
}
fn u16_at(b: &[u8], i: usize) -> u16 {
    (b[i] as u16) << 8 | b[i + 1] as u16
}

pub fn parse_mac_ptt(b: &[u8]) -> MacPtt {
    MacPtt { mi: b[1..10].try_into().unwrap(), algid: b[10], keyid: u16_at(b, 11), source: u24(b, 13), group: u16_at(b, 16) as u32 }
}

pub fn parse_mac_end_ptt(b: &[u8]) -> MacEndPtt {
    MacEndPtt { color_code: u16_at(b, 1) & 0xfff, source: u24(b, 13), group: u16_at(b, 16) as u32 }
}

// ── Messages ─────────────────────────────────────────────────────────────────

/// Length of each message by opcode (B1 B2 MCO), when the opcode alone
/// says (Table 8-2 and BBAC-1's; op25 p25p2_tdma.cc mac_msg_len).
#[rustfmt::skip]
const MSG_LEN: [u8; 256] = [
     0,  7,  8,  7,  0, 16,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     0, 14, 15,  0,  0, 15,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     5,  7,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     9,  7,  9,  0,  9,  8,  9,  0, 10, 10,  9,  0, 10,  0,  0,  0,
     0,  0,  0,  0,  9,  7,  0,  0, 10,  0,  7,  0, 10,  8, 14,  7,
     9,  9,  0,  0,  9,  0,  0,  9, 10,  0,  7, 10, 10,  7,  0,  9,
     9, 29,  9,  9,  9,  9, 10, 13,  9,  9,  9, 11,  9,  9,  0,  0,
     8,  0,  0,  7, 11,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    16,  0,  0, 11, 13, 11, 11, 11, 10,  0,  0,  0,  0,  0,  0,  0,
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    11,  0,  0,  8, 15, 12, 15, 32, 12, 12,  0, 27, 14, 29, 29, 32,
     0,  0,  0,  0,  0,  0,  9,  0, 14, 29, 11, 27, 14,  0, 40, 11,
    28,  0,  0, 14, 17, 14,  0,  0, 16,  8, 11,  0, 13, 19,  0,  0,
     0,  0, 16, 14,  0,  0, 12,  0, 22,  0, 11, 13, 11,  0, 15,  0,
];

/// One MAC message: opcode (B1 B2 MCO), MFID (manufacturers' messages
/// only; else 0), octets (the opcode's first).
pub struct MacMsg<'a> {
    pub op: u8,
    pub mfid: u8,
    pub bytes: &'a [u8],
}

/// The messages in a MAC PDU's octets: from octet 1, each one's length from
/// its opcode, its length octet, or the table (BBAC-1 §8.3); an unknown
/// length, or the null message, ends the walk.
pub fn messages(pdu: &[u8]) -> Vec<MacMsg<'_>> {
    let mut out = Vec::new();
    let mut p = 1;
    while p < pdu.len() {
        let op = pdu[p];
        let at = |i: usize| pdu.get(p + i).copied().unwrap_or(0) as usize;
        let (mfid, len) = match op {
            // The null message fills the rest.
            0x00 => break,
            0x08 => (0, at(1) & 0x3f),
            0x11 => (0, ((at(1) & 3) + 1) * 2 + 2),
            0x12 => (0, ((at(1) & 3) + 1) * 3 + 2),
            // Motorola's two first-release messages have no length octet (its
            // BSI receive guide §1.6): the abbreviated supergroup voice
            // channel user, and the supergroup voice channel update.
            0x80 if at(1) == MFID_MOTOROLA as usize => (MFID_MOTOROLA, 8),
            0x83 if at(1) == MFID_MOTOROLA as usize => (MFID_MOTOROLA, 7),
            // A manufacturer's (B1 B2 = %10): its MFID, then the length.
            _ if op >> 6 == 2 => (at(1) as u8, at(2) & 0x3f),
            _ => (0, MSG_LEN[op as usize] as usize),
        };
        if len == 0 || p + len > pdu.len() {
            break;
        }
        out.push(MacMsg { op, mfid, bytes: &pdu[p..p + len] });
        p += len;
    }
    out
}

/// What a message says, for the messages a voice channel's tracker reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mac {
    /// GROUP VOICE CHANNEL USER (§8.3.1.2, MCO $01 / extended $21): the
    /// call's options, group and talker (its WUID; the extended form adds
    /// its SUID, whose ID stands in when it has no WUID here).
    GroupVoiceUser { opts: ServiceOptions, group: u16, source: u32 },
    /// UNIT TO UNIT VOICE CHANNEL USER (§8.3.1.3, $02 / $22).
    UnitVoiceUser { opts: ServiceOptions, target: u32, source: u32 },
    /// TELEPHONE INTERCONNECT VOICE CHANNEL USER (§8.3.1.4, $03).
    PhoneVoiceUser { opts: ServiceOptions, timer_ds: u16, unit: u32 },
    /// Motorola's supergroup voice channel user (abbreviated $80, extended
    /// $A0; the layouts op25 and SDRTrunk read).
    PatchVoiceUser { sg: u16, source: u32 },
    /// NETWORK STATUS BROADCAST (§8.3.1.25, $7B / $FB): sent unscrambled
    /// (BBAC-1 §7.2.5), with what the scrambling is seeded from.
    NetworkStatus { wacn: u32, sys_id: u16, color_code: u16 },
    /// Motorola's MAC_MOT_BSI ($85; its BSI receive guide §1.7.3): the
    /// channel's FCC call sign, in the SACCH (and a TDMA channel's FACCH).
    BaseStationId(CallSign),
    Other,
}

impl MacMsg<'_> {
    pub fn decode(&self) -> Mac {
        let b = self.bytes;
        let unit = |wuid: u32, id: u32| if wuid != 0 { wuid } else { id };
        match (self.op, self.mfid) {
            (0x01, 0) => Mac::GroupVoiceUser { opts: ServiceOptions(b[1]), group: u16_at(b, 2), source: u24(b, 4) },
            (0x21, 0) => Mac::GroupVoiceUser { opts: ServiceOptions(b[1]), group: u16_at(b, 2), source: unit(u24(b, 4), u24(b, 11)) },
            (0x02, 0) => Mac::UnitVoiceUser { opts: ServiceOptions(b[1]), target: u24(b, 2), source: u24(b, 5) },
            (0x22, 0) => Mac::UnitVoiceUser { opts: ServiceOptions(b[1]), target: u24(b, 2), source: unit(u24(b, 5), u24(b, 12)) },
            (0x03, 0) => Mac::PhoneVoiceUser { opts: ServiceOptions(b[1]), timer_ds: u16_at(b, 2), unit: u24(b, 4) },
            (0x80, MFID_MOTOROLA) => Mac::PatchVoiceUser { sg: u16_at(b, 3), source: u24(b, 5) },
            (0xa0, MFID_MOTOROLA) if b.len() >= 9 => Mac::PatchVoiceUser { sg: u16_at(b, 4), source: u24(b, 6) },
            (0x85, MFID_MOTOROLA) if b.len() >= 9 => Mac::BaseStationId(CallSign::from_octets(&b[3..9])),
            (0x7b, 0) => {
                let v = (u24(b, 2) as u64) << 8 | b[5] as u64;
                Mac::NetworkStatus { wacn: (v >> 12) as u32, sys_id: (v & 0xfff) as u16, color_code: u16_at(b, 9) & 0xfff }
            }
            (0xfb, 0) => {
                let v = (u24(b, 2) as u64) << 8 | b[5] as u64;
                Mac::NetworkStatus { wacn: (v >> 12) as u32, sys_id: (v & 0xfff) as u16, color_code: u16_at(b, 11) & 0xfff }
            }
            _ => Mac::Other,
        }
    }

    /// The talker a voice channel user message names.
    pub fn talker(&self) -> Option<u32> {
        match self.decode() {
            Mac::GroupVoiceUser { source, .. } | Mac::UnitVoiceUser { source, .. } | Mac::PatchVoiceUser { source, .. } => Some(source),
            Mac::PhoneVoiceUser { unit, .. } => Some(unit),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(op: u8, b: &[u8]) -> Mac {
        let mut v = vec![op];
        v.extend_from_slice(b);
        MacMsg { op, mfid: if op >> 6 == 2 { b[0] } else { 0 }, bytes: &v }.decode()
    }

    // Each message as its figure lays it out (octet 1: B1 B2 MCO).

    /// Figures 8-13, 8-14: the extended form's talker is its WUID; its SUID's ID only without one.
    #[test]
    fn group_voice_user() {
        assert_eq!(msg(0x01, &[0x80, 0x01, 0x01, 0x12, 0x34, 0x56]), Mac::GroupVoiceUser { opts: ServiceOptions(0x80), group: 0x0101, source: 0x123456 });
        let ext = [0x00, 0x01, 0x01, 0x12, 0x34, 0x56, 0xbe, 0xe0, 0x03, 0xa9, 0x65, 0x43, 0x21];
        assert_eq!(msg(0x21, &ext), Mac::GroupVoiceUser { opts: ServiceOptions(0), group: 0x0101, source: 0x123456 });
        let mut no_wuid = ext;
        no_wuid[3..6].fill(0);
        assert_eq!(msg(0x21, &no_wuid), Mac::GroupVoiceUser { opts: ServiceOptions(0), group: 0x0101, source: 0x654321 });
    }

    /// Figures 8-15, 8-16, 8-17.
    #[test]
    fn unit_and_phone_voice_users() {
        assert_eq!(msg(0x02, &[0x40, 0x04, 0x05, 0x06, 0x01, 0x02, 0x03]), Mac::UnitVoiceUser { opts: ServiceOptions(0x40), target: 0x040506, source: 0x010203 });
        assert_eq!(msg(0x03, &[0x04, 0x02, 0x58, 0x0a, 0x0b, 0x0c]), Mac::PhoneVoiceUser { opts: ServiceOptions(0x04), timer_ds: 600, unit: 0x0a0b0c });
    }

    /// Figures 8-47, 8-48: WACN, System ID, color code.
    #[test]
    fn network_status() {
        let abbr = [0x12, 0xbe, 0xe0, 0x03, 0xa9, 0x10, 0x01, 0x70, 0x02, 0x93];
        assert_eq!(msg(0x7b, &abbr), Mac::NetworkStatus { wacn: 0xbee00, sys_id: 0x3a9, color_code: 0x293 });
        let ext = [0x12, 0xbe, 0xe0, 0x03, 0xa9, 0x10, 0x01, 0x20, 0x01, 0x70, 0x02, 0x93];
        assert_eq!(msg(0xfb, &ext), Mac::NetworkStatus { wacn: 0xbee00, sys_id: 0x3a9, color_code: 0x293 });
    }

    /// Figure 8-8: the color code in octets 1-2's low 12 bits.
    #[test]
    fn end_ptt() {
        let mut b = [0u8; 18];
        b[0] = MAC_END_PTT << 5 | 7 << 2;
        b[1..3].copy_from_slice(&[0x02, 0x93]);
        b[13..16].copy_from_slice(&[0x12, 0x34, 0x56]);
        b[16..18].copy_from_slice(&[0x08, 0x9f]);
        let p = MacPdu::new(b.to_vec());
        assert_eq!((p.opcode, p.offset), (MAC_END_PTT, 7));
        assert_eq!(parse_mac_end_ptt(&p.bytes), MacEndPtt { color_code: 0x293, source: 0x123456, group: 0x089f });
    }

    /// BBAC-1 Table 8-7: offsets 0..5 count non-SACCH bursts on from the next; 6 and 7 aren't positions.
    #[test]
    fn offset() {
        let pdu = |offset: u8| MacPdu::new(vec![MAC_PTT << 5 | offset << 2]);
        // A SACCH's next non-SACCH burst is the superframe's first.
        assert_eq!(pdu(0).first_4v(0), Some(0));
        assert_eq!(pdu(1).first_4v(0), Some(1));
        // A FACCH in position 2: its next is 3; offset 3 is three on from it.
        assert_eq!(pdu(3).first_4v(3), Some(1));
        assert_eq!(pdu(5).first_4v(0), Some(0));
        assert_eq!((pdu(6).first_4v(0), pdu(7).first_4v(0)), (None, None));
    }

    /// The walk: lengths by opcode, by length octet, a manufacturer's; the null message ends it.
    #[test]
    fn walk() {
        let pdu = [MAC_ACTIVE << 5, 0x01, 0, 0, 0x65, 0, 0, 1, 0x90, 0x90, 4, 0xaa, 0x00, 0, 0];
        let ms = messages(&pdu);
        let got: Vec<(u8, u8, usize)> = ms.iter().map(|m| (m.op, m.mfid, m.bytes.len())).collect();
        assert_eq!(got, [(0x01, 0, 7), (0x90, 0x90, 4)]);
        // Motorola's $83 has no length octet: 7 long, and the walk goes on past it.
        let pdu = [MAC_ACTIVE << 5, 0x83, 0x90, 0x02, 0x01, 0x07, 0xd1, 0x00, 0x85, 0x90, 0x09, 0, 0, 0, 0, 0, 0];
        let got: Vec<(u8, usize)> = messages(&pdu).iter().map(|m| (m.op, m.bytes.len())).collect();
        assert_eq!(got, [(0x83, 7), (0x85, 9)]);
    }

    /// The BSI guide's Figure for MAC_MOT_BSI: length 9, eight characters from octet 3.
    #[test]
    fn base_station_id() {
        // "WPXT459": W=$2C P=$25 X=$2D T=$29 4=$09 5=$0A 9=$0E, then none.
        let chars = [0x2cu64, 0x25, 0x2d, 0x29, 0x09, 0x0a, 0x0e, 0x00];
        let v = chars.iter().fold(0u64, |v, &c| v << 6 | c);
        let mut b = vec![0x90, 0x09];
        b.extend_from_slice(&v.to_be_bytes()[2..]);
        let Mac::BaseStationId(c) = msg(0x85, &b) else { panic!("{:?}", msg(0x85, &b)) };
        assert_eq!(c.text(), "WPXT459");
    }
}
