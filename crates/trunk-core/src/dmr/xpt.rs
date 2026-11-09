//! Receive-only Hytera XPT signaling. Inputs are integrity-checked DMR payloads,
//! MSB first; CRC/FEC, RF reception, TDMA and voice remain in the DMR receiver.
//! Wire fields researched from DSD-FME audio_work, revision
//! 34753963b8855c6c07b79250cb4de4f2c38a4613; see docs/guides/xpt.md.

use super::slot::{FID_HYTERA, Lc, LcFrom};
use crate::bits::field;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Control {
    Status { sequence: u8, free: u8, slots: [Occupancy; 6] },
    Adjacent { sequence: u8, sites: [Neighbor; 4] },
    Short { free: u8, priority: u8, target_hash: u8 },
    Setup { repeater: u8, slot: u8, group: bool, target: u16, source: u16, free: u8, handshake: u8, reserved: u8 },
}

/// Target is an eight-bit group hint or private-target hash, never a radio ID.
/// Preserve the raw status: 1 and status/target combinations remain uncertain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Occupancy {
    pub status: u8,
    pub target_hint: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Neighbor {
    pub site: u8,
    pub free: u8,
    pub reserved: u8,
}

/// Parse exactly ten CSBK bytes, excluding CRC. Top bits are sequence, not LB/PF.
pub fn csbk(b: &[u8]) -> Option<Control> {
    if b.len() != 10 || b[1] != FID_HYTERA {
        return None;
    }
    let sequence = b[0] >> 6;
    match b[0] & 0x3f {
        0x0a if sequence <= 2 => Some(Control::Status {
            sequence,
            free: b[2] >> 4,
            slots: std::array::from_fn(|i| Occupancy { status: field(b, 20 + i * 2, 22 + i * 2) as u8, target_hint: b[4 + i] }),
        }),
        0x0b => Some(Control::Adjacent {
            sequence,
            sites: std::array::from_fn(|i| Neighbor {
                site: b[2 + i * 2] >> 3,
                free: b[3 + i * 2] >> 4,
                reserved: (b[2 + i * 2] & 7) << 4 | (b[3 + i * 2] & 15),
            }),
        }),
        _ => None,
    }
}

/// Five packed bytes containing all 36 short-LC bits (last nibble is padding).
/// The caller checks BPTC and CRC8 before passing them here. SLCO has no FID.
pub fn slco(b: &[u8]) -> Option<Control> {
    if b.len() != 5 || b[0] >> 4 != 8 || b[4] & 15 != 0 {
        return None;
    }
    Some(Control::Short { free: b[1] & 15, priority: b[2] >> 4, target_hash: ((b[2] & 15) << 4) | b[3] >> 4 })
}

/// Setup/alert is often carried in a terminator burst; it is not a voice terminator.
pub fn flco(b: &[u8]) -> Option<Control> {
    if b.len() != 9 || b[1] != FID_HYTERA || b[0] & 0x3f != 9 {
        return None;
    }
    Some(Control::Setup {
        repeater: b[2] >> 4,
        slot: b[0] >> 7,
        group: b[0] & 0x40 != 0,
        target: u16::from_be_bytes([b[4], b[5]]),
        source: u16::from_be_bytes([b[7], b[8]]),
        free: b[3] >> 4,
        handshake: b[3] & 15,
        reserved: b[2] & 15,
    })
}

/// XPT voice IDs occupy 16 bits. Apply only after this receiver/site detected
/// XPT; FID 0x68 alone also occurs on other Hytera systems.
pub fn voice_lc(mut lc: Lc, detected: bool) -> Lc {
    if detected && matches!(lc.fid(), 0 | FID_HYTERA) && matches!(lc.flco(), 0 | 3) {
        lc.0[3] = 0;
        lc.0[6] = 0;
    }
    lc
}

