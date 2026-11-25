//! Receive-only Hytera XPT signaling. Inputs are integrity-checked DMR payloads,
//! MSB first; CRC/FEC, RF reception, TDMA and voice remain in the DMR receiver,
//! and what the site makes of them is in [`super::trunking::Site`].
//! Payload formats and receive-only behavior are described in docs/developer/protocols/xpt.md.

use super::slot::{FID_HYTERA, Lc, SlotEvent};
use super::trunking::Variant;
use crate::bits::field;
use serde::Serialize;

/// The XPT control message an event carries on a site heard as `variant`, if any: positive XPT
/// signaling. None on a site heard granting as standard trunking does ([`Variant::grants`]): Hytera
/// radios on a Tier III site send Hytera's own blocks too, which aren't XPT's there.
pub fn control_on(variant: Option<Variant>, e: &SlotEvent) -> Option<Control> {
    if variant.is_some_and(Variant::grants) {
        return None;
    }
    control(e)
}

/// The XPT control message an event carries, if any: positive XPT signaling.
pub fn control(e: &SlotEvent) -> Option<Control> {
    match e {
        SlotEvent::Csbk { csbk: k, mbc: false } => csbk(&k.0),
        SlotEvent::Lc { lc, .. } => flco(&lc.0),
        SlotEvent::Slco(raw) => slco(raw),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Control {
    Status { sequence: u8, free: u8, slots: [Occupancy; 6] },
    Adjacent { sequence: u8, sites: [Neighbor; 4] },
    Short { free: u8, priority: u8, target_hash: u8 },
    Setup { repeater: u8, slot: u8, group: bool, target: u16, source: u16, free: u8, handshake: u8, reserved: u8 },
    /// Data/control preamble, not a voice grant. Repeater hints are raw observations.
    Preamble { data: bool, group: bool, blocks: u8, free: u8, priority: u8, target: u16, priority_hash: u8, source: u16, reserved: u8 },
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
        // Unlike the status banks, these top bits retain their normal LB/PF meaning.
        // Do not interpret a protected preamble's payload as clear signaling.
        0x3d if b[0] & 0x40 == 0 => Some(Control::Preamble {
            data: b[2] & 0x80 != 0,
            group: b[2] & 0x40 != 0,
            blocks: b[3],
            free: b[4] >> 4,
            priority: b[4] & 15,
            target: u16::from_be_bytes([b[5], b[6]]),
            priority_hash: b[7],
            source: u16::from_be_bytes([b[8], b[9]]),
            reserved: b[2] & 0x3f,
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

/// A repeater number (1–8) as signalled; 0 and the rest say none.
pub fn repeater(n: u8) -> Option<u32> {
    (1..=8).contains(&n).then_some(n as u32)
}

/// XPT voice IDs occupy 16 bits: the bytes above them (bits 24–31, 48–55)
/// are cleared, so [`Lc::target`] and [`Lc::source`] read them. The channel
/// ([`super::Channel`]) applies it once its carrier is known to be XPT's;
/// FID 0x68 alone also occurs on other Hytera systems.
pub fn voice_lc(mut lc: Lc) -> Lc {
    if matches!(lc.fid(), 0 | FID_HYTERA) && matches!(lc.flco(), 0 | 3) {
        lc.0[3] = 0;
        lc.0[6] = 0;
    }
    lc
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
        assert_eq!(Lc([0xc9, 0x68, 0xc7, 0x30, 0, 25, 0xaa, 0x12, 0x34]).voice_user(), None, "a setup is not voice");
        let lc = voice_lc(voice(25, false));
        assert_eq!((lc.target(), lc.source()), (25, 0x1234));
        assert_eq!((repeater(0), repeater(5), repeater(15)), (None, Some(5), None));
    }

    #[test]
    fn captured_data_preambles_are_not_voice_grants_or_voice_repeater_assignments() {
        // 453.6375 MHz, supplied 20261110-025018/source-3 capture.
        let bytes = [0xbd, 0x68, 0x80, 0x12, 0xf0, 0x02, 0x08, 0, 0, 0x10];
        let preamble = csbk(&bytes).unwrap();
        assert_eq!(preamble, Control::Preamble {
            data: true, group: false, blocks: 18, free: 15, priority: 0,
            target: 520, priority_hash: 0, source: 16, reserved: 0,
        });
        let mut changed = bytes;
        changed[0] = 0x3d; // LB is not an XPT sequence number here.
        assert_eq!(csbk(&changed), Some(preamble.clone()));
        changed[0] |= 0x40;
        assert!(csbk(&changed).is_none(), "protected payload is not clear XPT evidence");
        changed = bytes;
        changed[1] = 0;
        assert!(csbk(&changed).is_none(), "standard DMR preamble has a different address layout");
        // Exercise fields that are zero in this capture, using the documented layout.
        assert_eq!(csbk(&[0x3d, 0x68, 0x45, 7, 0x23, 0x12, 0x34, 0xab, 0x56, 0x78]), Some(Control::Preamble {
            data: false, group: true, blocks: 7, free: 2, priority: 3,
            target: 0x1234, priority_hash: 0xab, source: 0x5678, reserved: 5,
        }));
    }
}
