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
//!   DWORD 2…  BLOCK_HOPS hops × N bins, hop by hop, bins in table order,
//!             each sample I (bits 0–15) and Q (16–31), Q15
//!   then      power slice: first bin (bits 0–15), count P (16–31), then P values
//! ```
//!
//! The power slice is how the host sees the whole band (issue 08). Each bin sums
//! I² + Q² of its output samples, saturating at 40 bits; each block's packet sends
//! the next [`power_words`] bins' sums, shifted right by [`POWER_SHIFT`], and clears
//! them. So a bin's value covers the frames since the scan last passed it, at most
//! 16 blocks at 64 bins a block. It's in the channel packet because a packet
//! streams out across its block, leaving no room between packets for another.
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

pub const VERSION: u8 = 2;
/// The packet core ID the bank answers to.
pub const CORE_ID: u8 = 0x25;
pub const HEADER_BYTES: usize = 16;
/// Bins one table can hold. A packet leaves the radio as one USB DMA
/// transaction of 2048 DWORDs (`fx3_gpif`), 4 of them bladeRF's header:
/// 2 + 48 × 42 = 2018 fits, 43 bins wouldn't.
pub const MAX_BINS: usize = 42;
/// Dropped from a bin's 40-bit power sum to send it in 32 bits.
pub const POWER_SHIFT: u32 = 8;
const POWER_SUM_MAX: u64 = (1 << 40) - 1;

