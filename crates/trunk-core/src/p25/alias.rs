//! Talker aliases: the name a radio is programmed with ("E12 CAPT"), sent
//! over the air alongside its unit ID during voice. Trunk Recorder's
//! unit_tags_ota.cc (decoders; Motorola de-obfuscation © 2024 Ilya Smirnov /
//! @ilyacodes) and op25 p25p1_fdma.cc / p25p2_tdma.cc (collecting the
//! fragments):
//!
//! - Motorola, Phase 1: link control LCO 0x15 (header) then 0x17 (data
//!   blocks), MFID 0x90. The payload carries the radio ID, the talkgroup,
//!   WACN / System ID and an obfuscated alias under a CRC-16/GSM.
//! - Motorola, Phase 2: MAC messages 0x91 (header) / 0x95 (data), MFID 0x90.
//! - Harris, Phase 1: LCO 0x32 + 0x33 (and a repeat, 0x34 + 0x35), MFID
//!   0xA4: 7 plain ASCII characters each. Names no radio: it is whoever is
//!   talking.
//! - Harris, Phase 2: MAC message 0xA8, MFID 0xA4, plain ASCII.

use crate::bits::crc_ccitt_bytes;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub unit: u32,
    pub alias: String,
    /// Which decoder ("MotoP25_FDMA", "HarrisP25_TDMA", …) — Trunk Recorder's names.
    pub source: &'static str,
    /// The talkgroup it was sent on, when the payload says.
    pub talkgroup: Option<u32>,
}

const MOTOROLA: u8 = 0x90;
const HARRIS: u8 = 0xa4;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect()
}

/// The alias length (UTF-16 characters) from the first byte of its code.
fn motorola_len(code: &str) -> Option<usize> {
    const CODES: [&str; 14] = ["94", "32", "95", "9d", "1b", "77", "b5", "6e", "24", "61", "2d", "7d", "83", "29"];
    CODES.iter().position(|&c| c == code).map(|i| i + 1)
}

/// The payload as hex — (talkgroup, WACN, System ID, radio, alias code) at
/// the offsets of unit_tags_ota.cc — then CRC check and de-obfuscation.
fn decode_motorola(payload: &str, at: usize, source: &'static str) -> Option<Alias> {
    let field = |from: usize, len: usize| payload.get(from..from + len);
    let tg = u32::from_str_radix(field(0, 4)?, 16).ok()?;
    let (wacn, sys, radio) = (field(at, 5)?, field(at + 5, 3)?, field(at + 8, 6)?);
    let len = motorola_len(field(at + 14, 2)?)?;
    let code = field(at + 14, len * 4)?;
    let checksum = u16::from_str_radix(field(at + 14 + len * 4, 4)?, 16).ok()?;
    let body = unhex(&format!("{wacn}{sys}{radio}{code}"))?;
    if crc_ccitt_bytes(&body) != checksum || body.len() < 8 {
        return None;
    }
    let unit = u32::from_str_radix(radio, 16).ok()?;
    let alias = motorola_deobfuscate(&body[7..]);
    let alias = alias.trim_end();
    (!alias.is_empty()).then(|| Alias { unit, alias: alias.to_string(), source, talkgroup: Some(tg) })
}

/// Motorola's alias obfuscation, undone (unit_tags_ota.cc decode_mot_alias).
pub fn motorola_deobfuscate(encoded: &[u8]) -> String {
    let mut acc = encoded.len() as u16;
    let mut out = Vec::with_capacity(encoded.len());
    for &e in encoded {
        let lcg = acc.wrapping_mul(293).wrapping_add(0x72e9);
        let v = SUBSTITUTION[e as usize].wrapping_sub((lcg >> 8) as u8);
        out.push(v.wrapping_mul(INVERSE_ODD[(lcg as u8 | 1) as usize >> 1]));
        acc = acc.wrapping_add(e as u16 + 1);
    }
    out.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).filter(|&c| (32..128).contains(&c)).map(|c| c as u8 as char).collect()
}

