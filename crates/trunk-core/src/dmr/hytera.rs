//! Hytera's applications in DMR short data: radio registration (RRS) and the location protocol
//! (LP), its reports as text and, experimentally, compressed. Each is a Hytera application
//! message (HDAP) after two octets of Hytera's own. See docs/developer/protocols/dmr-gps.md for
//! the sources and what's left unread.

use super::data::Packet;
use crate::bits::hex;
use crate::position::Position;
use crate::trunk::message::{Message, MessageType};

/// HDAP protocols: the location protocol, radio registration, and the compressed location
/// reports seen on the air (protocol 1 with the location protocol's opcodes).
pub const LP: u8 = 0x08;
pub const RRS: u8 = 0x11;
pub const LP_COMPRESSED: u8 = 0x01;

/// One HDAP message (dmr-kaitai `hytera_dmr_application_protocol.ksy`): its protocol (the
/// header's low 7 bits; the top one asks for an acknowledgement), opcode and body. `framed`: its
/// length, checksum and closing `03` checked; a compressed report has none of them that's known.
#[derive(Debug, PartialEq, Eq)]
pub struct Hdap<'a> {
    pub protocol: u8,
    pub opcode: u16,
    pub body: &'a [u8],
    pub framed: bool,
}

/// The HDAP message in a short-data payload: after Hytera's two octets (`01 00`, `02 00`; what
/// they say isn't known), header, opcode, length (u16, big endian), body, checksum (the octets
/// from the opcode to the body's end, summed, complemented, plus 0x33) and `03`.
pub fn hdap(payload: &[u8]) -> Option<Hdap<'_>> {
    let b = payload.get(2..)?;
    let (protocol, opcode) = (*b.first()? & 0x7f, u16::from_be_bytes([*b.get(1)?, *b.get(2)?]));
    if protocol == LP_COMPRESSED {
        return Some(Hdap { protocol, opcode, body: &b[3..], framed: false });
    }
    let n = u16::from_be_bytes([*b.get(3)?, *b.get(4)?]) as usize;
    if b.len() != n + 7 || b[n + 6] != 3 {
        return None;
    }
    let sum = b[1..n + 5].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    ((!sum).wrapping_add(0x33) == b[n + 5]).then_some(Hdap { protocol, opcode, body: &b[5..5 + n], framed: true })
}

/// A checked short-data packet as a message: a location report (with the position, from the
/// packet's sender), a registration, or else its octets, raw. Never a grant.
pub fn packet(p: &Packet) -> Message {
    let mut m = Message {
        kind: MessageType::Status,
        source: p.source() as i64,
        meta: format!("DMR short data from {} to {}: {}", p.source(), p.destination(), hex(&p.payload)),
        ..Default::default()
    };
    let Some(h) = hdap(&p.payload) else { return m };
    match h.protocol {
        LP => {
            m.kind = MessageType::Location;
            m.meta = format!("Hytera location report from {} (opcode {:04x}): {}", p.source(), h.opcode, hex(h.body));
            if let Some((pos, said)) = location_text(&h, p.source()) {
                m.meta = format!("Hytera location report from {}: {:.6}, {:.6} ({said})", pos.unit, pos.lat, pos.lon);
                m.position = Some(pos);
            }
        }
        LP_COMPRESSED if h.opcode == 0xd003 => {
            m.kind = MessageType::Location;
            m.meta = format!("Hytera compressed location report from {}: {}", p.source(), hex(&p.payload));
            if let Some(pos) = compressed_gps(p) {
                let b = &p.payload;
                m.meta = format!(
                    "Hytera compressed location report from {}: {:.6}, {:.6} (experimental N/W profile; UTC {:02}:{:02}:{:02}, {:02}-{:02}-{:02}; raw {})",
                    pos.unit, pos.lat, pos.lon, b[9], b[10], b[11], b[14], b[13], b[12], hex(b)
                );
                m.position = Some(pos);
            }
        }
        RRS if h.framed => {
            let Some(ip) = h.body.get(..4) else { return m };
            let address = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
            let data = h.body;
            let detail = match (h.opcode, data.len()) {
                (3, 4) => {
                    m.kind = MessageType::Registration;
                    "registration request".into()
                }
                (1, 4) => {
                    m.kind = MessageType::Deregistration;
                    "offline notification".into()
                }
                (2, 4) => "status check".into(),
                (0x80, 9) => {
                    m.kind = MessageType::Acknowledge;
                    format!("registration answer result {} renewal {} s", data[4], u32::from_be_bytes(data[5..9].try_into().unwrap()))
                }
                (0x82, 5) => format!("status answer state {}", data[4]),
                _ => return m,
            };
            // The answering dispatcher is the sender, not the registered radio: both are said.
            m.meta = format!("Hytera RRS {detail} for IP {address}, from {} to {}", p.source(), p.destination());
        }
        _ => {}
    }
    m
}

