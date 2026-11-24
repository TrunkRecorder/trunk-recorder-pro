//! Embedded GPS Info full LC (FLCO 8), standard DMR and Hytera.
//! ETSI TS 102 361-2 §7.1.1.3 / Table 7.3 and §§7.2.15–17 define
//! signed 25-bit longitude and signed 24-bit latitude. Hytera FID 0x68
//! uses the same layout (SDRTrunk LCOpcode / GPSInformation).
//! See docs/developer/protocols/dmr-gps.md for sources and scope.

use super::slot::{FID_HYTERA, FID_STANDARD, Lc};
use crate::bits::field;
use crate::position::Position;

/// Maximum age of the voice LC naming a GPS word's talker, seconds.
/// As with talker aliases, do not attach new metadata to an old transmission.
pub const HOLD_S: f64 = 4.0;

/// Decode an integrity-checked LC; the word itself does not name a radio.
pub fn lc(lc: &Lc, talker: Option<u32>) -> Option<Position> {
    if lc.flco() != 8 || lc.0[0] & 0x80 != 0 {
        return None;
    }
    let source = match lc.fid() {
        FID_STANDARD => "DMR",
        FID_HYTERA => "HyteraDMR",
        _ => return None,
    };
    // Sign extend before scaling: two's complement, not sign/magnitude.
    let lon = ((field(&lc.0, 23, 48) << 7) as i32 >> 7) as f64 * 360.0 / (1u32 << 25) as f64;
    let lat = ((field(&lc.0, 48, 72) << 8) as i32 >> 8) as f64 * 180.0 / (1u32 << 24) as f64;
    if (lat, lon) == (0.0, 0.0) {
        return None;
    }
    Some(Position { unit: talker.filter(|&u| u != 0)?, lat, lon, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_and_hytera_use_signed_coordinates() {
        for fid in [FID_STANDARD, FID_HYTERA] {
            for (lon, lon_bytes) in [(90.0, [0x00, 0x80, 0, 0]), (-90.0, [0x01, 0x80, 0, 0])] {
                for (lat, lat_bytes) in [(45.0, [0x40, 0, 0]), (-45.0, [0xc0, 0, 0])] {
                    let mut b = [8, fid, 0, 0, 0, 0, 0, 0, 0];
                    b[2..6].copy_from_slice(&lon_bytes);
                    b[6..].copy_from_slice(&lat_bytes);
                    let p = lc(&Lc(b), Some(123)).unwrap();
                    assert_eq!((p.unit, p.lat, p.lon), (123, lat, lon));
                    assert_eq!(p.source, if fid == FID_STANDARD { "DMR" } else { "HyteraDMR" });
                    // Accuracy 7 means unknown; not necessarily an invalid fix.
                    b[2] |= 0x0e;
                    assert_eq!(lc(&Lc(b), Some(123)), Some(p));
                }
            }
        }
    }

    #[test]
    fn unknown_talker_no_fix_or_other_message_is_not_a_position() {
        let mut b = [8, 0, 0, 0x80, 0, 0, 0x40, 0, 0];
        assert_eq!(lc(&Lc(b), None), None);
        assert_eq!(lc(&Lc(b), Some(0)), None);
        b[1] = 0x10;
        assert_eq!(lc(&Lc(b), Some(5)), None);
        b[1] = 0;
        b[0] = 0;
        assert_eq!(lc(&Lc(b), Some(5)), None);
        b[0] = 0x88;
        assert_eq!(lc(&Lc(b), Some(5)), None, "protected LC");
        assert_eq!(lc(&Lc([8, 0, 0x0e, 0, 0, 0, 0, 0, 0]), Some(5)), None);
        assert!(lc(&Lc([8, 0, 0, 0, 0, 0, 0x40, 0, 0]), Some(5)).is_some());
        assert!(lc(&Lc([8, 0, 0, 0x80, 0, 0, 0, 0, 0]), Some(5)).is_some());
    }
}
