//! Defined short-data packets, ETSI TS 102 361-1 §8.2.1.
//! Only unconfirmed, single-fragment, rate-1/2 packets are assembled.
//! Headers always require their real CRC, even on keyed-CRC voice systems.

use super::slot::{DT_DATA_HEADER, SlotEvent};

#[derive(Clone, Debug)]
pub struct Packet {
    pub header: [u8; 12],
    /// Application bytes, without packet CRC or bit padding.
    pub payload: Vec<u8>,
}

impl Packet {
    pub fn source(&self) -> u32 {
        crate::bits::field(&self.header, 40, 64)
    }
    pub fn destination(&self) -> u32 {
        crate::bits::field(&self.header, 16, 40)
    }
}

#[derive(Default)]
pub struct Assembler {
    pending: Option<Pending>,
}
struct Pending {
    header: [u8; 12],
    blocks: usize,
    bytes: Vec<u8>,
    start: f64,
}

/// A bounded packet cannot survive a retune, a voice transmission, a bad
/// block, another header, or a gap longer than its possible airtime.
impl Assembler {
    pub fn push(&mut self, e: &SlotEvent, t: f64) -> Option<Packet> {
        if self.pending.as_ref().is_some_and(|p| t < p.start || t - p.start > 4.0) {
            self.pending = None;
        }
        match e {
            SlotEvent::Block { data_type: DT_DATA_HEADER, bytes } => {
                self.pending = None;
                let b = bytes;
                let blocks = (((b[0] & 0x30) >> 4) << 4 | b[1] & 15) as usize;
                // SAP 10: short data; DPF 13: defined; no response/SARQ;
                // full message, 1..63 appended blocks, octet-aligned padding.
                if b[0] & 15 != 13
                    || b[0] & 0x40 != 0
                    || b[1] >> 4 != 10
                    || b[8] & 3 != 1
                    || blocks == 0
                    || b[9] & 7 != 0
                    || !super::fec::crc16_ok(&super::fec::unpack(b).try_into().unwrap(), super::fec::MASK_DATA_HEADER)
                {
                    return None;
                }
                self.pending = Some(Pending { header: *b, blocks, bytes: Vec::with_capacity(blocks * 12), start: t });
            }
            SlotEvent::Block { data_type: 7, bytes } => {
                let p = self.pending.as_mut()?;
                p.bytes.extend_from_slice(bytes);
                if p.bytes.len() == p.blocks * 12 {
                    let mut p = self.pending.take().unwrap();
                    let n = p.bytes.len() - 4;
                    let expected = u32::from_le_bytes(p.bytes[n..].try_into().unwrap());
                    if crc32(&p.bytes[..n]) != expected {
                        return None;
                    }
                    let pad = (p.header[9] / 8) as usize;
                    if pad > n {
                        return None;
                    }
                    p.bytes.truncate(n - pad);
                    return Some(Packet { header: p.header, payload: p.bytes });
                }
            }
            // Control blocks/short LC may be interleaved with data. A voice
            // burst/header/terminator or unsupported data rate ends assembly.
            SlotEvent::Voice { .. } | SlotEvent::Lc { .. } | SlotEvent::Privacy { .. } | SlotEvent::Data { .. } | SlotEvent::Block { .. } => {
                self.pending = None
            }
            _ => {}
        }
        None
    }
}

