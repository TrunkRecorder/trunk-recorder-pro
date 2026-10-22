//! Outbound signalling packets: what a P25 control channel says, as
//! TIA-102.AABC-D defines it (Motorola's and L3Harris's own as AABH does).
//!
//! A message has a single-block form (a TSBK) and, for most, a multi-block
//! one (an AMBTC, [`Mbt`]) that adds what the short form leaves implicit: a
//! separate receive channel, units' home systems. Both decode to the same
//! [`Msg`]. Fields are read by octet, as the spec's figures number them;
//! units are their working IDs (WUIDs), as the short forms give them.
//!
//! Nothing here knows the band plan or what a recorder makes of a message:
//! [`crate::trunk::message`] does.

use super::mbt::Mbt;
use super::tsbk::Tsbk;

/// Manufacturer IDs whose opcodes are their own (BAAC-C §2.3: $00 and $01 are the standard's).
pub const MFID_MOTOROLA: u8 = 0x90;
pub const MFID_L3HARRIS: u8 = 0xa4;

/// A site's FCC call sign as Motorola's base station ID sends it (the
/// ASTRO 25 Base Station Identification Receive Implementation Guide, 1.0):
/// eight 6-bit characters, each its ASCII less 43 ('0'..'9' $05..$0E,
/// 'A'..'Z' $16..$2F), $00 for none. The same in a control channel's
/// MOT_BSI_GRANT, a voice channel's LC_MOT_BSI and a Phase 2 MAC_MOT_BSI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallSign([u8; 8]);

impl CallSign {
    /// From the six octets that carry it (character 1 in the first's top bits).
    pub fn from_octets(b: &[u8]) -> CallSign {
        let v = b[..6].iter().fold(0u64, |v, &x| v << 8 | x as u64);
        let mut c = [0u8; 8];
        for (i, x) in c.iter_mut().enumerate() {
            let n = (v >> (42 - 6 * i) & 0x3f) as u8;
            // (Values the guide doesn't give a character are dropped, as none is.)
            *x = match n {
                0x05..=0x0e | 0x16..=0x2f => n + 43,
                _ => 0,
            };
        }
        CallSign(c)
    }

    /// The call sign ("WPXT459"); empty when it says none.
    pub fn text(&self) -> String {
        self.0.iter().filter(|&&c| c != 0).map(|&c| c as char).collect()
    }
}

/// A channel: a 4-bit identifier (which band plan) and a 12-bit number (§2.3.9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(pub u16);

impl Channel {
    pub fn iden(self) -> u8 {
        (self.0 >> 12) as u8
    }
    pub fn number(self) -> u16 {
        self.0 & 0xfff
    }
}

/// A service's options (§2.3.24; a data grant's E, P, D and M are the same, §2.3.12).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceOptions(pub u8);

impl ServiceOptions {
    pub fn emergency(self) -> bool {
        self.0 & 0x80 != 0
    }
    /// Encrypted.
    pub fn protected(self) -> bool {
        self.0 & 0x40 != 0
    }
    pub fn duplex(self) -> bool {
        self.0 & 0x20 != 0
    }
    /// Packet mode (circuit otherwise).
    pub fn packet(self) -> bool {
        self.0 & 0x10 != 0
    }
    pub fn priority(self) -> u8 {
        self.0 & 7
    }
}

/// A registration's or affiliation's answer (RV, GAV: §6.2.8, §6.2.21).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Accept,
    Fail,
    Deny,
    Refused,
}

impl Answer {
    fn from_bits(v: u8) -> Self {
        [Answer::Accept, Answer::Fail, Answer::Deny, Answer::Refused][(v & 3) as usize]
    }
    pub fn name(self) -> &'static str {
        match self {
            Answer::Accept => "accepted",
            Answer::Fail => "failed",
            Answer::Deny => "denied",
            Answer::Refused => "refused",
        }
    }
}

/// A channel identifier's band plan (IDEN_UP, IDEN_UP_VU, IDEN_UP_TDMA).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Identifier {
    pub iden: u8,
    /// The SU receive (FNE transmit) frequency of channel 0.
    pub base_hz: u64,
    pub spacing_hz: u64,
    /// SU transmit − receive (0: the same, or none given).
    pub tx_offset_hz: i64,
    pub bandwidth_hz: u32,
    /// TDMA slots per carrier (1: FDMA).
    pub slots: u8,
    /// The (WACN, System ID) it is for, when the extended IDEN_UP_TDMA names
    /// one (a neighbouring system's, perhaps); none: this site's.
    pub system: Option<(u32, u16)>,
}

impl Identifier {
    /// A channel's carrier frequency and TDMA slot (§2.3.9.2).
    pub fn resolve(&self, ch: Channel) -> (u64, u8) {
        let slots = self.slots.max(1) as u16;
        let n = ch.number();
        (self.base_hz + self.spacing_hz * (n / slots) as u64, (n % slots) as u8)
    }

    /// Channel Type (§2.3.40, Table 2.3.40-1): bandwidth and slots per carrier.
    fn channel_type(t: u8) -> Option<(u32, u8)> {
        Some(match t {
            0..=1 => (12_500, 1),
            2 => (6_250, 1),
            3 | 5 => (12_500, 2),
            4 => (25_000, 4),
            _ => return None,
        })
    }
}

/// A secondary control channel (SCCB, SCCB_EXP).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Secondary {
    pub ch: Channel,
    pub rx: Option<Channel>,
    /// System Service Class (§2.3.32).
    pub class: u8,
}

/// A site's identity and primary control channel (RFSS_STS_BCST,
/// NET_STS_BCST and ADJ_STS_BCST each state some of it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Site {
    pub lra: u8,
    pub wacn: Option<u32>,
    pub sys_id: u16,
    pub rfss: u8,
    pub site: u8,
    pub ch: Channel,
    pub rx: Option<Channel>,
    /// System Service Class (§2.3.32).
    pub class: u8,
}

