//! P25 Phase 2 TDMA: the layers between an H-DQPSK dibit stream and the
//! AMBE+2 vocoder. A port of the archived engine's `phase2.ts`, itself
//! boatbod/op25 gr-op25_repeater (GPLv3): p25p2_framer / sync / isch / duid /
//! vf (AMBE codeword layout + FEC) / tdma (bursts, ESS, ACCH MAC PDUs,
//! CRC-12) and apps/tdma/lfsr.py (the scrambler).
//!
//! The channel (TIA-102.BBAC): 6000 sym/s; a 30 ms timeslot is 180 dibits;
//! 12 slots make a 360 ms superframe. Two logical voice channels alternate:
//! slots 0..9 are ch0, ch1, ch0, …; slots 10/11 are the SACCHs of ch1 / ch0.
//! Each slot starts with a 20-dibit ISCH — S-ISCH (the sync word) in slots
//! 2, 3, 6, 7, 10, 11, I-ISCH (slot location) in the others. The burst is
//! packet dibits 10..179, XOR-scrambled with a mask seeded from WACN /
//! System ID / NAC, which come from the control channel.

use std::collections::VecDeque;

use super::fec::{golay23_decode, golay23_decode_soft, golay24_decode, rs_decode, rs_decode_erasures};
use crate::dsp::Symbol;
use crate::tables::{DUID_LOOKUP, ISCH_CODEWORDS, LFSR_SEED_MATRIX};

pub const SYMBOL_RATE: f64 = 6000.0;
pub const SLOT_DIBITS: usize = 180;
pub const BURST_DIBITS: usize = 170;
/// 40-bit S-ISCH / frame sync (op25 P25P2_FRAME_SYNC_MAGIC).
const SYNC: u64 = 0x57_5D57_F7FF;
const SYNC_INV: u64 = SYNC ^ 0xAA_AAAA_AAAA;

/// Burst types (op25 duid_lookup values).
pub const BURST_4V: i8 = 0;
pub const BURST_SACCH_S: i8 = 3;
pub const BURST_LCCH_S: i8 = 4;
pub const BURST_2V: i8 = 6;
pub const BURST_FACCH_S: i8 = 9;
pub const BURST_SACCH_U: i8 = 12;
pub const BURST_LCCH_U: i8 = 13;
pub const BURST_FACCH_U: i8 = 15;

/// Which logical channel (0/1) each of the 12 slots belongs to (op25 which_slot).
pub const SLOT_CHANNEL: [usize; 12] = [0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 0];

// ── Scrambler ────────────────────────────────────────────────────────────────

/// op25 p25p2_lfsr(nac, sysid, wacn).xorsyms: the 2160-dibit (one
/// superframe) XOR mask. Slot s, burst dibit i is scrambled with
/// `mask[s·180 + i]`.
pub fn xor_mask(nac: u32, sys_id: u32, wacn: u32) -> Vec<u8> {
    let seed = ((wacn as u64 & 0xfffff) << 24) | ((sys_id as u64 & 0xfff) << 12) | (nac as u64 & 0xfff);
    // reg = seed · M over GF(2); rows and the register are MSB = column 0.
    let mut r = 0u64;
    for (i, row) in LFSR_SEED_MATRIX.iter().enumerate() {
        if seed >> (43 - i) & 1 != 0 {
            r ^= row;
        }
    }
    let field = |from: u32, len: u32| ((r >> (44 - from - len)) & ((1 << len) - 1)) as u32;
    let (mut s1, mut s2, mut s3, mut s4, mut s5, mut s6) = (field(0, 4), field(4, 5), field(9, 6), field(15, 5), field(20, 14), field(34, 10));
    let mut bits = [0u8; 4320];
    for b in bits.iter_mut() {
        *b = (s1 >> 3 & 1) as u8;
        let (c1, c2, c3, c4, c5, c6) = (s1 >> 3 & 1, s2 >> 4 & 1, s3 >> 5 & 1, s4 >> 4 & 1, s5 >> 13 & 1, s6 >> 9 & 1);
        s1 = (s1 << 1 & 0xf) | (c1 ^ c2);
        s2 = (s2 << 1 & 0x1f) | (c1 ^ c3);
        s3 = (s3 << 1 & 0x3f) | (c1 ^ c4);
        s4 = (s4 << 1 & 0x1f) | (c1 ^ c5);
        s5 = (s5 << 1 & 0x3fff) | (c1 ^ c6);
        s6 = (s6 << 1 & 0x3ff) | c1;
    }
    (0..2160).map(|i| bits[2 * i] << 1 | bits[2 * i + 1]).collect()
}

