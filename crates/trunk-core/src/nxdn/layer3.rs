//! NXDN layer 3 messages (TS 1-A §6): octet 0 is two flags and the 6-bit
//! message type; the elements follow. The same type means different
//! messages on control and traffic channels (§6.4.5), so parsing takes
//! which one it came from.

/// Where a message was heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// A control channel's CAC (outbound).
    Control,
    /// A traffic or conventional channel's SACCH / FACCH1 / FACCH2.
    Traffic,
}

/// A Location ID (§6.5.2): category, system code and site code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    pub raw: u32,
}

impl Location {
    /// 0 global, 1 local, 2 regional (3 reserved).
    pub fn category(self) -> u8 {
        (self.raw >> 22) as u8
    }
    fn site_bits(self) -> u32 {
        match self.category() {
            0 => 12,
            2 => 8,
            _ => 5,
        }
    }
    pub fn system(self) -> u32 {
        (self.raw & 0x3f_ffff) >> self.site_bits()
    }
    pub fn site(self) -> u32 {
        self.raw & ((1 << self.site_bits()) - 1)
    }
}

/// How a site numbers its channels (SITE_INFO's Channel Access
/// Information): logical channel numbers (a table maps them to
/// frequencies) or Direct Frequency Assignment (base + number × step).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelAccess {
    pub dfa: bool,
    /// Hz; 0 unknown.
    pub step_hz: u32,
    pub base_hz: u64,
}

impl ChannelAccess {
    pub fn from_raw(raw: u32) -> Self {
        // Bit 23: RCN (1 = DFA), 22–21 step, 20–18 base (TS 1-A v2: later than the v1.3 we have;
        // as SDRTrunk and DSD-FME read it).
        let dfa = raw >> 23 & 1 != 0;
        let step_hz = match raw >> 21 & 3 {
            2 => 1250,
            3 => 3125,
            _ => 0,
        };
        let base_hz = match raw >> 18 & 7 {
            1 => 100_000_000,
            2 => 330_000_000,
            3 => 400_000_000,
            // DSD-FME: 750 MHz; SDRTrunk falls back to 450. Unconfirmed.
            4 => 750_000_000,
            _ => 0,
        };
        ChannelAccess { dfa, step_hz, base_hz }
    }

    /// The frequency of DFA channel `ofn`, when the site says how.
    pub fn dfa_hz(&self, ofn: u16) -> Option<f64> {
        (self.dfa && self.step_hz > 0 && self.base_hz > 0 && ofn > 0).then(|| self.base_hz as f64 + ofn as f64 * self.step_hz as f64)
    }
}

/// A traffic channel named in an assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Logical channel number (1–1023).
    Number(u16),
    /// Direct Frequency Assignment: outbound / inbound frequency numbers,
    /// and the bandwidth (0: 6.25 kHz / NXDN48, 1: 12.5 kHz / NXDN96).
    Dfa { ofn: u16, ifn: u16, bandwidth: u8 },
}

/// Call types (§6.5.12).
pub const CALL_BROADCAST: u8 = 0;
pub const CALL_CONFERENCE: u8 = 1;
pub const CALL_UNSPECIFIED: u8 = 2;
pub const CALL_INDIVIDUAL: u8 = 4;
pub const CALL_INTERCONNECT: u8 = 6;
pub const CALL_SPEED_DIAL: u8 = 7;

/// The fields every call control message starts with (octets 1–6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallHead {
    /// CC Option: 0x80 emergency, 0x40 inter-system, 0x20 priority paging.
    pub cc_option: u8,
    pub call_type: u8,
    /// Voice Call Option (5 bits): bit 4 duplex, bits 2–0 transmission mode
    /// (000 4800 EHR, 010 9600 EHR, 011 9600 EFR).
    pub option: u8,
    pub source: u16,
    pub destination: u16,
}