/// What a control channel message says.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    // ── Voice (§4.2) ──
    /// GRP_V_CH_GRANT (§4.2.1).
    GroupVoiceGrant { opts: ServiceOptions, ch: Channel, rx: Option<Channel>, group: u16, source: u32 },
    /// GRP_V_CH_GRANT_UPDT (§4.2.2): one or two calls in progress.
    GroupVoiceUpdate { calls: Vec<(Channel, u16)> },
    /// GRP_V_CH_GRANT_UPDT_EXP (§4.2.3).
    GroupVoiceUpdateExplicit { opts: ServiceOptions, ch: Channel, rx: Option<Channel>, group: u16 },
    /// UU_V_CH_GRANT (§4.2.5). Only the extended form has options.
    UnitVoiceGrant { opts: Option<ServiceOptions>, ch: Channel, rx: Option<Channel>, target: u32, source: u32 },
    /// UU_V_CH_GRANT_UPDT (§4.2.8).
    UnitVoiceUpdate { opts: Option<ServiceOptions>, ch: Channel, rx: Option<Channel>, target: u32, source: u32 },
    /// UU_ANS_REQ (§4.2.4).
    UnitAnswerRequest { opts: ServiceOptions, target: u32, source: u32 },
    /// TELE_INT_CH_GRANT (§4.2.6): `unit` the caller, or the called when the
    /// call came from the telephone network. `timer_ds`: the call's allowed
    /// length in tenths of a second (0: none given).
    PhoneGrant { opts: ServiceOptions, ch: Channel, rx: Option<Channel>, timer_ds: u16, unit: u32 },
    /// TELE_INT_CH_GRANT_UPDT (§4.2.9).
    PhoneUpdate { opts: ServiceOptions, ch: Channel, rx: Option<Channel>, timer_ds: u16, unit: u32 },
    /// TELE_INT_ANS_REQ (§4.2.7): the calling number, if given.
    PhoneAnswerRequest { digits: String, target: u32 },
    // ── Data (§5.2) ──
    /// SN-DATA_CHN_GNT (§5.2.5).
    DataGrant { opts: ServiceOptions, ch: Channel, rx: Option<Channel>, target: u32 },
    // ── Control and status (§6.2) ──
    /// ACK_RSP_FNE (§6.2.1), MOT_ACK_RSP_FNE (AABH §4.4.8). `source`: the
    /// other unit, when the message names one.
    Acknowledge { service: u8, source: Option<u32>, target: u32 },
    /// QUE_RSP (§6.2.14), MOT_QUE_RSP (AABH §4.4.6). `info` (when valid): a
    /// group service's options and group, or the other unit.
    Queued { service: u8, reason: u8, info: Option<u32>, target: u32 },
    /// DENY_RSP (§6.2.5), MOT_DENY_RSP (AABH §4.4.7). `own_reason`:
    /// Motorola's reason code, not the standard's (its RC bit).
    Denied { service: u8, reason: u8, own_reason: bool, info: Option<u32>, target: u32 },
    /// EXT_FNCT_CMD (§6.2.6), MOT_EXT_FNCT_CMD (AABH §4.4.5): class,
    /// operand, the 24-bit arguments.
    ExtendedFunction { class: u8, operand: u8, args: u32, target: u32 },
    /// CALL_ALRT (§6.2.4).
    CallAlert { target: u32, source: u32 },
    /// STS_UPDT (§6.2.18): user status (high octet) and unit status.
    StatusUpdate { status: u16, target: u32, source: u32 },
    /// STS_Q (§6.2.17).
    StatusQuery { target: u32, source: u32 },
    /// MSG_UPDT (§6.2.10): a short message.
    MessageUpdate { message: u16, target: u32, source: u32 },
    /// GRP_AFF_Q (§6.2.7).
    AffiliationQuery { target: u32, source: u32 },
    /// GRP_AFF_RSP (§6.2.8).
    Affiliation { answer: Answer, announcement: u16, group: u16, target: u32 },
    /// LOC_REG_RSP (§6.2.23).
    LocationRegistration { answer: Answer, group: u16, rfss: u8, site: u8, target: u32 },
    /// U_REG_RSP (§6.2.21): the unit's WUID, and its ID on its home system.
    Registration { answer: Answer, sys_id: u16, id: u32, unit: u32 },
    /// U_REG_CMD (§6.2.20).
    RegistrationCommand { target: u32, source: u32 },
    /// U_DE_REG_ACK (§6.2.22).
    Deregistration { wacn: u32, sys_id: u16, id: u32 },
    // ── The site ──
    /// IDEN_UP (§6.2.9), IDEN_UP_VU (§6.2.29), IDEN_UP_TDMA (§6.2.32).
    Identifier(Identifier),
    /// RFSS_STS_BCST (§6.2.15). `networked`: the site can reach the rest of
    /// the system (A); off, it is in site trunking.
    RfssStatus { site: Site, networked: bool },
    /// NET_STS_BCST (§6.2.11).
    NetworkStatus(Site),
    /// ADJ_STS_BCST (§6.2.2); `own_plan` false: ADJ_STS_BCST_UNC, whose
    /// channel is on its own system's band plan, not this one's. `wacn`
    /// (extended form): none or 0 means this site's.
    AdjacentStatus { site: Site, own_plan: bool, conventional: bool, failed: bool, valid: bool, networked: bool },
    /// SCCB (§6.2.16), SCCB_EXP (§6.2.28): the entries that are valid (a
    /// System Service Class of $00 marks one that isn't).
    SecondaryControl { rfss: u8, site: u8, channels: Vec<Secondary> },
    /// SYNC_BCST (§6.2.33). `synced`: Phase 2 traffic channels keep the
    /// control channel's burst timing (US clear). `time`: the system's
    /// year, month, day, hour, minute, and the micro-slots (7.5 ms each,
    /// 8000 a minute) into the minute; `time_valid`: IST clear, the system
    /// vouches for it. (Decoded either way: DCFD sets IST with its time
    /// right to the second, so a system's time marked invalid is shown, and
    /// only a valid one is gone by.)
    SyncBroadcast { synced: bool, time: Option<(u16, u8, u8, u8, u8, u16)>, time_valid: bool },
    // ── Motorola (MFID $90, AABH §4.4) ──
    /// MOT_GRG_ADD_CMD: groups (up to 3; 0 unused) patched into a supergroup.
    MotPatchAdd { sg: u16, groups: [u16; 3] },
    /// MOT_GRG_DEL_CMD: groups leaving it (the supergroup itself: dissolved).
    MotPatchDelete { sg: u16, groups: [u16; 3] },
    /// MOT_GRG_CN_GRANT. Its octet 2 has only the P (encrypted) bit.
    MotPatchGrant { protected: bool, ch: Channel, rx: Option<Channel>, sg: u16, source: u32 },
    /// MOT_GRG_CN_GRANT_UPDT.
    MotPatchUpdate { calls: Vec<(Channel, u16)> },
    /// Opcode $0A (not in AABH; as SDRTrunk reads it): a radio's emergency button.
    MotEmergencyAlarm { group: u16, source: u32 },
    /// MOT_BSI_GRANT (opcode $0B; Motorola's BSI receive guide §1.7.1, not
    /// AABH): the site's FCC call sign, and the channel it's sent on (in
    /// Morse code, or this control channel).
    MotBaseStationId { callsign: String, ch: Channel },
    // ── L3Harris (MFID $A4) ──
    /// GRG_EXENC_CMD (AABH §4.3.2.1): a supergroup's member, a talkgroup
    /// (with its algorithm and key) or a radio, added or removed.
    HarrisRegroup { simulselect: bool, sg: u16, member: HarrisMember, activate: bool, key_id: u16 },
    // ── Anything else ──
    /// Encrypted arguments (the P bit): not readable.
    Protected,
    /// A message this module names but doesn't read.
    Other(&'static str),
    Unknown,
}

/// What an L3Harris regroup adds or removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarrisMember {
    Talkgroup { group: u16, algid: u8 },
    Unit(u32),
}

/// One message: who defined its opcode (`mfid`), the opcode, what it says.
#[derive(Clone, Debug, PartialEq)]
pub struct Osp {
    pub mfid: u8,
    pub opcode: u8,
    pub msg: Msg,
}

impl Osp {
    /// The standard's message ($00 and $01: BAAC-C §2.3), or the manufacturer's.
    pub fn standard(&self) -> bool {
        self.mfid <= 1
    }
}

/// Fields by octet, big-endian, as the spec's figures number them.
#[derive(Clone, Copy)]
struct Octets<'a>(&'a [u8]);

impl Octets<'_> {
    fn u8(&self, i: usize) -> u8 {
        self.0[i]
    }
    fn bit(&self, i: usize, b: u8) -> bool {
        self.0[i] >> b & 1 != 0
    }
    fn u16(&self, i: usize) -> u16 {
        u16::from_be_bytes([self.0[i], self.0[i + 1]])
    }
    fn u24(&self, i: usize) -> u32 {
        (self.0[i] as u32) << 16 | (self.0[i + 1] as u32) << 8 | self.0[i + 2] as u32
    }
    fn u32(&self, i: usize) -> u32 {
        u32::from_be_bytes([self.0[i], self.0[i + 1], self.0[i + 2], self.0[i + 3]])
    }
    fn ch(&self, i: usize) -> Channel {
        Channel(self.u16(i))
    }
    /// A receive channel: $FFFF means none (§2.3.9).
    fn rx(&self, i: usize) -> Option<Channel> {
        Some(self.ch(i)).filter(|c| c.0 != 0xffff)
    }
    fn opts(&self, i: usize) -> ServiceOptions {
        ServiceOptions(self.u8(i))
    }
    /// A 20-bit WACN and 12-bit System ID, from octet `i`.
    fn system(&self, i: usize) -> (u32, u16) {
        let v = self.u32(i);
        (v >> 12, (v & 0xfff) as u16)
    }
}