/// Printable ASCII up to a NUL, trailing spaces trimmed.
fn ascii(b: &[u8]) -> Option<String> {
    let s: String = b.iter().take_while(|&&c| c != 0).filter(|c| (0x20..=0x7e).contains(*c)).map(|&c| c as char).collect();
    let s = s.trim_end();
    (!s.is_empty()).then(|| s.to_string())
}

// ── Phase 1: link control words ──────────────────────────────────────────────

/// Collects alias fragments from one voice channel's Phase 1 link control.
#[derive(Default)]
pub struct LcAliases {
    /// Motorola: the header (0) and data blocks (1..).
    moto: [Option<[u8; 9]>; 10],
    /// Harris part A of the odd (0) and even (1) conveyance.
    harris_a: [Option<[u8; 9]>; 2],
}

impl LcAliases {
    /// A clear link control word. `talker`: the unit link control named for
    /// this transmission, which a Harris alias belongs to.
    pub fn lcw(&mut self, w: &[u8; 9], talker: Option<u32>, talkgroup: Option<u32>) -> Option<Alias> {
        // Clear, explicit MFID.
        if w[0] & 0xc0 != 0 {
            return None;
        }
        match (w[0] & 0x3f, w[1]) {
            (0x15, MOTOROLA) => {
                self.moto = Default::default();
                self.moto[0] = Some(*w);
                None
            }
            (0x17, MOTOROLA) => {
                let head = self.moto[0]?;
                let messages = (head[4] & 0xf) as usize;
                let k = (w[2] & 0xf) as usize;
                if k == 0 || k > messages || messages >= 10 || w[3] >> 4 != head[7] >> 4 {
                    return None;
                }
                self.moto[k] = Some(*w);
                (k == messages).then(|| self.motorola(messages)).flatten()
            }
            (0x32, HARRIS) | (0x34, HARRIS) => {
                self.harris_a[(w[0] & 0x3f == 0x34) as usize] = Some(*w);
                None
            }
            (0x33, HARRIS) | (0x35, HARRIS) => {
                let a = self.harris_a[(w[0] & 0x3f == 0x35) as usize].take()?;
                let text: Vec<u8> = a[2..9].iter().chain(&w[2..9]).copied().collect();
                Some(Alias { unit: talker.filter(|&u| u != 0)?, alias: ascii(&text)?, source: "HarrisP25_FDMA", talkgroup })
            }
            _ => None,
        }
    }

    /// op25 / unit_tags_ota.cc assemble_payload: header bytes 2..9, then each
    /// block's bytes 3..9 less its first nibble (the block's sequence).
    fn motorola(&self, messages: usize) -> Option<Alias> {
        let mut p = hex(&self.moto[0]?[2..9]);
        for b in &self.moto[1..=messages] {
            p.push_str(&hex(&b.as_ref()?[3..9])[1..]);
        }
        decode_motorola(&p, 14, "MotoP25_FDMA")
    }
}

// ── Phase 2: MAC messages ────────────────────────────────────────────────────

/// Length of each MAC message by opcode, when the opcode alone says
/// (op25 p25p2_tdma.cc mac_msg_len).
#[rustfmt::skip]
const MAC_MSG_LEN: [u8; 256] = [
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

/// One MAC message: opcode, MFID (manufacturer-specific messages only; else 0), bytes.
pub struct MacMsg<'a> {
    pub op: u8,
    pub mfid: u8,
    pub bytes: &'a [u8],
}

/// The messages in a MAC PDU's payload (op25 decode_mac_msg): from byte 1,
/// each message's length from its opcode, its length field, or the table;
/// an unknown length ends the walk.
pub fn mac_messages(pdu: &[u8]) -> Vec<MacMsg<'_>> {
    let mut out = Vec::new();
    let mut p = 1;
    while p < pdu.len() {
        let op = pdu[p];
        let at = |i: usize| pdu.get(p + i).copied().unwrap_or(0) as usize;
        let (mfid, len) = match op {
            0x00 => (0, pdu.len() - p),
            0x08 => (0, at(1) & 0x3f),
            0x11 => (0, ((at(1) & 3) + 1) * 2 + 2),
            0x12 => (0, ((at(1) & 3) + 1) * 3 + 2),
            _ if op >> 6 == 2 => (at(1) as u8, at(2) & 0x3f),
            _ => (0, MAC_MSG_LEN[op as usize] as usize),
        };
        if len == 0 {
            break;
        }
        out.push(MacMsg { op, mfid, bytes: &pdu[p..(p + len).min(pdu.len())] });
        p += len;
    }
    out
}