impl CallHead {
    pub fn group(&self) -> bool {
        matches!(self.call_type, CALL_BROADCAST | CALL_CONFERENCE | CALL_UNSPECIFIED)
    }
    pub fn emergency(&self) -> bool {
        self.cc_option & 0x80 != 0
    }
    /// NXDN96 voice (the transmission mode says 9600 bps).
    pub fn rate_9600(&self) -> bool {
        self.option & 2 != 0
    }
    /// Full-rate (EFR) voice, which isn't decoded.
    pub fn efr(&self) -> bool {
        self.option & 7 == 3
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A voice transmission's header (traffic): who to whom, and the cipher
    /// (0 clear, 1 scrambler, 2 DES, 3 AES).
    VCall { head: CallHead, cipher: u8, key_id: u8 },
    /// DES / AES initialization vector.
    VCallIv,
    /// End of a transmission.
    TxRel { head: CallHead },
    /// The traffic channel is released (trunked).
    TxRelEx { head: CallHead },
    /// A grant (control channel; on a traffic channel only in composite
    /// control operation): the call and its channel.
    VCallAssgn { head: CallHead, timer: u8, channel: Channel },
    /// The same, repeated for late entry (control) or naming other calls (traffic).
    VCallAssgnDup { head: CallHead, timer: u8, channel: Channel },
    DCallAssgn { head: CallHead, channel: Channel },
    Idle,
    Disc { head: CallHead },
    SiteInfo { location: Location, service: u16, access: ChannelAccess, version: u8, adjacent: u8, control: [u16; 2] },
    SrvInfo { location: Location, service: u16 },
    CchInfo { location: Location, flags: u8, control: [u16; 2] },
    /// Up to 4 adjacent sites: (location, site number, control channel).
    AdjSiteInfo { sites: Vec<(Location, u8, u16)> },
    /// One segment of a Kenwood talker alias ([`super::alias`]).
    TalkerAlias(super::alias::Segment),
    /// Manufacturer-specific, other than a Kenwood talker alias.
    PropForm { manufacturer: u8 },
    Other { kind: u8 },
}

pub const VCALL: u8 = 0x01;
pub const VCALL_IV: u8 = 0x03;
pub const VCALL_ASSGN: u8 = 0x04;
pub const VCALL_ASSGN_DUP: u8 = 0x05;
pub const TX_REL_EX: u8 = 0x07;
pub const TX_REL: u8 = 0x08;
pub const DCALL_ASSGN_DUP: u8 = 0x0d;
pub const DCALL_ASSGN: u8 = 0x0e;
pub const IDLE: u8 = 0x10;
pub const DISC: u8 = 0x11;
pub const SITE_INFO: u8 = 0x18;
pub const SRV_INFO: u8 = 0x19;
pub const CCH_INFO: u8 = 0x1a;
pub const ADJ_SITE_INFO: u8 = 0x1b;
pub const PROP_FORM: u8 = 0x3f;

fn u16_at(o: &[u8], i: usize) -> u16 {
    (o[i] as u16) << 8 | o[i + 1] as u16
}
fn u24_at(o: &[u8], i: usize) -> u32 {
    (o[i] as u32) << 16 | (o[i + 1] as u32) << 8 | o[i + 2] as u32
}
/// 10 bits starting `skip` bits into octet `i`.
fn ten_at(o: &[u8], i: usize, skip: u32) -> u16 {
    ((u24_at(o, i) >> (14 - skip)) & 0x3ff) as u16
}

impl Message {
    /// The message type of the message starting at `o[0]`.
    pub fn kind(o: &[u8]) -> u8 {
        o[0] & 0x3f
    }

