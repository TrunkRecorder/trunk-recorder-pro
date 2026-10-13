//! Trunking messages (Trunk Recorder's `TrunkMessage`) and the P25 TSBK
//! parser (Trunk Recorder's P25Parser::decode_tsbk, opcode for opcode, with
//! its IDEN band-plan tables and TDMA slot mapping). Field positions are
//! `bitset_shift_mask(tsbk, shift, mask)` on the block as a 96-bit integer.
//! MBTs arrive re-packed as the TSBK they stand for ([`Mbt::to_tsbk`]).

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::p25::mbt::Mbt;
use crate::p25::Tsbk;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MessageType {
    Grant,
    Status,
    Update,
    ControlChannel,
    Registration,
    Deregistration,
    Affiliation,
    SysId,
    Acknowledge,
    Location,
    PatchAdd,
    PatchDelete,
    DataGrant,
    UuAnsReq,
    UuVGrant,
    UuVUpdate,
    CallAlert,
    /// A neighbouring site (`sys_id`, `rfss`, `site`, its control channel `freq_hz`).
    Adjacent,
    /// A request refused (`source` the unit that asked; `talkgroup` a group call's group).
    Deny,
    /// A request waiting for its turn (as [`MessageType::Deny`]).
    Queued,
    /// A radio's emergency button (`source`, on `talkgroup`).
    EmergencyAlarm,
    /// The site's base station ID (`callsign`).
    BaseStationId,
    #[default]
    Unknown,
}