/// A unit's WUID, or (when it has none here: "No unit", §2.3.30) its home ID.
fn unit(wuid: u32, id: u32) -> u32 {
    if wuid != 0 { wuid } else { id }
}

/// Two (channel, group) pairs, as one call when they are the same (AABC
/// doesn't say a lone call fills both; sites repeat it, or leave $FFFF).
fn calls(a: (Channel, u16), b: (Channel, u16)) -> Vec<(Channel, u16)> {
    let mut v = vec![a];
    if b != a && b.0 .0 != 0xffff && b.1 != 0 {
        v.push(b);
    }
    v
}

/// Up to three groups (0 unused).
fn groups3(o: Octets, i: usize) -> [u16; 3] {
    [o.u16(i), o.u16(i + 2), o.u16(i + 4)]
}

/// The band plan of IDEN_UP (§6.2.9: 9-bit bandwidth × 125 Hz, 9-bit offset
/// × 250 kHz) or IDEN_UP_VU / _TDMA (§6.2.29, §6.2.32: bandwidth or channel
/// type, 14-bit offset × the spacing). `bits`: identifier, type and offset,
/// spacing (the TSBK's octets 2..5). The offset's top bit is its sign, set
/// for a transmit frequency above the receive.
fn identifier(opcode: u8, bits: u32, base: u32, system: Option<(u32, u16)>) -> Option<Identifier> {
    let iden = (bits >> 28) as u8;
    let spacing_hz = (bits & 0x3ff) as u64 * 125;
    let signed = |v: u32, width: u32, unit: i64| {
        let mag = (v & ((1 << (width - 1)) - 1)) as i64 * unit;
        if v >> (width - 1) & 1 != 0 { mag } else { -mag }
    };
    let (bandwidth_hz, slots, tx_offset_hz) = match opcode {
        0x3d => ((bits >> 19 & 0x1ff) * 125, 1, signed(bits >> 10 & 0x1ff, 9, 250_000)),
        0x34 => {
            let bw = match bits >> 24 & 0xf {
                4 => 6_250,
                5 => 12_500,
                _ => 0,
            };
            (bw, 1, signed(bits >> 10 & 0x3fff, 14, spacing_hz as i64))
        }
        _ => {
            let (bw, slots) = Identifier::channel_type((bits >> 24 & 0xf) as u8)?;
            (bw, slots, signed(bits >> 10 & 0x3fff, 14, spacing_hz as i64))
        }
    };
    Some(Identifier { iden, base_hz: base as u64 * 5, spacing_hz, tx_offset_hz, bandwidth_hz, slots, system })
}

/// TELE_INT_ANS_REQ's ten 4-bit digits (Table 2.3.13-1; $F, escape, ends them).
fn digits(o: Octets, from: usize) -> String {
    let mut s = String::new();
    for i in 0..10 {
        let d = o.u8(from + i / 2) >> if i % 2 == 0 { 4 } else { 0 } & 0xf;
        match d {
            0..=9 => s.push((b'0' + d) as char),
            10 => s.push('*'),
            11 => s.push('#'),
            15 => break,
            _ => {}
        }
    }
    s
}

/// A CRC-valid TSBK.
pub fn from_tsbk(t: &Tsbk) -> Osp {
    let o = Octets(&t[..10]);
    let (opcode, mfid) = (t[0] & 0x3f, t[1]);
    let msg = if t[0] & 0x40 != 0 { Msg::Protected } else { tsbk_msg(o, opcode, mfid) };
    Osp { mfid, opcode, msg }
}