/// Collects alias fragments from one TDMA slot's MAC messages.
#[derive(Default)]
pub struct MacAliases {
    moto: [Option<Vec<u8>>; 10],
}

impl MacAliases {
    /// One MAC message. `talker`: the unit talking on this slot now.
    pub fn msg(&mut self, m: &MacMsg, talker: Option<u32>, talkgroup: Option<u32>) -> Option<Alias> {
        let b = m.bytes;
        match (m.op, m.mfid) {
            (0x91, MOTOROLA) if b.len() >= 9 => {
                self.moto = Default::default();
                self.moto[0] = Some(b.to_vec());
                None
            }
            (0x95, MOTOROLA) if b.len() >= 5 => {
                let head = self.moto[0].as_ref()?;
                let messages = head[5] as usize;
                let k = b[3] as usize;
                if k == 0 || k > messages || messages >= 10 || b[4] >> 4 != head[8] >> 4 {
                    return None;
                }
                self.moto[k] = Some(b.to_vec());
                (k == messages).then(|| self.motorola(messages)).flatten()
            }
            (0xa8, HARRIS) if b.len() > 3 => {
                Some(Alias { unit: talker.filter(|&u| u != 0)?, alias: ascii(&b[3..])?, source: "HarrisP25_TDMA", talkgroup })
            }
            _ => None,
        }
    }

    /// unit_tags_ota.cc assemble_payload_p2: header bytes 3..17, then each
    /// block's bytes 4..17 less its first nibble.
    fn motorola(&self, messages: usize) -> Option<Alias> {
        let head = self.moto[0].as_ref()?;
        let mut p = hex(head.get(3..17)?);
        for b in &self.moto[1..=messages] {
            p.push_str(&hex(b.as_ref()?.get(4..17)?)[1..]);
        }
        decode_motorola(&p, 12, "MotoP25_TDMA")
    }
}

/// A unit a MAC message names as talking: Group Voice Channel User,
/// abbreviated (0x01) or extended (0x21).
pub fn mac_talker(m: &MacMsg) -> Option<u32> {
    let b = m.bytes;
    let u24 = |i: usize| Some((*b.get(i)? as u32) << 16 | (*b.get(i + 1)? as u32) << 8 | *b.get(i + 2)? as u32);
    match m.op {
        0x01 => u24(4),
        0x21 => u24(11),
        _ => None,
    }
}

