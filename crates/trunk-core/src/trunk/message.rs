//! Trunking messages (Trunk Recorder's `TrunkMessage`), and what a P25
//! control channel's messages ([`Osp`]) mean to a recorder: calls granted
//! and in progress, patches, units, the site. Channels resolve on the band
//! plan the parser learns from the air.

use std::collections::BTreeMap;
use std::fmt::Write;

use super::calls::Voice;
use crate::p25::osp::{Answer, Channel, HarrisMember, Identifier, Msg, Osp, ServiceOptions, MFID_MOTOROLA};

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
    /// A telephone interconnect call (`talkgroup` and `source` the radio).
    PhoneGrant,
    PhoneUpdate,
    CallAlert,
    /// A radio's status (`source`, to `talkgroup` the unit it's for).
    StatusUpdate,
    /// A radio's short message (as [`MessageType::StatusUpdate`]).
    MessageUpdate,
    /// A neighbouring site (`wacn`, `sys_id`, `rfss`, `site`, its control
    /// channel `freq_hz`) a radio could move to.
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
            PhoneGrant => "phone_grant",
            PhoneUpdate => "phone_update",
            CallAlert => "call_alert",
            StatusUpdate => "status_update",
            MessageUpdate => "message_update",
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
    /// A grant's (or update's): what the call's voice is on.
    pub voice: Voice,
    pub sys_id: u32,
    pub wacn: u32,
    pub nac: u16,
    pub rfss: u32,
    pub site: u32,
    pub patch: Option<Patch>,
    /// NXDN: the site's (or carrier's) RAN.
    pub ran: Option<u8>,
    pub opcode: u8,
    /// Motorola: the site's callsign (its CW ID), from its base station ID.
    pub callsign: Option<String>,
    /// Human-readable summary, as Trunk Recorder logs it.
    pub meta: String,
}


/// What a deny or queued response's service type (the request's opcode)
/// asked for: the standard's ISPs (AABC-D Annex D), or Motorola's (AABH §4.2).
fn service_name(service: u8, mfid: u8) -> String {
    match (mfid, service) {
        (MFID_MOTOROLA, 0x00) => "supergroup voice".into(),
        (MFID_MOTOROLA, s) => format!("Motorola service 0x{s:02x}"),
        (_, 0x00) => "group voice".into(),
        (_, 0x04) => "unit-to-unit voice".into(),
        (_, 0x05) => "unit-to-unit answer".into(),
        (_, 0x08..=0x0a) => "telephone".into(),
        (_, 0x10..=0x14) => "data".into(),
        (_, 0x18) => "status update".into(),
        (_, 0x1c) => "message update".into(),
        (_, 0x1f) => "call alert".into(),
        (_, 0x28) => "affiliation".into(),
        (_, 0x2c) => "registration".into(),
        (_, s) => format!("service 0x{s:02x}"),
    }
}

/// Whether a service type is for a group, so a response's additional
/// information is the call's options and group (AABC-D §6.2.5, AABH §4.4.6).
fn group_service(service: u8) -> bool {
    matches!(service, 0x00 | 0x11)
}