fn tsbk_msg(o: Octets, opcode: u8, mfid: u8) -> Msg {
    use Msg::*;
    let vendor = if mfid <= 1 { 0 } else { mfid };
    match (vendor, opcode) {
        (0, 0x00) => GroupVoiceGrant { opts: o.opts(2), ch: o.ch(3), rx: None, group: o.u16(5), source: o.u24(7) },
        (0, 0x02) => GroupVoiceUpdate { calls: calls((o.ch(2), o.u16(4)), (o.ch(6), o.u16(8))) },
        (0, 0x03) => GroupVoiceUpdateExplicit { opts: o.opts(2), ch: o.ch(4), rx: o.rx(6), group: o.u16(8) },
        (0, 0x04) => UnitVoiceGrant { opts: None, ch: o.ch(2), rx: None, target: o.u24(4), source: o.u24(7) },
        (0, 0x05) => UnitAnswerRequest { opts: o.opts(2), target: o.u24(4), source: o.u24(7) },
        (0, 0x06) => UnitVoiceUpdate { opts: None, ch: o.ch(2), rx: None, target: o.u24(4), source: o.u24(7) },
        (0, 0x08) => PhoneGrant { opts: o.opts(2), ch: o.ch(3), rx: None, timer_ds: o.u16(5), unit: o.u24(7) },
        (0, 0x09) => PhoneUpdate { opts: o.opts(2), ch: o.ch(3), rx: None, timer_ds: o.u16(5), unit: o.u24(7) },
        (0, 0x0a) => PhoneAnswerRequest { digits: digits(o, 2), target: o.u24(7) },
        (0, 0x14) => DataGrant { opts: o.opts(2), ch: o.ch(3), rx: o.rx(5), target: o.u24(7) },
        (0, 0x16) => Other("SN-DATA_CHN_ANN_EXP"),
        (0, 0x18) => StatusUpdate { status: o.u16(2), target: o.u24(4), source: o.u24(7) },
        (0, 0x1a) => StatusQuery { target: o.u24(4), source: o.u24(7) },
        (0, 0x1c) => MessageUpdate { message: o.u16(2), target: o.u24(4), source: o.u24(7) },
        (0, 0x1d) => Other("RAD_MON_CMD"),
        (0, 0x1e) => Other("RAD_MON_ENH_CMD"),
        (0, 0x1f) => CallAlert { target: o.u24(4), source: o.u24(7) },
        // AIV, then EX: the additional information is the target's home
        // system (EX), or the other unit.
        (0, 0x20) => Acknowledge { service: o.u8(2) & 0x3f, source: (o.bit(2, 7) && !o.bit(2, 6)).then(|| o.u24(4)), target: o.u24(7) },
        (0, 0x21) => Queued { service: o.u8(2) & 0x3f, reason: o.u8(3), info: o.bit(2, 7).then(|| o.u24(4)), target: o.u24(7) },
        (0, 0x24) => ExtendedFunction { class: o.u8(2), operand: o.u8(3), args: o.u24(4), target: o.u24(7) },
        (0, 0x27) => Denied { service: o.u8(2) & 0x3f, reason: o.u8(3), own_reason: false, info: o.bit(2, 7).then(|| o.u24(4)), target: o.u24(7) },
        (0, 0x28) => Affiliation { answer: Answer::from_bits(o.u8(2)), announcement: o.u16(3), group: o.u16(5), target: o.u24(7) },
        (0, 0x29) => SecondaryControl { rfss: o.u8(2), site: o.u8(3), channels: sccb(&[Secondary { ch: o.ch(4), rx: o.rx(7), class: o.u8(9) }]) },
        (0, 0x2a) => AffiliationQuery { target: o.u24(4), source: o.u24(7) },
        (0, 0x2b) => LocationRegistration { answer: Answer::from_bits(o.u8(2)), group: o.u16(3), rfss: o.u8(5), site: o.u8(6), target: o.u24(7) },
        (0, 0x2c) => Registration { answer: Answer::from_bits(o.u8(2) >> 4), sys_id: o.u16(2) & 0xfff, id: o.u24(4), unit: o.u24(7) },
        (0, 0x2d) => RegistrationCommand { target: o.u24(4), source: o.u24(7) },
        (0, 0x2f) => {
            let (wacn, sys_id) = o.system(3);
            Deregistration { wacn, sys_id, id: o.u24(7) }
        }
        (0, 0x30) => SyncBroadcast { synced: !o.bit(3, 3), time: Some(sync_time(o)).filter(|t| t.0 > 2000 || t.1 > 0), time_valid: !o.bit(3, 2) },
        (0, 0x33 | 0x34 | 0x3d) => identifier(opcode, o.u32(2), o.u32(6), None).map_or(Unknown, Identifier),
        (0, 0x35) => Other("TIME_DATE_ANN"),
        (0, 0x36) => Other("ROAM_ADDR_CMD"),
        (0, 0x37) => Other("ROAM_ADDR_UPDT"),
        (0, 0x38) => Other("SYS_SRV_BCST"),
        (0, 0x39) => {
            let a = Secondary { ch: o.ch(4), rx: None, class: o.u8(6) };
            let b = Secondary { ch: o.ch(7), rx: None, class: o.u8(9) };
            SecondaryControl { rfss: o.u8(2), site: o.u8(3), channels: sccb(&[a, b]) }
        }
        (0, 0x3a) => {
            let site = Site { lra: o.u8(2), wacn: None, sys_id: o.u16(3) & 0xfff, rfss: o.u8(5), site: o.u8(6), ch: o.ch(7), rx: None, class: o.u8(9) };
            RfssStatus { site, networked: o.bit(3, 4) }
        }
        (0, 0x3b) => {
            let (wacn, sys_id) = o.system(3);
            NetworkStatus(Site { lra: o.u8(2), wacn: Some(wacn), sys_id, rfss: 0, site: 0, ch: o.ch(7), rx: None, class: o.u8(9) })
        }
        (0, 0x3c) => {
            let site = Site { lra: o.u8(2), wacn: None, sys_id: o.u16(3) & 0xfff, rfss: o.u8(5), site: o.u8(6), ch: o.ch(7), rx: None, class: o.u8(9) };
            AdjacentStatus { site, own_plan: true, conventional: o.bit(3, 7), failed: o.bit(3, 6), valid: o.bit(3, 5), networked: o.bit(3, 4) }
        }
        (MFID_MOTOROLA, 0x00) => MotPatchAdd { sg: o.u16(2), groups: groups3(o, 4) },
        (MFID_MOTOROLA, 0x01) => MotPatchDelete { sg: o.u16(2), groups: groups3(o, 4) },
        (MFID_MOTOROLA, 0x02) => MotPatchGrant { protected: o.bit(2, 6), ch: o.ch(3), rx: None, sg: o.u16(5), source: o.u24(7) },
        (MFID_MOTOROLA, 0x03) => MotPatchUpdate { calls: calls((o.ch(2), o.u16(4)), (o.ch(6), o.u16(8))) },
        (MFID_MOTOROLA, 0x04) => ExtendedFunction { class: o.u8(2), operand: o.u8(3), args: o.u24(4), target: o.u24(7) },
        (MFID_MOTOROLA, 0x05) => Other("MOT_TRAFFIC_CHANNEL"),
        (MFID_MOTOROLA, 0x06) => Queued { service: o.u8(2) & 0x3f, reason: o.u8(3), info: o.bit(2, 7).then(|| o.u24(4)), target: o.u24(7) },
        // No AIV: invalid additional information is zero.
        (MFID_MOTOROLA, 0x07) => Denied { service: o.u8(2) & 0x3f, reason: o.u8(3), own_reason: o.bit(2, 7), info: Some(o.u24(4)).filter(|&i| i != 0), target: o.u24(7) },
        (MFID_MOTOROLA, 0x08) => Acknowledge { service: o.u8(2) & 0x3f, source: Some(o.u24(4)), target: o.u24(7) },
        (MFID_MOTOROLA, 0x0a) => MotEmergencyAlarm { group: o.u16(5), source: o.u24(7) },
        (MFID_MOTOROLA, 0x0b) => {
            let b: Vec<u8> = (2..8).map(|i| o.u8(i)).collect();
            MotBaseStationId { callsign: CallSign::from_octets(&b).text(), ch: o.ch(8) }
        }
        (MFID_L3HARRIS, 0x30) => {
            // GRG_Options: simulselect, talkgroup (G), activate (A), sequence.
            let opts = o.u8(2);
            let member = if opts & 0x40 != 0 { HarrisMember::Talkgroup { group: o.u16(8), algid: o.u8(7) } } else { HarrisMember::Unit(o.u24(7)) };
            HarrisRegroup { simulselect: opts & 0x80 != 0, sg: o.u16(3), member, activate: opts & 0x20 != 0, key_id: o.u16(5) }
        }
        _ => Unknown,
    }
}

/// SYNC_BCST's date and time (Figure 6.2.33-1): year (since 2000, 7 bits),
/// month (4), day (5), hours (5), minutes (6) and micro-slots (13), from
/// octet 5 to the end of octet 9.
fn sync_time(o: Octets) -> (u16, u8, u8, u8, u8, u16) {
    let v = o.u32(5);
    (2000 + (v >> 25) as u16, (v >> 21 & 0xf) as u8, (v >> 16 & 0x1f) as u8, (v >> 11 & 0x1f) as u8, (v >> 5 & 0x3f) as u8, ((v & 0x1f) << 8 | o.0[9] as u32) as u16)
}

/// SCCB entries, less those a System Service Class of $00 marks invalid.
fn sccb(entries: &[Secondary]) -> Vec<Secondary> {
    let mut v: Vec<Secondary> = entries.iter().copied().filter(|e| e.class != 0).collect();
    v.dedup();
    v
}

/// A CRC-valid AMBTC: the header's [3..6] is an address (a source or a
/// target), [8..10] two octets of arguments; `d` the data blocks' octets
/// (the second block from 12).
pub fn from_mbt(m: &Mbt) -> Osp {
    let (opcode, mfid) = (m.opcode(), m.mfid());
    let need = match (mfid, opcode) {
        (0, 0x04 | 0x06) => 20,
        (0, 0x28) => 12,
        _ => 8,
    };
    let msg = if m.data.len() < need {
        Msg::Unknown
    } else if m.header[1] & 0x3f == 0x3f {
        Msg::Protected
    } else {
        mbt_msg(Octets(&m.header), Octets(&m.data), opcode, mfid)
    };
    Osp { mfid, opcode, msg }
}

