//! The FPGA filter bank's packets (`issues/02-wire-format.md`): the one
//! contract between the RTL and the host. No I/O here, so the browser build
//! compiles it too.
//!
//! A packet is what libbladeRF's `BLADERF_FORMAT_PACKET_META` puts in one
//! buffer. All fields are little-endian.
//!
//! ```text
//! byte 0x00  u16  payload length in DWORDs (the header's 4 not counted)
//!      0x02  u8   packet flags (0)
//!      0x03  u8   core ID (CORE_ID)
//!      0x04  u64  timestamp: the input sample count at the block's first sample
//!                 (the model's). On the radio, bladeRF's `fifo_writer` fills the
//!                 header and stamps the packet's first word instead, a pipeline
//!                 delay later: time a block by its counter (`block × 12288`).
//!      0x0c  u32  BLADERF_META_FLAG_* (0)
//! payload
//!   DWORD 0   version (bits 0–7), kind (8–15), table generation (16–23), bin count N (24–31)
//!   DWORD 1   block counter
//!   DWORD 2…  kind 0, channels: BLOCK_HOPS hops × N bins, hop by hop, bins in table order,
//!             each sample I (bits 0–15) and Q (16–31), Q15
//!             kind 1, power: M unsigned power values, bin 0 first (issue 08)
//! ```
//!
//! Block b of channels carries the hops made from input samples
//! `b·12288 .. (b+1)·12288`, the same block as the [`super::Channelizer`]'s.
//! A jump in the block counter or timestamp means lost blocks. N = 0 is a valid
//! packet, so the host's clock keeps moving with no bins on. A new bin table
//! takes effect at a block boundary, and the generation says which table a
//! packet used.
//!
//! libbladeRF copies a packet's whole length into the caller's buffer, however
//! big it says the buffer is: the host must pass at least [`MAX_PAYLOAD_DWORDS`].
//! Payload lengths of `n·256 − 4` DWORDs jam a sync buffer, and a packet must
//! fit one USB 3 DMA buffer ([`MAX_BINS`]); no packet here breaks either
//! (tested below). USB 2's buffers are half that: 21 bins at most there.

use super::filter_bank::{BLOCK_HOPS, HOP, M};