/// Power values a packet of `bins` carries. 64 (the whole band in 16 blocks), or
/// 24 at [`MAX_BINS`], where the USB buffer leaves room for no more.
pub const fn power_words(bins: usize) -> usize {
    if bins < MAX_BINS { 64 } else { 24 }
}
/// The most payload one packet can carry over USB 3 (`GPIF_BUF_SIZE_SS` less the header).
pub const USB3_MAX_PAYLOAD_DWORDS: usize = 2048 - 4;
pub const MAX_PAYLOAD_DWORDS: usize = 3 + BLOCK_HOPS * MAX_BINS + power_words(MAX_BINS);

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// `iq[hop * bins + i]` is table entry `i` at that hop.
    /// `power[k]` is bin `power_start + k` (mod M), its sum shifted right by [`POWER_SHIFT`].
    Channels { bins: usize, iq: Vec<[i16; 2]>, power_start: u16, power: Vec<u32> },
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
        let Body::Channels { bins, power, .. } = &self.body;
        3 + BLOCK_HOPS * bins + power.len()
    }

    /// Header and payload, as one libbladeRF buffer holds them.
    pub fn encode(&self) -> Vec<u8> {
        let n = self.payload_dwords();
        let mut out = Vec::with_capacity(HEADER_BYTES + 4 * n);
        out.extend((n as u16).to_le_bytes());
        out.extend([0, CORE_ID]);
        out.extend(self.timestamp.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        let Body::Channels { bins, iq, power_start, power } = &self.body;
        out.extend([VERSION, 0, self.generation, *bins as u8]);
        out.extend(self.block.to_le_bytes());
        for s in iq {
            out.extend(s[0].to_le_bytes());
            out.extend(s[1].to_le_bytes());
        }
        out.extend((*power_start as u32 | (power.len() as u32) << 16).to_le_bytes());
        power.iter().for_each(|v| out.extend(v.to_le_bytes()));
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
    if kind != 0 {
        return Err(WireError::Kind(kind));
    }
    if bins > MAX_BINS {
        return Err(WireError::TooManyBins(bins));
    }
    let word = |i: usize| u32::from_le_bytes(p[4 * i..4 * i + 4].try_into().unwrap());
    let trailer = 2 + BLOCK_HOPS * bins;
    if p.len() / 4 <= trailer {
        return Err(WireError::Truncated);
    }
    let (power_start, count) = ((word(trailer) & 0xffff) as u16, (word(trailer) >> 16) as usize);
    let expected = trailer + 1 + count;
    if p.len() / 4 != expected {
        return Err(WireError::Length { header: p.len() / 4, expected });
    }
    let iq = (2..trailer).map(|i| { let w = word(i); [w as u16 as i16, (w >> 16) as u16 as i16] }).collect();
    let power = (trailer + 1..expected).map(word).collect();
    let body = Body::Channels { bins, iq, power_start, power };
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
    /// Each bin's power since the scan last read it, and the scan's next bin.
    sum: Vec<u64>,
    scan: usize,
}

impl Packer {
    pub fn new(table: Vec<u16>, generation: u8) -> Self {
        assert!(table.len() <= MAX_BINS && table.iter().all(|&b| (b as usize) < M));
        Packer { table, generation, block: 0, hops: 0, iq: Vec::new(), sum: vec![0; M], scan: 0 }
    }

    /// One frame of all M bins; a packet when it completes a block.
    pub fn push(&mut self, frame: &[[i16; 2]]) -> Option<Packet> {
        self.iq.extend(self.table.iter().map(|&b| frame[b as usize]));
        for (s, x) in self.sum.iter_mut().zip(frame) {
            let (i, q) = (x[0] as i64, x[1] as i64);
            *s = (*s + (i * i + q * q) as u64).min(POWER_SUM_MAX);
        }
        self.hops += 1;
        if self.hops < BLOCK_HOPS {
            return None;
        }
        let power_start = self.scan as u16;
        let power = (0..power_words(self.table.len()))
            .map(|k| {
                let b = (self.scan + k) % M;
                let v = (self.sum[b] >> POWER_SHIFT) as u32;
                self.sum[b] = 0;
                v
            })
            .collect();
        self.scan = (self.scan + power_words(self.table.len())) % M;
        let p = Packet {
            timestamp: self.block as u64 * (BLOCK_HOPS * HOP) as u64,
            generation: self.generation,
            block: self.block,
            body: Body::Channels { bins: self.table.len(), iq: std::mem::take(&mut self.iq), power_start, power },
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
        let power = (0..power_words(bins) as u32).map(|v| v * 99_991).collect();
        Packet { timestamp: 0x0123_4567_89ab, generation: 7, block: 0xdead_beef, body: Body::Channels { bins, iq, power_start: 960, power } }
    }

    #[test]
    fn packets_round_trip() {
        let mut stream = Vec::new();
        let all: Vec<Packet> = (0..=MAX_BINS).map(channels).collect();
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
        v[HEADER_BYTES] = 1;
        assert_eq!(parse(&v).unwrap_err(), WireError::Version(1));
        let mut v = good.clone();
        v[HEADER_BYTES + 1] = 9;
        assert_eq!(parse(&v).unwrap_err(), WireError::Kind(9));
        // A trailer that claims one more value than there is.
        let mut v = good.clone();
        let count = HEADER_BYTES + 4 * (2 + 48 * 3) + 2;
        v[count] += 1;
        assert_eq!(parse(&v).unwrap_err(), WireError::Length { header: 3 + 48 * 3 + 64, expected: 3 + 48 * 3 + 65 });
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
        for n in (0..=MAX_BINS).map(|n| channels(n).payload_dwords()) {
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
        let Body::Channels { bins, iq, .. } = &out[1].body;
        assert_eq!(*bins, 3);
        assert_eq!(iq[3 * 5 + 1], [1023, BLOCK_HOPS as i16 + 5]);
    }

    #[test]
    fn the_power_scan_covers_every_bin_and_clears_as_it_goes() {
        let mut packer = Packer::new(vec![1], 1);
        let frame = |v: i16| -> Vec<[i16; 2]> { (0..M).map(|_| [v, 0]).collect() };
        let block = |p: &mut Packer, v: i16| {
            let mut got = None;
            for _ in 0..BLOCK_HOPS {
                got = p.push(&frame(v)).or(got);
            }
            let Body::Channels { power_start, power, .. } = got.unwrap().body;
            (power_start, power)
        };
        // Bins read in block 0 saw 48 frames; those read in block 1 saw 96.
        let (s0, p0) = block(&mut packer, 100);
        let (s1, p1) = block(&mut packer, 100);
        assert_eq!((s0, s1, p0.len()), (0, 64, 64));
        let unit = 100 * 100 * BLOCK_HOPS as u64 >> POWER_SHIFT;
        assert_eq!((p0[0] as u64, p1[0] as u64), (unit, 2 * unit));
        // Sixteen blocks reach every bin, and the next round starts again at 0.
        let mut last = 0;
        for _ in 2..17 {
            last = block(&mut packer, 0).0;
        }
        assert_eq!(last, 0);
    }

    #[test]
    fn power_sums_saturate_rather_than_wrap() {
        let mut packer = Packer::new(vec![], 1);
        packer.sum[0] = POWER_SUM_MAX - 10;
        let frame = vec![[i16::MIN, i16::MIN]; M];
        for _ in 0..BLOCK_HOPS - 1 {
            assert!(packer.push(&frame).is_none());
        }
        let Body::Channels { power, .. } = packer.push(&frame).unwrap().body;
        assert_eq!(power[0] as u64, POWER_SUM_MAX >> POWER_SHIFT);
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