fn mbt_msg(h: Octets, d: Octets, opcode: u8, mfid: u8) -> Msg {
    use Msg::*;
    let vendor = if mfid <= 1 { 0 } else { mfid };
    match (vendor, opcode) {
        // Header: the source. Data: reserved, Channel (T), (R), group.
        (0, 0x00) => GroupVoiceGrant { opts: h.opts(8), ch: d.ch(2), rx: d.rx(4), group: d.u16(6), source: h.u24(3) },
        // Header: the source's WUID. Data: its SUID (WACN, System ID, ID),
        // the target's WUID, Channel (T); then (R), the target's SUID.
        (0, 0x04 | 0x06) => {
            let (ch, rx, target, source) = (d.ch(10), d.rx(12), unit(d.u24(7), d.u24(17)), unit(h.u24(3), d.u24(4)));
            let opts = Some(h.opts(8));
            if opcode == 0x04 { UnitVoiceGrant { opts, ch, rx, target, source } } else { UnitVoiceUpdate { opts, ch, rx, target, source } }
        }
        // Header: the target. Data: reserved, the source's WACN and System ID, its ID.
        (0, 0x05) => UnitAnswerRequest { opts: h.opts(8), target: h.u24(3), source: d.u24(5) },
        // Header: the unit. Data: Channel (T), (R), the call timer.
        (0, 0x08 | 0x09) => {
            let (opts, ch, rx, timer_ds, unit) = (h.opts(8), d.ch(0), d.rx(2), d.u16(4), h.u24(3));
            if opcode == 0x08 { PhoneGrant { opts, ch, rx, timer_ds, unit } } else { PhoneUpdate { opts, ch, rx, timer_ds, unit } }
        }
        // Header: the target. Data: the source's WACN (from header octet 8) and System ID, its ID.
        (0, 0x1f) => CallAlert { target: h.u24(3), source: d.u24(2) },
        // Header: the target. Data: LG/GAV last, after the group's home and the groups.
        (0, 0x28) => Affiliation { answer: Answer::from_bits(d.u8(8)), announcement: d.u16(4), group: d.u16(6), target: h.u24(3) },
        // Header: the unit's WUID. Data: its home system and ID, RV.
        (0, 0x2c) => Registration { answer: Answer::from_bits(d.u8(5)), sys_id: d.u16(0) & 0xfff, id: d.u24(2), unit: h.u24(3) },
        // Header: identifier and type, WACN, System ID. Data: base, offset and spacing.
        (0, 0x33) => {
            let wacn = (h.u16(4) as u32) << 4 | (h.u8(8) >> 4) as u32;
            let sys_id = h.u16(8) & 0xfff;
            let bits = (h.u8(3) as u32) << 24 | d.u24(4);
            identifier(0x33, bits, d.u32(0), Some((wacn, sys_id))).map_or(Unknown, Identifier)
        }
        // Header: LRA, R/A and System ID. Data: RFSS, site, Channel (T), (R), class.
        (0, 0x3a) => {
            let site = Site { lra: h.u8(3), wacn: None, sys_id: h.u16(4) & 0xfff, rfss: d.u8(0), site: d.u8(1), ch: d.ch(2), rx: d.rx(4), class: d.u8(6) };
            RfssStatus { site, networked: h.bit(4, 4) }
        }
        // Header: LRA, System ID. Data: WACN, Channel (T), (R), class.
        (0, 0x3b) => {
            let wacn = d.u24(0) >> 4;
            NetworkStatus(Site { lra: h.u8(3), wacn: Some(wacn), sys_id: h.u16(4) & 0xfff, rfss: 0, site: 0, ch: d.ch(3), rx: d.rx(5), class: d.u8(7) })
        }
        // Header: LRA, flags and System ID, RFSS, site. Data: Channel (T),
        // (R), class, WACN (0: this site's).
        (0, 0x3c | 0x3e) => {
            let wacn = Some(d.u24(5) >> 4).filter(|&w| w != 0);
            let site = Site { lra: h.u8(3), wacn, sys_id: h.u16(4) & 0xfff, rfss: h.u8(8), site: h.u8(9), ch: d.ch(0), rx: d.rx(2), class: d.u8(4) };
            AdjacentStatus { site, own_plan: opcode == 0x3c, conventional: h.bit(4, 7), failed: h.bit(4, 6), valid: h.bit(4, 5), networked: h.bit(4, 4) }
        }
        (0, 0x14) => Other("SN-DATA_CHN_GNT"),
        (0, 0x16) => Other("SN-DATA_CHN_ANN_EXP"),
        (0, 0x18) => Other("STS_UPDT"),
        (0, 0x1a) => Other("STS_Q"),
        (0, 0x1c) => Other("MSG_UPDT"),
        (0, 0x20) => Other("ACK_RSP_FNE"),
        (0, 0x24) => Other("EXT_FNCT_CMD"),
        (0, 0x36 | 0x37) => Other("ROAM_ADDR"),
        // Header: the source. Data: Channel (T), (R), the supergroup (AABH Figure 4-8).
        (MFID_MOTOROLA, 0x02) => MotPatchGrant { protected: h.bit(8, 6), ch: d.ch(0), rx: d.rx(2), sg: d.u16(4), source: h.u24(3) },
        _ => Unknown,
    }
}