pub const VERSION: u8 = 1;
/// The packet core ID the bank answers to.
pub const CORE_ID: u8 = 0x25;
pub const HEADER_BYTES: usize = 16;
/// Bins one table can hold. A packet leaves the radio as one USB DMA
/// transaction of 2048 DWORDs (`fx3_gpif`), 4 of them bladeRF's header:
/// 2 + 48 × 42 = 2018 fits, 43 bins wouldn't.
pub const MAX_BINS: usize = 42;
/// The most payload one packet can carry over USB 3 (`GPIF_BUF_SIZE_SS` less the header).
pub const USB3_MAX_PAYLOAD_DWORDS: usize = 2048 - 4;
pub const MAX_PAYLOAD_DWORDS: usize = 2 + BLOCK_HOPS * MAX_BINS;
const POWER_DWORDS: usize = 2 + M;

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// `iq[hop * bins + i]` is table entry `i` at that hop.
    Channels { bins: usize, iq: Vec<[i16; 2]> },
    Power(Vec<u32>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub timestamp: u64,
    pub generation: u8,
    pub block: u32,
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WireError {
    Truncated,
    Version(u8),
    Kind(u8),
    Core(u8),
    TooManyBins(usize),
    /// The header's length doesn't match what the payload says it holds.
    Length { header: usize, expected: usize },
}

impl Packet {
    pub fn payload_dwords(&self) -> usize {
        match &self.body {
            Body::Channels { bins, .. } => 2 + BLOCK_HOPS * bins,
            Body::Power(_) => POWER_DWORDS,
        }
    }

    /// Header and payload, as one libbladeRF buffer holds them.
    pub fn encode(&self) -> Vec<u8> {
        let n = self.payload_dwords();
        let mut out = Vec::with_capacity(HEADER_BYTES + 4 * n);
        out.extend((n as u16).to_le_bytes());
        out.extend([0, CORE_ID]);
        out.extend(self.timestamp.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        let (kind, bins) = match &self.body {
            Body::Channels { bins, .. } => (0, *bins as u8),
            Body::Power(_) => (1, 0),
        };
        out.extend([VERSION, kind, self.generation, bins]);
        out.extend(self.block.to_le_bytes());
        match &self.body {
            Body::Channels { iq, .. } => iq.iter().for_each(|s| {
                out.extend(s[0].to_le_bytes());
                out.extend(s[1].to_le_bytes());
            }),
            Body::Power(p) => p.iter().for_each(|v| out.extend(v.to_le_bytes())),
        }
        out
    }
}

/// One packet from the front of `bytes`, and what follows it.
pub fn parse(bytes: &[u8]) -> Result<(Packet, &[u8]), WireError> {
    let header = bytes.get(..HEADER_BYTES).ok_or(WireError::Truncated)?;
    let len = u16::from_le_bytes([header[0], header[1]]) as usize;
    if header[3] != CORE_ID {
        return Err(WireError::Core(header[3]));
    }
    let timestamp = u64::from_le_bytes(header[4..12].try_into().unwrap());
    let end = HEADER_BYTES + 4 * len;
    let payload = bytes.get(HEADER_BYTES..end).ok_or(WireError::Truncated)?;
    Ok((parse_payload(timestamp, payload)?, &bytes[end..]))
}

/// A payload as `bladerf_sync_rx` hands it over: its timestamp comes in the metadata.
pub fn parse_payload(timestamp: u64, p: &[u8]) -> Result<Packet, WireError> {
    if p.len() < 8 || p.len() % 4 != 0 {
        return Err(WireError::Truncated);
    }
    let (version, kind, generation, bins) = (p[0], p[1], p[2], p[3] as usize);
    if version != VERSION {
        return Err(WireError::Version(version));
    }
    let block = u32::from_le_bytes(p[4..8].try_into().unwrap());
    let words = p[8..].chunks_exact(4);
    let expected = match kind {
        0 if bins > MAX_BINS => return Err(WireError::TooManyBins(bins)),
        0 => 2 + BLOCK_HOPS * bins,
        1 => POWER_DWORDS,
        k => return Err(WireError::Kind(k)),
    };
    if p.len() / 4 != expected {
        return Err(WireError::Length { header: p.len() / 4, expected });
    }
    let body = match kind {
        0 => Body::Channels { bins, iq: words.map(|w| [i16::from_le_bytes([w[0], w[1]]), i16::from_le_bytes([w[2], w[3]])]).collect() },
        _ => Body::Power(words.map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect()),
    };
    Ok(Packet { timestamp, generation, block, body })
}

/// What the FPGA does after the bank: picks the table's bins from every hop
/// and sends a packet each [`BLOCK_HOPS`] hops. Feed it the model's frames.
pub struct Packer {
    pub table: Vec<u16>,
    pub generation: u8,
    block: u32,
    hops: usize,
    iq: Vec<[i16; 2]>,
}

impl Packer {
    pub fn new(table: Vec<u16>, generation: u8) -> Self {
        assert!(table.len() <= MAX_BINS && table.iter().all(|&b| (b as usize) < M));
        Packer { table, generation, block: 0, hops: 0, iq: Vec::new() }
    }

    /// One frame of all M bins; a packet when it completes a block.
    pub fn push(&mut self, frame: &[[i16; 2]]) -> Option<Packet> {
        self.iq.extend(self.table.iter().map(|&b| frame[b as usize]));
        self.hops += 1;
        if self.hops < BLOCK_HOPS {
            return None;
        }
        let p = Packet {
            timestamp: self.block as u64 * (BLOCK_HOPS * HOP) as u64,
            generation: self.generation,
            block: self.block,
            body: Body::Channels { bins: self.table.len(), iq: std::mem::take(&mut self.iq) },
        };
        self.hops = 0;
        self.block = self.block.wrapping_add(1);
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(bins: usize) -> Packet {
        let iq = (0..BLOCK_HOPS * bins).map(|i| [i as i16, -(i as i16) - 1]).collect();
        Packet { timestamp: 0x0123_4567_89ab, generation: 7, block: 0xdead_beef, body: Body::Channels { bins, iq } }
    }

    #[test]
    fn packets_round_trip() {
        let power = Packet { timestamp: 5, generation: 1, block: 16, body: Body::Power((0..M as u32).map(|v| v * 99_991).collect()) };
        let mut stream = Vec::new();
        let all: Vec<Packet> = (0..=MAX_BINS).map(channels).chain([power]).collect();
        all.iter().for_each(|p| stream.extend(p.encode()));
        let mut rest = &stream[..];
        for want in &all {
            let (got, more) = parse(rest).unwrap();
            assert_eq!(&got, want);
            rest = more;
        }
        assert!(rest.is_empty());
    }

    #[test]
    fn bad_packets_are_errors_not_panics() {
        let good = channels(3).encode();
        for cut in 0..good.len() {
            assert!(parse(&good[..cut]).is_err(), "cut at {cut}");
        }
        let mut v = good.clone();
        v[HEADER_BYTES] = 2;
        assert_eq!(parse(&v).unwrap_err(), WireError::Version(2));
        let mut v = good.clone();
        v[HEADER_BYTES + 1] = 9;
        assert_eq!(parse(&v).unwrap_err(), WireError::Kind(9));
        let mut v = good.clone();
        v[HEADER_BYTES + 3] = 4;
        assert_eq!(parse(&v).unwrap_err(), WireError::Length { header: 2 + 48 * 3, expected: 2 + 48 * 4 });
        let mut v = good.clone();
        v[HEADER_BYTES + 3] = MAX_BINS as u8 + 1;
        assert_eq!(parse(&v).unwrap_err(), WireError::TooManyBins(MAX_BINS + 1));
        let mut v = good.clone();
        v[3] = 0;
        assert_eq!(parse(&v).unwrap_err(), WireError::Core(0));
        // Every byte value at every payload position: an error or a packet, never a panic.
        for i in HEADER_BYTES..HEADER_BYTES + 8 {
            for b in 0..=255u8 {
                let mut v = good.clone();
                v[i] = b;
                let _ = parse(&v);
            }
        }
    }

    #[test]
    fn no_packet_has_a_length_that_jams_libbladerf() {
        let lens = (0..=MAX_BINS).map(|n| channels(n).payload_dwords()).chain([POWER_DWORDS]);
        for n in lens {
            assert!(n <= USB3_MAX_PAYLOAD_DWORDS, "payload of {n} DWORDs is more than one USB 3 DMA buffer");
            assert_ne!((n + 4) % 256, 0, "payload of {n} DWORDs");
        }
    }

    #[test]
    fn the_packer_cuts_blocks_on_the_channelizers_boundaries() {
        let mut packer = Packer::new(vec![0, 1023, 77], 3);
        let mut out = Vec::new();
        for hop in 0..BLOCK_HOPS * 2 {
            let frame: Vec<[i16; 2]> = (0..M).map(|b| [b as i16, hop as i16]).collect();
            out.extend(packer.push(&frame));
        }
        assert_eq!(out.len(), 2);
        assert_eq!((out[1].block, out[1].timestamp), (1, 12288));
        let Body::Channels { bins, iq } = &out[1].body else { panic!() };
        assert_eq!(*bins, 3);
        assert_eq!(iq[3 * 5 + 1], [1023, BLOCK_HOPS as i16 + 5]);
    }

    /// The RTL testbench's vectors (`fpga/sim/vectors/`, README there): the
    /// default design's ROM, an input that starts the bank from zero, and the
    /// packets the model makes of it. Fails if the model drifts from them;
    /// `BANK_GOLDEN_WRITE=1` rewrites them.
    #[test]
    fn golden_vectors_match_the_model() {
        use super::super::filter_bank::{Design, FixedBank, BIN_HZ, FS};
        let d = Design::default();
        // Two blocks: tones on and between table bins, one near full scale, a
        // pseudo-random floor, and the extreme codes.
        let mut x = 0x2545_f491u32;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x % 401) as f64 - 200.0
        };
        let tones = [(3.0 * BIN_HZ, 300.0), (40.5 * BIN_HZ, 1500.0), (-100.25 * BIN_HZ, 120.0), (-1.0 * BIN_HZ, 40.0)];
        let input: Vec<[i16; 2]> = (0..2 * BLOCK_HOPS * HOP)
            .map(|n| {
                if n == 5000 {
                    return [2047, -2048];
                }
                let (mut i, mut q) = (rnd(), rnd());
                for (f, a) in tones {
                    let ph = 2.0 * std::f64::consts::PI * f * n as f64 / FS;
                    i += a * ph.cos();
                    q += a * ph.sin();
                }
                [i.round().clamp(-2048.0, 2047.0) as i16, q.round().clamp(-2048.0, 2047.0) as i16]
            })
            .collect();
        let table = vec![3, 40, 41, 924, 1023, 0, 512];
        let mut bank = FixedBank::new(&d);
        let mut packer = Packer::new(table, 1);
        // A table committed during block 0 (generation 2) takes over at block 1.
        let (mut first, mut second) = (Packer::new(vec![3, 40, 41, 924, 1023, 0, 512], 1), Packer::new(vec![512, 7, 1023, 600], 2));
        // The output gain shift the radio runs with.
        let mut gained = FixedBank::new(&Design { gain_shift: 2, ..d.clone() });
        let mut gained_packer = Packer::new(vec![3, 40, 41, 924, 1023, 0, 512], 1);
        let mut gained_packets = Vec::new();
        gained.push(&input, |f| gained_packets.extend(gained_packer.push(f).map(|p| p.encode())));
        let (mut packets, mut switched) = (Vec::new(), Vec::new());
        bank.push(&input, |f| {
            packets.extend(packer.push(f).map(|p| p.encode()));
            let (a, b) = (first.push(f), second.push(f));
            switched.extend(a.filter(|p| p.block == 0).or(b.filter(|p| p.block == 1)).map(|p| p.encode()));
        });
        // The tones sum to nearly full scale, so two fold values saturate: the RTL must clamp them the same way.
        assert_eq!(bank.saturated, super::super::filter_bank::Saturated { fold: 2, fft: 0, output: 0 });

        let files = [
            ("input.sc16", input.iter().flat_map(|s| [s[0].to_le_bytes(), s[1].to_le_bytes()].concat()).collect::<Vec<u8>>()),
            ("coefficients.hex", bank.coefficients().iter().map(|c| format!("{:05x}\n", c & 0x3ffff)).collect::<String>().into_bytes()),
            ("expected.pkt", packets.concat()),
            ("expected_switch.pkt", switched.concat()),
            ("expected_gain.pkt", gained_packets.concat()),
            ("../../rtl/fb_rom_pkg.vhd", rom_vhdl(&bank)),
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fpga/sim/vectors");
        for (name, bytes) in files {
            let path = dir.join(name);
            if std::env::var_os("BANK_GOLDEN_WRITE").is_some() {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&path, &bytes).unwrap();
            }
            let on_disk = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(on_disk == bytes, "{name} no longer matches the model: check the change, then BANK_GOLDEN_WRITE=1");
        }
    }

    /// The RTL's ROMs and the constants they go with, as a VHDL package.
    fn rom_vhdl(bank: &super::super::filter_bank::FixedBank) -> Vec<u8> {
        let list = |v: &mut dyn Iterator<Item = i32>| v.map(|c| c.to_string()).collect::<Vec<_>>().chunks(16).map(|l| l.join(", ")).collect::<Vec<_>>().join(",\n    ");
        format!(
            "-- Made by golden_vectors_match_the_model (crates/trunk-core/src/dsp/bank_wire.rs) from the model's\n\
             -- Design::default(). Don't edit: run BANK_GOLDEN_WRITE=1 cargo test -p trunk-core dsp::bank_wire.\n\
             package fb_rom_pkg is\n\
             \x20   constant FOLD_SHIFT : natural := {};\n\
             \x20   type int_array is array (natural range <>) of integer;\n\
             \x20   constant COEF : int_array(0 to {}) := (\n    {});\n\
             \x20   constant TW_RE : int_array(0 to {}) := (\n    {});\n\
             \x20   constant TW_IM : int_array(0 to {}) := (\n    {});\n\
             end package;\n",
            bank.fold_shift(),
            bank.coefficients().len() - 1,
            list(&mut bank.coefficients().iter().copied()),
            bank.twiddles().len() - 1,
            list(&mut bank.twiddles().iter().map(|t| t.0)),
            bank.twiddles().len() - 1,
            list(&mut bank.twiddles().iter().map(|t| t.1)),
        )
        .into_bytes()
    }
}