// ── ISCH / DUID ──────────────────────────────────────────────────────────────

fn word40(d: &[u8]) -> u64 {
    d[..20].iter().fold(0u64, |v, &x| v << 2 | (x & 3) as u64)
}

/// op25 isch_lookup: 20 dibits → I-ISCH value (0..127), −2 (S-ISCH) or −1.
pub fn isch_lookup(d: &[u8]) -> i32 {
    let v = word40(d);
    let (mut best, mut best_dist) = (-1, 8); // (40, 9, 16) code: corrects ≤ 7
    for &(cw, val) in ISCH_CODEWORDS.iter() {
        let dist = (v ^ cw).count_ones();
        if dist == 0 {
            return val as i32;
        }
        if dist < best_dist {
            best_dist = dist;
            best = val as i32;
        }
    }
    best
}

/// Burst DUID dibits sit at burst positions 10, 47, 132, 169.
const DUID_POS: [usize; 4] = [10, 47, 132, 169];

/// The burst type, or −1.
pub fn duid_decode(burst: &[u8]) -> i8 {
    let v = DUID_POS.iter().fold(0usize, |v, &p| v << 2 | (burst[p] & 3) as usize);
    DUID_LOOKUP[v]
}

// ── AMBE voice codeword (p25p2_vf.cc) ────────────────────────────────────────

/// extract_vcw: vf bit k ← (which of c0..c3, bit index).
pub(crate) const VCW_MAP: [(u8, u8); 72] = [
    (0, 23), (0, 5), (1, 10), (2, 3), (0, 22), (0, 4), (1, 9), (2, 2), (0, 21), (0, 3), (1, 8), (2, 1),
    (0, 20), (0, 2), (1, 7), (2, 0), (0, 19), (0, 1), (1, 6), (3, 13), (0, 18), (0, 0), (1, 5), (3, 12),
    (0, 17), (1, 22), (1, 4), (3, 11), (0, 16), (1, 21), (1, 3), (3, 10), (0, 15), (1, 20), (1, 2), (3, 9),
    (0, 14), (1, 19), (1, 1), (3, 8), (0, 13), (1, 18), (1, 0), (3, 7), (0, 12), (1, 17), (2, 10), (3, 6),
    (0, 11), (1, 16), (2, 9), (3, 5), (0, 10), (1, 15), (2, 8), (3, 4), (0, 9), (1, 14), (2, 7), (3, 3),
    (0, 8), (1, 13), (2, 6), (3, 2), (0, 7), (1, 12), (2, 5), (3, 1), (0, 6), (1, 11), (2, 4), (3, 0),
];

/// op25 extract_vcw: 36 dibits → c0 (24 bits), c1 (23), c2 (11), c3 (14),
/// and each bit's reliability (index = bit position, MSB first, per word).
pub fn extract_vcw(d: &[u8], rel: Option<&[f32]>, w1: &mut [f32; 23]) -> [u32; 4] {
    let mut c = [0u32; 4];
    for (k, &(w, i)) in VCW_MAP.iter().enumerate() {
        let dib = d[k >> 1];
        let b = if k & 1 != 0 { dib & 1 } else { dib >> 1 & 1 };
        if b != 0 {
            c[w as usize] |= 1 << i;
        }
        if let (1, Some(r)) = (w, rel) {
            w1[22 - i as usize] = r[k];
        }
    }
    c
}

/// The AMBE c1 modulator seeded from u0 (mbe_demodulateAmbe3600x2450Data):
/// 23 bits, first PN bit in the MSB.
pub fn ambe_pn23(u0: u32) -> u32 {
    let mut pr = 16 * u0;
    let mut m1 = 0;
    for _ in 1..24 {
        pr = (173 * pr + 13849) % 65536;
        m1 = m1 << 1 | (pr >> 15 & 1);
    }
    m1
}

