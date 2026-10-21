//! Link control words (LDU1's and the TDULC's): what a voice channel says
//! about its call, as TIA-102.AABF-D §7 defines them (Motorola's as AABH
//! §5.2 does). Octet 0 is the format: P (protected: the rest encrypted), SF
//! (set: the standard's implicit MFID; clear: octet 1 names the
//! manufacturer, whose opcodes are its own) and the 6-bit opcode (LCO).

use super::osp::{CallSign, ServiceOptions, MFID_MOTOROLA};

/// One link control word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkControl {
    pub lco: u8,
    /// The manufacturer: 0 for the standard's words (SF set, or MFID $00 / $01).
    pub mfid: u8,
    pub protected: bool,
    pub msg: Lc,
    /// The word itself (manufacturers' formats: talker aliases, positions).
    pub raw: [u8; 9],
}

/// What it says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lc {
    /// LCO 0 (§7.3.1): the group call on this channel and its talker.
    /// `extended`: S, the talker's full ID follows in a Source ID Extension
    /// (`source` 0 if it has no address here).
    GroupVoiceUser { opts: ServiceOptions, group: u16, source: u32, extended: bool },
    /// Motorola's LCO 0 (AABH §5.2.1): a patch's call, by its supergroup.
    /// Octet 2 has only the P (encrypted) bit.
    PatchVoiceUser { opts: ServiceOptions, sg: u16, source: u32, extended: bool },
    /// LCO 3 (§7.3.3), and its extended form LCO 10 (§7.3.29), whose source
    /// is the talker's ID (its full ID following).
    UnitVoiceUser { opts: ServiceOptions, target: u32, source: u32, extended: bool },
    /// LCO 6 (§7.3.6): a telephone call, and the radio on it.
    PhoneVoiceUser { opts: ServiceOptions, timer_ds: u16, unit: u32 },
    /// LCO 9 (§7.3.32): the full ID of the last word's source.
    SourceIdExtension { wacn: u32, sys_id: u16, id: u32 },
    /// LCO 15 (§7.3.8): the call is over and the channel released (not
    /// just this transmission).
    CallTermination { unit: u32 },
    /// Motorola's LC_MOT_BSI (LCO 5; its BSI receive guide §1.7.2): the
    /// channel's FCC call sign, at a call's start and now and then in it.
    BaseStationId(CallSign),
    /// The rest is encrypted.
    Protected,
    #[default]
    Other,
}

impl LinkControl {
    /// A voice channel user word: the call's group, or its target.
    pub fn voice_user(&self) -> bool {
        matches!(self.msg, Lc::GroupVoiceUser { .. } | Lc::PatchVoiceUser { .. } | Lc::UnitVoiceUser { .. } | Lc::PhoneVoiceUser { .. })
    }
    /// The call's group (a patch's: its supergroup).
    pub fn group(&self) -> Option<u32> {
        match self.msg {
            Lc::GroupVoiceUser { group, .. } => Some(group as u32),
            Lc::PatchVoiceUser { sg, .. } => Some(sg as u32),
            _ => None,
        }
    }
    pub fn target(&self) -> Option<u32> {
        match self.msg {
            Lc::UnitVoiceUser { target, .. } => Some(target),
            _ => None,
        }
    }
    /// The talker, as the word names it (0: not named).
    pub fn source(&self) -> Option<u32> {
        match self.msg {
            Lc::GroupVoiceUser { source, .. } | Lc::PatchVoiceUser { source, .. } | Lc::UnitVoiceUser { source, .. } => Some(source),
            Lc::PhoneVoiceUser { unit, .. } => Some(unit),
            _ => None,
        }
    }
    pub fn opts(&self) -> Option<ServiceOptions> {
        match self.msg {
            Lc::GroupVoiceUser { opts, .. } | Lc::PatchVoiceUser { opts, .. } | Lc::UnitVoiceUser { opts, .. } | Lc::PhoneVoiceUser { opts, .. } => Some(opts),
            _ => None,
        }
    }
    /// The talker's full ID follows (S).
    pub fn extended(&self) -> bool {
        matches!(self.msg, Lc::GroupVoiceUser { extended: true, .. } | Lc::PatchVoiceUser { extended: true, .. } | Lc::UnitVoiceUser { extended: true, .. })
    }
}

/// The octets AAAD-B's keystream covers (Table 5-3: L1..L8, the LC's 64
/// bits after the format octet). An explicit MFID (SF clear) stays clear
/// (AABF-D §7.2): only octets 2-8 are encrypted then.
pub fn encrypted_octets(w: &[u8; 9]) -> std::ops::Range<usize> {
    if w[0] & 0x40 != 0 { 1..9 } else { 2..9 }
}

