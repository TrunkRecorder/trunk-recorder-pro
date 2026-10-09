//! Kenwood talker aliases: the name a radio is programmed with ("E12 CAPT"),
//! sent on the traffic channel as a manufacturer-specific PROP_FORM message
//! (type 0x3F) in the SACCH superframe and in FACCH1. DSD-FME's
//! `NXDN_decode_Alias16` is the reference for the layout.
//!
//! ```text
//! octet  0      1     2  3      4            5 .. 8
//!        0x3F   MFID  82 04     seg | total  four alias bytes
//!               0x68                          (high nibble: this segment, 1-based;
//!                                              low nibble: how many there are)
//! ```
//!
//! The segments are sent in turn, over and over, for as long as the radio
//! talks. Put together (`total` × 4 bytes) the alias is followed by a 16-bit
//! big-endian checksum, the sum of every alias byte before it (checked against
//! aliases heard off a live Kenwood NEXEDGE site; DSD-FME's source says only
//! that it "appears to have an embedded CRC"). The alias is plain ASCII,
//! padded with NULs.
//!
//! Aliases in other character sets (DSD-FME's Shift-JIS / Big5 builds) are
//! not decoded: an alias with a byte outside printable ASCII is dropped.

/// Kenwood's manufacturer ID.
pub const KENWOOD: u8 = 0x68;
/// The two octets after the MFID that mark a PROP_FORM message as an alias.
const SIGNATURE: [u8; 2] = [0x82, 0x04];
/// Most segments a message can name (4 bits).
const MAX_SEGMENTS: usize = 15;

/// One segment of an alias, as sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// 1-based position.
    pub index: u8,
    /// How many segments the alias has.
    pub total: u8,
    pub text: [u8; 4],
}

/// The alias segment in a PROP_FORM message (`o[0]` is the message type
/// octet), if it is one.
pub fn segment(o: &[u8]) -> Option<Segment> {
    if o.len() < 9 || o[0] & 0x3f != super::layer3::PROP_FORM || o[1] != KENWOOD || o[2..4] != SIGNATURE {
        return None;
    }
    let (index, total) = (o[4] >> 4, o[4] & 0xf);
    if index == 0 || total == 0 || index > total {
        return None;
    }
    Some(Segment { index, total, text: [o[5], o[6], o[7], o[8]] })
}

/// Collects one transmission's segments into its alias.
#[derive(Default)]
pub struct Aliases {
    total: u8,
    got: [Option<[u8; 4]>; MAX_SEGMENTS],
    /// The alias last returned, so each transmission names its radio once.
    done: Option<String>,
}

impl Aliases {
    /// A segment heard. The alias, once, when it is whole and its checksum is good.
    pub fn push(&mut self, s: Segment) -> Option<String> {
        if s.index == 0 || s.index > s.total || s.total as usize > MAX_SEGMENTS {
            return None;
        }
        if s.total != self.total {
            // Another alias (or the first): start over.
            self.total = s.total;
            self.got = [None; MAX_SEGMENTS];
            self.done = None;
        }
        self.got[s.index as usize - 1] = Some(s.text);
        let n = self.total as usize;
        let mut bytes = Vec::with_capacity(4 * n);
        for g in &self.got[..n] {
            bytes.extend_from_slice(&(*g)?);
        }
        let alias = decode(&bytes)?;
        if self.done.as_deref() == Some(&alias) {
            return None;
        }
        self.done = Some(alias.clone());
        Some(alias)
    }

    /// The transmission ended, or its talker changed: what was heard isn't the next one's.
    pub fn reset(&mut self) {
        *self = Aliases::default();
    }
}

