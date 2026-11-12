//! Informational Hytera signaling outside the confirmed XPT voice controls.
//! Inputs have already passed the slot decoder's integrity checks. Unknown
//! fields remain payload bytes: none of these messages establishes XPT, opens
//! a voice call, changes privacy, or learns a frequency/site identifier.

use super::slot::{FID_HYTERA, LcFrom, SlotEvent};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Diagnostic {
    /// FLCO 0x30 in a terminator burst: TD_LC, not a voice termination.
    DataTerminator { payload: [u8; 7] },
    /// Captured repeatedly on the data carrier; semantics are not established.
    ProprietaryCsbk { opcode: u8, flags: u8, payload: [u8; 8] },
    ProprietaryLc { opcode: u8, flags: u8, payload: [u8; 7] },
    /// SLCO 0x0c with the observed embedded manufacturer value 0x68.
    /// `payload` is the remaining 16 data bits, not an asserted system/radio ID.
    ProprietaryShortLc { opcode: u8, payload: u16 },
}

pub fn diagnostic(event: &SlotEvent) -> Option<Diagnostic> {
    match event {
        SlotEvent::Csbk { csbk, mbc: false } if csbk.fid() == FID_HYTERA && csbk.opcode() == 0x0d => {
            let b = &csbk.0;
            Some(Diagnostic::ProprietaryCsbk {
                opcode: csbk.opcode(), flags: b[0] >> 6,
                payload: b[2..].try_into().ok()?,
            })
        }
        SlotEvent::Lc { lc, from } if lc.fid() == FID_HYTERA => {
            let b = &lc.0;
            let payload = b[2..].try_into().ok()?;
            match (lc.flco(), from) {
                (0x30, LcFrom::Terminator) => Some(Diagnostic::DataTerminator { payload }),
                (0x0d, _) => Some(Diagnostic::ProprietaryLc {
                    opcode: lc.flco(), flags: b[0] >> 6, payload,
                }),
                _ => None,
            }
        }
        SlotEvent::Slco(b) if b[0] >> 4 == 0x0c && (b[0] << 4 | b[1] >> 4) == FID_HYTERA && b[4] & 15 == 0 => {
            Some(Diagnostic::ProprietaryShortLc { opcode: 0x0c, payload: ((b[1] as u16 & 15) << 12) | (b[2] as u16) << 4 | (b[3] as u16 >> 4) })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmr::{Csbk, Lc};

    #[test]
    fn captured_proprietary_payloads_stay_uninterpreted() {
        let csbk = Csbk([0x8d, 0x68, 0x7f, 0xff, 0, 0, 0, 0, 0, 0]);
        assert_eq!(diagnostic(&SlotEvent::Csbk { csbk, mbc: false }), Some(Diagnostic::ProprietaryCsbk {
            opcode: 13, flags: 2, payload: [0x7f, 0xff, 0, 0, 0, 0, 0, 0],
        }));
        assert!(diagnostic(&SlotEvent::Csbk { csbk, mbc: true }).is_none());
        let mut other = csbk;
        other.0[1] = 0x10;
        assert!(diagnostic(&SlotEvent::Csbk { csbk: other, mbc: false }).is_none());
        assert_eq!(diagnostic(&SlotEvent::Lc { lc: Lc([0x8d, 0x68, 0, 0x7f, 0xff, 0x80, 0, 2, 8]), from: LcFrom::Terminator }), Some(Diagnostic::ProprietaryLc {
            opcode: 13, flags: 2, payload: [0, 0x7f, 0xff, 0x80, 0, 2, 8],
        }));
        assert_eq!(diagnostic(&SlotEvent::Slco([0xc6, 0x87, 0xff, 0xfe, 0xe0])), Some(Diagnostic::ProprietaryShortLc { opcode: 12, payload: 0x7fff }));
        assert!(diagnostic(&SlotEvent::Slco([0xc1, 0x07, 0xff, 0xfe, 0xe0])).is_none());
        assert!(diagnostic(&SlotEvent::Slco([0xc6, 0x87, 0xff, 0xfe, 0xe1])).is_none());
    }

    #[test]
    fn data_terminator_is_not_a_voice_lc_or_an_encryption_indication() {
        let lc = Lc([0x30, 0x68, 0xf0, 0x7f, 0xff, 0, 0, 0x10, 0x60]);
        assert_eq!(lc.voice_user(), None);
        assert_eq!(diagnostic(&SlotEvent::Lc { lc, from: LcFrom::Terminator }), Some(Diagnostic::DataTerminator { payload: [0xf0, 0x7f, 0xff, 0, 0, 0x10, 0x60] }));
        assert!(diagnostic(&SlotEvent::Lc { lc, from: LcFrom::Header }).is_none());
        assert!(diagnostic(&SlotEvent::Lc { lc, from: LcFrom::Embedded }).is_none());
    }
}