pub const SIGNAL_HOLD_S: f64 = 3.0;
pub const SETUP_HOLD_S: f64 = 4.0;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Timed<T> {
    pub value: T,
    pub heard_s: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Call {
    pub freq_hz: u64,
    pub repeater: Option<u32>,
    pub slot: u8,
    pub target: u32,
    pub source: u32,
    pub group: bool,
    pub encrypted: bool,
    pub late_entry: bool,
    pub heard_s: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    /// Configured home talkgroup → repeater; air traffic does not establish ownership.
    pub home_groups: BTreeMap<u32, u32>,
    pub free: Option<Timed<u8>>,
    pub priority: Option<Timed<(u8, u8)>>,
    /// Logical slot (1-based) → last observation; banks age independently.
    pub occupancy: BTreeMap<u32, Timed<Occupancy>>,
    pub neighbors: BTreeMap<u8, Timed<Neighbor>>,
    pub setups: Vec<Timed<Control>>,
    pub calls: Vec<Call>,
}

impl Status {
    /// Repeaters with at least one explicitly idle slot in fresh status banks.
    /// Unknown/private/inactive states never imply availability.
    pub fn available_repeaters(&self) -> Vec<u32> {
        let mut repeaters: Vec<u32> = self
            .occupancy
            .iter()
            .filter(|(lsn, v)| **lsn <= 16 && v.value.status == 0 && v.value.target_hint == 0)
            .map(|(&lsn, _)| (lsn - 1) / 2 + 1)
            .collect();
        repeaters.dedup();
        repeaters
    }
}

/// One tracker per configured site. No global state, neighbor activity merging,
/// hash-based ID learning, or guessed RF frequencies.
#[derive(Default)]
pub struct Tracker {
    pub state: Status,
}

impl Tracker {
    pub fn expire(&mut self, now: f64) {
        let fresh = |t: f64| now - t < SIGNAL_HOLD_S;
        if self.state.free.as_ref().is_some_and(|v| !fresh(v.heard_s)) {
            self.state.free = None;
        }
        if self.state.priority.as_ref().is_some_and(|v| !fresh(v.heard_s)) {
            self.state.priority = None;
        }
        self.state.occupancy.retain(|_, v| fresh(v.heard_s));
        self.state.neighbors.retain(|_, v| fresh(v.heard_s));
        self.state.setups.retain(|v| now - v.heard_s < SETUP_HOLD_S);
        self.state.calls.retain(|v| fresh(v.heard_s));
    }

    pub fn control(&mut self, message: Control, now: f64) {
        self.expire(now);
        let free = match &message {
            Control::Status { sequence, free, slots } => {
                for (i, &value) in slots.iter().enumerate() {
                    let lsn = *sequence as u32 * 6 + i as u32 + 1;
                    self.state.occupancy.insert(lsn, Timed { value, heard_s: now });
                }
                Some(*free)
            }
            Control::Short { free, priority, target_hash } => {
                self.state.priority = (*priority != 0).then_some(Timed { value: (*priority, *target_hash), heard_s: now });
                Some(*free)
            }
            Control::Adjacent { sites, .. } => {
                for &value in sites.iter().filter(|n| n.site != 0) {
                    self.state.neighbors.insert(value.site, Timed { value, heard_s: now });
                }
                None
            }
            Control::Setup { repeater, slot, free, .. } => {
                self.state
                    .setups
                    .retain(|v| !matches!(v.value, Control::Setup { repeater: r, slot: s, .. } if r == *repeater && s == *slot));
                self.state.setups.push(Timed { value: message.clone(), heard_s: now });
                Some(*free)
            }
        };
        if let Some(value) = free {
            self.state.free = (value != 0).then_some(Timed { value, heard_s: now });
        }
    }

    pub fn voice(&mut self, hz: u64, repeater: Option<u32>, slot: u8, lc: Lc, from: LcFrom, now: f64) {
        self.expire(now);
        let lc = voice_lc(lc, true);
        let Some(group) = lc.voice_user() else { return };
        if lc.target() == 0 {
            return;
        }
        let previous = self.state.calls.iter().find(|c| c.freq_hz == hz && c.slot == slot).cloned();
        // A delayed terminator must not tear down a replacement call.
        if from == LcFrom::Terminator {
            self.state.calls.retain(|c| !(c.freq_hz == hz && c.slot == slot && c.target == lc.target() && c.source == lc.source()));
            return;
        }
        self.state.calls.retain(|c| !(c.freq_hz == hz && c.slot == slot));
        let same = previous.as_ref().filter(|c| c.target == lc.target() && c.source == lc.source());
        self.state.calls.push(Call {
            freq_hz: hz,
            repeater,
            slot,
            target: lc.target(),
            source: lc.source(),
            group,
            encrypted: lc.encrypted() || (from == LcFrom::Embedded && same.is_some_and(|c| c.encrypted)),
            late_entry: same.map_or(from == LcFrom::Embedded, |c| c.late_entry),
            heard_s: now,
        });
        self.state.setups.retain(
            |v| !matches!(v.value, Control::Setup { target, source, .. } if target as u32 == lc.target() && source as u32 == lc.source()),
        );
    }

    pub fn privacy(&mut self, hz: u64, slot: u8) {
        if let Some(c) = self.state.calls.iter_mut().find(|c| c.freq_hz == hz && c.slot == slot) {
            c.encrypted = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Control {
        flco(&[0xc9, 0x68, 0x27, 0x30, 0, 25, 0xaa, 0x12, 0x34]).unwrap()
    }
    fn voice(target: u8, encrypted: bool) -> Lc {
        Lc([0, 0x68, if encrypted { 0x40 } else { 0 }, 0xab, 0, target, 0xcd, 0x12, 0x34])
    }

    #[test]
    fn wire_fields_and_sequence_are_not_protection_flags() {
        let message = csbk(&[0x8a, 0x68, 0x35, 0x9f, 1, 2, 3, 4, 5, 6]).unwrap();
        assert!(
            matches!(message, Control::Status { sequence: 2, free: 3, slots } if slots[0] == Occupancy { status: 1, target_hint: 1 } && slots[5].status == 3)
        );
        assert_eq!(
            setup(),
            Control::Setup { repeater: 2, slot: 1, group: true, target: 25, source: 0x1234, free: 3, handshake: 0, reserved: 7 }
        );
        assert_eq!(slco(&[0x80, 2, 0x34, 0x50, 0]), Some(Control::Short { free: 2, priority: 3, target_hash: 0x45 }));
        assert!(
            matches!(csbk(&[0x4b, 0x68, 0x2b, 0x47, 0, 0, 0, 0, 0, 0]), Some(Control::Adjacent { sequence: 1, sites }) if sites[0] == Neighbor { site: 5, free: 4, reserved: 0x37 })
        );
    }

    #[test]
    fn malformed_unknown_and_non_xpt_hytera() {
        for n in 0..20 {
            assert!(csbk(&vec![0; n]).is_none());
            assert!(flco(&vec![0; n]).is_none());
            assert!(slco(&vec![0; n]).is_none());
        }
        assert!(csbk(&[0xca, 0x68, 0, 0, 0, 0, 0, 0, 0, 0]).is_none(), "reserved bank cannot overrun channel state");
        assert!(csbk(&[0x0a, 0x10, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
        assert!(flco(&[0, 0x68, 0, 0, 0, 0, 0, 0, 0]).is_none(), "FID alone does not establish XPT");
        let setup_lc = Lc([0xc9, 0x68, 0xc7, 0x30, 0, 25, 0xaa, 0x12, 0x34]);
        assert_eq!((setup_lc.target(), setup_lc.source()), (25, 0x1234));
        assert!(!setup_lc.encrypted() && !setup_lc.emergency(), "setup repeater fields are not service options");
        let lc = voice(25, false);
        assert_eq!(voice_lc(lc, false), lc);
        assert_eq!((voice_lc(lc, true).target(), voice_lc(lc, true).source()), (25, 0x1234));
    }

    #[test]
    fn setup_free_changes_banks_and_site_isolation() {
        let mut a = Tracker::default();
        let b = Tracker::default();
        a.control(setup(), 0.0);
        assert_eq!(a.state.setups.len(), 1);
        assert!(a.state.calls.is_empty(), "setup is not confirmed voice");
        assert_eq!(a.state.free.as_ref().unwrap().value, 3);
        a.control(Control::Short { free: 4, priority: 0, target_hash: 0 }, 1.0);
        assert_eq!(a.state.free.as_ref().unwrap().value, 4);
        a.control(Control::Status { sequence: 0, free: 4, slots: [Occupancy::default(); 6] }, 1.0);
        a.control(Control::Status { sequence: 2, free: 4, slots: [Occupancy { status: 2, target_hint: 99 }; 6] }, 2.0);
        assert_eq!(a.state.occupancy.len(), 12);
        assert_eq!(a.state.available_repeaters(), [1, 2, 3]);
        assert_eq!(a.state.occupancy[&13].value.target_hint, 99);
        a.expire(4.1);
        assert_eq!(a.state.occupancy.len(), 6, "banks age separately");
        assert!(a.state.setups.is_empty());
        assert_eq!(b.state, Status::default(), "unrelated sites do not share state");
        a.control(Control::Short { free: 0, priority: 0, target_hash: 0 }, 4.2);
        assert!(a.state.free.is_none(), "no free repeater clears prior assignment");
        a.control(Control::Status { sequence: 2, free: 0, slots: [Occupancy::default(); 6] }, 4.3);
        assert_eq!(a.state.available_repeaters(), [7, 8], "reserved bank entries do not imply a ninth voice repeater");
    }

    #[test]
    fn concurrent_voice_late_entry_termination_and_stale_replacement() {
        let mut t = Tracker::default();
        t.control(setup(), 0.0);
        t.voice(451_000_000, Some(2), 1, voice(25, false), LcFrom::Header, 0.1);
        assert!(t.state.setups.is_empty());
        t.voice(451_000_000, Some(2), 0, voice(26, true), LcFrom::Embedded, 0.2);
        t.voice(452_000_000, Some(3), 1, voice(27, false), LcFrom::Embedded, 0.3);
        assert_eq!(t.state.calls.len(), 3);
        assert!(t.state.calls[1].encrypted && t.state.calls[1].late_entry);
        t.voice(451_000_000, Some(2), 1, voice(25, false), LcFrom::Terminator, 0.4);
        assert_eq!(t.state.calls.len(), 2);
        t.voice(451_000_000, Some(2), 0, voice(28, false), LcFrom::Header, 0.5);
        t.voice(451_000_000, Some(2), 0, voice(26, true), LcFrom::Terminator, 0.6);
        assert_eq!(t.state.calls.len(), 2, "old terminator cannot clear replacement");
        t.privacy(451_000_000, 0);
        assert!(t.state.calls.iter().find(|c| c.target == 28).unwrap().encrypted);
        t.expire(3.4);
        assert_eq!(t.state.calls.len(), 1);
        t.expire(3.6);
        assert!(t.state.calls.is_empty(), "lost terminators time out");
    }
}