    /// Parse the message starting at `o[0]` (as many octets as the channel
    /// carries; short messages leave fields at zero). `dfa`: the site uses
    /// Direct Frequency Assignment (from its SITE_INFO).
    pub fn parse(o: &[u8], ctx: Context, dfa: bool) -> Message {
        let mut b = [0u8; 22];
        let n = o.len().min(22);
        b[..n].copy_from_slice(&o[..n]);
        let o = &b;
        let head = CallHead { cc_option: o[1], call_type: o[2] >> 5, option: o[2] & 0x1f, source: u16_at(o, 3), destination: u16_at(o, 5) };
        let channel = || {
            if dfa {
                Channel::Dfa { ofn: u16_at(o, 8), ifn: u16_at(o, 10), bandwidth: o[7] & 3 }
            } else {
                Channel::Number(ten_at(o, 7, 6))
            }
        };
        match (Self::kind(o), ctx) {
            (VCALL, Context::Traffic) => Message::VCall { head, cipher: o[7] >> 6, key_id: o[7] & 0x3f },
            (VCALL_IV, Context::Traffic) => Message::VCallIv,
            (TX_REL, _) => Message::TxRel { head },
            (TX_REL_EX, _) => Message::TxRelEx { head },
            (VCALL_ASSGN, _) => Message::VCallAssgn { head, timer: o[7] >> 2, channel: channel() },
            (VCALL_ASSGN_DUP, _) => Message::VCallAssgnDup { head, timer: o[7] >> 2, channel: channel() },
            (DCALL_ASSGN | DCALL_ASSGN_DUP, Context::Control) => Message::DCallAssgn { head, channel: channel() },
            (IDLE, _) => Message::Idle,
            (DISC, _) => Message::Disc { head },
            (SITE_INFO, Context::Control) => Message::SiteInfo {
                location: Location { raw: u24_at(o, 1) },
                service: u16_at(o, 6),
                access: ChannelAccess::from_raw(u24_at(o, 11)),
                version: o[14],
                adjacent: o[15] >> 4,
                control: [ten_at(o, 15, 4), ten_at(o, 16, 6)],
            },
            (SRV_INFO, _) => Message::SrvInfo { location: Location { raw: u24_at(o, 1) }, service: u16_at(o, 4) },
            (CCH_INFO, _) => Message::CchInfo { location: Location { raw: u24_at(o, 1) }, flags: o[4] >> 4, control: [ten_at(o, 4, 6), ten_at(o, 6, 6)] },
            (ADJ_SITE_INFO, _) => {
                let sites = (0..4)
                    .map(|k| 1 + 5 * k)
                    .filter(|&i| i + 5 <= n)
                    .map(|i| (Location { raw: u24_at(o, i) }, o[i + 3] >> 2 & 0xf, ten_at(o, i + 3, 6)))
                    .filter(|s| s.0.raw != 0 && s.2 != 0)
                    .collect();
                Message::AdjSiteInfo { sites }
            }
            (PROP_FORM, _) => match super::alias::segment(&o[..n]) {
                Some(seg) => Message::TalkerAlias(seg),
                None => Message::PropForm { manufacturer: o[1] },
            },
            (kind, _) => Message::Other { kind },
        }
    }

    /// Short name, for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Message::VCall { .. } => "VCALL",
            Message::VCallIv => "VCALL_IV",
            Message::TxRel { .. } => "TX_REL",
            Message::TxRelEx { .. } => "TX_REL_EX",
            Message::VCallAssgn { .. } => "VCALL_ASSGN",
            Message::VCallAssgnDup { .. } => "VCALL_ASSGN_DUP",
            Message::DCallAssgn { .. } => "DCALL_ASSGN",
            Message::Idle => "IDLE",
            Message::Disc { .. } => "DISC",
            Message::SiteInfo { .. } => "SITE_INFO",
            Message::SrvInfo { .. } => "SRV_INFO",
            Message::CchInfo { .. } => "CCH_INFO",
            Message::AdjSiteInfo { .. } => "ADJ_SITE_INFO",
            Message::TalkerAlias(_) => "TALKER_ALIAS",
            Message::PropForm { .. } => "PROP_FORM",
            Message::Other { .. } => "OTHER",
        }
    }
}

/// Encoders for tests and the synthesizer: the octets of a message.
pub mod build {
    use super::*;

    fn head(kind: u8, h: &CallHead) -> Vec<u8> {
        vec![kind, h.cc_option, h.call_type << 5 | h.option & 0x1f, (h.source >> 8) as u8, h.source as u8, (h.destination >> 8) as u8, h.destination as u8]
    }

    pub fn vcall(h: &CallHead, cipher: u8, key_id: u8) -> Vec<u8> {
        let mut o = head(VCALL, h);
        o.push(cipher << 6 | key_id & 0x3f);
        o.push(0);
        o
    }

    pub fn tx_rel(h: &CallHead) -> Vec<u8> {
        let mut o = head(TX_REL, h);
        o.extend([0, 0]);
        o
    }