/// DMR packet CRC32: adjacent octets exchanged, MSB-first polynomial,
/// initial remainder zero, no final complement. Wire check is little endian.
fn crc32(bytes: &[u8]) -> u32 {
    debug_assert_eq!(bytes.len() % 2, 0);
    let mut crc = 0u32;
    for pair in bytes.chunks_exact(2) {
        for b in [pair[1], pair[0]] {
            crc ^= (b as u32) << 24;
            for _ in 0..8 {
                crc = (crc << 1) ^ if crc & 0x8000_0000 != 0 { 0x04c1_1db7 } else { 0 };
            }
        }
    }
    crc
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub fn unhex(s: &str) -> Vec<u8> {
        s.as_bytes().chunks_exact(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()).collect()
    }
    // Anonymous golden vectors generated independently with bit-wise CRCs.
    pub fn gps() -> ([u8; 12], Vec<u8>) {
        (unhex("0da300ff0000007b012895d1").try_into().unwrap(), unhex("010001d003000000010c000001011a2039b38270945f850000000000000000000ad90466"))
    }
    pub fn rrs() -> ([u8; 12], Vec<u8>) {
        (unhex("0da200ff0000007b01386cc3").try_into().unwrap(), unhex("020011000300040a00007ba6030000000000000064d19527"))
    }
    fn header(a: &mut Assembler, h: [u8; 12], t: f64) {
        assert!(a.push(&SlotEvent::Block { data_type: 6, bytes: h }, t).is_none());
    }
    fn blocks(a: &mut Assembler, data: &[u8], t: f64) -> Vec<Packet> {
        data.chunks_exact(12)
            .enumerate()
            .filter_map(|(i, b)| a.push(&SlotEvent::Block { data_type: 7, bytes: b.try_into().unwrap() }, t + i as f64 * 0.06))
            .collect()
    }
    #[test]
    fn golden_packets_check_crcs_and_strip_padding() {
        for (h, b) in [gps(), rrs()] {
            let mut a = Assembler::default();
            header(&mut a, h, 0.0);
            let p = blocks(&mut a, &b, 0.06).pop().unwrap();
            assert_eq!((p.source(), p.destination()), (123, 65280));
            assert_eq!(p.payload.len(), b.len() - 4 - (h[9] / 8) as usize);
            assert_eq!(p.payload, b[..p.payload.len()]);
            assert!(blocks(&mut a, &b, 0.3).is_empty(), "one output per packet");
        }
    }
    #[test]
    fn damaged_missing_reordered_and_duplicate_blocks_fail_closed() {
        let (h, b) = gps();
        let variants = [
            {
                let mut d = b.clone();
                d[5] ^= 1;
                d
            },
            b[12..].to_vec(),
            [b[12..24].to_vec(), b[..12].to_vec(), b[24..].to_vec()].concat(),
            [b[..12].to_vec(), b[..12].to_vec(), b[12..].to_vec()].concat(),
        ];
        for data in variants {
            let mut a = Assembler::default();
            header(&mut a, h, 0.0);
            assert!(blocks(&mut a, &data, 0.06).is_empty());
        }
        let mut a = Assembler::default();
        let mut bad = h;
        bad[5] ^= 1;
        header(&mut a, bad, 0.0);
        assert!(blocks(&mut a, &b, 0.06).is_empty());
    }
    #[test]
    fn slot_isolation_timeout_and_interruption() {
        let (h, b) = gps();
        let (rh, rb) = rrs();
        let mut a = [Assembler::default(), Assembler::default()];
        header(&mut a[0], h, 0.0);
        header(&mut a[1], rh, 0.03);
        assert!(blocks(&mut a[0], &b[..12], 0.06).is_empty());
        assert_eq!(blocks(&mut a[1], &rb, 0.09).len(), 1);
        assert_eq!(blocks(&mut a[0], &b[12..], 0.18).len(), 1);
        for e in [
            SlotEvent::Data { data_type: 7, ok: false },
            SlotEvent::Data { data_type: 8, ok: true },
            SlotEvent::Lc { lc: super::super::slot::Lc([0; 9]), from: super::super::slot::LcFrom::Header },
        ] {
            header(&mut a[0], h, 1.0);
            a[0].push(&e, 1.06);
            assert!(blocks(&mut a[0], &b, 1.12).is_empty());
        }
        header(&mut a[0], h, 2.0);
        assert!(blocks(&mut a[0], &b, 6.1).is_empty());
        header(&mut a[0], h, 2.0);
        assert!(blocks(&mut a[0], &b, 1.0).is_empty());
        header(&mut a[0], h, 2.0);
        header(&mut a[0], rh, 2.06);
        assert_eq!(blocks(&mut a[0], &rb, 2.12).len(), 1);
    }
}
