//! GPS positions radios send while they talk, in manufacturer-specific
//! link control (Phase 1) and MAC messages (Phase 2). They name no radio:
//! the position is the talker's. Layouts are SDRTrunk's (`lc/motorola`,
//! `lc/l3harris`, `mac/structure/l3harris/L3HarrisGPS`), from traffic it
//! has seen; the standard's own Location on PTT (TIA-102.AABF-D-2,
//! BBAD-A-1) isn't read.
//!
//! - Motorola, Phase 1: link control 0x06, MFID 0x90. Latitude and
//!   longitude, each a sign bit and 23 bits of a half-turn (90° / 180°).
//! - L3Harris, Phase 1: link control 0x2A then 0x2B, MFID 0xA4, octets
//!   2..9 of each, together 14 octets in L3Harris's format (below).
//! - L3Harris, Phase 2: MAC message 0xAA, MFID 0xA4, from its octet 3.
//!
//! L3Harris's format, per coordinate: 16 bits of ten-thousandths of a
//! minute, a hemisphere bit (south / west), 7 bits of minutes, 8 of degrees;
//! then a time of day and a heading, not read.

use crate::position::Position;

use super::alias::MacMsg;

const MOTOROLA: u8 = 0x90;
const HARRIS: u8 = 0xa4;

/// A believable position: on the globe, and not the 0, 0 of no fix.
fn plausible(lat: f64, lon: f64) -> Option<(f64, f64)> {
    (lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat, lon) != (0.0, 0.0)).then_some((lat, lon))
}

/// Motorola's: a link control word's octets 3..9.
fn motorola(w: &[u8; 9]) -> Option<(f64, f64)> {
    let half = |o: usize, turn: f64| {
        let v = ((w[o] as u32 & 0x7f) << 16 | (w[o + 1] as u32) << 8 | w[o + 2] as u32) as f64 * turn / 0x7f_ffff as f64;
        if w[o] & 0x80 != 0 { -v } else { v }
    };
    plausible(half(3, 90.0), half(6, 180.0))
}

/// L3Harris's: latitude then longitude, 4 octets each.
fn harris(d: &[u8]) -> Option<(f64, f64)> {
    let coord = |o: usize, max: u32| {
        let (frac, min, deg) = ((d[o] as u32) << 8 | d[o + 1] as u32, d[o + 2] as u32 & 0x7f, d[o + 3] as u32);
        if frac >= 10_000 || min >= 60 || deg > max {
            return None;
        }
        let v = deg as f64 + (min as f64 + frac as f64 / 10_000.0) / 60.0;
        Some(if d[o + 2] & 0x80 != 0 { -v } else { v })
    };
    if d.len() < 8 {
        return None;
    }
    plausible(coord(0, 90)?, coord(4, 180)?)
}

/// Collects positions from one voice channel's Phase 1 link control.
#[derive(Default)]
pub struct LcPositions {
    /// L3Harris: the first of the two words.
    harris: Option<[u8; 9]>,
}

impl LcPositions {
    /// One link control word (`mfid` as [`super::voice::LinkControl::mfid`]).
    /// `talker`: the unit talking now.
    pub fn lcw(&mut self, w: &[u8; 9], mfid: u8, talker: Option<u32>) -> Option<Position> {
        let (lat, lon, source) = match (mfid, w[0] & 0x3f) {
            (MOTOROLA, 0x06) => {
                let (lat, lon) = motorola(w)?;
                (lat, lon, "MotoP25_FDMA")
            }
            (HARRIS, 0x2a) => {
                self.harris = Some(*w);
                return None;
            }
            (HARRIS, 0x2b) => {
                let a = self.harris.take()?;
                let d: Vec<u8> = a[2..].iter().chain(&w[2..]).copied().collect();
                let (lat, lon) = harris(&d)?;
                (lat, lon, "HarrisP25_FDMA")
            }
            _ => return None,
        };
        Some(Position { unit: talker.filter(|&u| u != 0)?, lat, lon, source })
    }
}

/// A position in a Phase 2 MAC message. `talker`: the unit talking on the slot now.
pub fn mac(m: &MacMsg, talker: Option<u32>) -> Option<Position> {
    if (m.op, m.mfid) != (0xaa, HARRIS) {
        return None;
    }
    let (lat, lon) = harris(m.bytes.get(3..)?)?;
    Some(Position { unit: talker.filter(|&u| u != 0)?, lat, lon, source: "HarrisP25_TDMA" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    fn near(got: Option<Position>, lat: f64, lon: f64) {
        let p = got.expect("a position");
        assert!((p.lat - lat).abs() < 1e-5 && (p.lon - lon).abs() < 1e-5, "{p:?}");
    }

    /// SDRTrunk's L3Harris samples (L3HarrisTalkerGpsLocation), from the opcode on.
    #[test]
    fn l3harris_mac_position() {
        let b = hex("AAA41106CC031E150E8A53C0E69200061C05E00D9000");
        let m = MacMsg { op: 0xaa, mfid: HARRIS, bytes: &b };
        // 30° 3.1740′ N, 83° 10.5390′ W.
        near(mac(&m, Some(1234)), 30.0 + 3.174 / 60.0, -(83.0 + 10.539 / 60.0));
        assert_eq!(mac(&m, None), None, "nobody talking");
        assert_eq!(mac(&MacMsg { op: 0xaa, mfid: MOTOROLA, bytes: &b }, Some(1)), None);
    }

    #[test]
    fn l3harris_link_control_position_takes_both_words() {
        let mut p = LcPositions::default();
        let d = hex("06CC031E150E8A53C0E69200061C");
        let mut a = [0x2a, HARRIS, 0, 0, 0, 0, 0, 0, 0];
        let mut b = [0x2b, HARRIS, 0, 0, 0, 0, 0, 0, 0];
        a[2..].copy_from_slice(&d[..7]);
        b[2..].copy_from_slice(&d[7..]);
        assert_eq!(p.lcw(&b, HARRIS, Some(7)), None, "the second alone");
        assert_eq!(p.lcw(&a, HARRIS, Some(7)), None);
        near(p.lcw(&b, HARRIS, Some(7)), 30.0 + 3.174 / 60.0, -(83.0 + 10.539 / 60.0));
    }

    #[test]
    fn motorola_link_control_position() {
        // 38.9° N (38.9 / 90 of 0x7fffff), 77.03° W.
        let lat = (38.9 / 90.0 * 0x7f_ffff as f64).round() as u32;
        let lon = (77.03 / 180.0 * 0x7f_ffff as f64).round() as u32 | 0x80_0000;
        let mut w = [0x06, MOTOROLA, 0, 0, 0, 0, 0, 0, 0];
        w[3..6].copy_from_slice(&lat.to_be_bytes()[1..]);
        w[6..9].copy_from_slice(&lon.to_be_bytes()[1..]);
        near(LcPositions::default().lcw(&w, MOTOROLA, Some(5)), 38.9, -77.03);
        // No fix: 0, 0.
        assert_eq!(LcPositions::default().lcw(&[0x06, MOTOROLA, 0, 0, 0, 0, 0, 0, 0], MOTOROLA, Some(5)), None);
    }
}