/// Why a request was refused: the standard's reasons (AABC-D Annex B), or
/// (`own`) Motorola's.
fn deny_reason(r: u8, own: bool) -> String {
    let s = match (own, r) {
        (true, 0x00) => "secure request on a clear supergroup",
        (true, 0x01) => "clear request on a secure supergroup",
        (true, _) => return format!("reason 0x{r:02x}"),
        (_, 0x10) => "requesting unit not valid",
        (_, 0x11) => "requesting unit not authorized",
        (_, 0x12) => "requesting unit not registered with its home",
        (_, 0x20) => "target unit not valid",
        (_, 0x21) => "target unit not authorized",
        (_, 0x22) => "the radio failed authentication",
        (_, 0x23) => "the system failed authentication",
        (_, 0x24) => "the radio couldn't be authenticated",
        (_, 0x25) => "target unit not registered with its home",
        (_, 0x26) => "target unit's home unknown",
        (_, 0x27) => "target unit unavailable",
        (_, 0x2f) => "target unit refused the call",
        (_, 0x30) => "target group not valid",
        (_, 0x31) => "target group not authorized",
        (_, 0x40) => "invalid dialing",
        (_, 0x41) => "telephone number not authorized",
        (_, 0x42) => "PSTN not valid",
        (_, 0x50) => "call timeout",
        (_, 0x51) => "landline terminated the call",
        (_, 0x52) => "subscriber unit terminated the call",
        (_, 0x53) => "no network resources",
        (_, 0x54) => "no RF resources",
        (_, 0x55) => "already being set up by another party",
        (_, 0x5f) => "call preempted",
        (_, 0x60) => "site access denied",
        // $61-$EF are the system's own; these two are Motorola's.
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

/// Why a request was queued (AABC-D Annex C).
fn queued_reason(r: u8) -> String {
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

/// An extended function (AABC-D Table 2.3.15-1, AABH Table 4-2).
fn extended_function(class: u8, operand: u8, args: u32, mfid: u8) -> String {
    match (class, operand) {
        (0x00, 0x00) => "radio check".into(),
        (0x00, 0x7d) => "radio detach".into(),
        (0x00, 0x7e) => "radio uninhibit".into(),
        (0x00, 0x7f) => "radio inhibit".into(),
        (0x00, 0x80) => "radio check acknowledged".into(),
        (0x00, 0xfd) => "radio detach acknowledged".into(),
        (0x00, 0xfe) => "radio uninhibit acknowledged".into(),
        (0x00, 0xff) => "radio inhibit acknowledged".into(),
        // Arguments are top-justified (AABH §4.4.5): the group in octets 0-1.
        (0x02, 0x00) if mfid == MFID_MOTOROLA => format!("regrouped onto supergroup {}", args >> 8),
        (0x02, 0x01) if mfid == MFID_MOTOROLA => format!("regroup on supergroup {} cancelled", args >> 8),
        (0x02, 0x00) => "dynamic regroup".into(),
        (0x02, 0x01) => "regroup cancelled".into(),
        _ => format!("extended function class 0x{class:02x} operand 0x{operand:02x} args 0x{args:06x}"),
    }
}

/// An encryption algorithm ID, by name.
fn algid_name(algid: u8) -> String {
    match crate::p25::crypt::algid_name(algid) {
        "unknown" => format!("ALGID 0x{algid:02x}"),
        n => n.into(),
    }
}

fn mhz(hz: u64) -> String {
    if hz == 0 {
        "?".into()
    } else {
        format!("{:.5}", hz as f64 / 1e6)
    }
}

/// One per system: what its control channel says ([`Osp`]), as
/// [`Message`]s. Learns the band plan (IDEN_UP*) and the site's system
/// (NET_STS_BCST) from the air; the band plan can also be preloaded (see
/// [`P25Parser::bandplan_to_string`]).
#[derive(Default, Clone)]
pub struct P25Parser {
    pub tables: BTreeMap<u8, Identifier>,
    /// The site's (WACN, System ID): a band plan for another system's
    /// channels (an extended IDEN_UP_TDMA naming it) isn't this one's.
    system: Option<(u32, u16)>,
}

impl P25Parser {
    /// A channel's carrier frequency (0: its band plan unknown yet) and, on a
    /// TDMA carrier, its slot.
    pub fn resolve(&self, ch: Channel) -> (u64, Option<u8>) {
        match self.tables.get(&ch.iden()) {
            Some(i) => {
                let (hz, slot) = i.resolve(ch);
                (hz, (i.slots > 1).then_some(slot))
            }
            None => (0, None),
        }
    }

    pub fn channel_to_hz(&self, ch: Channel) -> u64 {
        self.resolve(ch).0
    }

    /// The band plan as text (one identifier per line), to persist between
    /// runs: it almost never changes, and without it a grant heard before the
    /// next IDEN broadcast can't be followed.
    pub fn bandplan_to_string(&self) -> String {
        let mut s = String::new();
        for (id, t) in &self.tables {
            let _ = writeln!(s, "{id} {} {} {} {} {} {}", t.tx_offset_hz, t.spacing_hz, t.base_hz, (t.slots > 1) as u8, t.slots, t.bandwidth_hz as f64 / 1000.0);
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
            let iden = p(0) as u8;
            let slots = (p(5) as u8).max(1);
            self.tables.insert(
                iden,
                Identifier { iden, tx_offset_hz: p(1) as i64, spacing_hz: p(2) as u64, base_hz: p(3) as u64, slots, bandwidth_hz: (p(6) * 1000.0) as u32, system: None },
            );
        }
    }

    /// A message's channel: its frequency and TDMA slot.
    fn on(&self, m: &mut Message, ch: Channel) {
        let (hz, slot) = self.resolve(ch);
        m.freq_hz = hz;
        m.voice = slot.map_or(Voice::P25, |slot| Voice::P25Tdma { slot });
    }

    /// What a control channel message means. `nac` from its frame's NID.
    pub fn parse(&mut self, o: &Osp, nac: u16, time_s: f64) -> Vec<Message> {
        let mut m = Message { time_s, opcode: o.opcode, nac, source: -1, ..Default::default() };
        let mut out = Vec::new();
        let opts = |m: &mut Message, o: ServiceOptions| {
            m.emergency = o.emergency();
            m.encrypted = o.protected();
            m.duplex = o.duplex();
            m.mode = o.packet();
            m.priority = o.priority();
        };
        let slot = |m: &Message| m.voice.slot().map_or(String::new(), |s| format!(" slot {s}"));
        let enc = |m: &Message| if m.encrypted { " ENC" } else { "" };
        match &o.msg {
            Msg::GroupVoiceGrant { opts: so, ch, group, source, .. } => {
                opts(&mut m, *so);
                self.on(&mut m, *ch);
                (m.kind, m.talkgroup, m.source) = (MessageType::Grant, *group as u32, *source as i64);
                m.meta = format!("Grant TG {} {} MHz src {}{}{}", m.talkgroup, mhz(m.freq_hz), m.source, slot(&m), enc(&m));
            }
            Msg::GroupVoiceUpdate { calls } | Msg::MotPatchUpdate { calls } => {
                let what = if matches!(o.msg, Msg::MotPatchUpdate { .. }) { "Patch update SG" } else { "Update TG" };
                for &(ch, group) in calls {
                    let mut u = m.clone();
                    self.on(&mut u, ch);
                    (u.kind, u.talkgroup) = (MessageType::Update, group as u32);
                    u.meta = format!("{what} {group} {} MHz{}", mhz(u.freq_hz), slot(&u));
                    out.push(u);
                }
                return out;
            }
            Msg::GroupVoiceUpdateExplicit { opts: so, ch, group, .. } => {
                opts(&mut m, *so);
                self.on(&mut m, *ch);
                (m.kind, m.talkgroup) = (MessageType::Update, *group as u32);
                m.meta = format!("Explicit update TG {} {} MHz{}{}", m.talkgroup, mhz(m.freq_hz), slot(&m), enc(&m));
            }
            Msg::UnitVoiceGrant { opts: so, ch, target, source, .. } | Msg::UnitVoiceUpdate { opts: so, ch, target, source, .. } => {
                if let Some(so) = so {
                    opts(&mut m, *so);
                }
                self.on(&mut m, *ch);
                let grant = matches!(o.msg, Msg::UnitVoiceGrant { .. });
                m.kind = if grant { MessageType::UuVGrant } else { MessageType::UuVUpdate };
                (m.talkgroup, m.source) = (*target, *source as i64);
                m.meta = format!("Unit-to-unit {} {source} → {target} {} MHz{}{}", if grant { "grant" } else { "update" }, mhz(m.freq_hz), slot(&m), enc(&m));
            }
            Msg::UnitAnswerRequest { opts: so, target, source } => {
                opts(&mut m, *so);
                (m.kind, m.talkgroup, m.source) = (MessageType::UuAnsReq, *target, *source as i64);
                m.meta = format!("Unit-to-unit answer request {source} → {target}");
            }
            Msg::PhoneGrant { opts: so, ch, timer_ds, unit, .. } | Msg::PhoneUpdate { opts: so, ch, timer_ds, unit, .. } => {
                opts(&mut m, *so);
                self.on(&mut m, *ch);
                let grant = matches!(o.msg, Msg::PhoneGrant { .. });
                m.kind = if grant { MessageType::PhoneGrant } else { MessageType::PhoneUpdate };
                (m.talkgroup, m.source) = (*unit, *unit as i64);
                let timer = if *timer_ds > 0 { format!(" for {:.1} s", *timer_ds as f64 / 10.0) } else { String::new() };
                m.meta = format!("Telephone {} unit {unit} {} MHz{}{timer}{}", if grant { "grant" } else { "update" }, mhz(m.freq_hz), slot(&m), enc(&m));
            }
            Msg::PhoneAnswerRequest { digits, target } => {
                m.source = *target as i64;
                m.meta = format!("Telephone call for unit {target}{}", if digits.is_empty() { String::new() } else { format!(" from {digits}") });
            }
            Msg::DataGrant { opts: so, ch, target, .. } => {
                opts(&mut m, *so);
                self.on(&mut m, *ch);
                (m.kind, m.source) = (MessageType::DataGrant, *target as i64);
                m.meta = format!("Data grant unit {target} {} MHz", mhz(m.freq_hz));
            }
            Msg::Acknowledge { service, source, target } => {
                (m.kind, m.source) = (MessageType::Acknowledge, *target as i64);
                let from = source.map_or(String::new(), |s| format!(" from {s}"));
                m.meta = format!("Acknowledge {} for unit {target}{from}", service_name(*service, o.mfid));
            }
            Msg::Queued { service, reason, info, target } | Msg::Denied { service, reason, info, target, .. } => {
                let deny = matches!(o.msg, Msg::Denied { .. });
                m.kind = if deny { MessageType::Deny } else { MessageType::Queued };
                m.source = *target as i64;
                let group = info.filter(|_| group_service(*service)).map(|i| i & 0xffff);
                m.talkgroup = group.unwrap_or(0);
                let why = match o.msg {
                    Msg::Denied { own_reason, .. } => deny_reason(*reason, own_reason),
                    _ => queued_reason(*reason),
                };
                let on = group.map_or(String::new(), |g| format!(" on TG {g}"));
                m.meta = format!("{} {} for unit {target}{on}: {why}", if deny { "Denied" } else { "Queued" }, service_name(*service, o.mfid));
            }
            Msg::ExtendedFunction { class, operand, args, target } => {
                m.source = *target as i64;
                m.meta = format!("Unit {target}: {}", extended_function(*class, *operand, *args, o.mfid));
            }
            Msg::CallAlert { target, source } => {
                (m.kind, m.talkgroup, m.source) = (MessageType::CallAlert, *target, *source as i64);
                m.meta = format!("Call alert {source} → {target}");
            }
            Msg::StatusUpdate { status, target, source } => {
                (m.kind, m.talkgroup, m.source) = (MessageType::StatusUpdate, *target, *source as i64);
                m.meta = format!("Status from {source} to {target}: user 0x{:02x} unit 0x{:02x}", status >> 8, status & 0xff);
            }
            Msg::MessageUpdate { message, target, source } => {
                (m.kind, m.talkgroup, m.source) = (MessageType::MessageUpdate, *target, *source as i64);
                m.meta = format!("Message from {source} to {target}: 0x{message:04x}");
            }
            Msg::StatusQuery { target, .. } | Msg::AffiliationQuery { target, .. } | Msg::RegistrationCommand { target, .. } => {
                m.source = *target as i64;
                m.meta = format!("{} unit {target}", o.msg.alias());
            }
            Msg::Affiliation { answer, group, target, .. } => {
                (m.talkgroup, m.source) = (*group as u32, *target as i64);
                if *answer == Answer::Accept {
                    m.kind = MessageType::Affiliation;
                }
                m.meta = format!("Affiliation {target} → TG {group}{}", refused(*answer));
            }
            Msg::LocationRegistration { answer, group, rfss, site, target } => {
                (m.talkgroup, m.source) = (*group as u32, *target as i64);
                if *answer == Answer::Accept {
                    m.kind = MessageType::Location;
                }
                m.meta = format!("Location registration {target} TG {group} at {rfss}-{site}{}", refused(*answer));
            }
            Msg::Registration { answer, unit, id, .. } => {
                m.source = *unit as i64;
                if *answer == Answer::Accept {
                    m.kind = MessageType::Registration;
                }
                let home = if id != unit { format!(" (ID {id})") } else { String::new() };
                m.meta = format!("Registration {unit}{home}{}", refused(*answer));
            }
            Msg::Deregistration { id, .. } => {
                (m.kind, m.source) = (MessageType::Deregistration, *id as i64);
                m.meta = format!("Deregistration {id}");
            }
            Msg::Identifier(i) => {
                let kind = if i.slots > 1 { format!(", {} slots", i.slots) } else { String::new() };
                m.meta = format!("IDEN {}: base {} MHz, step {} Hz{kind}", i.iden, mhz(i.base_hz), i.spacing_hz);
                if i.system.is_none() || i.system == self.system {
                    self.tables.insert(i.iden, Identifier { system: None, ..*i });
                } else {
                    let (w, s) = i.system.unwrap_or_default();
                    m.meta += &format!(" for system {w:x}.{s:x}: not this one's");
                }
            }
            Msg::RfssStatus { site: s, networked } => {
                self.on(&mut m, s.ch);
                (m.kind, m.sys_id, m.rfss, m.site) = (MessageType::SysId, s.sys_id as u32, s.rfss as u32, s.site as u32);
                m.meta = format!("RFSS status sysid {:x} rfss {} site {} CC {} MHz{}", s.sys_id, s.rfss, s.site, mhz(m.freq_hz), if *networked { "" } else { " (site trunking)" });
            }
            Msg::NetworkStatus(s) => {
                // Whether or not the channel resolves: the WACN is what lets
                // the site lock (and a band plan for this system) through.
                let wacn = s.wacn.unwrap_or(0);
                self.system = Some((wacn, s.sys_id));
                self.on(&mut m, s.ch);
                (m.kind, m.wacn, m.sys_id) = (MessageType::Status, wacn, s.sys_id as u32);
                m.meta = format!("Network status wacn {wacn:x} sysid {:x} CC {} MHz", s.sys_id, mhz(m.freq_hz));
            }
            Msg::AdjacentStatus { site: s, own_plan, conventional, failed, valid, .. } => {
                // A neighbour on another band plan (ADJ_STS_BCST_UNC) has a
                // channel this one can't resolve.
                if *own_plan {
                    self.on(&mut m, s.ch);
                }
                (m.sys_id, m.rfss, m.site) = (s.sys_id as u32, s.rfss as u32, s.site as u32);
                m.wacn = s.wacn.or(self.system.map(|(w, _)| w)).unwrap_or(0);
                let mut flags = String::new();
                for (on, f) in [(*conventional, " conventional"), (*failed, " failed"), (!*valid, " unverified")] {
                    if on {
                        flags += f;
                    }
                }
                // A site radios can move to: a trunked one that is up.
                if !conventional && !failed {
                    m.kind = MessageType::Adjacent;
                }
                m.meta = format!("Adjacent site {:x}.{} {} CC {} MHz{flags}", s.sys_id, s.rfss, s.site, mhz(m.freq_hz));
            }
            Msg::SecondaryControl { rfss, site, channels } => {
                for c in channels {
                    let mut u = m.clone();
                    self.on(&mut u, c.ch);
                    u.meta = format!("Secondary CC rfss {rfss} site {site}: {} MHz", mhz(u.freq_hz));
                    if u.freq_hz != 0 {
                        u.kind = MessageType::ControlChannel;
                    }
                    out.push(u);
                }
                return out;
            }
            Msg::SyncBroadcast { synced, time } => {
                let when = time.map_or(String::new(), |(y, mo, d, h, mi)| format!(", {y}-{mo:02}-{d:02} {h:02}:{mi:02}"));
                m.meta = format!("TDMA sync broadcast: traffic channels {}synced{when}", if *synced { "" } else { "not " });
            }
            Msg::MotPatchAdd { sg, groups } | Msg::MotPatchDelete { sg, groups } => {
                let add = matches!(o.msg, Msg::MotPatchAdd { .. });
                m.kind = if add { MessageType::PatchAdd } else { MessageType::PatchDelete };
                let p = Patch { sg: *sg as u32, ga: groups.map(u32::from) };
                m.meta = format!("Moto patch {} sg {}: {} {} {}", if add { "add" } else { "delete" }, p.sg, p.ga[0], p.ga[1], p.ga[2]);
                m.patch = Some(p);
            }
            Msg::MotPatchGrant { protected, ch, sg, source, .. } => {
                self.on(&mut m, *ch);
                (m.kind, m.encrypted, m.talkgroup, m.source) = (MessageType::Grant, *protected, *sg as u32, *source as i64);
                m.meta = format!("Patch grant SG {sg} {} MHz src {source}{}{}", mhz(m.freq_hz), slot(&m), enc(&m));
            }
            Msg::MotEmergencyAlarm { group, source } => {
                (m.kind, m.emergency, m.talkgroup, m.source) = (MessageType::EmergencyAlarm, true, *group as u32, *source as i64);
                m.meta = format!("Emergency alarm from {source} on TG {group}");
            }
            Msg::MotBaseStationId { callsign, ch } => {
                self.on(&mut m, *ch);
                m.kind = MessageType::BaseStationId;
                m.meta = format!("Base station ID {callsign} {} MHz", mhz(m.freq_hz));
                m.callsign = Some(callsign.clone());
            }
            Msg::HarrisRegroup { simulselect, sg, member, activate, key_id } => {
                let what = if *simulselect { "simulselect" } else { "patch" };
                let sg = *sg as u32;
                match *member {
                    HarrisMember::Talkgroup { group, algid } => {
                        let tg = group as u32;
                        m.kind = if *activate { MessageType::PatchAdd } else { MessageType::PatchDelete };
                        m.patch = Some(Patch { sg, ga: [tg, tg, tg] });
                        m.encrypted = algid != 0 && algid != 0x80;
                        let enc = if m.encrypted { format!(" {} key 0x{key_id:04x}", algid_name(algid)) } else { String::new() };
                        m.meta = format!("L3Harris {what} {} sg {sg} TG {tg}{enc}", if *activate { "add" } else { "delete" });
                    }
                    // A radio regrouped onto the supergroup: no talkgroup patched.
                    HarrisMember::Unit(unit) => {
                        m.source = unit as i64;
                        m.meta = format!("L3Harris {what} sg {sg}: unit {unit} {}", if *activate { "joins" } else { "leaves" });
                    }
                }
            }
            Msg::Protected => m.meta = format!("Protected opcode 0x{:02x}{}", o.opcode, mfid(o)),
            Msg::Other(name) => m.meta = format!("{name}{}", mfid(o)),
            Msg::Unknown => m.meta = format!("Opcode 0x{:02x}{}", o.opcode, mfid(o)),
        }
        out.push(m);
        out
    }
}

/// A refused answer, for the log.
fn refused(a: Answer) -> String {
    if a == Answer::Accept { String::new() } else { format!(" {}", a.name()) }
}

fn mfid(o: &Osp) -> String {
    if o.standard() { String::new() } else { format!(" mfid 0x{:02x}", o.mfid) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::osp::from_tsbk;

    /// Opcode `op` from manufacturer `mfid`, with `fields` (octets 2..10), through `p`.
    fn parse(p: &mut P25Parser, op: u8, mfid: u8, fields: [u8; 8]) -> Vec<Message> {
        let mut blk = [0u8; 12];
        blk[0] = 0x80 | op;
        blk[1] = mfid;
        blk[2..10].copy_from_slice(&fields);
        p.parse(&from_tsbk(&blk), 0x443, 0.0)
    }
    fn msg(op: u8, mfid: u8, fields: [u8; 8]) -> Message {
        parse(&mut P25Parser::default(), op, mfid, fields).pop().unwrap()
    }

    /// IDEN_UP 1: base 851.00625 MHz, 6.25 kHz steps; IDEN_UP_TDMA 2: 2 slots
    /// per 12.5 kHz carrier from 769.00625 MHz.
    fn with_band_plan() -> P25Parser {
        let mut p = P25Parser::default();
        let base = (851_006_250u32 / 5).to_be_bytes();
        parse(&mut p, 0x3d, 0, [0x10, 0x00, 0x00, 50, base[0], base[1], base[2], base[3]]);
        let base = (769_006_250u32 / 5).to_be_bytes();
        parse(&mut p, 0x33, 0, [0x23, 0x00, 0x00, 100, base[0], base[1], base[2], base[3]]);
        p
    }

    #[test]
    fn iden_then_grant_resolves_frequency() {
        let mut p = with_band_plan();
        assert_eq!(p.channel_to_hz(Channel(0x1000 | 100)), 851_006_250 + 100 * 6250);
        let m = parse(&mut p, 0x00, 0, [0x00, 0x10, 100, 0x01, 0x01, 0x0a, 0x0b, 0x0c]).pop().unwrap();
        assert_eq!((m.kind, m.freq_hz, m.talkgroup, m.source, m.voice.tdma()), (MessageType::Grant, 851_631_250, 0x0101, 0x0a0b0c, false));
    }

    /// AABC-D Figure 4.2.5-1 has no service options: a channel iden of 0xC
    /// doesn't make a unit-to-unit call emergency and encrypted.
    #[test]
    fn a_unit_to_unit_grant_has_no_options() {
        let m = msg(0x04, 0, [0xc0, 0x01, 0x04, 0x05, 0x06, 0x01, 0x02, 0x03]);
        assert_eq!((m.kind, m.source, m.talkgroup, m.encrypted, m.emergency, m.priority), (MessageType::UuVGrant, 0x010203, 0x040506, false, false, 0));
    }

    /// Two calls on the two slots of one TDMA carrier are two calls.
    #[test]
    fn an_update_names_both_slots_of_a_carrier() {
        let mut p = with_band_plan();
        let ms = parse(&mut p, 0x02, 0, [0x20, 0x02, 0x01, 0x01, 0x20, 0x03, 0x02, 0x02]);
        let got: Vec<_> = ms.iter().map(|m| (m.talkgroup, m.freq_hz, m.voice.slot().unwrap_or(0))).collect();
        assert_eq!(got, [(0x0101, 769_018_750, 0), (0x0202, 769_018_750, 1)]);
    }

    /// AABC-D §2.3.9.2: slot = channel MOD slots per carrier (4 on a type 4 carrier).
    #[test]
    fn a_four_slot_carriers_slot() {
        let mut p = P25Parser::default();
        parse(&mut p, 0x33, 0, [0x34, 0x00, 0x00, 100, 0, 0, 0, 1]);
        assert_eq!(p.resolve(Channel(0x3000 | 7)), (5 + 12_500, Some(3)));
    }

    /// A site whose band plan comes only in the extended IDEN_UP_TDMA (for
    /// its own WACN and System ID): its network status, though its channel
    /// can't be resolved yet, says which system it is, and the plan is taken.
    #[test]
    fn network_status_before_the_band_plan() {
        let mut p = P25Parser::default();
        let m = parse(&mut p, 0x3b, 0, [0x12, 0xbe, 0xe0, 0x09, 0xef, 0x10, 0x01, 0x70]).pop().unwrap();
        assert_eq!((m.kind, m.wacn, m.sys_id, m.freq_hz), (MessageType::Status, 0xbee00, 0x9ef, 0));
        let iden = |sys: u16| {
            let mut h = crate::p25::mbt::tests::ambtc_header(0x33, [0x11, 0xbe, 0xe0], [0x00 | (sys >> 8) as u8, sys as u8]);
            h[2] = 0;
            crate::p25::osp::from_mbt(&crate::p25::mbt::Mbt { header: h, data: vec![0x09, 0x15, 0x75, 0x62, 0xcb, 0x00, 0x32, 0] })
        };
        p.parse(&iden(0x3a9), 0x443, 0.0);
        assert!(p.tables.is_empty(), "another system's");
        p.parse(&iden(0x9ef), 0x443, 0.0);
        assert_eq!(p.channel_to_hz(Channel(0x1000)), 762_006_250);
    }

    /// SCCB_EXP is one channel: Channel (T), then Channel (R), the FNE's receive (not another CC).
    #[test]
    fn sccb_exp_is_one_control_channel() {
        let mut p = with_band_plan();
        let ms = parse(&mut p, 0x29, 0, [1, 7, 0x10, 0x05, 0, 0x10, 0x06, 0x70]);
        let ccs: Vec<u64> = ms.iter().filter(|m| m.kind == MessageType::ControlChannel).map(|m| m.freq_hz).collect();
        assert_eq!(ccs, [851_006_250 + 5 * 6250]);
    }

    /// A refused location registration (RV 3) is not an affiliation.
    #[test]
    fn a_refused_location_registration() {
        assert_eq!(msg(0x2b, 0, [0x00, 0x01, 0x01, 1, 7, 0x0a, 0x0b, 0x0c]).kind, MessageType::Location);
        assert_eq!(msg(0x2b, 0, [0x03, 0x01, 0x01, 1, 7, 0x0a, 0x0b, 0x0c]).kind, MessageType::Unknown);
    }

    /// A neighbour radios can move to; a failed or conventional one isn't.
    #[test]
    fn adjacent_sites() {
        let mut p = with_band_plan();
        let m = parse(&mut p, 0x3c, 0, [0x12, 0x33, 0xa9, 1, 7, 0x10, 0x01, 0x70]).pop().unwrap();
        assert_eq!((m.kind, m.sys_id, m.rfss, m.site, m.freq_hz), (MessageType::Adjacent, 0x3a9, 1, 7, 851_012_500));
        assert_ne!(parse(&mut p, 0x3c, 0, [0x12, 0x73, 0xa9, 1, 7, 0x10, 0x01, 0x70]).pop().unwrap().kind, MessageType::Adjacent);
        assert_ne!(parse(&mut p, 0x3c, 0, [0x12, 0xb3, 0xa9, 1, 7, 0x10, 0x01, 0x70]).pop().unwrap().kind, MessageType::Adjacent);
    }

    #[test]
    fn telephone_calls() {
        let mut p = with_band_plan();
        let m = parse(&mut p, 0x08, 0, [0x04, 0x10, 0x01, 0x02, 0x58, 0x0a, 0x0b, 0x0c]).pop().unwrap();
        assert_eq!((m.kind, m.talkgroup, m.source, m.freq_hz), (MessageType::PhoneGrant, 0x0a0b0c, 0x0a0b0c, 851_012_500));
        assert!(m.meta.ends_with("for 60.0 s"), "{}", m.meta);
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
        // Extended function: unit 0x0a0b0c regrouped onto supergroup 2001 (arguments top-justified).
        let m = msg(0x04, 0x90, [0x02, 0x00, 0x07, 0xd1, 0x00, 0x0a, 0x0b, 0x0c]);
        assert!(m.meta.ends_with("regrouped onto supergroup 2001"), "{}", m.meta);
    }

    #[test]
    fn deny_and_queued_responses() {
        // Standard deny: group voice on TG 0x0101 for unit 0x123456, target group not authorized.
        let m = msg(0x27, 0, [0x80, 0x31, 0x00, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert_eq!((m.kind, m.source, m.talkgroup), (MessageType::Deny, 0x123456, 0x0101));
        assert!(m.meta.ends_with("target group not authorized"), "{}", m.meta);
        // AIV clear: no talkgroup, whatever the octets say.
        let m = msg(0x27, 0, [0x00, 0x31, 0x00, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert_eq!(m.talkgroup, 0);
        // Motorola's own reason (RC set): a clear request on a secure supergroup.
        let m = msg(0x07, 0x90, [0x80, 0x01, 0x00, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert!(m.kind == MessageType::Deny && m.meta.ends_with("clear request on a secure supergroup"), "{}", m.meta);
        // Queued, no channel free.
        let m = msg(0x21, 0, [0x80, 0x40, 0x00, 0x01, 0x01, 0x12, 0x34, 0x56]);
        assert!(m.kind == MessageType::Queued && m.meta.ends_with("no channel free"), "{}", m.meta);
    }

    #[test]
    fn a_protected_message_is_not_read() {
        let mut blk = [0u8; 12];
        blk[0] = 0xc0; // last block, protected: GRP_V_CH_GRANT's arguments are encrypted
        blk[2..10].copy_from_slice(&[0x00, 0x10, 100, 0x01, 0x01, 0x0a, 0x0b, 0x0c]);
        let m = with_band_plan().parse(&from_tsbk(&blk), 0x443, 0.0).pop().unwrap();
        assert_eq!(m.kind, MessageType::Unknown);
    }

    #[test]
    fn the_band_plan_persists() {
        let p = with_band_plan();
        let mut q = P25Parser::default();
        q.bandplan_from_str(&p.bandplan_to_string());
        assert_eq!(q.tables, p.tables);
    }
}