impl MessageType {
    pub fn as_str(self) -> &'static str {
        use MessageType::*;
        match self {
            Grant => "grant",
            Status => "status",
            Update => "update",
            ControlChannel => "control_channel",
            Registration => "registration",
            Deregistration => "deregistration",
            Affiliation => "affiliation",
            SysId => "sysid",
            Acknowledge => "acknowledge",
            Location => "location",
            PatchAdd => "patch_add",
            PatchDelete => "patch_delete",
            DataGrant => "data_grant",
            UuAnsReq => "uu_ans_req",
            UuVGrant => "uu_v_grant",
            UuVUpdate => "uu_v_update",
            CallAlert => "call_alert",
            Adjacent => "adjacent",
            Deny => "deny",
            Queued => "queued",
            EmergencyAlarm => "emergency_alarm",
            BaseStationId => "base_station_id",
            Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub sg: u32,
    pub ga: [u32; 3],
}

/// Frequencies in Hz (0 = unresolved); `source` −1 when the message has none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    pub kind: MessageType,
    /// Seconds of air since the source started (sample clock, not wall time).
    pub time_s: f64,
    pub freq_hz: u64,
    pub talkgroup: u32,
    pub source: i64,
    pub encrypted: bool,
    pub emergency: bool,
    pub duplex: bool,
    pub mode: bool,
    pub priority: u8,
    pub phase2_tdma: bool,
    pub tdma_slot: u8,
    pub sys_id: u32,
    pub wacn: u32,
    pub nac: u16,
    pub rfss: u32,
    pub site: u32,
    /// SmartNet: the voice channel is analog FM (P25 otherwise).
    pub analog: bool,
    pub patch: Option<Patch>,
    /// DMR: the colour code; a DMR grant's slot is `tdma_slot`.
    pub color_code: Option<u8>,
    /// NXDN: the voice channel's rate, and the site's (or carrier's) RAN.
    pub nxdn: Option<crate::nxdn::Rate>,
    pub ran: Option<u8>,
    pub opcode: u8,
    /// Motorola: the site's callsign (its CW ID), from its base station ID.
    pub callsign: Option<String>,
    /// Human-readable summary, as Trunk Recorder logs it.
    pub meta: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreqTable {
    pub offset_hz: i64,
    pub step_hz: u64,
    pub base_hz: u64,
    pub phase2_tdma: bool,
    pub slots_per_carrier: u32,
    pub bandwidth_khz: f64,
}

/// What a deny or queued response's service type (the request's opcode) asked for.
fn service_name(service: u32) -> String {
    match service {
        0x00 => "group voice".into(),
        0x04 => "unit-to-unit voice".into(),
        0x05 => "unit-to-unit answer".into(),
        0x08 => "telephone".into(),
        0x10..=0x14 => "data".into(),
        0x28 => "affiliation".into(),
        0x2c => "registration".into(),
        s => format!("service 0x{s:02x}"),
    }
}

/// Why a request was refused: the standard's reasons, or (`custom`)
/// Motorola's and L3Harris's own.
fn deny_reason(r: u32, custom: bool) -> String {
    let s = match (custom, r) {
        (true, 0x00) => "secure request on a clear supergroup",
        (true, 0x01) => "clear request on a secure supergroup",
        (true, _) => return format!("reason 0x{r:02x}"),
        (_, 0x10) => "requesting unit not valid",
        (_, 0x11) => "requesting unit not authorized",
        (_, 0x20) => "target unit not valid",
        (_, 0x21) => "target unit not authorized",
        (_, 0x2f) => "target unit refused the call",
        (_, 0x30) => "target group not valid",
        (_, 0x31) => "target group not authorized",
        (_, 0x40) => "invalid dialing",
        (_, 0x41) => "telephone number not authorized",
        (_, 0x42) => "PSTN not valid",
        (_, 0x50) => "call timeout",
        (_, 0x51) => "landline terminated the call",
        (_, 0x52) => "subscriber unit terminated the call",
        (_, 0x5f) => "call preempted",
        (_, 0x60) => "site access denied",
        (_, 0x67) => "PTT collide",
        (_, 0x77) => "PTT bonk",
        (_, 0xf0) => "call options not valid for the service",
        (_, 0xf1) => "protection option not valid",
        (_, 0xf2) => "duplex option not valid",
        (_, 0xf3) => "circuit or packet mode not valid",
        (_, 0xff) => "system doesn't support the service",
        _ => return format!("reason 0x{r:02x}"),
    };
    s.into()
}

/// Why a request was queued.
fn queued_reason(r: u32) -> String {
    match r {
        0x10 => "requesting unit busy in another service".into(),
        0x20 => "target unit busy in another service".into(),
        0x2f => "target unit queued the call".into(),
        0x30 => "target group already active".into(),
        0x40 => "no channel free".into(),
        0x41 => "no telephone line free".into(),
        0x42 => "no data channel free".into(),
        0x50 => "a superseding service is active".into(),
        _ => format!("reason 0x{r:02x}"),
    }
}

/// An encryption algorithm ID, by name.
fn algid_name(algid: u32) -> String {
    match algid {
        0x80 => "clear".into(),
        0x81 => "DES-OFB".into(),
        0x83 => "3DES".into(),
        0x84 => "AES-256".into(),
        0x85 => "AES-128".into(),
        0xaa => "ADP".into(),
        a => format!("ALGID 0x{a:02x}"),
    }
}

/// A registration or affiliation response's value other than 0 (accepted).
fn refusal(v: u32) -> &'static str {
    match v {
        1 => "failed",
        2 => "denied",
        _ => "refused",
    }
}

fn mhz(hz: u64) -> String {
    if hz == 0 {
        "?".into()
    } else {
        format!("{:.5}", hz as f64 / 1e6)
    }
}

/// One per system. Learns the band plan (IDEN_UP*) from the air; the plan can
/// also be preloaded (see [`TsbkParser::bandplan_to_string`]).
#[derive(Default, Clone)]
pub struct TsbkParser {
    pub tables: BTreeMap<u8, FreqTable>,
}

impl TsbkParser {
    pub fn channel_to_hz(&self, ch: u32) -> u64 {
        let Some(t) = self.tables.get(&(((ch >> 12) & 0xf) as u8)) else { return 0 };
        let channel = (ch & 0xfff) as u64;
        if t.phase2_tdma {
            t.base_hz + t.step_hz * (channel / t.slots_per_carrier.max(1) as u64)
        } else {
            t.base_hz + t.step_hz * channel
        }
    }

    pub fn tdma_slot(&self, ch: u32) -> Option<u8> {
        self.tables.get(&(((ch >> 12) & 0xf) as u8)).filter(|t| t.phase2_tdma).map(|_| (ch & 1) as u8)
    }