#[derive(Clone, Copy, Debug)]
pub struct AmbeFrame {
    /// mbelib's ambe_d[49]: u0 (12) u1 (12) u2 (11) u3 (14), MSB first.
    pub bits: [u8; 49],
    /// Bits corrected in c0 + c1 (uncorrectable c0 counts 4).
    pub errs: u32,
}

/// op25 process_vcw: FEC-decode one 36-dibit voice codeword. With `rel`
/// (72 bit reliabilities), c1 is decoded soft (Chase-II).
pub fn decode_vcw(d: &[u8], rel: Option<&[f32]>) -> AmbeFrame {
    let mut w1 = [0f32; 23];
    let [c0, c1, c2, c3] = extract_vcw(d, rel, &mut w1);
    let r0 = golay24_decode(c0);
    let u0 = r0.data;
    let c1 = c1 ^ ambe_pn23(u0);
    let r1 = if rel.is_some() { golay23_decode_soft(c1, &w1) } else { golay23_decode(c1) };
    let u = [(u0, 12), (r1.data, 12), (c2, 11), (c3, 14)];
    let mut bits = [0u8; 49];
    let mut p = 0;
    for (v, w) in u {
        for b in (0..w).rev() {
            bits[p] = (v >> b & 1) as u8;
            p += 1;
        }
    }
    AmbeFrame { bits, errs: (if r0.errs < 0 { 4 } else { r0.errs as u32 }) + r1.errs.max(0) as u32 }
}

// ── ACCH / MAC PDUs (p25p2_tdma.cc handle_acch_frame, crc12) ────────────────

/// op25 crc12 over `len` bits (xorout 0xfff).
pub fn crc12(bits: &[u8], len: usize) -> u32 {
    const POLY: [u8; 13] = [1, 1, 0, 0, 0, 1, 0, 0, 1, 0, 1, 1, 1];
    let mut buf = vec![0u8; len + 12];
    for i in 0..len {
        buf[i] = bits[i] & 1;
    }
    for i in 0..len {
        if buf[i] != 0 {
            for j in 0..13 {
                buf[i + j] ^= POLY[j];
            }
        }
    }
    buf[len..].iter().fold(0, |c, &b| c << 1 | b as u32) ^ 0xfff
}

const SACCH_RUNS: [(usize, usize); 3] = [(11, 36), (48, 84), (133, 36)];
const FACCH_RUNS: [(usize, usize); 4] = [(11, 36), (48, 31), (100, 32), (133, 36)];

#[derive(Clone, Debug)]
pub struct MacPdu {
    pub opcode: u8,
    pub offset: u8,
    pub bytes: Vec<u8>,
}

pub const MAC_SIGNAL: u8 = 0;
pub const MAC_PTT: u8 = 1;
pub const MAC_END_PTT: u8 = 2;
pub const MAC_IDLE: u8 = 3;
pub const MAC_ACTIVE: u8 = 4;
pub const MAC_HANGTIME: u8 = 6;

