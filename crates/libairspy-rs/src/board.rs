//! Board information reads, ported from `airspy_board_id_read`,
//! `airspy_version_string_read`, and
//! `airspy_board_partid_serialno_read` in `airspy.c`.

use crate::commands::Command;
use crate::device::Device;
use crate::error::{Error, Result};

/// `VERSION_LOCAL_SIZE` in `airspy_version_string_read` — the wire
/// read requests `VERSION_LOCAL_SIZE - 1` bytes.
const VERSION_LOCAL_SIZE: usize = 128;

/// Words in `airspy_read_partid_serialno_t` (airspy.h): `part_id[2]`
/// followed by `serial_no[4]`.
const PARTID_SERIALNO_WORDS: usize = 6;
/// Wire width of each word (`uint32_t`).
const WORD_LEN: usize = core::mem::size_of::<u32>();
/// `sizeof(airspy_read_partid_serialno_t)` — the transfer length in
/// `airspy_board_partid_serialno_read` (airspy.c).
const PARTID_SERIALNO_LEN: usize = PARTID_SERIALNO_WORDS * WORD_LEN;

/// MCU part id and serial number
/// (`airspy_read_partid_serialno_t` in `airspy.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartIdSerial {
    /// `part_id[2]`
    pub part_id: [u32; 2],
    /// `serial_no[4]`
    pub serial_no: [u32; 4],
}

impl PartIdSerial {
    /// Decode the 24-byte wire representation: `part_id[0..2]` then
    /// `serial_no[0..4]`, each a little-endian `u32` (C applies
    /// `TO_LE` to every word).
    fn from_le_bytes(raw: &[u8; PARTID_SERIALNO_LEN]) -> Self {
        // Panic-free, zero-allocation decode: peel one word-sized
        // chunk per word. The `if let` always matches (the array holds
        // exactly PARTID_SERIALNO_WORDS words) but keeps every path
        // non-panicking per the library rules.
        let mut words = [0u32; PARTID_SERIALNO_WORDS];
        let mut rest: &[u8] = raw;
        for word in &mut words {
            if let Some((chunk, tail)) = rest.split_first_chunk::<WORD_LEN>() {
                *word = u32::from_le_bytes(*chunk);
                rest = tail;
            }
        }
        Self {
            part_id: [words[0], words[1]],
            serial_no: [words[2], words[3], words[4], words[5]],
        }
    }
}

/// What a C caller sees in the version buffer: bytes up to the first
/// NUL, lossily decoded (the firmware string is ASCII in practice).
fn parse_version(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

impl Device {
    /// Read the raw board id byte (`airspy_board_id_read`). Interpret
    /// it with [`crate::commands::BoardId::from_u8`].
    pub fn board_id(&self) -> Result<u8> {
        let mut value = [0u8; 1];
        let n = self.vendor_in(Command::BoardIdRead, 0, 0, &mut value)?;
        if n < value.len() {
            // C: result < 1 → AIRSPY_ERROR_LIBUSB.
            return Err(Error::TransferLengthMismatch {
                expected: value.len(),
                actual: n,
            });
        }
        Ok(value[0])
    }

    /// Read the firmware version string (`airspy_version_string_read`).
    ///
    /// The wire read requests `VERSION_LOCAL_SIZE - 1` bytes exactly as
    /// C does; the caller-buffer truncation dance of the C signature
    /// has no Rust counterpart — the full string is returned.
    pub fn version_string(&self) -> Result<String> {
        let mut raw = [0u8; VERSION_LOCAL_SIZE - 1];
        let n = self.vendor_in(Command::VersionStringRead, 0, 0, &mut raw)?;
        // C only rejects result < 0 (a libusb error), which vendor_in
        // already surfaced; any byte count is accepted.
        Ok(parse_version(&raw[..n]))
    }

    /// Read the MCU part id and serial number
    /// (`airspy_board_partid_serialno_read`).
    pub fn partid_serialno(&self) -> Result<PartIdSerial> {
        let mut raw = [0u8; PARTID_SERIALNO_LEN];
        let n = self.vendor_in(Command::BoardPartIdSerialNoRead, 0, 0, &mut raw)?;
        if n < PARTID_SERIALNO_LEN {
            // C: result < length → AIRSPY_ERROR_LIBUSB.
            return Err(Error::TransferLengthMismatch {
                expected: PARTID_SERIALNO_LEN,
                actual: n,
            });
        }
        Ok(PartIdSerial::from_le_bytes(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_buffer_size_matches_c() {
        // VERSION_LOCAL_SIZE (128) in airspy_version_string_read; the
        // wire read requests VERSION_LOCAL_SIZE - 1 bytes.
        assert_eq!(VERSION_LOCAL_SIZE, 128);
    }

    #[test]
    fn version_string_stops_at_first_nul() {
        // C's fixed buffer is zero-initialized and the caller copy is
        // NUL-terminated; the Rust surface returns what a C caller
        // would see as the string.
        assert_eq!(
            parse_version(b"AirSpy NOS v1.0.0-rc10\0\0\0\0"),
            "AirSpy NOS v1.0.0-rc10"
        );
        assert_eq!(parse_version(b""), "");
        assert_eq!(parse_version(b"\0garbage after nul"), "");
        assert_eq!(parse_version(b"no nul at all"), "no nul at all");
    }

    #[test]
    fn version_string_is_lossy_for_non_utf8() {
        // A C caller gets raw bytes; the Rust port substitutes the
        // replacement character rather than failing the read.
        assert_eq!(parse_version(&[0x41, 0xFF, 0x42]), "A\u{FFFD}B");
    }

    #[test]
    fn partid_serialno_parses_little_endian_words() {
        // airspy_read_partid_serialno_t: uint32_t part_id[2] then
        // uint32_t serial_no[4], each converted with TO_LE in C.
        let mut raw = [0u8; PARTID_SERIALNO_LEN];
        raw[0..4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        raw[4..8].copy_from_slice(&0x9ABC_DEF0u32.to_le_bytes());
        raw[8..12].copy_from_slice(&0x0000_0001u32.to_le_bytes());
        raw[12..16].copy_from_slice(&0x0000_0002u32.to_le_bytes());
        raw[16..20].copy_from_slice(&0x0000_0003u32.to_le_bytes());
        raw[20..24].copy_from_slice(&0x0000_0004u32.to_le_bytes());
        let parsed = PartIdSerial::from_le_bytes(&raw);
        assert_eq!(parsed.part_id, [0x1234_5678, 0x9ABC_DEF0]);
        assert_eq!(parsed.serial_no, [1, 2, 3, 4]);
    }

    #[test]
    fn partid_serialno_len_matches_c_struct() {
        // sizeof(airspy_read_partid_serialno_t) = 6 * sizeof(uint32_t).
        assert_eq!(PARTID_SERIALNO_LEN, 24);
    }
}