impl Msg {
    /// The spec's name for it (for logs).
    pub fn alias(&self) -> &'static str {
        use Msg::*;
        match self {
            GroupVoiceGrant { .. } => "GRP_V_CH_GRANT",
            GroupVoiceUpdate { .. } => "GRP_V_CH_GRANT_UPDT",
            GroupVoiceUpdateExplicit { .. } => "GRP_V_CH_GRANT_UPDT_EXP",
            UnitVoiceGrant { .. } => "UU_V_CH_GRANT",
            UnitVoiceUpdate { .. } => "UU_V_CH_GRANT_UPDT",
            UnitAnswerRequest { .. } => "UU_ANS_REQ",
            PhoneGrant { .. } => "TELE_INT_CH_GRANT",
            PhoneUpdate { .. } => "TELE_INT_CH_GRANT_UPDT",
            PhoneAnswerRequest { .. } => "TELE_INT_ANS_REQ",
            DataGrant { .. } => "SN-DATA_CHN_GNT",
            Acknowledge { .. } => "ACK_RSP_FNE",
            Queued { .. } => "QUE_RSP",
            Denied { .. } => "DENY_RSP",
            ExtendedFunction { .. } => "EXT_FNCT_CMD",
            CallAlert { .. } => "CALL_ALRT",
            StatusUpdate { .. } => "STS_UPDT",
            StatusQuery { .. } => "STS_Q",
            MessageUpdate { .. } => "MSG_UPDT",
            AffiliationQuery { .. } => "GRP_AFF_Q",
            Affiliation { .. } => "GRP_AFF_RSP",
            LocationRegistration { .. } => "LOC_REG_RSP",
            Registration { .. } => "U_REG_RSP",
            RegistrationCommand { .. } => "U_REG_CMD",
            Deregistration { .. } => "U_DE_REG_ACK",
            Identifier(_) => "IDEN_UP",
            RfssStatus { .. } => "RFSS_STS_BCST",
            NetworkStatus(_) => "NET_STS_BCST",
            AdjacentStatus { .. } => "ADJ_STS_BCST",
            SecondaryControl { .. } => "SCCB",
            SyncBroadcast { .. } => "SYNC_BCST",
            MotPatchAdd { .. } => "MOT_GRG_ADD_CMD",
            MotPatchDelete { .. } => "MOT_GRG_DEL_CMD",
            MotPatchGrant { .. } => "MOT_GRG_CN_GRANT",
            MotPatchUpdate { .. } => "MOT_GRG_CN_GRANT_UPDT",
            MotEmergencyAlarm { .. } => "MOT_EMERG_ALRM",
            MotBaseStationId { .. } => "MOT_BSI",
            HarrisRegroup { .. } => "GRG_EXENC_CMD",
            Protected => "protected",
            Other(name) => name,
            Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::mbt::tests::{ambtc_header, pdu_bits};
    use crate::p25::mbt::{decode, Pdu};

    /// A TSBK: opcode `op` (last block) from `mfid`, octets 2..10 `args`.
    fn tsbk(op: u8, mfid: u8, args: [u8; 8]) -> Osp {
        let mut t = [0u8; 12];
        t[0] = 0x80 | op;
        t[1] = mfid;
        t[2..10].copy_from_slice(&args);
        from_tsbk(&t)
    }

    /// An AMBTC: opcode `op` from `mfid`, header octets 3..6 `addr` and
    /// 8..10 `tail`, data `body`, through the PDU decoder.
    fn mbt(op: u8, mfid: u8, addr: [u8; 3], tail: [u8; 2], body: &[u8]) -> Osp {
        let mut h = ambtc_header(op, addr, tail);
        h[2] = mfid;
        let Pdu::Mbt(m) = decode(std::iter::once((&pdu_bits(h, body)[..], None))) else { panic!("no MBT 0x{op:02x}") };
        from_mbt(&m)
    }

    // Each test builds the message as its figure lays it out, octet by octet.

    /// Figure 4.2.1-1, 4.2.1-2.
    #[test]
    fn group_voice_grant() {
        let want = Msg::GroupVoiceGrant { opts: ServiceOptions(0xc4), ch: Channel(0x1234), rx: None, group: 0x0101, source: 0x0a0b0c };
        assert_eq!(tsbk(0x00, 0, [0xc4, 0x12, 0x34, 0x01, 0x01, 0x0a, 0x0b, 0x0c]).msg, want);
        let m = mbt(0x00, 0, [0x0a, 0x0b, 0x0c], [0xc4, 0], &[0, 0, 0x12, 0x34, 0x56, 0x78, 0x01, 0x01]);
        assert_eq!(m.msg, Msg::GroupVoiceGrant { opts: ServiceOptions(0xc4), ch: Channel(0x1234), rx: Some(Channel(0x5678)), group: 0x0101, source: 0x0a0b0c });
    }

    /// Figure 4.2.2-1: two calls, or one (repeated).
    #[test]
    fn group_voice_update() {
        let two = tsbk(0x02, 0, [0x10, 0x02, 0x01, 0x01, 0x10, 0x03, 0x02, 0x02]);
        assert_eq!(two.msg, Msg::GroupVoiceUpdate { calls: vec![(Channel(0x1002), 0x0101), (Channel(0x1003), 0x0202)] });
        let one = tsbk(0x02, 0, [0x10, 0x02, 0x01, 0x01, 0x10, 0x02, 0x01, 0x01]);
        assert_eq!(one.msg, Msg::GroupVoiceUpdate { calls: vec![(Channel(0x1002), 0x0101)] });
    }

    /// Figure 4.2.3-1.
    #[test]
    fn group_voice_update_explicit() {
        let m = tsbk(0x03, 0, [0x40, 0, 0x10, 0x01, 0xff, 0xff, 0x01, 0x01]);
        assert_eq!(m.msg, Msg::GroupVoiceUpdateExplicit { opts: ServiceOptions(0x40), ch: Channel(0x1001), rx: None, group: 0x0101 });
    }

    /// Figure 4.2.5-1 has no service options: octets 2-3 are the channel.
    /// (Iden 0xC set its top bits, which were read as emergency and
    /// encrypted.) Figure 4.2.5-2's are in the header.
    #[test]
    fn unit_voice_grant() {
        let m = tsbk(0x04, 0, [0xc0, 0x01, 0x04, 0x05, 0x06, 0x01, 0x02, 0x03]);
        assert_eq!(m.msg, Msg::UnitVoiceGrant { opts: None, ch: Channel(0xc001), rx: None, target: 0x040506, source: 0x010203 });
        // Source WUID 0x0a0b0c (header), its SUID (BEE00.3A9.010203), target
        // WUID 0x040506, Channel (T) 0x1001; (R) 0x1001, target's SUID.
        let body = [0xbe, 0xe0, 0x03, 0xa9, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x10, 0x01, 0x10, 0x01, 0xe0, 0x03, 0xa9, 0x04, 0x05, 0x06];
        for (op, update) in [(0x04, false), (0x06, true)] {
            let m = mbt(op, 0, [0x0a, 0x0b, 0x0c], [0x40, 0], &body);
            let (opts, ch, rx, target, source) = (Some(ServiceOptions(0x40)), Channel(0x1001), Some(Channel(0x1001)), 0x040506, 0x0a0b0c);
            let want = if update { Msg::UnitVoiceUpdate { opts, ch, rx, target, source } } else { Msg::UnitVoiceGrant { opts, ch, rx, target, source } };
            assert_eq!(m.msg, want, "0x{op:02x}");
        }
        // No WUID here ("No unit"): the home ID.
        let m = mbt(0x04, 0, [0, 0, 0], [0, 0], &body);
        assert!(matches!(m.msg, Msg::UnitVoiceGrant { source: 0x010203, .. }), "{:?}", m.msg);
    }

    /// Figure 4.2.6-1, 4.2.6-2, 4.2.9-1.
    #[test]
    fn phone_grant() {
        let m = tsbk(0x08, 0, [0x04, 0x10, 0x01, 0x02, 0x58, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::PhoneGrant { opts: ServiceOptions(0x04), ch: Channel(0x1001), rx: None, timer_ds: 600, unit: 0x0a0b0c });
        let m = tsbk(0x09, 0, [0x04, 0x10, 0x01, 0, 0, 0x0a, 0x0b, 0x0c]);
        assert!(matches!(m.msg, Msg::PhoneUpdate { unit: 0x0a0b0c, timer_ds: 0, .. }));
        let m = mbt(0x08, 0, [0x0a, 0x0b, 0x0c], [0x04, 0], &[0x10, 0x01, 0x20, 0x01, 0x02, 0x58, 0, 0]);
        assert_eq!(m.msg, Msg::PhoneGrant { opts: ServiceOptions(0x04), ch: Channel(0x1001), rx: Some(Channel(0x2001)), timer_ds: 600, unit: 0x0a0b0c });
    }

    /// Figure 4.2.7-1: digits, $F ending them.
    #[test]
    fn phone_answer_request() {
        let m = tsbk(0x0a, 0, [0x55, 0x51, 0x23, 0x4f, 0xff, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::PhoneAnswerRequest { digits: "5551234".into(), target: 0x0a0b0c });
    }

    /// Figure 5.2.5-1.
    #[test]
    fn data_grant() {
        let m = tsbk(0x14, 0, [0x10, 0x10, 0x01, 0x20, 0x01, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::DataGrant { opts: ServiceOptions(0x10), ch: Channel(0x1001), rx: Some(Channel(0x2001)), target: 0x0a0b0c });
    }

    /// Figure 6.2.1-1 to 6.2.1-3: the additional information is a source
    /// only with AIV set and EX clear.
    #[test]
    fn acknowledge() {
        let src = tsbk(0x20, 0, [0x80 | 0x04, 0, 0x01, 0x02, 0x03, 0x0a, 0x0b, 0x0c]);
        assert_eq!(src.msg, Msg::Acknowledge { service: 0x04, source: Some(0x010203), target: 0x0a0b0c });
        let ex = tsbk(0x20, 0, [0xc0 | 0x04, 0xbe, 0xe0, 0x03, 0xa9, 0x0a, 0x0b, 0x0c]);
        assert_eq!(ex.msg, Msg::Acknowledge { service: 0x04, source: None, target: 0x0a0b0c });
        let none = tsbk(0x20, 0, [0x04, 0, 0x01, 0x02, 0x03, 0x0a, 0x0b, 0x0c]);
        assert_eq!(none.msg, Msg::Acknowledge { service: 0x04, source: None, target: 0x0a0b0c });
    }

    /// Figure 6.2.5-1, 6.2.14-1: AIV clear, the information is not valid.
    #[test]
    fn deny_and_queued() {
        let m = tsbk(0x27, 0, [0x80, 0x31, 0x00, 0x01, 0x01, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::Denied { service: 0, reason: 0x31, own_reason: false, info: Some(0x000101), target: 0x0a0b0c });
        let m = tsbk(0x21, 0, [0x00, 0x40, 0x00, 0x01, 0x01, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::Queued { service: 0, reason: 0x40, info: None, target: 0x0a0b0c });
        // AABH Figure 4-12: RC (a Motorola reason), no AIV.
        let m = tsbk(0x07, MFID_MOTOROLA, [0x80, 0x01, 0x00, 0x01, 0x01, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::Denied { service: 0, reason: 1, own_reason: true, info: Some(0x000101), target: 0x0a0b0c });
    }

    /// Figure 6.2.8-1: GAV in octet 2's low bits; the announcement group, then the group.
    #[test]
    fn affiliation() {
        let m = tsbk(0x28, 0, [0x03, 0x00, 0x09, 0x01, 0x01, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::Affiliation { answer: Answer::Refused, announcement: 9, group: 0x0101, target: 0x0a0b0c });
        let m = mbt(0x28, 0, [0x0a, 0x0b, 0x0c], [0, 0], &[0xe0, 0x3a, 0, 0, 0, 9, 0x01, 0x01, 0, 0, 0, 0]);
        assert_eq!(m.msg, Msg::Affiliation { answer: Answer::Accept, announcement: 9, group: 0x0101, target: 0x0a0b0c });
    }

    /// Figure 6.2.23-1: RV in octet 2 bits 1-0.
    #[test]
    fn location_registration() {
        let m = tsbk(0x2b, 0, [0x02, 0x01, 0x01, 1, 7, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::LocationRegistration { answer: Answer::Deny, group: 0x0101, rfss: 1, site: 7, target: 0x0a0b0c });
    }

    /// Figure 6.2.21-1: RV in octet 2 bits 5-4, System ID, Source ID, Source Address.
    #[test]
    fn registration() {
        let m = tsbk(0x2c, 0, [0x13, 0xa9, 0x01, 0x02, 0x03, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::Registration { answer: Answer::Fail, sys_id: 0x3a9, id: 0x010203, unit: 0x0a0b0c });
        let m = mbt(0x2c, 0, [0x0a, 0x0b, 0x0c], [0, 0], &[0xe3, 0xa9, 0x01, 0x02, 0x03, 0, 0, 0]);
        assert_eq!(m.msg, Msg::Registration { answer: Answer::Accept, sys_id: 0x3a9, id: 0x010203, unit: 0x0a0b0c });
    }

    /// Figure 6.2.22-1.
    #[test]
    fn deregistration() {
        let m = tsbk(0x2f, 0, [0, 0xbe, 0xe0, 0x03, 0xa9, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::Deregistration { wacn: 0xbee00, sys_id: 0x3a9, id: 0x0a0b0c });
    }

    /// Annex F's example: identifier 0, 12.5 kHz wide (BW $64), SU transmit
    /// 45 MHz below (offset %0 1011 0100), 6.25 kHz spacing ($32), base
    /// 851.00625 MHz: channel 9 is 851.0625 MHz.
    #[test]
    fn annex_f_identifier() {
        let bits = 0x0u32 << 28 | 0x64 << 19 | 0b0_1011_0100 << 10 | 0x32;
        let mut a = [0u8; 8];
        a[..4].copy_from_slice(&bits.to_be_bytes());
        a[4..].copy_from_slice(&170_201_250u32.to_be_bytes());
        let Msg::Identifier(i) = tsbk(0x3d, 0, a).msg else { panic!() };
        assert_eq!((i.iden, i.bandwidth_hz, i.tx_offset_hz, i.spacing_hz, i.base_hz, i.slots), (0, 12_500, -45_000_000, 6_250, 851_006_250, 1));
        assert_eq!(i.resolve(Channel(9)), (851_062_500, 0));
    }

    /// §2.3.40 and §2.3.9.2: a 4-slot carrier's slot is the channel MOD 4,
    /// its frequency the channel DIV 4; a 25 kHz bandwidth.
    #[test]
    fn tdma_identifier() {
        // Identifier 2, type 4 (4 slots), offset +5 × spacing, spacing 12.5 kHz.
        let bits = 2u32 << 28 | 4 << 24 | (1 << 13 | 5) << 10 | 100;
        let mut a = [0u8; 8];
        a[..4].copy_from_slice(&bits.to_be_bytes());
        a[4..].copy_from_slice(&(769_006_250u32 / 5).to_be_bytes());
        let Msg::Identifier(i) = tsbk(0x33, 0, a).msg else { panic!() };
        assert_eq!((i.slots, i.bandwidth_hz, i.tx_offset_hz), (4, 25_000, 62_500));
        assert_eq!(i.resolve(Channel(0x2000 | 7)), (769_006_250 + 12_500, 3));
        // IDEN_UP_VU: BW VU %0101 is 12.5 kHz.
        let Msg::Identifier(i) = tsbk(0x34, 0, [0x15, 0x00, 0x00, 0x64, 0, 0, 0, 1]).msg else { panic!() };
        assert_eq!((i.iden, i.bandwidth_hz, i.tx_offset_hz), (1, 12_500, 0));
    }

    /// Figure 6.32.2-2 (CLMRN's sample, channel type 1: FDMA): a band plan
    /// for another system names it.
    #[test]
    fn tdma_identifier_extended() {
        let b: Vec<u8> = (0..40).step_by(2).map(|i| u8::from_str_radix(&"37fd0011bee0813309ef4afd09157562cb003200"[i..i + 2], 16).unwrap()).collect();
        let m = Mbt { header: b[..12].try_into().unwrap(), data: b[12..].to_vec() };
        let Msg::Identifier(i) = from_mbt(&m).msg else { panic!() };
        assert_eq!((i.iden, i.base_hz, i.spacing_hz, i.slots, i.tx_offset_hz, i.system), (1, 762_006_250, 6_250, 1, 30_000_000, Some((0xbee00, 0x9ef))));
    }

    /// Figure 6.2.15-1: R and A in octet 3; 6.2.15-2.
    #[test]
    fn rfss_status() {
        let m = tsbk(0x3a, 0, [0x12, 0x13, 0xa9, 1, 7, 0x10, 0x01, 0x70]);
        let site = Site { lra: 0x12, wacn: None, sys_id: 0x3a9, rfss: 1, site: 7, ch: Channel(0x1001), rx: None, class: 0x70 };
        assert_eq!(m.msg, Msg::RfssStatus { site, networked: true });
        let m = mbt(0x3a, 0, [0x12, 0x03, 0xa9], [0, 0], &[1, 7, 0x10, 0x01, 0x20, 0x01, 0x70, 0]);
        assert_eq!(m.msg, Msg::RfssStatus { site: Site { rx: Some(Channel(0x2001)), ..site }, networked: false });
    }

    /// Figure 6.2.11-1, 6.2.11-2.
    #[test]
    fn network_status() {
        let site = Site { lra: 0x12, wacn: Some(0xbee00), sys_id: 0x3a9, rfss: 0, site: 0, ch: Channel(0x1234), rx: None, class: 0x70 };
        assert_eq!(tsbk(0x3b, 0, [0x12, 0xbe, 0xe0, 0x03, 0xa9, 0x12, 0x34, 0x70]).msg, Msg::NetworkStatus(site));
        let m = mbt(0x3b, 0, [0x12, 0x03, 0xa9], [0, 0], &[0xbe, 0xe0, 0x00, 0x12, 0x34, 0x56, 0x78, 0x70]);
        assert_eq!(m.msg, Msg::NetworkStatus(Site { rx: Some(Channel(0x5678)), ..site }));
    }

    /// Figure 6.2.2-1 (C F V A in octet 3), 6.2.2-2 (the WACN, 0: this site's).
    #[test]
    fn adjacent_status() {
        let m = tsbk(0x3c, 0, [0x12, 0x63, 0xa9, 1, 7, 0x10, 0x01, 0x70]);
        let site = Site { lra: 0x12, wacn: None, sys_id: 0x3a9, rfss: 1, site: 7, ch: Channel(0x1001), rx: None, class: 0x70 };
        assert_eq!(m.msg, Msg::AdjacentStatus { site, own_plan: true, conventional: false, failed: true, valid: true, networked: false });
        let m = mbt(0x3c, 0, [0x12, 0x33, 0xa9], [1, 7], &[0x10, 0x01, 0x20, 0x01, 0x70, 0xbe, 0xe1, 0x00]);
        let Msg::AdjacentStatus { site: s, valid: true, networked: true, .. } = m.msg else { panic!("{:?}", m.msg) };
        assert_eq!((s.wacn, s.rx, s.rfss, s.site), (Some(0xbee10), Some(Channel(0x2001)), 1, 7));
    }

    /// Figure 6.2.16-1 (two, $00 class: not one), 6.2.28-1 (one, with its receive channel).
    #[test]
    fn secondary_control() {
        let m = tsbk(0x39, 0, [1, 7, 0x10, 0x05, 0x70, 0x10, 0x06, 0x00]);
        assert_eq!(m.msg, Msg::SecondaryControl { rfss: 1, site: 7, channels: vec![Secondary { ch: Channel(0x1005), rx: None, class: 0x70 }] });
        let m = tsbk(0x29, 0, [1, 7, 0x10, 0x05, 0, 0x20, 0x05, 0x70]);
        assert_eq!(m.msg, Msg::SecondaryControl { rfss: 1, site: 7, channels: vec![Secondary { ch: Channel(0x1005), rx: Some(Channel(0x2005)), class: 0x70 }] });
    }

    /// Figure 6.2.33-1: US and IST in octet 3; 2026-10-16 12:34 and 6789
    /// micro-slots (50.9175 s).
    #[test]
    fn sync_broadcast() {
        let t = (26u32 << 25 | 10 << 21 | 16 << 16 | 12 << 11 | 34 << 5 | 6789 >> 8).to_be_bytes();
        let m = tsbk(0x30, 0, [0, 0x00, 0, t[0], t[1], t[2], t[3], (6789 & 0xff) as u8]);
        assert_eq!(m.msg, Msg::SyncBroadcast { synced: true, time: Some((2026, 10, 16, 12, 34, 6789)), time_valid: true });
        let m = tsbk(0x30, 0, [0, 0x0c, 0, 0, 0, 0, 0, 0]);
        assert_eq!(m.msg, Msg::SyncBroadcast { synced: false, time: None, time_valid: false }, "nothing in it");
        let m = tsbk(0x30, 0, [0, 0x04, 0, t[0], t[1], t[2], t[3], (6789 & 0xff) as u8]);
        assert_eq!(m.msg, Msg::SyncBroadcast { synced: true, time: Some((2026, 10, 16, 12, 34, 6789)), time_valid: false }, "IST: a time all the same");
    }

    /// Figure 6.2.18-1, 6.2.10-1.
    #[test]
    fn status_and_message() {
        assert_eq!(tsbk(0x18, 0, [0x12, 0x34, 0x04, 0x05, 0x06, 0x01, 0x02, 0x03]).msg, Msg::StatusUpdate { status: 0x1234, target: 0x040506, source: 0x010203 });
        assert_eq!(tsbk(0x1c, 0, [0x12, 0x34, 0x04, 0x05, 0x06, 0x01, 0x02, 0x03]).msg, Msg::MessageUpdate { message: 0x1234, target: 0x040506, source: 0x010203 });
    }

    /// AABH Figures 4-5 to 4-9.
    #[test]
    fn motorola_patches() {
        assert_eq!(tsbk(0x00, MFID_MOTOROLA, [0x07, 0xd1, 0, 0x65, 0, 0x66, 0, 0]).msg, Msg::MotPatchAdd { sg: 2001, groups: [101, 102, 0] });
        assert_eq!(tsbk(0x01, MFID_MOTOROLA, [0x07, 0xd1, 0x07, 0xd1, 0, 0, 0, 0]).msg, Msg::MotPatchDelete { sg: 2001, groups: [2001, 0, 0] });
        // Octet 2: only P (bit 6) is defined; bit 7 is reserved, not emergency.
        let m = tsbk(0x02, MFID_MOTOROLA, [0xc0, 0x10, 0x01, 0x07, 0xd1, 0x0a, 0x0b, 0x0c]);
        assert_eq!(m.msg, Msg::MotPatchGrant { protected: true, ch: Channel(0x1001), rx: None, sg: 2001, source: 0x0a0b0c });
        let m = mbt(0x02, MFID_MOTOROLA, [0x0a, 0x0b, 0x0c], [0x40, 0], &[0x10, 0x01, 0x20, 0x01, 0x07, 0xd1, 0, 0]);
        assert_eq!(m.msg, Msg::MotPatchGrant { protected: true, ch: Channel(0x1001), rx: Some(Channel(0x2001)), sg: 2001, source: 0x0a0b0c });
        let m = tsbk(0x03, MFID_MOTOROLA, [0x10, 0x02, 0x07, 0xd1, 0x10, 0x03, 0x07, 0xd2]);
        assert_eq!(m.msg, Msg::MotPatchUpdate { calls: vec![(Channel(0x1002), 2001), (Channel(0x1003), 2002)] });
    }

    /// The BSI guide's MOT_BSI_GRANT: eight characters from octet 2 (values
    /// with no character, and padding, dropped), then the channel.
    #[test]
    fn base_station_id() {
        let v = [0x2cu64, 0x25, 0x2d, 0x29, 0x09, 0x0a, 0x0e, 0x00].iter().fold(0u64, |v, &c| v << 6 | c);
        let mut a = [0u8; 8];
        a[..6].copy_from_slice(&v.to_be_bytes()[2..]);
        a[6..].copy_from_slice(&[0x10, 0x5c]);
        let m = tsbk(0x0b, MFID_MOTOROLA, a);
        assert!(matches!(&m.msg, Msg::MotBaseStationId { callsign, ch } if callsign == "WPXT459" && ch.number() == 0x05c), "{:?}", m.msg);
        // $01..$04 and $0F..$15 are no characters.
        let junk = [0x01u64, 0x16, 0x10, 0x05, 0, 0, 0, 0].iter().fold(0u64, |v, &c| v << 6 | c);
        assert_eq!(CallSign::from_octets(&junk.to_be_bytes()[2..]).text(), "A0");
    }

    /// A manufacturer's opcode is its own: Motorola $04 isn't a unit-to-unit grant.
    #[test]
    fn a_manufacturers_opcode_is_not_the_standards() {
        let a = [0x02, 0x00, 0x07, 0xd1, 0x00, 0xf1, 0x12, 0x34];
        assert!(matches!(tsbk(0x04, 0, a).msg, Msg::UnitVoiceGrant { .. }));
        assert!(matches!(tsbk(0x04, 1, a).msg, Msg::UnitVoiceGrant { .. }));
        assert!(matches!(tsbk(0x04, MFID_MOTOROLA, a).msg, Msg::ExtendedFunction { class: 2, operand: 0, args: 0x07d100, .. }));
        assert_eq!(tsbk(0x04, 0x81, a).msg, Msg::Unknown);
    }

    /// AABH §4.3.2.1: a talkgroup (algorithm, group) or a radio.
    #[test]
    fn l3harris_regroup() {
        let m = tsbk(0x30, MFID_L3HARRIS, [0x60, 0xfe, 0x4d, 0x01, 0x23, 0x84, 0x00, 0x65]);
        assert_eq!(m.msg, Msg::HarrisRegroup { simulselect: false, sg: 65101, member: HarrisMember::Talkgroup { group: 101, algid: 0x84 }, activate: true, key_id: 0x0123 });
        let m = tsbk(0x30, MFID_L3HARRIS, [0x80, 0xfe, 0x4d, 0, 0, 0x80, 0x00, 0x65]);
        assert!(matches!(m.msg, Msg::HarrisRegroup { simulselect: true, member: HarrisMember::Unit(0x800065), activate: false, .. }));
    }

    /// §2.1: the P bit (octet 0 bit 6) encrypts the arguments; an MBT's SAP 63 does.
    #[test]
    fn protected_messages_are_not_read() {
        let mut t = [0u8; 12];
        t[0] = 0xc0;
        t[2..10].copy_from_slice(&[0xc4, 0x12, 0x34, 0x01, 0x01, 0x0a, 0x0b, 0x0c]);
        assert_eq!(from_tsbk(&t).msg, Msg::Protected);
        let mut h = ambtc_header(0x00, [0x0a, 0x0b, 0x0c], [0, 0]);
        h[1] = 0xc0 | 0x3f;
        let m = Mbt { header: h, data: vec![0; 8] };
        assert_eq!(from_mbt(&m).msg, Msg::Protected);
    }
}