#[rustfmt::skip]
const SUBSTITUTION: [u8; 256] = [
    0xd2, 0xf6, 0xd4, 0x2b, 0x63, 0x49, 0x94, 0x5e, 0xa7, 0x5c, 0x70, 0x69, 0xf7, 0x08, 0xb1, 0x7d,
    0x38, 0xcf, 0xcc, 0xd8, 0x51, 0x8f, 0xd5, 0x93, 0x6a, 0xf3, 0xef, 0x7e, 0xfb, 0x64, 0xf4, 0x35,
    0x27, 0x07, 0x31, 0x14, 0x87, 0x98, 0x76, 0x34, 0xca, 0x92, 0x33, 0x1b, 0x4f, 0x8c, 0x09, 0x40,
    0x32, 0x36, 0x77, 0x12, 0xd3, 0xc3, 0x01, 0xab, 0x72, 0x81, 0x95, 0xc9, 0xc0, 0xe9, 0x65, 0x52,
    0x24, 0x30, 0x1c, 0xdb, 0x88, 0xe8, 0x97, 0x9d, 0x58, 0x26, 0x04, 0x39, 0xac, 0x2a, 0x9e, 0xaa,
    0x25, 0xd7, 0xce, 0xeb, 0x96, 0xf5, 0x0e, 0x8d, 0xdc, 0xa9, 0x2f, 0xdd, 0x1f, 0xea, 0x91, 0xb7,
    0xd6, 0x89, 0x8b, 0xd1, 0xb0, 0x99, 0x13, 0x7a, 0xe7, 0x9a, 0xb5, 0x86, 0xff, 0x46, 0x85, 0xb2,
    0x73, 0xda, 0xbf, 0xd0, 0x71, 0xcb, 0x4d, 0x80, 0x15, 0x67, 0x16, 0x1a, 0x20, 0x8e, 0x45, 0x3e,
    0xf2, 0x2e, 0x66, 0x90, 0x74, 0x8a, 0x6f, 0x78, 0xbb, 0x53, 0x03, 0x11, 0x68, 0xcd, 0x44, 0x17,
    0x28, 0x5f, 0x1e, 0x84, 0x75, 0x79, 0x6e, 0x9b, 0x2c, 0xbe, 0x62, 0x2d, 0xf1, 0x7c, 0xb8, 0x83,
    0xd9, 0x4e, 0x6d, 0x02, 0x61, 0x3d, 0xa8, 0x06, 0xb9, 0xf8, 0x9c, 0x37, 0x3a, 0x23, 0xc1, 0x50,
    0xed, 0x9f, 0xaf, 0x3b, 0xbd, 0x82, 0xba, 0xa0, 0xdf, 0xc2, 0x47, 0x22, 0xf0, 0xee, 0xa1, 0xfe,
    0xa2, 0x10, 0x5b, 0x48, 0x57, 0xa3, 0x05, 0x60, 0x7b, 0x0d, 0xf9, 0x6c, 0xb3, 0x56, 0x4c, 0xbc,
    0x29, 0xa4, 0x0f, 0xec, 0xb6, 0xa5, 0xa6, 0x3c, 0x7f, 0x6b, 0xb4, 0x21, 0xad, 0xae, 0xc4, 0xc8,
    0xc5, 0x5d, 0xde, 0xe0, 0x1d, 0x19, 0x4b, 0xc6, 0x0c, 0x3f, 0x5a, 0xc7, 0xe1, 0x59, 0x55, 0x54,
    0x4a, 0x43, 0x42, 0xe2, 0xe3, 0xfa, 0x00, 0xe4, 0xe5, 0x18, 0x41, 0x0b, 0x0a, 0xe6, 0xfc, 0xfd,
];