/// Location protocol reports whose body ends with the GPS data: an answer to a request (A002),
/// emergency (B003), triggered (C003) and condition (D003) reports.
const LP_REPORTS: [u16; 4] = [0xa002, 0xb003, 0xc003, 0xd003];

/// A location report's GPS data, the 40 ASCII octets ending its body (dmr-kaitai `gpsdata.ksy`,
/// dmrshark `dmr-data.c`): status `A` (a fix) or `V`, time HHMMSS and date DDMMYY (UTC), N/S,
/// latitude DDMM.MMMM, E/W, longitude DDDMM.MMMM, speed (knots) and heading (degrees), 3 digits
/// each. The position, from radio `unit`, and the rest said.
fn location_text(h: &Hdap, unit: u32) -> Option<(Position, String)> {
    if !h.framed || !LP_REPORTS.contains(&h.opcode) || unit == 0 {
        return None;
    }
    let g = h.body.get(h.body.len().checked_sub(40)?..)?;
    let text = |a: usize, b: usize| std::str::from_utf8(&g[a..b]).ok();
    let digits = |a: usize, b: usize| text(a, b).filter(|t| t.bytes().all(|c| c.is_ascii_digit()));
    // Degrees, then minutes as MM.MMMM.
    let angle = |a: usize, deg: usize, limit: f64| -> Option<f64> {
        let d: f64 = digits(a, a + deg)?.parse().ok()?;
        let m = text(a + deg, a + deg + 7).filter(|t| t.as_bytes()[2] == b'.')?.parse::<f64>().ok()?;
        let v = d + m / 60.0;
        (m < 60.0 && v <= limit).then_some(v)
    };
    if g[0] != b'A' {
        return None;
    }
    let lat = angle(14, 2, 90.0)? * match g[13] { b'N' => 1.0, b'S' => -1.0, _ => return None };
    let lon = angle(24, 3, 180.0)? * match g[23] { b'E' => 1.0, b'W' => -1.0, _ => return None };
    let (time, date) = (digits(1, 7)?, digits(7, 13)?);
    let (speed, heading) = (digits(34, 37).unwrap_or("?"), digits(37, 40).unwrap_or("?"));
    let said = format!("UTC {}:{}:{}, {}-{}-{}; {speed} kn, heading {heading}°", &time[..2], &time[2..4], &time[4..], &date[4..], &date[2..4], &date[..2]);
    Some((Position { unit, lat, lon, source: "HyteraDMR_LP" }, said))
}