/// op25 handle_acch_frame (SACCH / FACCH): RS(63,35) with erasures, CRC-12.
pub fn decode_acch(burst: &[u8], fast: bool) -> Option<MacPdu> {
    let runs: &[(usize, usize)] = if fast { &FACCH_RUNS } else { &SACCH_RUNS };
    let mut bits: Vec<u8> = Vec::with_capacity(312);
    for &(s, n) in runs {
        for &d in &burst[s..s + n] {
            bits.push(d >> 1 & 1);
            bits.push(d & 1);
        }
    }
    let mut hb = [0u8; 63];
    let first = if fast { 9 } else { 5 };
    let len = if fast { 270 } else { 312 };
    let erasures: &[usize] =
        if fast { &[0, 1, 2, 3, 4, 5, 6, 7, 8, 54, 55, 56, 57, 58, 59, 60, 61, 62] } else { &[0, 1, 2, 3, 4, 57, 58, 59, 60, 61, 62] };
    for (j, i) in (0..len).step_by(6).enumerate() {
        hb[first + j] = bits[i..i + 6].iter().fold(0, |v, &b| v << 1 | b);
    }
    if rs_decode_erasures(&mut hb, 28, erasures, 0) < 0 {
        return None;
    }
    let payload = if fast { 144 } else { 168 };
    for (j, i) in (0..payload).step_by(6).enumerate() {
        for b in 0..6 {
            bits[i + b] = hb[first + j] >> (5 - b) & 1;
        }
    }
    // op25 reads the CRC from the received (uncorrected) bits past the
    // payload — only the payload was rewritten. Kept as op25 does it.
    let rx = bits[payload..payload + 12].iter().fold(0u32, |v, &b| v << 1 | b as u32);
    if rx != crc12(&bits, payload) {
        return None;
    }
    let bytes: Vec<u8> = bits[..payload].chunks(8).map(|c| c.iter().fold(0u8, |v, &b| v << 1 | b)).collect();
    Some(MacPdu { opcode: bytes[0] >> 5 & 7, offset: bytes[0] >> 2 & 7, bytes })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacPtt {
    pub algid: u8,
    pub keyid: u16,
    pub source: u32,
    pub group: u32,
}

/// MAC_PTT fields (op25 handle_mac_ptt).
pub fn parse_mac_ptt(b: &[u8]) -> MacPtt {
    MacPtt {
        algid: b[10],
        keyid: (b[11] as u16) << 8 | b[12] as u16,
        source: (b[13] as u32) << 16 | (b[14] as u32) << 8 | b[15] as u32,
        group: (b[16] as u32) << 8 | b[17] as u32,
    }
}

// ── ESS (4V ESS-B + 2V ESS-A, RS(44,16,29)) ──────────────────────────────────

/// 4 hexbits of ESS-B from a 4V burst.
pub fn read_ess_b(x: &[u8]) -> [u8; 4] {
    std::array::from_fn(|h| (x[84 + 3 * h] & 3) << 4 | (x[85 + 3 * h] & 3) << 2 | (x[86 + 3 * h] & 3))
}
/// 28 hexbits of ESS-A from a 2V burst (skipping the DUID dibit at 132).
pub fn read_ess_a(x: &[u8]) -> [u8; 28] {
    let mut hb = [0u8; 28];
    let mut j = 84;
    for (i, h) in hb.iter_mut().enumerate() {
        *h = (x[j] & 3) << 4 | (x[j + 1] & 3) << 2 | (x[j + 2] & 3);
        j += if i == 15 { 4 } else { 3 };
    }
    hb
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ess {
    pub algid: u8,
    pub keyid: u16,
}

/// op25 handle_4V2V_ess: rs28.decode(ESS_B, ESS_A), ≤ 14 corrections.
pub fn decode_ess(ess_b: &[u8; 16], ess_a: &[u8; 28]) -> Option<Ess> {
    let mut cw = [0u8; 63];
    for i in 0..16 {
        cw[19 + i] = ess_b[i] & 63;
    }
    for i in 0..28 {
        cw[35 + i] = ess_a[i] & 63;
    }
    let ec = rs_decode(&mut cw, 28, 19);
    if !(0..=14).contains(&ec) {
        return None;
    }
    let b = &cw[19..35];
    Some(Ess { algid: ((b[0] as u16) << 2 | (b[1] as u16) >> 4) as u8, keyid: ((b[1] as u16 & 15) << 12) | (b[2] as u16) << 6 | b[3] as u16 })
}

// ── Framer (p25p2_framer.cc + p25p2_sync.cc) ─────────────────────────────────

const EXPECTED_SYNC: [i32; 12] = [0, 1, -2, -2, 4, 5, -2, -2, 8, 9, -2, -2];

/// One timeslot as framed: the 180 packet dibits (ISCH first) and their bit
/// reliabilities (hi, lo per dibit).
#[derive(Clone, Debug)]
pub struct Packet {
    /// Superframe slot 0..11.
    pub slot: usize,
    /// Channel-sample instant of the packet's first dibit.
    pub sample: f64,
    pub dibits: [u8; SLOT_DIBITS],
    pub rel: Vec<f32>,
}

/// Streaming slot framer, the op25 way: a 40-bit S-ISCH (≤ 4 bit errors,
/// either polarity) starts a packet and renews a 10-packet allowance; I-ISCH
/// packets anchor the slot count. Only packets whose slot is known with
/// confidence come out.
pub struct Framer {
    hist: VecDeque<Symbol>,
    /// Index (in symbols pushed) of hist[0].
    base: u64,
    n: u64,
    sr: u64,
    in_sync: u32,
    pkt_start: Option<u64>,
    inverted: bool,
    slot_id: usize,
    confident: bool,
    pub packets: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Framer { hist: VecDeque::new(), base: 0, n: 0, sr: 0, in_sync: 0, pkt_start: None, inverted: false, slot_id: 0, confident: false, packets: 0 }
    }
}

impl Framer {
    pub fn push(&mut self, s: &Symbol, out: &mut Vec<Packet>) {
        self.hist.push_back(*s);
        let n = self.n;
        self.n += 1;
        self.sr = (self.sr << 2 | s.dibit as u64) & 0xFF_FFFF_FFFF;
        if n >= 19 {
            let (e, ei) = ((self.sr ^ SYNC).count_ones(), (self.sr ^ SYNC_INV).count_ones());
            if e <= 4 || ei <= 4 {
                self.inverted = ei < e;
                self.pkt_start = Some(n - 19);
                self.in_sync = 10;
                self.trim();
                return;
            }
        }
        if let Some(start) = self.pkt_start {
            if self.in_sync > 0 && n + 1 - start >= SLOT_DIBITS as u64 {
                self.finish(start, out);
                self.in_sync -= 1;
                self.pkt_start = if self.in_sync > 0 { Some(start + SLOT_DIBITS as u64) } else { None };
            }
        }
        self.trim();
    }

    fn trim(&mut self) {
        // Keep what the next packet can still need.
        let keep_from = self.pkt_start.unwrap_or(self.n.saturating_sub(40)).min(self.n.saturating_sub(40));
        while self.base < keep_from && !self.hist.is_empty() {
            self.hist.pop_front();
            self.base += 1;
        }
    }

    fn finish(&mut self, start: u64, out: &mut Vec<Packet>) {
        let at = (start - self.base) as usize;
        let inv = if self.inverted { 2 } else { 0 };
        let mut dibits = [0u8; SLOT_DIBITS];
        let mut rel = vec![0f32; 2 * SLOT_DIBITS];
        for i in 0..SLOT_DIBITS {
            let s = &self.hist[at + i];
            dibits[i] = s.dibit ^ inv;
            rel[2 * i] = s.rel_hi;
            rel[2 * i + 1] = s.rel_lo;
        }
        // check_confidence
        self.slot_id = (self.slot_id + 1) % 12;
        let rc = isch_lookup(&dibits);
        let mut checkval = rc;
        let mut chn = -1;
        if rc >= 0 {
            chn = rc >> 5 & 3;
            checkval = (rc >> 3 & 3) * 4 + chn;
        }
        if EXPECTED_SYNC[self.slot_id] != checkval && checkval != -1 {
            self.confident = false;
        }
        if chn >= 0 {
            self.confident = true;
            self.slot_id = checkval as usize;
        }
        if self.confident && self.slot_id < 12 {
            self.packets += 1;
            out.push(Packet { slot: self.slot_id, sample: self.hist[at].sample, dibits, rel });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors from the archived TS encoder (phase2.ts encodeSlot, loopback-
    // tested there against op25): scrambler seed NAC 0x443 / SysID 0x445 /
    // WACN 0xbee00.
    const MASK0: &str = "2332320000101011101003303203123012311112";
    /// Slot 4, 4V: u = (0x123,0x456,0x78,0x1abc) (0xfff,0,0x7ff,0x3fff) (0x800,1,0x400,0x2000) (0xf0,0xf0f,0x155,0x2aaa).
    const V4: &str = "010110122210103301030313210030222331030010331223003223303010111221222002023313121010121213310332022211110021300103203131032322221131331022013102023310223032002302000301230310223310";
    /// Slot 10, scrambled SACCH: MAC_PTT source 1118562, group 2207, clear.
    const PTT: &str = "111311311113331333330211132310100122212222231320131130123302330330003002210001001112013113103030032002320323323033130100310213222213103100110223210002001130230222133210033333230301";

    fn dibits(s: &str) -> Vec<u8> {
        s.bytes().map(|b| b - b'0').collect()
    }
    fn descramble(p: &[u8], slot: usize, mask: &[u8]) -> Vec<u8> {
        (0..BURST_DIBITS).map(|i| p[10 + i] ^ mask[slot * SLOT_DIBITS + i]).collect()
    }

    #[test]
    fn scrambler_matches_op25() {
        let m = xor_mask(0x443, 0x445, 0xbee00);
        assert_eq!(m[..40], dibits(MASK0)[..]);
        assert_eq!(m.iter().map(|&v| v as u32).sum::<u32>(), 3188);
    }

    #[test]
    fn isch_and_duid() {
        let v4 = dibits(V4);
        assert_eq!(isch_lookup(&v4), 8); // slot 4: channel 0, location 1
        assert_eq!(duid_decode(&v4[10..]), BURST_4V);
        let ptt = dibits(PTT);
        assert_eq!(isch_lookup(&ptt), -2); // S-ISCH
        assert_eq!(duid_decode(&ptt[10..]), BURST_SACCH_S);
    }

    #[test]
    fn voice_codewords() {
        let m = xor_mask(0x443, 0x445, 0xbee00);
        let x = descramble(&dibits(V4), 4, &m);
        let want = [[0x123u32, 0x456, 0x78, 0x1abc], [0xfff, 0, 0x7ff, 0x3fff], [0x800, 1, 0x400, 0x2000], [0xf0, 0xf0f, 0x155, 0x2aaa]];
        for (k, &a) in [11, 48, 96, 133].iter().enumerate() {
            let f = decode_vcw(&x[a..a + 36], None);
            assert_eq!(f.errs, 0);
            let mut u = [0u32; 4];
            let mut p = 0;
            for (j, w) in [12, 12, 11, 14].iter().enumerate() {
                for _ in 0..*w {
                    u[j] = u[j] << 1 | f.bits[p] as u32;
                    p += 1;
                }
            }
            assert_eq!(u, want[k]);
        }
        // A flipped bit in c0 and one in c1 are corrected.
        let mut y = x.clone();
        y[11] ^= 2;
        y[12] ^= 2; // vcw bit 2 = c1 bit 10
        let f = decode_vcw(&y[11..47], None);
        assert_eq!((f.errs, f.bits), (2, decode_vcw(&x[11..47], None).bits));
    }

    #[test]
    fn mac_ptt() {
        let m = xor_mask(0x443, 0x445, 0xbee00);
        let mut x = descramble(&dibits(PTT), 10, &m);
        let pdu = decode_acch(&x, false).expect("PTT decodes");
        assert_eq!((pdu.opcode, pdu.offset), (MAC_PTT, 0));
        assert_eq!(parse_mac_ptt(&pdu.bytes), MacPtt { algid: 0x80, keyid: 0, source: 1118562, group: 2207 });
        // RS(63,35) corrects symbol errors in the payload.
        for i in [20, 60, 70, 150] {
            x[i] ^= 3;
        }
        assert_eq!(decode_acch(&x, false).map(|p| p.bytes), Some(pdu.bytes));
    }

    #[test]
    fn framer_needs_an_i_isch() {
        // S-ISCH slot, then an I-ISCH slot: only the second is trusted (slot 4).
        let mut f = Framer::default();
        let mut out = Vec::new();
        for d in dibits(PTT).into_iter().chain(dibits(V4)).chain(std::iter::repeat(0).take(40)) {
            f.push(&Symbol { dibit: d, ..Default::default() }, &mut out);
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].slot, 4);
        assert_eq!(out[0].dibits[..], dibits(V4)[..]);
    }

    #[test]
    fn pn_first_bit() {
        assert_eq!(ambe_pn23(0) >> 22, 0);
    }
}