/// The alias in a whole message body (text, NUL padding, 16-bit sum), or
/// `None` when the sum is wrong or the text isn't printable ASCII.
fn decode(bytes: &[u8]) -> Option<String> {
    let (text, sum) = bytes.split_at(bytes.len().checked_sub(2)?);
    let want = u16::from_be_bytes([sum[0], sum[1]]);
    if text.iter().map(|&b| b as u16).fold(0u16, u16::wrapping_add) != want {
        return None;
    }
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    // Nothing but padding may follow the text.
    if text[end..].iter().any(|&b| b != 0) {
        return None;
    }
    let text = &text[..end];
    if !text.iter().all(|b| (0x20..0x7f).contains(b)) {
        return None;
    }
    let alias = std::str::from_utf8(text).ok()?.trim();
    (!alias.is_empty()).then(|| alias.to_string())
}

/// The octets of an alias message, to build test and synthesized traffic.
pub mod build {
    use super::*;

    /// The messages that carry `alias` (ASCII, at most 58 characters), one per
    /// segment, the last one holding the checksum.
    pub fn segments(alias: &str) -> Vec<Vec<u8>> {
        let mut body = alias.as_bytes().to_vec();
        let sum = body.iter().map(|&b| b as u16).fold(0u16, u16::wrapping_add);
        body.extend_from_slice(&sum.to_be_bytes());
        while !body.len().is_multiple_of(4) {
            // The checksum ends the message: pad before it, not after.
            body.insert(body.len() - 2, 0);
        }
        let total = (body.len() / 4) as u8;
        body.chunks(4)
            .enumerate()
            .map(|(i, c)| {
                let mut o = vec![super::super::layer3::PROP_FORM, KENWOOD, SIGNATURE[0], SIGNATURE[1], (i as u8 + 1) << 4 | total];
                o.extend_from_slice(c);
                o
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const E12: [&str; 3] = ["3f6882041345313220", "3f6882042343415054", "3f68820433000001f0"];
    const MEDIC: [&str; 3] = ["3f688204134d454449", "3f6882042343203700", "3f68820433000001b9"];

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    /// The aliases a run of messages names, in order.
    fn heard(msgs: &[&str]) -> Vec<String> {
        let mut a = Aliases::default();
        msgs.iter().filter_map(|m| a.push(segment(&hex(m)).unwrap())).collect()
    }

    #[test]
    fn known_messages_decode() {
        assert_eq!(heard(&E12), ["E12 CAPT"]);
        assert_eq!(heard(&MEDIC), ["MEDIC 7"]);
        // The sender's layout: text, NUL padding before the sum.
        assert_eq!(build::segments("E12 CAPT").iter().map(|o| o.iter().map(|b| format!("{b:02x}")).collect::<String>()).collect::<Vec<_>>(), E12);
    }

    #[test]
    fn it_names_the_radio_once_however_often_it_repeats() {
        let all: Vec<&str> = E12.iter().cycle().take(9).copied().collect();
        assert_eq!(heard(&all), ["E12 CAPT"]);
    }

    #[test]
    fn it_joins_a_transmission_heard_from_the_middle() {
        assert_eq!(heard(&[E12[2], E12[0], E12[1]]), ["E12 CAPT"]);
        assert_eq!(heard(&[E12[1], E12[2], E12[0], E12[1]]), ["E12 CAPT"]);
    }

    #[test]
    fn segments_may_come_in_any_order() {
        assert_eq!(heard(&[E12[2], E12[1], E12[0]]), ["E12 CAPT"]);
    }

    #[test]
    fn a_missing_segment_waits_for_the_repeat() {
        let mut a = Aliases::default();
        assert!(a.push(segment(&hex(E12[0])).unwrap()).is_none());
        assert!(a.push(segment(&hex(E12[2])).unwrap()).is_none());
        assert_eq!(a.push(segment(&hex(E12[1])).unwrap()).as_deref(), Some("E12 CAPT"));
    }

    #[test]
    fn a_bad_checksum_or_text_is_dropped() {
        assert!(heard(&[E12[0], E12[1], "3f68820433000001f1"]).is_empty(), "off by one");
        // 0xe3 is not ASCII (a Shift-JIS lead byte), though the checksum (0x0124) is right: left alone.
        assert!(heard(&["3f68820412e3410000", "3f6882042200000124"]).is_empty());
        // A control character in the text.
        assert!(heard(&["3f68820412410a4200", "3f688204220000008d"]).is_empty());
        // Text after the padding began (0x41 + 0x42 + 0x41 = 0xc4).
        assert!(heard(&["3f6882041241420041", "3f68820422000000c4"]).is_empty());
        // Nothing but padding.
        assert!(heard(&["3f6882041200000000", "3f6882042200000000"]).is_empty());
        // Spaces only.
        assert!(heard(&["3f6882041220202020", "3f68820422000000 80".replace(' ', "").as_str()]).is_empty());
    }

    #[test]
    fn segments_of_two_radios_are_not_made_into_a_third_alias() {
        // The two share a length; their segments alternate (a talker change
        // missed). Whatever is made of that must be one of the two, never a mixture.
        let mut a = Aliases::default();
        let mut got = Vec::new();
        for (x, y) in E12.iter().zip(MEDIC.iter()) {
            got.extend(a.push(segment(&hex(x)).unwrap()));
            got.extend(a.push(segment(&hex(y)).unwrap()));
        }
        for g in &got {
            assert!(g == "E12 CAPT" || g == "MEDIC 7", "{g}");
        }
        assert_eq!(got, ["MEDIC 7"]);
    }

    #[test]
    fn another_radio_replaces_the_alias() {
        let mut a = Aliases::default();
        let mut got = Vec::new();
        for name in ["E12 CAPT", "MEDIC 7", "E12 CAPT", "LONGER NAME 12"] {
            for s in build::segments(name) {
                got.extend(a.push(segment(&s).unwrap()));
            }
        }
        assert_eq!(got, ["E12 CAPT", "MEDIC 7", "E12 CAPT", "LONGER NAME 12"]);
    }

    #[test]
    fn built_aliases_round_trip() {
        for name in ["A", "AB", "ENGINE 12", "E12 CAPT", "MEDIC 7 ", "12345678901234567890", &"X".repeat(58)] {
            let mut a = Aliases::default();
            let got: Vec<String> = build::segments(name).iter().filter_map(|s| a.push(segment(s).unwrap())).collect();
            assert_eq!(got, [name.trim()], "{name}");
        }
    }

    #[test]
    fn other_prop_forms_are_not_aliases() {
        assert!(segment(&hex("3f6882042400000000")).is_some());
        assert!(segment(&hex("3f6a82041400000000")).is_none(), "another manufacturer");
        assert!(segment(&hex("3f6881041400000000")).is_none(), "another PROP_FORM");
        assert!(segment(&hex("3f6882040400000000")).is_none(), "segment 0");
        assert!(segment(&hex("3f6882042100000000")).is_none(), "segment 2 of 1");
        assert!(segment(&hex("3f68820450000000")).is_none(), "short");
        assert!(segment(&[]).is_none());
        assert!(segment(&hex("046882041345313220")).is_none(), "not PROP_FORM");
        // The message type is in the low six bits: the top two are not part of it.
        assert!(segment(&hex("ff6882041345313220")).is_some());
    }

    /// Whatever the air throws at it: never a panic, never anything but printable ASCII.
    #[test]
    fn noise_never_panics() {
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut a = Aliases::default();
        for _ in 0..200_000 {
            let r = next();
            let mut o = [0u8; 9];
            o.copy_from_slice(&r.to_le_bytes().iter().chain(&[(r >> 56) as u8]).copied().take(9).collect::<Vec<_>>());
            if r & 3 != 0 {
                // Mostly well-formed headers, so segments get as far as assembly.
                o[..4].copy_from_slice(&[0x3f, KENWOOD, 0x82, 0x04]);
            }
            if let Some(s) = segment(&o) {
                if let Some(alias) = a.push(s) {
                    assert!(alias.bytes().all(|b| (0x20..0x7f).contains(&b)) && alias == alias.trim() && !alias.is_empty());
                }
            }
            // Segments no message could have carried.
            let raw = Segment { index: (r >> 8) as u8, total: (r >> 16) as u8, text: [0; 4] };
            let _ = a.push(raw);
            if r % 97 == 0 {
                a.reset();
            }
        }
    }
}