/// Multiplicative inverses mod 256 of the odd numbers 1, 3, 5, … 255.
#[rustfmt::skip]
const INVERSE_ODD: [u8; 128] = [
    0x01, 0xab, 0xcd, 0xb7, 0x39, 0xa3, 0xc5, 0xef, 0xf1, 0x1b, 0x3d, 0xa7, 0x29, 0x13, 0x35, 0xdf,
    0xe1, 0x8b, 0xad, 0x97, 0x19, 0x83, 0xa5, 0xcf, 0xd1, 0xfb, 0x1d, 0x87, 0x09, 0xf3, 0x15, 0xbf,
    0xc1, 0x6b, 0x8d, 0x77, 0xf9, 0x63, 0x85, 0xaf, 0xb1, 0xdb, 0xfd, 0x67, 0xe9, 0xd3, 0xf5, 0x9f,
    0xa1, 0x4b, 0x6d, 0x57, 0xd9, 0x43, 0x65, 0x8f, 0x91, 0xbb, 0xdd, 0x47, 0xc9, 0xb3, 0xd5, 0x7f,
    0x81, 0x2b, 0x4d, 0x37, 0xb9, 0x23, 0x45, 0x6f, 0x71, 0x9b, 0xbd, 0x27, 0xa9, 0x93, 0xb5, 0x5f,
    0x61, 0x0b, 0x2d, 0x17, 0x99, 0x03, 0x25, 0x4f, 0x51, 0x7b, 0x9d, 0x07, 0x89, 0x73, 0x95, 0x3f,
    0x41, 0xeb, 0x0d, 0xf7, 0x79, 0xe3, 0x05, 0x2f, 0x31, 0x5b, 0x7d, 0xe7, 0x69, 0x53, 0x75, 0x1f,
    0x21, 0xcb, 0xed, 0xd7, 0x59, 0xc3, 0xe5, 0x0f, 0x11, 0x3b, 0x5d, 0xc7, 0x49, 0x33, 0x55, 0xff,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The obfuscation itself (decode_mot_alias run backwards), to build
    /// over-the-air messages from an alias.
    fn obfuscate(text: &str) -> Vec<u8> {
        let plain: Vec<u8> = text.bytes().flat_map(|c| [0, c]).collect();
        let mut inv_sub = [0u8; 256];
        for (i, &s) in SUBSTITUTION.iter().enumerate() {
            inv_sub[s as usize] = i as u8;
        }
        let mut acc = plain.len() as u16;
        plain
            .iter()
            .map(|&d| {
                let lcg = acc.wrapping_mul(293).wrapping_add(0x72e9);
                let e = inv_sub[d.wrapping_mul(lcg as u8 | 1).wrapping_add((lcg >> 8) as u8) as usize];
                acc = acc.wrapping_add(e as u16 + 1);
                e
            })
            .collect()
    }

    /// The payload after the header's fixed fields: WACN, System ID, radio,
    /// alias code, CRC — as hex.
    fn payload(wacn: u32, sys: u32, radio: u32, text: &str) -> Option<String> {
        let code = obfuscate(text);
        let body = format!("{wacn:05x}{sys:03x}{radio:06x}{}", hex(&code));
        let crc = crc_ccitt_bytes(&unhex(&body).unwrap());
        // The alias's first byte must be the length code for this length.
        (motorola_len(&body[14..16]) == Some(text.len())).then(|| format!("{body}{crc:04x}"))
    }

    /// Phase 1 link control words carrying `payload`: the header holds the
    /// talkgroup, block count and sequence; each block 11 nibbles of payload.
    fn p1_words(tg: u16, seq: u8, payload: &str) -> Vec<[u8; 9]> {
        let blocks: Vec<&str> = payload.as_bytes().chunks(11).map(|c| std::str::from_utf8(c).unwrap()).collect();
        let n = blocks.len() as u8;
        let head_hex = format!("{tg:04x}{n:02x}0000{seq:x}000");
        let mut h = [0u8; 9];
        h[0] = 0x15;
        h[1] = MOTOROLA;
        h[2..9].copy_from_slice(&unhex(&head_hex).unwrap());
        let mut out = vec![h];
        for (k, b) in blocks.iter().enumerate() {
            let b = format!("{b:0<11}");
            let mut w = [0u8; 9];
            w[0] = 0x17;
            w[1] = MOTOROLA;
            w[2] = k as u8 + 1;
            w[3..9].copy_from_slice(&unhex(&format!("{seq:x}{b}")).unwrap());
            out.push(w);
        }
        out
    }

    #[test]
    fn length_codes_are_the_obfuscated_first_byte() {
        // The first byte depends on the length alone, which is how the
        // length is read before the rest is decoded.
        for len in 1..=14 {
            assert!(payload(0xbee00, 1, 1, &"X".repeat(len)).is_some(), "length {len}");
        }
    }

    #[test]
    fn motorola_phase1_over_the_air() {
        // DCFD (WACN BEE00, System 445), 859.0375 MHz, 2026-09-30 08:22:50:
        // the terminators (TDULC) after a transmission on TG 3747. Trunk
        // Recorder decoded the same: 1102841 "6531 PEAKE".
        let words = [
            "15900ea306010058e1",
            "1790015bee0044510d",
            "17900253f9616686e6",
            "179003533c91faebb6",
            "179004559675c023a6",
            "1790055c17ef81933b",
            "1790065f6600000000",
        ];
        let mut a = LcAliases::default();
        let mut got = None;
        for w in words {
            let w: [u8; 9] = unhex(w).unwrap().try_into().unwrap();
            got = a.lcw(&w, None, None).or(got);
        }
        assert_eq!(got, Some(Alias { unit: 1_102_841, alias: "6531 PEAKE".into(), source: "MotoP25_FDMA", talkgroup: Some(3747) }));
    }

    #[test]
    fn deobfuscation_inverts() {
        assert_eq!(motorola_deobfuscate(&obfuscate("ENGINE 12")), "ENGINE 12");
    }

    #[test]
    fn motorola_phase1() {
        let text = "E12 CAPT".to_string();
        let p = payload(0xbee00, 0x445, 1_118_562, &text).unwrap();
        let mut a = LcAliases::default();
        let words = p1_words(2207, 3, &p);
        let mut got = None;
        for w in &words {
            got = a.lcw(w, None, None).or(got);
        }
        assert_eq!(got, Some(Alias { unit: 1_118_562, alias: text, source: "MotoP25_FDMA", talkgroup: Some(2207) }));
        // A block from another sequence is ignored.
        let mut a = LcAliases::default();
        let mut w = words.clone();
        let last = w.len() - 1;
        w[last][3] ^= 0x10;
        assert!(w.iter().all(|w| a.lcw(w, None, None).is_none()));
    }

    #[test]
    fn motorola_phase2() {
        let text = "DISPATCH 3".to_string();
        let p = payload(0x12345, 0x1a2, 4321, &text).unwrap();
        // Header: op, mfid, len, talkgroup, count, 2 unknown, sequence, then 16 nibbles of payload.
        let (head_payload, rest) = p.split_at(16);
        let blocks: Vec<String> = rest.as_bytes().chunks(25).map(|c| format!("{:0<25}", std::str::from_utf8(c).unwrap())).collect();
        let mut head = vec![0x91, MOTOROLA, 17];
        head.extend(unhex(&format!("{:04x}{:02x}0000{:x}0{head_payload}", 55u16, blocks.len(), 5)).unwrap());
        let mut a = MacAliases::default();
        assert!(a.msg(&MacMsg { op: 0x91, mfid: MOTOROLA, bytes: &head }, None, None).is_none());
        let mut got = None;
        for (k, b) in blocks.iter().enumerate() {
            let mut m = vec![0x95, MOTOROLA, 17, k as u8 + 1];
            m.extend(unhex(&format!("5{b}")).unwrap());
            got = a.msg(&MacMsg { op: 0x95, mfid: MOTOROLA, bytes: &m }, None, None).or(got);
        }
        assert_eq!(got, Some(Alias { unit: 4321, alias: text, source: "MotoP25_TDMA", talkgroup: Some(55) }));
    }

    #[test]
    fn harris_phase1_and_2() {
        let mut a = LcAliases::default();
        let pa = [0x32, HARRIS, b'M', b'E', b'D', b'I', b'C', b' ', b'4'];
        let pb = [0x33, HARRIS, b'1', b' ', b' ', 0, 0, 0, 0];
        assert!(a.lcw(&pa, Some(77), Some(9)).is_none());
        assert_eq!(a.lcw(&pb, Some(77), Some(9)).map(|x| x.alias), Some("MEDIC 41".into()));
        // Part B alone, or no talker: nothing.
        assert!(a.lcw(&pb, Some(77), Some(9)).is_none());
        a.lcw(&pa, None, None);
        assert!(a.lcw(&pb, None, None).is_none());

        let pdu = [0x80, 0xa8, HARRIS, 11, b'D', b'I', b'S', b'P', b'A', b'T', b'C', b'H', 0x00, 0, 0, 0, 0, 0, 0, 0, 0];
        let msgs = mac_messages(&pdu);
        assert_eq!((msgs[0].op, msgs[0].mfid, msgs[0].bytes.len()), (0xa8, HARRIS, 11));
        let mut m = MacAliases::default();
        assert_eq!(m.msg(&msgs[0], Some(5), None).map(|x| (x.unit, x.alias)), Some((5, "DISPATCH".into())));
    }

    #[test]
    fn mac_walk_and_talker() {
        // Group Voice Channel User (abbreviated): grp 0x1234, src 0x0a0b0c; then null.
        let pdu = [0x80, 0x01, 0x00, 0x12, 0x34, 0x0a, 0x0b, 0x0c, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let msgs = mac_messages(&pdu);
        assert_eq!(msgs.len(), 2);
        assert_eq!(mac_talker(&msgs[0]), Some(0x0a0b0c));
    }
}