pub fn parse(w: &[u8; 9]) -> LinkControl {
    // SF set: the standard's format, octet 1 no MFID. Otherwise octet 1 names
    // the manufacturer. (LCO 0 and 3 are defined with SF clear and MFID 0;
    // with SF set they are read as the standard's all the same.)
    let mfid = if w[0] & 0x40 != 0 || w[1] <= 1 { 0 } else { w[1] };
    let (lco, protected) = (w[0] & 0x3f, w[0] & 0x80 != 0);
    let u16_at = |a: usize| (w[a] as u16) << 8 | w[a + 1] as u16;
    let u24 = |a: usize| (w[a] as u32) << 16 | (w[a + 1] as u32) << 8 | w[a + 2] as u32;
    let opts = ServiceOptions(w[2]);
    let msg = if protected {
        Lc::Protected
    } else {
        match (mfid, lco) {
            (0, 0x00) => Lc::GroupVoiceUser { opts, group: u16_at(4), source: u24(6), extended: w[3] & 1 != 0 },
            (0, 0x03) => Lc::UnitVoiceUser { opts, target: u24(3), source: u24(6), extended: false },
            (0, 0x06) => Lc::PhoneVoiceUser { opts, timer_ds: u16_at(4), unit: u24(6) },
            (0, 0x09) => Lc::SourceIdExtension { wacn: u24(2) >> 4, sys_id: u16_at(4) & 0xfff, id: u24(6) },
            (0, 0x0a) => Lc::UnitVoiceUser { opts, target: u24(3), source: u24(6), extended: true },
            (0, 0x0f) => Lc::CallTermination { unit: u24(6) },
            (MFID_MOTOROLA, 0x05) => Lc::BaseStationId(CallSign::from_octets(&w[2..8])),
            (MFID_MOTOROLA, 0x00) => Lc::PatchVoiceUser { opts: ServiceOptions(w[2] & 0x40), sg: u16_at(4), source: u24(6), extended: w[3] & 1 != 0 },
            _ => Lc::Other,
        }
    };
    LinkControl { lco, mfid, protected, msg, raw: *w }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each word as its figure lays it out.

    /// The BSI guide's LC_MOT_BSI: P SF clear, LCO 5, MFID $90, eight characters from octet 2.
    #[test]
    fn base_station_id() {
        // "KNJU834": K=$20 N=$23 J=$1F U=$2A 8=$0D 3=$08 4=$09, none.
        let v = [0x20u64, 0x23, 0x1f, 0x2a, 0x0d, 0x08, 0x09, 0x00].iter().fold(0u64, |v, &c| v << 6 | c);
        let mut w = [0x05, 0x90, 0, 0, 0, 0, 0, 0, 0];
        w[2..8].copy_from_slice(&v.to_be_bytes()[2..]);
        let Lc::BaseStationId(c) = parse(&w).msg else { panic!("{:?}", parse(&w).msg) };
        assert_eq!(c.text(), "KNJU834");
        // The standard's LCO 5 isn't it.
        w[1] = 0x00;
        assert!(!matches!(parse(&w).msg, Lc::BaseStationId(_)));
    }

    /// §7.3.1: options, S (octet 3 bit 0), group, source.
    #[test]
    fn group_voice_user() {
        let lc = parse(&[0x00, 0x00, 0x80, 0x01, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert_eq!(lc.msg, Lc::GroupVoiceUser { opts: ServiceOptions(0x80), group: 0x0101, source: 0x123456, extended: true });
        assert_eq!((lc.group(), lc.source(), lc.voice_user()), (Some(0x0101), Some(0x123456), true));
    }

    /// AABH §5.2.1: the supergroup; octet 2's bit 7 is reserved, not emergency.
    #[test]
    fn motorola_patch_voice_user() {
        let lc = parse(&[0x00, 0x90, 0xc0, 0x00, 0x02, 0x04, 0x12, 0x34, 0x56]);
        assert_eq!(lc.msg, Lc::PatchVoiceUser { opts: ServiceOptions(0x40), sg: 0x0204, source: 0x123456, extended: false });
        // Motorola's LCO 3 isn't the standard's unit-to-unit voice user.
        assert!(!parse(&[0x03, 0x90, 0x02, 0x04, 0x01, 0x01, 0x01, 0x02, 0x00]).voice_user());
    }

    /// §7.3.3 and §7.3.29: target, source (an ID in the extended form).
    #[test]
    fn unit_voice_user() {
        let lc = parse(&[0x03, 0x00, 0x40, 0x04, 0x05, 0x06, 0x01, 0x02, 0x03]);
        assert_eq!(lc.msg, Lc::UnitVoiceUser { opts: ServiceOptions(0x40), target: 0x040506, source: 0x010203, extended: false });
        let lc = parse(&[0x4a, 0x01, 0x00, 0x04, 0x05, 0x06, 0x01, 0x02, 0x03]);
        assert_eq!((lc.target(), lc.source(), lc.extended()), (Some(0x040506), Some(0x010203), true));
        // The SF bit: the standard's format, whatever octet 1 holds.
        assert!(parse(&[0x43, 0x90, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x07]).voice_user());
    }

    /// §7.3.6, §7.3.32, §7.3.8.
    #[test]
    fn phone_extension_and_termination() {
        assert_eq!(parse(&[0x46, 0, 0x04, 0, 0x02, 0x58, 0x0a, 0x0b, 0x0c]).msg, Lc::PhoneVoiceUser { opts: ServiceOptions(0x04), timer_ds: 600, unit: 0x0a0b0c });
        assert_eq!(parse(&[0x49, 0, 0xbe, 0xe0, 0x03, 0xa9, 0x01, 0x02, 0x03]).msg, Lc::SourceIdExtension { wacn: 0xbee00, sys_id: 0x3a9, id: 0x010203 });
        assert_eq!(parse(&[0x4f, 0, 0, 0, 0, 0, 0x0a, 0x0b, 0x0c]).msg, Lc::CallTermination { unit: 0x0a0b0c });
    }

    /// §7.1/§7.2: P encrypts octets 1-8, or (an explicit MFID) 2-8.
    #[test]
    fn protected_words() {
        let w = [0xc0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0];
        assert_eq!((parse(&w).msg, parse(&w).protected), (Lc::Protected, true));
        assert_eq!(encrypted_octets(&w), 1..9);
        assert_eq!(encrypted_octets(&[0x80, 0x90, 0, 0, 0, 0, 0, 0, 0]), 2..9);
    }
}
