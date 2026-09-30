//! Outbound Signalling Words: framing, FEC and CRC (OP25's rx_smartnet).
//!
//! An OSW is 84 bits on the air: the 8-bit sync `0xAC`, then 76 bits
//! interleaved 19 × 4. De-interleaved they are 38 (data, parity) pairs of a
//! rate-½ code, parity = data ⊕ previous data; the first 37 data bits are the
//! 16-bit address, the group flag, the 10-bit command and a 10-bit CRC, all
//! stored inverted / XOR-masked.
//!
//! Decoding differs from OP25's in two ways. The code is decoded by soft
//! Viterbi (two states) rather than the hard syndrome rule, which corrects
//! more. And a frame is taken without its trailing sync when the previous
//! frame fixed where it must be (a flywheel), so one corrupted sync costs one
//! OSW, not two.

pub const SYNC: u8 = 0xAC;
pub const FRAME_BITS: usize = 84;
const PAYLOAD_BITS: usize = 76;
const DATA_BITS: usize = 27;
const CRC_BITS: usize = 10;
const ADDR_XOR: u16 = !0x33C7;
const CMD_XOR: u16 = !0x32A & 0x3ff;
/// Hard decisions the Viterbi path may overrule in one frame.
const MAX_CORRECTED: u32 = 8;
/// Missed frames before the flywheel gives up.
const MAX_MISSES: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Osw {
    pub addr: u16,
    pub grp: bool,
    pub cmd: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FramerOut {
    /// A good OSW; `bit` is the index (in bits pushed) of its last bit.
    Osw(Osw, u64),
    /// An OSW was due (the flywheel was running) and failed its CRC.
    Bad(u64),
}

/// The 10-bit CRC over the 27 data bits (as transmitted).
fn crc(data: &[u8]) -> u16 {
    let mut acc: u16 = 0x0393;
    let mut op: u16 = 0x036E;
    for &b in &data[..DATA_BITS] {
        op = if op & 1 != 0 { (op >> 1) ^ 0x0225 } else { op >> 1 };
        if b & 1 != 0 {
            acc ^= op;
        }
    }
    acc
}

/// Deinterleave: payload bit k·4 + l was sent at position k + 19·l.
fn deinterleave(sent: &[f32]) -> [f32; PAYLOAD_BITS] {
    let mut out = [0.0; PAYLOAD_BITS];
    for k in 0..PAYLOAD_BITS / 4 {
        for l in 0..4 {
            out[k * 4 + l] = sent[k + l * 19];
        }
    }
    out
}

/// Soft Viterbi over the 38 (data, parity) pairs; `soft` > 0 = 1. Returns the
/// data bits and how many received hard decisions the path disagrees with.
fn viterbi(soft: &[f32; PAYLOAD_BITS]) -> ([u8; PAYLOAD_BITS / 2], u32) {
    const N: usize = PAYLOAD_BITS / 2;
    let corr = |bit: u8, s: f32| if bit == 1 { s } else { -s };
    // State = the previous data bit. Starts at 0.
    let mut metric = [0.0f32, f32::NEG_INFINITY];
    let mut from = [[0u8; 2]; N];
    for k in 0..N {
        let (sd, sp) = (soft[2 * k], soft[2 * k + 1]);
        let mut next = [f32::NEG_INFINITY; 2];
        for d in 0..2u8 {
            for prev in 0..2u8 {
                let m = metric[prev as usize] + corr(d, sd) + corr(d ^ prev, sp);
                if m > next[d as usize] {
                    next[d as usize] = m;
                    from[k][d as usize] = prev;
                }
            }
        }
        metric = next;
    }
    let mut bits = [0u8; N];
    let mut s = if metric[1] > metric[0] { 1u8 } else { 0 };
    for k in (0..N).rev() {
        bits[k] = s;
        s = from[k][s as usize];
    }
    let mut flips = 0;
    let mut prev = 0u8;
    for k in 0..N {
        flips += ((soft[2 * k] > 0.0) != (bits[k] == 1)) as u32;
        flips += ((soft[2 * k + 1] > 0.0) != ((bits[k] ^ prev) == 1)) as u32;
        prev = bits[k];
    }
    (bits, flips)
}

/// Decode the 76 payload bits (as sent, after the sync).
pub fn decode(sent: &[f32]) -> Option<Osw> {
    let (bits, flips) = viterbi(&deinterleave(sent));
    if flips > MAX_CORRECTED {
        return None;
    }
    let given = bits[DATA_BITS..DATA_BITS + CRC_BITS].iter().fold(0u16, |a, &b| (a << 1) | (!b & 1) as u16);
    if given != crc(&bits) {
        return None;
    }
    let field = |r: std::ops::Range<usize>| bits[r].iter().fold(0u16, |a, &b| (a << 1) | b as u16);
    Some(Osw { addr: field(0..16) ^ ADDR_XOR, grp: bits[16] == 0, cmd: field(17..27) ^ CMD_XOR })
}

/// The 84 bits of an OSW as sent (sync first) — for tests and synthesis.
pub fn encode(o: Osw) -> [u8; FRAME_BITS] {
    let mut data = [0u8; PAYLOAD_BITS / 2];
    let a = o.addr ^ ADDR_XOR;
    let c = o.cmd ^ CMD_XOR;
    for j in 0..16 {
        data[j] = (a >> (15 - j) & 1) as u8;
    }
    data[16] = !o.grp as u8;
    for j in 0..10 {
        data[17 + j] = (c >> (9 - j) & 1) as u8;
    }
    let crc = crc(&data);
    for j in 0..CRC_BITS {
        data[DATA_BITS + j] = (!(crc >> (9 - j)) & 1) as u8;
    }
    let mut coded = [0u8; PAYLOAD_BITS];
    let mut prev = 0;
    for k in 0..PAYLOAD_BITS / 2 {
        coded[2 * k] = data[k];
        coded[2 * k + 1] = data[k] ^ prev;
        prev = data[k];
    }
    let mut out = [0u8; FRAME_BITS];
    for j in 0..8 {
        out[j] = SYNC >> (7 - j) & 1;
    }
    for k in 0..PAYLOAD_BITS / 4 {
        for l in 0..4 {
            out[8 + k + l * 19] = coded[k * 4 + l];
        }
    }
    out
}

/// Finds OSWs in a bit stream. Accepts either polarity (locking to the first
/// that decodes).
pub struct Framer {
    /// The last FRAME_BITS + 8 soft bits (a frame and the next sync).
    ring: Vec<f32>,
    pos: usize,
    n: u64,
    /// Hard bits, newest in bit 0.
    hard: u128,
    /// Bits since the last good frame's end, while the flywheel runs.
    since: Option<u64>,
    misses: u32,
    invert: bool,
    pub good: u64,
    pub bad: u64,
}

const WINDOW: usize = FRAME_BITS + 8;

impl Default for Framer {
    fn default() -> Self {
        Framer { ring: vec![0.0; WINDOW], pos: 0, n: 0, hard: 0, since: None, misses: 0, invert: false, good: 0, bad: 0 }
    }
}

impl Framer {
    pub fn in_sync(&self) -> bool {
        self.since.is_some()
    }

    /// Sync errors (Hamming distance) of the 8 bits ending `back` bits ago, in a polarity.
    fn sync_errors(&self, back: u32, invert: bool) -> u32 {
        let b = (self.hard >> back) as u8;
        (b ^ SYNC ^ if invert { 0xff } else { 0 }).count_ones()
    }

    /// Decode the frame that starts FRAME_BITS + 8 bits ago (after its sync,
    /// before the next).
    fn try_frame(&self, invert: bool) -> Option<Osw> {
        let mut sent = [0.0f32; PAYLOAD_BITS];
        for (i, s) in sent.iter_mut().enumerate() {
            let v = self.ring[(self.pos + 8 + i) % WINDOW];
            *s = if invert { -v } else { v };
        }
        decode(&sent)
    }

    pub fn push(&mut self, soft: f32, out: &mut Vec<FramerOut>) {
        self.ring[self.pos] = soft;
        self.pos = (self.pos + 1) % WINDOW;
        self.hard = (self.hard << 1) | (soft > 0.0) as u128;
        self.n += 1;
        if self.n < WINDOW as u64 {
            return;
        }
        // The window: [sync][payload][next sync] ending now. Its frame ends 8 bits ago.
        let end = self.n - 9;
        match self.since.as_mut() {
            None => {
                for inv in [self.invert, !self.invert] {
                    let errs = self.sync_errors(FRAME_BITS as u32, inv) + self.sync_errors(0, inv);
                    if errs <= 1 {
                        if let Some(o) = self.try_frame(inv) {
                            self.invert = inv;
                            self.accept(o, end, out);
                            return;
                        }
                    }
                }
            }
            Some(since) => {
                *since += 1;
                let s = *since;
                let inv = self.invert;
                // Due now (± a slipped bit): a frame with a good sync at either end.
                if (FRAME_BITS as u64 - 2..=FRAME_BITS as u64 + 2).contains(&s) {
                    let lead = self.sync_errors(FRAME_BITS as u32, inv);
                    let trail = self.sync_errors(0, inv);
                    let ok = if s == FRAME_BITS as u64 { lead.min(trail) <= 1 || lead + trail <= 4 } else { lead + trail <= 1 };
                    if ok {
                        if let Some(o) = self.try_frame(inv) {
                            self.accept(o, end, out);
                            return;
                        }
                    }
                }
                if s == FRAME_BITS as u64 + 2 {
                    self.bad += 1;
                    out.push(FramerOut::Bad(end));
                    self.misses += 1;
                    if self.misses > MAX_MISSES {
                        self.since = None;
                    } else {
                        self.since = Some(2);
                    }
                }
            }
        }
    }

    fn accept(&mut self, o: Osw, end: u64, out: &mut Vec<FramerOut>) {
        self.good += 1;
        self.since = Some(0);
        self.misses = 0;
        out.push(FramerOut::Osw(o, end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soft(bits: &[u8]) -> Vec<f32> {
        bits.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect()
    }

    #[test]
    fn encode_decode_round_trip() {
        for o in [Osw { addr: 0x1f00, grp: false, cmd: 0x2f8 }, Osw { addr: 0x8053, grp: true, cmd: 0x1a3 }, Osw { addr: 0xffff, grp: true, cmd: 0x3ff }] {
            let f = encode(o);
            assert_eq!(decode(&soft(&f[8..])), Some(o));
        }
    }

    #[test]
    fn viterbi_corrects_scattered_errors() {
        let o = Osw { addr: 0x6123, grp: false, cmd: 0x30b };
        let mut s = soft(&encode(o)[8..]);
        // Flip (weakly) five sent bits spread over the interleaver.
        for &i in &[3usize, 20, 41, 58, 70] {
            s[i] = -0.3 * s[i];
        }
        assert_eq!(decode(&s), Some(o));
    }

    #[test]
    fn framer_finds_frames_and_flywheels_over_a_bad_sync() {
        let msgs: Vec<Osw> = (0..20).map(|i| Osw { addr: 0x1000 + i, grp: i % 2 == 0, cmd: 0x2f8 - i }).collect();
        let mut bits: Vec<u8> = crate::smartnet::rx::tests::prbs(37, 7);
        for m in &msgs {
            bits.extend_from_slice(&encode(*m));
        }
        bits.extend_from_slice(&encode(Osw::default())[..8]);
        // Corrupt frame 5's sync; and invert everything (the other polarity).
        bits[37 + 5 * FRAME_BITS + 2] ^= 1;
        bits[37 + 5 * FRAME_BITS + 5] ^= 1;
        let mut fr = Framer::default();
        let mut out = Vec::new();
        for &b in &bits {
            fr.push(if b == 1 { -1.0 } else { 1.0 }, &mut out);
        }
        let got: Vec<Osw> = out.iter().filter_map(|o| if let FramerOut::Osw(o, _) = o { Some(*o) } else { None }).collect();
        assert_eq!(got, msgs);
        assert_eq!(fr.bad, 0);
    }
}
