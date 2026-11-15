//! DMR over-the-air talker aliases (ETSI TS 102 361-2 §§5.4.3, 7.2.18–19).
//! FID 0 / 0x68 use FLCO 4–7; Motorola's equivalent uses 0x14–0x17.
//! Only integrity-checked, unprotected embedded LC enters assembly. Aliases do
//! not contain a radio ID: the voice decoder ([`super::voice`]) names the radio,
//! from the slot's voice LC, and resets the assembly when its talker changes.

use super::slot::{Lc, FID_HYTERA, FID_MOTOROLA};
use crate::alias::Alias;
use crate::bits::field;
use serde::Serialize;

/// Fragments (and a talker named by voice LC) older than this are not used.
pub const HOLD_S: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Format { Ascii7, Latin1, Utf8, Utf16Be }

impl Format {
    fn width(self) -> usize {
        match self { Self::Ascii7 => 7, Self::Latin1 | Self::Utf8 => 8, Self::Utf16Be => 16 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Decoded {
    pub text: String,
    pub complete: bool,
    pub format: Format,
    pub fid: u8,
    pub declared_units: u8,
    pub received_units: u8,
}

impl Decoded {
    fn source(&self) -> &'static str {
        match self.fid { FID_HYTERA => "HyteraDMR", FID_MOTOROLA => "MotoDMR", _ => "DMR" }
    }

    /// Radio `unit`'s name, once the alias is whole (a partial one is only shown by `tool dmr`).
    pub fn alias(&self, unit: u32, talkgroup: Option<u32>) -> Option<Alias> {
        self.complete.then(|| Alias { unit, alias: self.text.clone(), source: self.source(), talkgroup })
    }
}

fn block(lc: Lc) -> Option<usize> {
    match (lc.fid(), lc.flco()) {
        (0 | FID_HYTERA, 4..=7) => Some((lc.flco() - 4) as usize),
        (FID_MOTOROLA, 0x14..=0x17) => Some((lc.flco() - 0x14) as usize),
        _ => None,
    }
}

/// One slot's bounded, contiguous alias sequence. No tail is reused after a
/// new header, missing/reordered block, vendor change, protection or timeout.
#[derive(Default)]
pub struct Assembler {
    header: Option<(Format, u8, u8)>,
    parts: [Option<Vec<u8>>; 4],
    next: usize,
    last_s: f64,
    last: Option<Decoded>,
}

impl Assembler {
    pub fn reset(&mut self) { *self = Self::default(); }

    pub fn push(&mut self, lc: Lc, t: f64) -> Option<Decoded> {
        let index = block(lc)?;
        if t - self.last_s > HOLD_S || lc.0[0] & 0x80 != 0 { self.reset(); }
        if lc.0[0] & 0x80 != 0 { return None; }
        self.last_s = t;
        let start = if index == 0 {
            self.reset();
            self.last_s = t;
            let format = match lc.0[2] >> 6 { 0 => Format::Ascii7, 1 => Format::Latin1, 2 => Format::Utf8, _ => Format::Utf16Be };
            let units = (lc.0[2] >> 1) & 31;
            let capacity = if format == Format::Ascii7 { 217 } else { 216 };
            if units == 0 || units as usize * format.width() > capacity { return None; }
            self.header = Some((format, units, lc.fid()));
            if format == Format::Ascii7 { 23 } else { 24 }
        } else {
            let (_, _, fid) = self.header?;
            if fid != lc.fid() { self.reset(); return None; }
            16
        };
        let bits: Vec<_> = (start..72).map(|i| field(&lc.0, i, i + 1) as u8).collect();
        if index != self.next {
            if index < self.next && self.parts[index].as_ref() == Some(&bits) { return None; }
            self.reset();
            return None;
        }
        self.parts[index] = Some(bits);
        self.next += 1;
        let (format, declared_units, fid) = self.header?;
        let bits: Vec<_> = self.parts[..self.next].iter().flatten().flatten().copied().collect();
        let width = format.width();
        let received_units = (bits.len() / width).min(declared_units as usize) as u8;
        let complete = received_units == declared_units;
        let units: Vec<u16> = bits.chunks_exact(width).take(received_units as usize).map(|b| b.iter().fold(0, |v, &b| v << 1 | b as u16)).collect();
        let text = decode(format, &units, complete)?;
        let decoded = Decoded { text, complete, format, fid, declared_units, received_units };
        if self.last.as_ref() == Some(&decoded) { return None; }
        self.last = Some(decoded.clone());
        Some(decoded)
    }
}

fn decode(format: Format, units: &[u16], complete: bool) -> Option<String> {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    if units[end..].iter().any(|&u| u != 0) { return None; }
    let units = &units[..end];
    let text: String = match format {
        Format::Ascii7 | Format::Latin1 => units.iter().map(|&u| char::from_u32(u as u32)).collect::<Option<_>>()?,
        Format::Utf8 => {
            let bytes: Vec<_> = units.iter().map(|&u| u as u8).collect();
            match std::str::from_utf8(&bytes) {
                Ok(s) => s.to_string(),
                Err(e) if !complete && e.error_len().is_none() => std::str::from_utf8(&bytes[..e.valid_up_to()]).ok()?.to_string(),
                _ => return None,
            }
        }
        Format::Utf16Be => {
            let mut text = String::new();
            let mut it = char::decode_utf16(units.iter().copied()).peekable();
            while let Some(c) = it.next() {
                match c {
                    Ok(c) => text.push(c),
                    Err(e) if !complete && it.peek().is_none() && (0xd800..=0xdbff).contains(&e.unpaired_surrogate()) => break,
                    Err(_) => return None,
                }
            }
            text
        }
    };
    if text.chars().any(char::is_control) { return None; }
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(fid: u8, format: u8, units: &[u16]) -> Vec<Lc> {
        let width = [7, 8, 8, 16][format as usize];
        let bits: Vec<_> = units.iter().flat_map(|u| (0..width).rev().map(move |i| (u >> i & 1) as u8)).collect();
        let mut at = 0;
        let mut out = Vec::new();
        for i in 0..4 {
            let mut b = [0; 9];
            b[0] = (if fid == FID_MOTOROLA { 0x14 } else { 4 }) + i;
            b[1] = fid;
            let start = if i == 0 {
                b[2] = format << 6 | (units.len() as u8) << 1;
                if format == 0 { 23 } else { 24 }
            } else { 16 };
            for n in start..72 {
                if at == bits.len() { break; }
                b[n / 8] |= bits[at] << (7 - n % 8);
                at += 1;
            }
            out.push(Lc(b));
            if at == bits.len() { break; }
        }
        assert_eq!(at, bits.len());
        out
    }

    fn assemble(ms: &[Lc]) -> Decoded {
        let mut a = Assembler::default();
        let mut out = None;
        for (i, &lc) in ms.iter().enumerate() { out = a.push(lc, i as f64 * 0.72); }
        out.unwrap()
    }

    #[test]
    fn all_encodings_and_vendor_opcodes_use_code_units() {
        for fid in [0, FID_HYTERA, FID_MOTOROLA] {
            let ascii: Vec<u16> = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ12345".iter().map(|&b| b as u16).collect();
            let m = messages(fid, 0, &ascii);
            assert_ne!(m[0].0[2] & 1, 0, "7-bit header uses its 49th payload bit");
            assert_eq!(m.len(), 4);
            let d = assemble(&m);
            assert!(d.complete);
            assert_eq!(d.text, "ABCDEFGHIJKLMNOPQRSTUVWXYZ12345");
            let latin = assemble(&messages(fid, 1, &[b'C' as u16, b'a' as u16, b'f' as u16, 0xe9, 32, 49, 50]));
            assert_eq!(latin.text, "Café 12");
            let utf8: Vec<u16> = "É🚒".as_bytes().iter().map(|&b| b as u16).collect();
            let d = assemble(&messages(fid, 2, &utf8));
            assert_eq!((d.text.as_str(), d.declared_units, d.complete), ("É🚒", 6, true));
            let utf16: Vec<u16> = "AB🚒CD".encode_utf16().collect();
            assert_eq!(assemble(&messages(fid, 3, &utf16)).text, "AB🚒CD");
        }
    }

    #[test]
    fn captured_hytera_alias_is_an_incomplete_prefix_not_a_saved_name() {
        let mut a = Assembler::default();
        let d = a.push(Lc([4, 0x68, 0xda, 0, 0x48, 0, 0x79, 0, 0x74]), 3.5447).unwrap();
        assert_eq!((d.text.as_str(), d.complete), ("Hyt", false));
        let d = a.push(Lc([5, 0x68, 0, 0x65, 0, 0x72, 0, 0x61, 0]), 4.2647).unwrap();
        assert_eq!((d.text.as_str(), d.declared_units, d.received_units), ("Hytera", 13, 6));
        assert!(!d.complete && d.alias(5006, Some(149)).is_none());
    }

    #[test]
    fn partial_unicode_never_inserts_replacement_characters() {
        let units: Vec<u16> = "AB🚒CD".encode_utf16().collect();
        let ms = messages(0, 3, &units);
        let mut a = Assembler::default();
        let p = a.push(ms[0], 0.0).unwrap();
        assert_eq!((p.text.as_str(), p.complete), ("AB", false));
        assert_eq!(a.push(ms[1], 0.72).unwrap().text, "AB🚒CD");
        let bytes: Vec<_> = "ABCDE🚒".as_bytes().iter().map(|&b| b as u16).collect();
        let ms = messages(0, 2, &bytes);
        let mut a = Assembler::default();
        assert_eq!(a.push(ms[0], 0.0).unwrap().text, "ABCDE");
        assert_eq!(a.push(ms[1], 0.72).unwrap().text, "ABCDE🚒");
    }

    #[test]
    fn missing_reordered_conflicting_or_protected_blocks_never_complete() {
        let units: Vec<_> = b"ABCDEFGHIJKLMNOPQRST".iter().map(|&b| b as u16).collect();
        let ms = messages(0, 1, &units);
        let mut a = Assembler::default();
        assert!(a.push(ms[1], 0.0).is_none());
        a.push(ms[0], 0.0);
        assert!(a.push(ms[2], 0.7).is_none());
        assert!(a.push(ms[1], 1.4).is_none());
        a.push(ms[0], 2.0);
        a.push(ms[1], 2.7);
        assert!(a.push(ms[1], 2.8).is_none(), "identical duplicate is harmless");
        let mut conflict = ms[1]; conflict.0[2] ^= 1;
        assert!(a.push(conflict, 2.9).is_none());
        assert!(a.push(ms[2], 3.0).is_none());
        a.push(ms[0], 4.0);
        let mut protected = ms[1]; protected.0[0] |= 0x80;
        assert!(a.push(protected, 4.7).is_none());
        assert!(a.push(ms[2], 5.4).is_none());
        a.push(ms[0], 6.0);
        assert!(a.push(ms[1], 10.1).is_none(), "stale header is discarded");
        a.push(ms[0], 11.0);
        let mut other = ms[1]; other.0[1] = FID_HYTERA;
        assert!(a.push(other, 11.7).is_none());
        assert!(a.push(ms[2], 12.4).is_none());
        assert!(Assembler::default().push(Lc([4, 0, 0xfe, 0, 0, 0, 0, 0, 0]), 0.0).is_none(), "impossible UTF-16 length");
        assert!(decode(Format::Utf8, &[0xff], true).is_none());
        assert!(decode(Format::Utf16Be, &[0xd800], true).is_none());
        assert!(decode(Format::Latin1, &[10, 65], true).is_none());
        assert!(decode(Format::Latin1, &[65, 0, 66], true).is_none());
    }
}