    /// The band plan as text (one IDEN per line), to persist between runs: it
    /// almost never changes, and without it a grant heard before the next IDEN
    /// broadcast can't be followed.
    pub fn bandplan_to_string(&self) -> String {
        let mut s = String::new();
        for (id, t) in &self.tables {
            let _ = writeln!(s, "{id} {} {} {} {} {} {}", t.offset_hz, t.step_hz, t.base_hz, t.phase2_tdma as u8, t.slots_per_carrier, t.bandwidth_khz);
        }
        s
    }
    pub fn bandplan_from_str(&mut self, s: &str) {
        for line in s.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 6 {
                continue;
            }
            let p = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            self.tables.insert(
                p(0) as u8,
                FreqTable {
                    offset_hz: p(1) as i64,
                    step_hz: p(2) as u64,
                    base_hz: p(3) as u64,
                    phase2_tdma: p(4) != 0.0,
                    slots_per_carrier: p(5) as u32,
                    bandwidth_khz: p(6),
                },
            );
        }
    }

    /// An MBT without a TSBK form ([`Mbt::to_tsbk`]): logged, not acted on.
    pub fn unknown_mbt(m: &Mbt, nac: u16, time_s: f64) -> Message {
        let mfid = if m.mfid() == 0 { String::new() } else { format!(" mfid 0x{:02x}", m.mfid()) };
        Message { time_s, opcode: m.opcode(), nac, source: -1, meta: format!("MBT opcode 0x{:02x}{mfid}", m.opcode()), ..Default::default() }
    }

    /// One CRC-valid TSBK → messages. `nac` from the frame's NID.
    pub fn parse(&mut self, block: &Tsbk, nac: u16, time_s: f64) -> Vec<Message> {
        let t = block.iter().fold(0u128, |a, &b| (a << 8) | b as u128);
        let b = |shift: u32, mask: u64| ((t >> shift) as u64 & mask) as u32;
        let opcode = b(88, 0x3f) as u8;
        let mut out = Vec::new();
        let mut m = Message { time_s, opcode, nac, source: -1, ..Default::default() };
        let opts = |m: &mut Message| {
            m.emergency = b(72, 0x80) != 0;
            m.encrypted = b(72, 0x40) != 0;
            m.duplex = b(72, 0x20) != 0;
            m.mode = b(72, 0x10) != 0;
            m.priority = b(72, 0x07) as u8;
        };
        let set_slot = |p: &TsbkParser, m: &mut Message, ch: u32| {
            let s = p.tdma_slot(ch);
            m.phase2_tdma = s.is_some();
            m.tdma_slot = s.unwrap_or(0);
        };
        // MFID 0 or 1: the standard's messages. Any other: the manufacturer's
        // own, whose opcodes mean something else.
        let mfid = b(80, 0xff);
        let vendor = if mfid <= 1 { 0 } else { mfid };
        match (vendor, opcode) {
            (0x90, 0x00) => {
                m.kind = MessageType::PatchAdd;
                let p = Patch { sg: b(64, 0xffff), ga: [b(48, 0xffff), b(32, 0xffff), b(16, 0xffff)] };
                m.meta = format!("Moto patch add sg {}: {} {} {}", p.sg, p.ga[0], p.ga[1], p.ga[2]);
                m.patch = Some(p);
            }
            (0, 0x00) => {
                opts(&mut m);
                let ch = b(56, 0xffff);
                m.kind = MessageType::Grant;
                m.freq_hz = self.channel_to_hz(ch);
                m.talkgroup = b(40, 0xffff);
                m.source = b(16, 0xffffff) as i64;
                set_slot(self, &mut m, ch);
                m.meta = format!(
                    "Grant TG {} {} MHz src {}{}{}",
                    m.talkgroup,
                    mhz(m.freq_hz),
                    m.source,
                    if m.phase2_tdma { format!(" slot {}", m.tdma_slot) } else { String::new() },
                    if m.encrypted { " ENC" } else { "" }
                );
            }
            // Motorola patch delete: the supergroup and the groups leaving it.
            (0x90, 0x01) => {
                m.kind = MessageType::PatchDelete;
                let p = Patch { sg: b(64, 0xffff), ga: [b(48, 0xffff), b(32, 0xffff), b(16, 0xffff)] };
                m.meta = format!("Moto patch delete sg {}: {} {} {}", p.sg, p.ga[0], p.ga[1], p.ga[2]);
                m.patch = Some(p);
            }
            (0x90, 0x02) => {
                opts(&mut m);
                let ch = b(56, 0xffff);
                m.kind = MessageType::Grant;
                m.freq_hz = self.channel_to_hz(ch);
                m.talkgroup = b(40, 0xffff);
                m.source = b(16, 0xffffff) as i64;
                set_slot(self, &mut m, ch);
                m.meta = format!("Patch grant SG {} {} MHz src {}", m.talkgroup, mhz(m.freq_hz), m.source);
            }
            (0, 0x02) => {
                let (ch1, ga1, ch2, ga2) = (b(64, 0xffff), b(48, 0xffff), b(32, 0xffff), b(16, 0xffff));
                let (f1, f2) = (self.channel_to_hz(ch1), self.channel_to_hz(ch2));
                m.kind = MessageType::Update;
                m.freq_hz = f1;
                m.talkgroup = ga1;
                set_slot(self, &mut m, ch1);
                m.meta = format!("Update TG {ga1} {} MHz", mhz(f1));
                if f1 != f2 && ch2 != 0xffff {
                    out.push(m.clone());
                    m.freq_hz = f2;
                    m.talkgroup = ga2;
                    set_slot(self, &mut m, ch2);
                    m.meta = format!("Update TG {ga2} {} MHz", mhz(f2));
                }
            }
            (0x90, 0x03) => {
                let (ch1, sg1, ch2, sg2) = (b(64, 0xffff), b(48, 0xffff), b(32, 0xffff), b(16, 0xffff));
                let (f1, f2) = (self.channel_to_hz(ch1), self.channel_to_hz(ch2));
                m.kind = MessageType::Update;
                m.freq_hz = f1;
                m.talkgroup = sg1;
                set_slot(self, &mut m, ch1);
                m.meta = format!("Patch update SG {sg1} {} MHz", mhz(f1));
                if f1 != f2 {
                    out.push(m.clone());
                    m.freq_hz = f2;
                    m.talkgroup = sg2;
                    set_slot(self, &mut m, ch2);
                    m.meta = format!("Patch update SG {sg2} {} MHz", mhz(f2));
                }
            }
            (0, 0x03) => {
                m.emergency = b(72, 0x80) != 0;
                m.encrypted = b(72, 0x40) != 0;
                let ch1 = b(48, 0xffff);
                m.kind = MessageType::Update;
                m.freq_hz = self.channel_to_hz(ch1);
                m.talkgroup = b(16, 0xffff);
                set_slot(self, &mut m, ch1);
                m.meta = format!("Explicit update TG {} {} MHz", m.talkgroup, mhz(m.freq_hz));
            }
            // Motorola extended function: class, operand, arguments, target
            // radio. Class 2 regroups a radio onto a supergroup (operand 0) or
            // off it (1): no talkgroup is patched.
            (0x90, 0x04) => {
                let (class, operand, args, target) = (b(72, 0xff), b(64, 0xff), b(40, 0xffffff), b(16, 0xffffff));
                m.source = target as i64;
                m.meta = match (class, operand) {
                    (0x02, 0 | 1) => format!("Moto regroup sg {}: unit {target} {}", args & 0xffff, if operand == 0 { "joins" } else { "leaves" }),
                    _ => format!("Moto extended function class 0x{class:02x} operand 0x{operand:02x} args 0x{args:06x} unit {target}"),
                };
            }
            // Deny and queued responses, the standard's and Motorola's (the
            // same layout; Motorola's RC bit marks a reason of its own): the
            // service asked for (its request's opcode), why, more (a group
            // call's group), and the unit that asked.
            (0, 0x21 | 0x27) | (0x90, 0x06 | 0x07) => {
                let deny = opcode == 0x27 || opcode == 0x07;
                let (service, reason, info) = (b(72, 0x3f), b(64, 0xff), b(40, 0xffffff));
                m.kind = if deny { MessageType::Deny } else { MessageType::Queued };
                m.source = b(16, 0xffffff) as i64;
                let group = service == 0x00;
                if group {
                    m.talkgroup = info & 0xffff;
                }
                let why = if deny { deny_reason(reason, vendor == 0x90 && b(79, 1) == 1) } else { queued_reason(reason) };
                m.meta = format!(
                    "{} {} for unit {}{}: {why}",
                    if deny { "Denied" } else { "Queued" },
                    service_name(service),
                    m.source,
                    if group { format!(" on TG {}", m.talkgroup) } else { String::new() }
                );
            }
            // Motorola emergency alarm: the group and the radio.
            (0x90, 0x0a) => {
                m.kind = MessageType::EmergencyAlarm;
                m.emergency = true;
                m.talkgroup = b(40, 0xffff);
                m.source = b(16, 0xffffff) as i64;
                m.meta = format!("Emergency alarm from {} on TG {}", m.source, m.talkgroup);
            }
            // Motorola base station ID: the site's callsign, eight 6-bit
            // characters (+ 43 for ASCII; 0 none), and its channel.
            (0x90, 0x0b) => {
                let callsign: String = (0..8).map(|i| b(74 - 6 * i, 0x3f)).filter(|&c| c != 0).map(|c| (c as u8 + 43) as char).collect();
                m.kind = MessageType::BaseStationId;
                m.freq_hz = self.channel_to_hz(b(16, 0xffff));
                m.meta = format!("Base station ID {callsign} {} MHz", mhz(m.freq_hz));
                m.callsign = Some(callsign);
            }
            (0, 0x04) => {
                opts(&mut m);
                let ch = b(64, 0xffff);
                m.kind = MessageType::UuVGrant;
                m.freq_hz = self.channel_to_hz(ch);
                m.talkgroup = b(40, 0xffffff);
                m.source = b(16, 0xffffff) as i64;
                set_slot(self, &mut m, ch);
                m.meta = format!("Unit-to-unit grant {} → {} {} MHz", m.source, m.talkgroup, mhz(m.freq_hz));
            }
            (0x90, 0x05) => {
                m.meta = "MOTOROLA_OSP_TRAFFIC_CHANNEL_ID".into();
            }
            (0, 0x05) => {
                opts(&mut m);
                m.kind = MessageType::UuAnsReq;
                m.source = b(16, 0xffffff) as i64;
                m.talkgroup = b(40, 0xffffff);
                m.meta = format!("Unit-to-unit answer request {} → {}", m.talkgroup, m.source);
            }
            (0, 0x06) => {
                let ch = b(64, 0xffff);
                m.kind = MessageType::UuVUpdate;
                m.freq_hz = self.channel_to_hz(ch);
                m.talkgroup = b(40, 0xffffff);
                m.source = b(16, 0xffffff) as i64;
                set_slot(self, &mut m, ch);
                m.meta = format!("Unit-to-unit update {} → {} {} MHz", m.source, m.talkgroup, mhz(m.freq_hz));
            }
            (0, 0x14) => {
                m.emergency = b(72, 0x80) != 0;
                m.encrypted = b(72, 0x40) != 0;
                m.duplex = b(72, 0x20) != 0;
                m.mode = b(72, 0x10) != 0;
                m.kind = MessageType::DataGrant;
                m.source = b(16, 0xffffff) as i64;
                m.freq_hz = self.channel_to_hz(b(56, 0xffff));
                m.meta = format!("Data grant src {} {} MHz", m.source, mhz(m.freq_hz));
            }
            (0, 0x1f) => {
                m.kind = MessageType::CallAlert;
                m.source = b(16, 0xffffff) as i64;
                m.talkgroup = b(40, 0xffffff);
                m.meta = format!("Call alert {} → {}", m.source, m.talkgroup);
            }
            (0, 0x20) => {
                m.kind = MessageType::Acknowledge;
                m.talkgroup = b(40, 0xffff);
                m.source = b(16, 0xffffff) as i64;
                m.meta = format!("Acknowledge src {}", m.source);
            }
            (0, 0x28) => {
                m.source = b(16, 0xffffff) as i64;
                m.talkgroup = b(40, 0xffff);
                // GAV: 0 accepted; failed, denied or refused, the unit isn't on the group.
                match b(72, 0x3) {
                    0 => {
                        m.kind = MessageType::Affiliation;
                        m.meta = format!("Affiliation {} → TG {}", m.source, m.talkgroup);
                    }
                    gav => m.meta = format!("Affiliation {} → TG {} {}", m.source, m.talkgroup, refusal(gav)),
                }
            }
            (0, 0x29 | 0x39) => {
                let (f1, f2) = (self.channel_to_hz(b(48, 0xffff)), self.channel_to_hz(b(24, 0xffff)));
                m.meta = format!("Secondary CC rfss {} site {}: {} / {} MHz", b(72, 0xff), b(64, 0xff), mhz(f1), mhz(f2));
                if f1 != 0 && f2 != 0 {
                    m.kind = MessageType::ControlChannel;
                    m.freq_hz = f1;
                    out.push(m.clone());
                    m.freq_hz = f2;
                }
            }
            (0, 0x2b) => {
                m.kind = MessageType::Location;
                m.talkgroup = b(56, 0xffff);
                m.source = b(16, 0xffffff) as i64;
                m.meta = format!("Location registration {} TG {}", m.source, m.talkgroup);
            }
            (0, 0x2c) => {
                m.source = b(40, 0xffffff) as i64;
                // RV: 0 accepted; failed, denied or refused, the unit isn't on the system.
                match b(76, 0x3) {
                    0 => {
                        m.kind = MessageType::Registration;
                        m.meta = format!("Registration {}", m.source);
                    }
                    rv => m.meta = format!("Registration {} {}", m.source, refusal(rv)),
                }
            }
            (0, 0x2f) => {
                m.kind = MessageType::Deregistration;
                m.source = b(16, 0xffffff) as i64;
                m.meta = format!("Deregistration {}", m.source);
            }
            // L3Harris group regroup (explicit encryption): options (simulselect,
            // talkgroup target, activate, sequence number), supergroup, key
            // ID, target. A talkgroup target is its algorithm ID and the group.
            (0xa4, 0x30) => {
                let (opts, sg, key, target) = (b(72, 0xff), b(56, 0xffff), b(40, 0xffff), b(16, 0xffffff));
                let what = if opts & 0x80 != 0 { "simulselect" } else { "patch" };
                let add = opts & 0x20 != 0;
                if opts & 0x40 != 0 {
                    let (tg, algid) = (target & 0xffff, target >> 16);
                    m.kind = if add { MessageType::PatchAdd } else { MessageType::PatchDelete };
                    m.patch = Some(Patch { sg, ga: [tg, tg, tg] });
                    m.encrypted = algid != 0 && algid != 0x80;
                    let enc = if m.encrypted { format!(" {} key 0x{key:04x}", algid_name(algid)) } else { String::new() };
                    m.meta = format!("L3Harris {what} {} sg {sg} TG {tg}{enc}", if add { "add" } else { "delete" });
                } else {
                    // A radio regrouped onto the supergroup: no talkgroup patched.
                    m.source = target as i64;
                    m.meta = format!("L3Harris {what} sg {sg}: unit {target} {}", if add { "joins" } else { "leaves" });
                }
            }
            (0, 0x30) => {
                m.meta = "TDMA sync broadcast".into();
            }
            (0, 0x33) => {
                let (iden, ct, toff0, spac) = (b(76, 0xf) as u8, b(72, 0xf) as usize, b(58, 0x3fff) as i64, b(48, 0x3ff) as i64);
                let mut toff = toff0 & 0x1fff;
                if (toff0 >> 13) & 1 == 0 {
                    toff = -toff;
                }
                let slots = [1u32, 1, 1, 2, 4, 2].get(ct).copied().unwrap_or(1);
                let base = b(16, 0xffffffff) as u64 * 5;
                self.tables.insert(
                    iden,
                    FreqTable { offset_hz: toff * spac * 125, step_hz: spac as u64 * 125, base_hz: base, phase2_tdma: slots > 1, slots_per_carrier: slots, bandwidth_khz: 6.25 },
                );
                m.meta = format!("IDEN_UP_TDMA {iden}: base {} MHz, step {} Hz, {slots} slot(s)", mhz(base), spac * 125);
            }
            (0, 0x34) => {
                let (iden, bwvu, toff0, spac) = (b(76, 0xf) as u8, b(72, 0xf), b(58, 0x3fff) as i64, b(48, 0x3ff) as i64);
                let mut toff = toff0 & 0x1fff;
                if (toff0 >> 13) & 1 == 0 {
                    toff = -toff;
                }
                let base = b(16, 0xffffffff) as u64 * 5;
                let bw = match bwvu {
                    4 => 6.25,
                    5 => 12.5,
                    _ => 0.0,
                };
                self.tables.insert(
                    iden,
                    FreqTable { offset_hz: toff * spac * 125, step_hz: spac as u64 * 125, base_hz: base, phase2_tdma: false, slots_per_carrier: 0, bandwidth_khz: bw },
                );
                m.meta = format!("IDEN_UP_VU {iden}: base {} MHz, step {} Hz", mhz(base), spac * 125);
            }
            (0, 0x3a) => {
                m.kind = MessageType::SysId;
                m.sys_id = b(56, 0xfff);
                m.rfss = b(48, 0xff);
                m.site = b(40, 0xff);
                m.meta = format!("RFSS status sysid {:x} rfss {} site {}", m.sys_id, m.rfss, m.site);
            }
            (0, 0x3b) => {
                let f1 = self.channel_to_hz(b(24, 0xffff));
                m.meta = format!("Network status wacn {:x} sysid {:x} CC {} MHz", b(52, 0xfffff), b(40, 0xfff), mhz(f1));
                if f1 != 0 {
                    m.kind = MessageType::Status;
                    m.wacn = b(52, 0xfffff);
                    m.sys_id = b(40, 0xfff);
                    m.freq_hz = f1;
                }
            }
            (0, 0x3c) => {
                m.kind = MessageType::Adjacent;
                m.sys_id = b(56, 0xfff);
                m.rfss = b(48, 0xff);
                m.site = b(40, 0xff);
                m.freq_hz = self.channel_to_hz(b(24, 0xffff));
                m.meta = format!("Adjacent site rfss {} site {} CC {} MHz", m.rfss, m.site, mhz(m.freq_hz));
            }
            (0, 0x3d) => {
                let (iden, bw, toff0, spac) = (b(76, 0xf) as u8, b(67, 0x1ff), b(58, 0x1ff) as i64, b(48, 0x3ff) as i64);
                let mut toff = toff0 & 0xff;
                if (toff0 >> 8) & 1 == 0 {
                    toff = -toff;
                }
                let base = b(16, 0xffffffff) as u64 * 5;
                self.tables.insert(
                    iden,
                    FreqTable { offset_hz: toff * 250_000, step_hz: spac as u64 * 125, base_hz: base, phase2_tdma: false, slots_per_carrier: 1, bandwidth_khz: bw as f64 * 0.125 },
                );
                m.meta = format!("IDEN_UP {iden}: base {} MHz, step {} Hz", mhz(base), spac * 125);
            }
            (0, _) => m.meta = format!("TSBK opcode 0x{opcode:02x}"),
            (_, _) => m.meta = format!("TSBK opcode 0x{opcode:02x} mfid 0x{mfid:02x}"),
        }
        out.push(m);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TSBK from a 96-bit value.
    fn tsbk(t: u128) -> Tsbk {
        let mut blk = [0u8; 12];
        for (i, v) in blk.iter_mut().enumerate() {
            *v = (t >> (88 - 8 * i)) as u8;
        }
        blk
    }

    /// Opcode `op` from manufacturer `mfid`, with `fields` (octets 2..10).
    fn msg(op: u8, mfid: u8, fields: [u8; 8]) -> Message {
        let mut blk = [0u8; 12];
        blk[0] = 0x80 | op;
        blk[1] = mfid;
        blk[2..10].copy_from_slice(&fields);
        TsbkParser::default().parse(&blk, 0x443, 0.0).pop().unwrap()
    }

    #[test]
    fn a_manufacturers_opcode_is_not_the_standards() {
        // Motorola 0x04 (extended function) and 0x06 (queued) aren't unit-to-unit calls.
        let f = [0x02, 0x00, 0x00, 0xff, 0xf1, 0x12, 0x34, 0x56];
        assert_eq!(msg(0x04, 0, f).kind, MessageType::UuVGrant);
        assert_ne!(msg(0x04, 0x90, f).kind, MessageType::UuVGrant);
        assert_ne!(msg(0x06, 0x90, f).kind, MessageType::UuVUpdate);
        // MFID 1 is the standard's too.
        assert_eq!(msg(0x04, 1, f).kind, MessageType::UuVGrant);
    }

    #[test]
    fn l3harris_regroups_name_talkgroups_or_radios() {
        // Patch, talkgroup target, activate: TG 101 (ALGID 0x80, clear) into supergroup 65101.
        let m = msg(0x30, 0xa4, [0x60, 0xfe, 0x4d, 0x00, 0x00, 0x80, 0x00, 0x65]);
        assert_eq!((m.kind, m.patch.map(|p| (p.sg, p.ga[0])), m.encrypted), (MessageType::PatchAdd, Some((65101, 101)), false));
        // AES-256, key 0x0123, deactivate.
        let m = msg(0x30, 0xa4, [0x40, 0xfe, 0x4d, 0x01, 0x23, 0x84, 0x00, 0x65]);
        assert_eq!((m.kind, m.encrypted), (MessageType::PatchDelete, true));
        assert!(m.meta.contains("AES-256 key 0x0123"), "{}", m.meta);
        // A radio target (G clear): unit 0x800065 joins; nothing is patched.
        let m = msg(0x30, 0xa4, [0x20, 0xfe, 0x4d, 0x00, 0x00, 0x80, 0x00, 0x65]);
        assert_eq!((m.kind, m.patch, m.source), (MessageType::Unknown, None, 0x800065));
    }

    #[test]
    fn motorola_control_messages() {
        // Patch delete: TG 101 leaves supergroup 2001.
        let m = msg(0x01, 0x90, [0x07, 0xd1, 0x00, 0x65, 0, 0, 0, 0]);
        assert_eq!((m.kind, m.patch.map(|p| (p.sg, p.ga))), (MessageType::PatchDelete, Some((2001, [101, 0, 0]))));
        // Emergency alarm: unit 0x123456 on TG 0x0101.
        let m = msg(0x0a, 0x90, [0, 0, 0, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert_eq!((m.kind, m.source, m.talkgroup, m.emergency), (MessageType::EmergencyAlarm, 0x123456, 0x0101, true));
        // Base station ID "WQAB123": each character − 43, six bits apiece.
        let cs = b"WQAB123";
        let bits = cs.iter().fold(0u64, |a, &c| a << 6 | (c - 43) as u64) << 6; // and an empty eighth
        let mut f = [0u8; 8];
        f[..6].copy_from_slice(&bits.to_be_bytes()[2..]);
        let m = msg(0x0b, 0x90, f);
        assert_eq!((m.kind, m.callsign.as_deref()), (MessageType::BaseStationId, Some("WQAB123")));
    }

    #[test]
    fn deny_and_queued_responses() {
        // Standard deny: group voice on TG 0x0101 for unit 0x123456, target group not authorized.
        let m = msg(0x27, 0, [0x00, 0x31, 0x00, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert_eq!((m.kind, m.source, m.talkgroup), (MessageType::Deny, 0x123456, 0x0101));
        assert!(m.meta.ends_with("target group not authorized"), "{}", m.meta);
        // Motorola's own reason (RC set): a clear request on a secure supergroup.
        let m = msg(0x07, 0x90, [0x80, 0x01, 0x00, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert!(m.kind == MessageType::Deny && m.meta.ends_with("clear request on a secure supergroup"), "{}", m.meta);
        // Queued, no channel free.
        let m = msg(0x21, 0, [0x00, 0x40, 0x00, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert!(m.kind == MessageType::Queued && m.meta.ends_with("no channel free"), "{}", m.meta);
    }

    #[test]
    fn iden_then_grant_resolves_frequency() {
        let mut p = TsbkParser::default();
        // IDEN_UP id 1: base 851.00625 MHz (170201250 × 5 Hz), step 6.25 kHz (50 × 125).
        let base = 851_006_250u64 / 5;
        let t: u128 = (0x3d_u128 << 88) | (1u128 << 76) | (0x001u128 << 58) | (50u128 << 48) | ((base as u128) << 16);
        p.parse(&tsbk(t), 0x443, 0.0);
        assert_eq!(p.channel_to_hz((1 << 12) | 100), 851_006_250 + 100 * 6250);
    }
}
