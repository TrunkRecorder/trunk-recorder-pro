//! Bit helpers the protocols share: MSB-first bit fields and CRC-CCITT.

/// Bits `a..b` of `buf`, MSB first (bit 0 is the top bit of byte 0).
pub fn field(buf: &[u8], a: usize, b: usize) -> u32 {
    (a..b).fold(0, |v, i| v << 1 | (buf[i / 8] >> (7 - i % 8) & 1) as u32)
}

/// CRC-CCITT (x¹⁶+x¹²+x⁵+1, init 0, inverted: CRC-16/GSM) of `bits`, one bit per byte.
pub fn crc_ccitt(bits: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in bits {
        crc = if (crc >> 15) as u8 ^ (b & 1) != 0 { crc << 1 ^ 0x1021 } else { crc << 1 };
    }
    !crc
}

/// [`crc_ccitt`] of `data`'s bits, MSB first.
pub fn crc_ccitt_bytes(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { crc << 1 ^ 0x1021 } else { crc << 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unpack(bytes: &[u8]) -> Vec<u8> {
        bytes.iter().flat_map(|&b| (0..8).rev().map(move |i| b >> i & 1)).collect()
    }

    #[test]
    fn crc_ccitt_matches_the_reference() {
        // CRC-16/GSM: "123456789" → 0xCE3C.
        assert_eq!(crc_ccitt_bytes(b"123456789"), 0xce3c);
        assert_eq!(crc_ccitt(&unpack(b"123456789")), 0xce3c);
        let data: Vec<u8> = (0..40u32).map(|i| (i * 37 + 11) as u8).collect();
        for n in 0..data.len() {
            assert_eq!(crc_ccitt(&unpack(&data[..n])), crc_ccitt_bytes(&data[..n]));
        }
    }

    #[test]
    fn fields() {
        let b = [0b1010_0000, 0xff, 0x01];
        assert_eq!(field(&b, 0, 3), 0b101);
        assert_eq!(field(&b, 4, 12), 0x0f);
        assert_eq!(field(&b, 16, 24), 1);
        assert_eq!(field(&b, 5, 5), 0);
    }
}