    /// VCALL_ASSGN (or _DUP) with a channel number.
    pub fn vcall_assgn(dup: bool, h: &CallHead, timer: u8, channel: u16) -> Vec<u8> {
        let mut o = head(if dup { VCALL_ASSGN_DUP } else { VCALL_ASSGN }, h);
        o.push(timer << 2 | (channel >> 8) as u8 & 3);
        o.push(channel as u8);
        o
    }

    /// VCALL_ASSGN with a DFA channel (12 octets).
    pub fn vcall_assgn_dfa(h: &CallHead, timer: u8, ofn: u16, ifn: u16, bandwidth: u8) -> Vec<u8> {
        let mut o = head(VCALL_ASSGN, h);
        o.push(timer << 2 | bandwidth & 3);
        o.extend([(ofn >> 8) as u8, ofn as u8, (ifn >> 8) as u8, ifn as u8]);
        o
    }

    pub fn idle() -> Vec<u8> {
        let mut o = vec![IDLE];
        o.resize(9, 0);
        o
    }

    /// SITE_INFO (18 octets).
    pub fn site_info(location: u32, service: u16, access: u32, adjacent: u8, control: [u16; 2]) -> Vec<u8> {
        let mut o = vec![SITE_INFO];
        o.extend([(location >> 16) as u8, (location >> 8) as u8, location as u8]);
        o.extend([0x41, 0x11]); // channel structure: 1 BCCH, 1 group, 1 paging frame …
        o.extend([(service >> 8) as u8, service as u8]);
        o.extend([0, 0, 0]);
        o.extend([(access >> 16) as u8, (access >> 8) as u8, access as u8]);
        o.push(1);
        let tail = (adjacent as u32 & 0xf) << 20 | (control[0] as u32 & 0x3ff) << 10 | control[1] as u32 & 0x3ff;
        o.extend([(tail >> 16) as u8, (tail >> 8) as u8, tail as u8]);
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> CallHead {
        CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source: 901, destination: 2001 }
    }

    #[test]
    fn vcall_round_trip() {
        let o = build::vcall(&h(), 3, 5);
        assert_eq!(Message::parse(&o, Context::Traffic, false), Message::VCall { head: h(), cipher: 3, key_id: 5 });
    }

    #[test]
    fn assignment_channel_number_and_dfa() {
        let o = build::vcall_assgn(false, &h(), 4, 0x2a5);
        assert_eq!(Message::parse(&o, Context::Control, false), Message::VCallAssgn { head: h(), timer: 4, channel: Channel::Number(0x2a5) });
        let o = build::vcall_assgn_dfa(&h(), 4, 40_000, 39_000, 0);
        assert_eq!(Message::parse(&o, Context::Control, true), Message::VCallAssgn { head: h(), timer: 4, channel: Channel::Dfa { ofn: 40_000, ifn: 39_000, bandwidth: 0 } });
    }

    #[test]
    fn site_info_fields() {
        // Global category: system 0x123 (10 bits), site 0x045 (12 bits).
        let loc = 0x123 << 12 | 0x045;
        let access = 1 << 23 | 2 << 21 | 3 << 18;
        let o = build::site_info(loc, 0x0300, access, 2, [17, 1023]);
        let Message::SiteInfo { location, service, access, adjacent, control, .. } = Message::parse(&o, Context::Control, false) else { panic!() };
        assert_eq!((location.category(), location.system(), location.site()), (0, 0x123, 0x045));
        assert_eq!(service, 0x0300);
        assert_eq!(access, ChannelAccess { dfa: true, step_hz: 1250, base_hz: 400_000_000 });
        assert_eq!(access.dfa_hz(40_000), Some(450_000_000.0));
        assert_eq!(adjacent, 2);
        assert_eq!(control, [17, 1023]);
    }

    #[test]
    fn location_categories() {
        let regional = Location { raw: 2 << 22 | 0x1234 << 8 | 0x56 };
        assert_eq!((regional.system(), regional.site()), (0x1234, 0x56));
        let local = Location { raw: 1 << 22 | 0x1_2345 << 5 | 0x1a };
        assert_eq!((local.system(), local.site()), (0x1_2345, 0x1a));
    }
}