/// Observed 25-byte compressed LP report, plus its two-byte short-data
/// prefix. Only the observed 0x8/0x8 N/W flag profile is supported. Other
/// hemispheres/fix flags remain raw; do not guess their meanings.
pub fn compressed_gps(p: &Packet) -> Option<Position> {
    let b = &p.payload;
    if p.source() == 0 || b.len() != 27 || !b.starts_with(&[1, 0, 1, 0xd0, 3, 0]) {
        return None;
    }
    // Binary UTC hh mm ss dd MM yy. Reject absent or malformed fixes.
    let (hour, min, sec, day, month, year) = (b[9], b[10], b[11], b[12], b[13], b[14]);
    if hour > 23 || min > 59 || sec > 60 || month == 0 || month > 12 || year > 99 {
        return None;
    }
    let days = match month {
        2 => {
            if year % 4 == 0 {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day == 0 || day > days {
        return None;
    }
    let lat = u32::from_le_bytes(b[15..19].try_into().unwrap());
    let lon = u32::from_le_bytes(b[19..23].try_into().unwrap());
    if lat >> 28 != 8 || lon >> 28 != 8 {
        return None;
    }
    fn degrees(v: u32, limit: u32) -> Option<f64> {
        let v = v & 0x0fff_ffff;
        let (d, m) = (v / 1_000_000, v % 1_000_000);
        if m >= 600_000 || d > limit || (d == limit && m != 0) {
            return None;
        }
        Some(d as f64 + m as f64 / 600_000.0)
    }
    let (lat, lon) = (degrees(lat, 90)?, -degrees(lon, 180)?);
    if (lat, lon) == (0.0, 0.0) {
        return None;
    }
    Some(Position { unit: p.source(), lat, lon, source: "HyteraDMRCompressed" })
}

/// Capture-verified data-channel profiles, not general XPT grants, for `trunk-pro tool dmr` (the
/// live decoder doesn't report them: their fields aren't documented). The activity word is
/// exposed without assigning its bits to repeater numbers. Radio/slot attribution was
/// cross-checked against independent packet headers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum DataControl {
    Beacon { activity: u16, raw: [u8; 10] },
    ShortStatus { activity: u16, raw: [u8; 5] },
    Start { slot: u8, source: u16, activity: u16, raw: [u8; 9] },
    End { source: u16, activity: u16, raw: [u8; 9] },
}

impl DataControl {
    /// Observed high-bit busy indication, not a general repeater bitmap.
    /// In the supplied one-data-carrier sample it agrees with both CACH
    /// access bits (325/325 short words, allowing 60 ms assembly latency).
    pub fn busy_hint(&self) -> bool {
        let (Self::Beacon { activity, .. } | Self::ShortStatus { activity, .. } | Self::Start { activity, .. } | Self::End { activity, .. }) = self;
        activity & 0x8000 != 0
    }
}

/// Only the observed clear layouts are named. Unknown flags/layouts remain
/// raw in `control`; these messages never create voice grants or LCN mappings.
pub fn data_control(e: &super::slot::SlotEvent) -> Option<DataControl> {
    use super::slot::{FID_HYTERA, LcFrom, SlotEvent};
    let activity = |a, b| {
        let word = u16::from_be_bytes([a, b]);
        matches!(word, 0x7fff | 0xffff).then_some(word)
    };
    match e {
        SlotEvent::Csbk { csbk, mbc: false } if csbk.0[0] == 0x8d && csbk.fid() == FID_HYTERA && csbk.0[4..].iter().all(|&b| b == 0) => {
            Some(DataControl::Beacon { activity: activity(csbk.0[2], csbk.0[3])?, raw: csbk.0 })
        }
        SlotEvent::Slco(b) if b[0] >> 4 == 12 && crate::bits::field(b, 4, 12) == FID_HYTERA as u32 && b[4] & 15 == 0 => {
            let word = crate::bits::field(b, 12, 28) as u16;
            matches!(word, 0x7fff | 0xffff).then_some(DataControl::ShortStatus { activity: word, raw: *b })
        }
        SlotEvent::Lc { lc, from: LcFrom::Terminator } if lc.fid() == FID_HYTERA => {
            let b = lc.0;
            let word = activity(b[3], b[4])?;
            if b[0] & 0x7f == 0x0d && b[2] == 0 && b[5] == 0x80 && b[6] == 0 {
                let source = u16::from_be_bytes([b[7], b[8]]);
                return (source != 0).then_some(DataControl::Start { slot: b[0] >> 7, source, activity: word, raw: b });
            }
            if b[0] == 0x30 && b[2] == 0xf0 && b[5] == 0 && b[8] == 0x60 {
                // This data-end profile's source is one octet earlier than
                // a voice LC's source. Do not use Lc::source here.
                let source = u16::from_be_bytes([b[6], b[7]]);
                return (source != 0).then_some(DataControl::End { source, activity: word, raw: b });
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmr::data::tests::{gps, rrs};
    fn fixture(h: [u8; 12], mut b: Vec<u8>) -> Packet {
        b.truncate(b.len() - 4 - (h[9] / 8) as usize);
        Packet { header: h, payload: b }
    }
    #[test]
    fn compressed_coordinates_are_degrees_and_minutes_not_microdegrees() {
        let (h, b) = gps();
        let p = fixture(h, b);
        let m = packet(&p);
        let pos = m.position.unwrap();
        assert_eq!((m.kind, pos.unit, pos.lat, pos.lon), (MessageType::Location, 123, 45.5, -90.25));
        assert_eq!(pos.source, "HyteraDMRCompressed");
        assert!(m.meta.contains("experimental N/W"));
    }
    #[test]
    fn unknown_flags_invalid_date_and_bad_coordinates_stay_raw() {
        let (h, b) = gps();
        let p = fixture(h, b);
        for (index, value) in [(18, 0x02), (18, 0xc2), (22, 0x05), (9, 24), (10, 60), (12, 0), (13, 13), (12, 32)] {
            let mut q = p.clone();
            q.payload[index] = value;
            assert!(packet(&q).position.is_none(), "index {index} value {value}");
            assert!(packet(&q).meta.contains(&hex(&q.payload)));
        }
        for (offset, v) in [(15, 0x8000_0000 | 45_600_000u32), (19, 0x8000_0000 | 181_000_000)] {
            let mut q = p.clone();
            q.payload[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
            assert!(packet(&q).position.is_none());
        }
        for len in 0..p.payload.len() {
            let mut q = p.clone();
            q.payload.truncate(len);
            assert!(packet(&q).position.is_none());
        }
    }
    /// An HDAP message after Hytera's two octets: header, opcode, length, body, checksum, `03`.
    fn frame(protocol: u8, opcode: u16, body: &[u8]) -> Vec<u8> {
        let mut b = vec![1, 0, protocol];
        b.extend(opcode.to_be_bytes());
        b.extend((body.len() as u16).to_be_bytes());
        b.extend(body);
        let sum = b[3..].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        b.extend([(!sum).wrapping_add(0x33), 3]);
        b
    }

    /// A condition report (D003): request ID, the radio's IP, then the GPS data as text.
    fn text_report(gps: &str) -> Packet {
        let mut body = vec![0, 0, 0, 7, 10, 0, 0, 123];
        body.extend(gps.as_bytes());
        Packet { header: gps_packet().0, payload: frame(LP, 0xd003, &body) }
    }

    use crate::dmr::data::tests::gps as gps_packet;

    #[test]
    fn a_text_location_report_is_the_senders_position_in_any_hemisphere() {
        // 33°52.1234' S, 151°12.5678' E at 12:34:56 UTC on 23 Nov 2026, 12 knots heading 90°.
        let m = packet(&text_report("A123456231126S3352.1234E15112.5678012090"));
        let pos = m.position.expect("position");
        assert_eq!((m.kind, pos.unit, pos.source), (MessageType::Location, 123, "HyteraDMR_LP"));
        assert!((pos.lat + (33.0 + 52.1234 / 60.0)).abs() < 1e-9 && (pos.lon - (151.0 + 12.5678 / 60.0)).abs() < 1e-9, "{pos:?}");
        assert!(m.meta.contains("UTC 12:34:56, 26-11-23; 012 kn, heading 090°"), "{}", m.meta);
        let north_west = packet(&text_report("A123456231126N4530.0000W09015.0000000000")).position.unwrap();
        assert_eq!((north_west.lat, north_west.lon), (45.5, -90.25));
        // No fix, a field that isn't one, or minutes past 59: a report, no position.
        for gps in ["V123456231126S3352.1234E15112.5678012090", "A123456231126X3352.1234E15112.5678012090", "A123456231126S3372.1234E15112.5678012090", "A123456231126S33x2.1234E15112.5678012090"] {
            let m = packet(&text_report(gps));
            assert_eq!((m.kind, m.position), (MessageType::Location, None), "{gps}");
        }
        // A request (A001) carries no position; a broken checksum isn't HDAP at all.
        let mut body = vec![0, 0, 0, 7, 10, 0, 0, 123];
        body.extend(b"A123456231126S3352.1234E15112.5678012090");
        assert_eq!(packet(&Packet { header: gps_packet().0, payload: frame(LP, 0xa001, &body) }).position, None);
        let mut bad = text_report("A123456231126S3352.1234E15112.5678012090");
        let n = bad.payload.len();
        bad.payload[n - 2] ^= 1;
        assert_eq!((packet(&bad).kind, hdap(&bad.payload)), (MessageType::Status, None));
    }

    #[test]
    fn rrs_checks_internal_checksum_and_keeps_sender_separate() {
        let (h, b) = rrs();
        let p = fixture(h, b);
        let m = packet(&p);
        assert_eq!((m.kind, m.source, m.position), (MessageType::Registration, 123, None));
        assert!(m.meta.contains("10.0.0.123"));
        let mut bad = p.clone();
        bad.payload[7] ^= 1;
        assert_eq!(packet(&bad).kind, MessageType::Status);
        let mut answer = p.clone();
        answer.payload = vec![2, 0, 0x11, 0, 0x80, 0, 9, 10, 0, 0, 123, 0, 0, 0, 3, 0x84];
        let sum = answer.payload[3..].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        answer.payload.extend([(!sum).wrapping_add(0x33), 3]);
        answer.header[5..8].copy_from_slice(&[0, 0, 16]);
        let m = packet(&answer);
        assert_eq!((m.kind, m.source), (MessageType::Acknowledge, 16));
        assert!(m.meta.contains("renewal 900 s") && m.meta.contains("10.0.0.123"));
    }
}

#[cfg(test)]
mod data_control_tests {
    use super::*;
    use crate::dmr::{Csbk, Lc, LcFrom, SlotEvent};
    fn lc(b: [u8; 9]) -> SlotEvent {
        SlotEvent::Lc { lc: Lc(b), from: LcFrom::Terminator }
    }
    #[test]
    fn data_start_and_end_use_different_source_offsets() {
        for (slot, activity) in [(0, 0x7fffu16), (1, 0xffffu16)] {
            let [a, b] = activity.to_be_bytes();
            let start = [0x0d | slot << 7, 0x68, 0, a, b, 0x80, 0, 0x01, 0x23];
            assert_eq!(data_control(&lc(start)), Some(DataControl::Start { slot, source: 0x123, activity, raw: start }));
            let end = [0x30, 0x68, 0xf0, a, b, 0, 0x01, 0x23, 0x60];
            assert_eq!(data_control(&lc(end)), Some(DataControl::End { source: 0x123, activity, raw: end }));
            assert_ne!(Lc(end).source(), 0x123, "generic voice source offset is wrong for data end");
            assert_eq!(data_control(&lc(start)).unwrap().busy_hint(), activity == 0xffff);
            assert_eq!(data_control(&lc(end)).unwrap().busy_hint(), activity == 0xffff);
        }
    }
    #[test]
    fn shared_activity_word_is_not_a_frequency_mapping_or_voice_grant() {
        let b = [0x8d, 0x68, 0x7f, 0xff, 0, 0, 0, 0, 0, 0];
        let e = SlotEvent::Csbk { csbk: Csbk(b), mbc: false };
        assert_eq!(data_control(&e), Some(DataControl::Beacon { activity: 0x7fff, raw: b }));
        assert!(crate::dmr::xpt::control(&e).is_none());
        let raw = [0xc6, 0x87, 0xff, 0xfe, 0xe0];
        assert_eq!(data_control(&SlotEvent::Slco(raw)), Some(DataControl::ShortStatus { activity: 0x7fff, raw }));
        let mut other = raw;
        other[1] ^= 0x10;
        assert!(data_control(&SlotEvent::Slco(other)).is_none(), "short LC must name Hytera");
    }
    #[test]
    fn unknown_layouts_and_embedded_words_stay_raw() {
        let b = [0x0d, 0x68, 0, 0x7f, 0xff, 0x80, 0, 1, 0x23];
        for (at, v) in [(0, 0x4d), (1, 0), (2, 1), (3, 0), (5, 0), (6, 1)] {
            let mut changed = b;
            changed[at] = v;
            assert!(data_control(&lc(changed)).is_none(), "byte {at}");
        }
        for from in [LcFrom::Embedded, LcFrom::Header] {
            assert!(data_control(&SlotEvent::Lc { lc: Lc(b), from }).is_none());
        }
        let e = SlotEvent::Csbk { csbk: Csbk([0x48, 0x68, 0x40, 0x40, 0x10, 0, 0x29, 0, 0x31, 0xa9]), mbc: false };
        assert!(data_control(&e).is_none(), "opcode 8 is still unidentified");
        assert!(data_control(&lc([0x30, 0x68, 0, 0, 0, 41, 0, 1, 0x23])).is_none(), "voice terminator is not this data-end profile");
    }
}

#[cfg(test)]
mod unknown_rrs_test {
    #[test]
    fn unknown_rrs_opcode_stays_raw_even_with_a_valid_checksum() {
        let (header, mut payload) = crate::dmr::data::tests::rrs();
        payload.truncate(payload.len() - 4 - (header[9] / 8) as usize);
        payload[4] = 0x7f;
        let sum = payload[3..11].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        payload[11] = (!sum).wrapping_add(0x33);
        let p = crate::dmr::data::Packet { header, payload };
        let m = super::packet(&p);
        assert!(m.meta.starts_with("DMR short data"));
        assert!(m.meta.contains(&super::hex(&p.payload)));
        assert!(m.position.is_none());
    }
}
