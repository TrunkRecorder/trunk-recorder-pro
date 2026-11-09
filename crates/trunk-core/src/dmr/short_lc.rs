//! Carrier-wide CACH short LC: four 17-bit fragments, BPTC(68,36), CRC8.
//! Kept outside XPT because this transport is shared by DMR systems.
use super::fec::{pack, short_lc_row};

#[derive(Default)]
pub struct Assembler {
    bits: Vec<u8>,
}

impl Assembler {
    /// LCSS sequence must be first, continuation, continuation, last.
    /// A lost/invalid fragment resets assembly; it cannot reuse an old bank.
    pub fn push(&mut self, lcss: Option<u8>, fragment: [u8; 17]) -> Option<[u8; 5]> {
        match (lcss, self.bits.len()) {
            (Some(1), _) => {
                self.bits.clear();
                self.bits.extend(fragment);
                return None;
            }
            (Some(3), 17 | 34) => {
                self.bits.extend(fragment);
                return None;
            }
            (Some(2), 51) => self.bits.extend(fragment),
            _ => {
                self.bits.clear();
                return None;
            }
        }
        let raw = std::mem::take(&mut self.bits);
        let mut matrix: [u8; 68] = std::array::from_fn(|i| raw[if i == 67 { 67 } else { i * 4 % 67 }]);
        let mut payload = Vec::with_capacity(36);
        for row in matrix[..51].chunks_mut(17) {
            short_lc_row(row)?;
            payload.extend_from_slice(&row[..12]);
        }
        // Fourth row is column parity. Reject residual damage after row correction.
        if (0..17).any(|i| matrix[i] ^ matrix[17 + i] ^ matrix[34 + i] ^ matrix[51 + i] != 0) {
            return None;
        }
        if crc8(&payload) != 0 {
            return None;
        }
        payload.extend([0; 4]);
        Some(pack(&payload).try_into().unwrap())
    }
}

/// MSB-first CRC8, polynomial x^8+x^2+x+1, initial/final zero.
fn crc8(bits: &[u8]) -> u8 {
    bits.iter().fold(0u8, |crc, &bit| {
        let feedback = (crc >> 7) ^ bit;
        (crc << 1) ^ if feedback != 0 { 0x07 } else { 0 }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode() -> [u8; 68] {
        // Independently specified SLCO8: free=2, priority=3, hash=0x45.
        let mut data: Vec<u8> = [0x80, 0x02, 0x34, 0x50].iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1)).take(28).collect();
        let crc = crc8(&data);
        data.extend((0..8).rev().map(|i| (crc >> i) & 1));
        let parity_rows = [0b11011u8, 0b11111, 0b11101, 0b11100, 0b01110, 0b00111, 0b10001, 0b11010, 0b01101, 0b10100, 0b01010, 0b00101];
        let mut m = [0; 68];
        for r in 0..3 {
            m[r * 17..r * 17 + 12].copy_from_slice(&data[r * 12..r * 12 + 12]);
            let p = data[r * 12..r * 12 + 12].iter().zip(parity_rows).fold(0, |v, (&b, row)| if b != 0 { v ^ row } else { v });
            for i in 0..5 {
                m[r * 17 + 12 + i] = (p >> (4 - i)) & 1;
            }
        }
        for i in 0..17 {
            m[51 + i] = m[i] ^ m[17 + i] ^ m[34 + i];
        }
        let mut raw = [0; 68];
        for i in 0..68 {
            raw[if i == 67 { 67 } else { i * 4 % 67 }] = m[i];
        }
        raw
    }

    #[test]
    fn assembly_fec_crc_and_loss() {
        let raw = encode();
        let decode = |raw: [u8; 68]| {
            let mut a = Assembler::default();
            let mut result = None;
            for (i, lcss) in [1, 3, 3, 2].into_iter().enumerate() {
                result = a.push(Some(lcss), raw[i * 17..i * 17 + 17].try_into().unwrap());
            }
            result
        };
        assert_eq!(decode(raw).map(|b| (b[0], b[1], b[2], b[3] >> 4)), Some((0x80, 2, 0x34, 5)));
        let mut damaged = raw;
        damaged[0] ^= 1;
        assert_eq!(decode(damaged), decode(raw));
        damaged[1] ^= 1;
        damaged[2] ^= 1;
        damaged[3] ^= 1;
        assert!(decode(damaged).is_none());
        let mut a = Assembler::default();
        a.push(Some(1), raw[..17].try_into().unwrap());
        a.push(None, [0; 17]);
        assert!(a.push(Some(2), raw[51..].try_into().unwrap()).is_none());
    }
    #[test]
    fn cach_fragments_reach_the_common_channel_event_stream() {
        use crate::dmr::fec::hamming7_encode;
        use crate::dmr::{Burst, Channel, SlotEvent, SyncKind};
        const ORDER: [usize; 24] = [0, 4, 8, 12, 14, 18, 22, 1, 2, 3, 5, 6, 7, 9, 10, 11, 13, 15, 16, 17, 19, 20, 21, 23];
        let raw = encode();
        let mut channel = Channel::default();
        let mut out = Vec::new();
        for (i, lcss) in [1, 3, 3, 2].into_iter().enumerate() {
            let mut cach = [0; 12];
            let tact = hamming7_encode(((i as u32 & 1) << 2) | lcss);
            let bits: Vec<u8> = (0..7).rev().map(|b| (tact >> b & 1) as u8).chain(raw[i * 17..i * 17 + 17].iter().copied()).collect();
            for (k, bit) in bits.into_iter().enumerate() {
                let n = ORDER[k];
                cach[n / 2] |= bit << (1 - n % 2);
            }
            let burst = Burst {
                cach,
                dibits: crate::dmr::synth::csbk_burst(1, &[0; 10]),
                rel: [1.0; 264],
                sync: Some(SyncKind::BsData),
                sync_errs: 0,
                sample: i as f64 * 720.0,
            };
            channel.burst(&burst, &mut out);
        }
        assert!(channel.xpt);
        assert!(out.iter().any(|(_, e)| matches!(e, SlotEvent::Slco(b) if b[0] == 0x80 && b[1] == 2)));
    }
}
