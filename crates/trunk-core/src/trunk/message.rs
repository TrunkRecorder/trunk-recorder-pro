//! Trunking messages (Trunk Recorder's `TrunkMessage`) and the P25 TSBK
//! parser (Trunk Recorder's P25Parser::decode_tsbk, opcode for opcode, with
//! its IDEN band-plan tables and TDMA slot mapping). Field positions are
//! `bitset_shift_mask(tsbk, shift, mask)` on the block as a 96-bit integer.

use std::collections::BTreeMap;
use std::fmt::Write;

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
    pub opcode: u8,
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
        let mfid = b(80, 0xff);
        match opcode {
            0x00 => {
                if mfid == 0x90 {
                    m.kind = MessageType::PatchAdd;
                    let p = Patch { sg: b(64, 0xffff), ga: [b(48, 0xffff), b(32, 0xffff), b(16, 0xffff)] };
                    m.meta = format!("Moto patch add sg {}: {} {} {}", p.sg, p.ga[0], p.ga[1], p.ga[2]);
                    m.patch = Some(p);
                } else {
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
            }
            0x02 => {
                if mfid == 0x90 {
                    opts(&mut m);
                    let ch = b(56, 0xffff);
                    m.kind = MessageType::Grant;
                    m.freq_hz = self.channel_to_hz(ch);
                    m.talkgroup = b(40, 0xffff);
                    m.source = b(16, 0xffffff) as i64;
                    set_slot(self, &mut m, ch);
                    m.meta = format!("Patch grant SG {} {} MHz src {}", m.talkgroup, mhz(m.freq_hz), m.source);
                } else {
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
            }
            0x03 => {
                if mfid == 0x90 {
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
                } else {
                    m.emergency = b(72, 0x80) != 0;
                    m.encrypted = b(72, 0x40) != 0;
                    let ch1 = b(48, 0xffff);
                    m.kind = MessageType::Update;
                    m.freq_hz = self.channel_to_hz(ch1);
                    m.talkgroup = b(16, 0xffff);
                    set_slot(self, &mut m, ch1);
                    m.meta = format!("Explicit update TG {} {} MHz", m.talkgroup, mhz(m.freq_hz));
                }
            }
            0x04 => {
                opts(&mut m);
                let ch = b(64, 0xffff);
                m.kind = MessageType::UuVGrant;
                m.freq_hz = self.channel_to_hz(ch);
                m.talkgroup = b(40, 0xffffff);
                m.source = b(16, 0xffffff) as i64;
                set_slot(self, &mut m, ch);
                m.meta = format!("Unit-to-unit grant {} → {} {} MHz", m.source, m.talkgroup, mhz(m.freq_hz));
            }
            0x05 => {
                if mfid == 0x90 {
                    m.meta = "MOTOROLA_OSP_TRAFFIC_CHANNEL_ID".into();
                } else {
                    opts(&mut m);
                    m.kind = MessageType::UuAnsReq;
                    m.source = b(16, 0xffffff) as i64;
                    m.talkgroup = b(40, 0xffffff);
                    m.meta = format!("Unit-to-unit answer request {} → {}", m.talkgroup, m.source);
                }
            }
            0x06 => {
                let ch = b(64, 0xffff);
                m.kind = MessageType::UuVUpdate;
                m.freq_hz = self.channel_to_hz(ch);
                m.talkgroup = b(40, 0xffffff);
                m.source = b(16, 0xffffff) as i64;
                set_slot(self, &mut m, ch);
                m.meta = format!("Unit-to-unit update {} → {} {} MHz", m.source, m.talkgroup, mhz(m.freq_hz));
            }
            0x14 => {
                m.emergency = b(72, 0x80) != 0;
                m.encrypted = b(72, 0x40) != 0;
                m.duplex = b(72, 0x20) != 0;
                m.mode = b(72, 0x10) != 0;
                m.kind = MessageType::DataGrant;
                m.source = b(16, 0xffffff) as i64;
                m.freq_hz = self.channel_to_hz(b(56, 0xffff));
                m.meta = format!("Data grant src {} {} MHz", m.source, mhz(m.freq_hz));
            }
            0x1f => {
                m.kind = MessageType::CallAlert;
                m.source = b(16, 0xffffff) as i64;
                m.talkgroup = b(40, 0xffffff);
                m.meta = format!("Call alert {} → {}", m.source, m.talkgroup);
            }
            0x20 => {
                m.kind = MessageType::Acknowledge;
                m.talkgroup = b(40, 0xffff);
                m.source = b(16, 0xffffff) as i64;
                m.meta = format!("Acknowledge src {}", m.source);
            }
            0x28 => {
                m.kind = MessageType::Affiliation;
                m.source = b(16, 0xffffff) as i64;
                m.talkgroup = b(40, 0xffff);
                m.meta = format!("Affiliation {} → TG {}", m.source, m.talkgroup);
            }
            0x29 | 0x39 => {
                let (f1, f2) = (self.channel_to_hz(b(48, 0xffff)), self.channel_to_hz(b(24, 0xffff)));
                m.meta = format!("Secondary CC rfss {} site {}: {} / {} MHz", b(72, 0xff), b(64, 0xff), mhz(f1), mhz(f2));
                if f1 != 0 && f2 != 0 {
                    m.kind = MessageType::ControlChannel;
                    m.freq_hz = f1;
                    out.push(m.clone());
                    m.freq_hz = f2;
                }
            }
            0x2b => {
                m.kind = MessageType::Location;
                m.talkgroup = b(56, 0xffff);
                m.source = b(16, 0xffffff) as i64;
                m.meta = format!("Location registration {} TG {}", m.source, m.talkgroup);
            }
            0x2c => {
                m.kind = MessageType::Registration;
                m.source = b(40, 0xffffff) as i64;
                m.meta = format!("Registration {}", m.source);
            }
            0x2f => {
                m.kind = MessageType::Deregistration;
                m.source = b(16, 0xffffff) as i64;
                m.meta = format!("Deregistration {}", m.source);
            }
            0x30 => {
                if mfid == 0xa4 {
                    let ga = b(16, 0xffffff) & 0xffff;
                    let sg = b(56, 0xffff);
                    let add = b(77, 0x01) == 1;
                    m.kind = if add { MessageType::PatchAdd } else { MessageType::PatchDelete };
                    m.patch = Some(Patch { sg, ga: [ga, ga, ga] });
                    m.meta = format!("M/A-COM patch {} sg {sg} TG {ga}", if add { "add" } else { "delete" });
                } else {
                    m.meta = "TDMA sync broadcast".into();
                }
            }
            0x33 => {
                if mfid == 0 {
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
            }
            0x34 => {
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
            0x3a => {
                m.kind = MessageType::SysId;
                m.sys_id = b(56, 0xfff);
                m.rfss = b(48, 0xff);
                m.site = b(40, 0xff);
                m.meta = format!("RFSS status sysid {:x} rfss {} site {}", m.sys_id, m.rfss, m.site);
            }
            0x3b => {
                let f1 = self.channel_to_hz(b(24, 0xffff));
                m.meta = format!("Network status wacn {:x} sysid {:x} CC {} MHz", b(52, 0xfffff), b(40, 0xfff), mhz(f1));
                if f1 != 0 {
                    m.kind = MessageType::Status;
                    m.wacn = b(52, 0xfffff);
                    m.sys_id = b(40, 0xfff);
                    m.freq_hz = f1;
                }
            }
            0x3c => {
                m.kind = MessageType::Adjacent;
                m.sys_id = b(56, 0xfff);
                m.rfss = b(48, 0xff);
                m.site = b(40, 0xff);
                m.freq_hz = self.channel_to_hz(b(24, 0xffff));
                m.meta = format!("Adjacent site rfss {} site {} CC {} MHz", m.rfss, m.site, mhz(m.freq_hz));
            }
            0x3d => {
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
            _ => m.meta = format!("TSBK opcode 0x{opcode:02x}"),
        }
        out.push(m);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iden_then_grant_resolves_frequency() {
        let mut p = TsbkParser::default();
        // IDEN_UP id 1: base 851.00625 MHz (170201250 × 5 Hz), step 6.25 kHz (50 × 125).
        let base = 851_006_250u64 / 5;
        let t: u128 = (0x3d_u128 << 88) | (1u128 << 76) | (0x001u128 << 58) | (50u128 << 48) | ((base as u128) << 16);
        let mut blk = [0u8; 12];
        for (i, v) in blk.iter_mut().enumerate() {
            *v = (t >> (88 - 8 * i)) as u8;
        }
        p.parse(&blk, 0x443, 0.0);
        assert_eq!(p.channel_to_hz((1 << 12) | 100), 851_006_250 + 100 * 6250);
    }
}
