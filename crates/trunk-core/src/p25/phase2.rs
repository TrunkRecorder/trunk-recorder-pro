//! P25 Phase 2 TDMA: the layers between an H-DQPSK dibit stream and the
//! AMBE+2 vocoder. A port of the archived engine's `phase2.ts`, itself
//! boatbod/op25 gr-op25_repeater (GPLv3): p25p2_framer / sync / isch / duid /
//! tdma (bursts, ESS, ACCH MAC PDUs, CRC-12) and apps/tdma/lfsr.py (the
//! scrambler). The voice codewords (vf) are [`crate::ambe`], shared with DMR.
//!
//! The channel (TIA-102.BBAC): 6000 sym/s; a 30 ms timeslot is 180 dibits;
//! 12 slots make a 360 ms superframe. Two logical voice channels alternate:
//! slots 0..9 are ch0, ch1, ch0, …; slots 10/11 are the SACCHs of ch1 / ch0.
//! Each slot starts with a 20-dibit ISCH — S-ISCH (the sync word) in slots
//! 2, 3, 6, 7, 10, 11, I-ISCH (slot location) in the others. The burst is
//! packet dibits 10..179, XOR-scrambled with a mask seeded from WACN /
//! System ID / NAC, which come from the control channel. [`Framer`] puts
//! out one [`Packet`] per timeslot.

use std::collections::VecDeque;

use super::crypt::algid_name;
use super::fec::{hexbit_rel, rs_decode_soft};
use super::mac::MacPdu;
use super::voice::{hexbits_to_bytes, EncryptionSync};
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
    // A zero seed would scramble nothing (BBAC-1 §7.2.5, eq. 5).
    let seed = if seed == 0 { (1 << 44) - 1 } else { seed };
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

/// An I-ISCH's fields (BBAC §5.5, Figure 5-2): the logical channel, which
/// I-ISCH of the superframe it is, and the superframe's place in its
/// ultraframe. None for the reserved values (channel 2 or 3, location 3):
/// a decode that lands on one is wrong.
pub fn isch_info(v: i32) -> Option<(usize, usize, u8)> {
    let (chn, loc) = ((v >> 5 & 3) as usize, (v >> 3 & 3) as usize);
    (v >= 0 && chn < 2 && loc < 3).then_some((chn, loc, (v & 3) as u8))
}

/// Burst DUID dibits sit at burst positions 10, 47, 132, 169.
const DUID_POS: [usize; 4] = [10, 47, 132, 169];

/// The burst type, or −1.
pub fn duid_decode(burst: &[u8]) -> i8 {
    let v = DUID_POS.iter().fold(0usize, |v, &p| v << 2 | (burst[p] & 3) as usize);
    DUID_LOOKUP[v]
}

/// The DUID's 16 codewords (8 bits each): those whose every one-bit
/// neighbour decodes to them.
fn duid_codewords() -> &'static [(u8, i8); 16] {
    static CW: std::sync::OnceLock<[(u8, i8); 16]> = std::sync::OnceLock::new();
    CW.get_or_init(|| {
        let mut cw = [(0u8, 0i8); 16];
        let mut n = 0;
        for v in 0..=255u8 {
            let d = DUID_LOOKUP[v as usize];
            if d >= 0 && (0..8).all(|b| DUID_LOOKUP[(v ^ 1 << b) as usize] == d) {
                cw[n] = (v, d);
                n += 1;
            }
        }
        cw
    })
}

/// A burst type's DUID codeword (8 bits, the 4 DUID dibits in order).
pub fn duid_codeword(kind: i8) -> u8 {
    duid_codewords().iter().find(|c| c.1 == kind).map_or(0, |c| c.0)
}

/// The burst type by soft inclusive search (BBAC §5.4.2): the codeword
/// whose disagreeing bits are the least reliable, `rel` 2 per burst dibit.
pub fn duid_decode_soft(burst: &[u8], rel: &[f32]) -> i8 {
    let bits: Vec<(u8, f32)> = DUID_POS.iter().flat_map(|&p| [(burst[p] >> 1 & 1, rel[2 * p]), (burst[p] & 1, rel[2 * p + 1])]).collect();
    let cost = |cw: u8| (0..8).filter(|&i| (cw >> (7 - i) & 1) != bits[i].0).map(|i| bits[i].1).sum::<f32>();
    duid_codewords().iter().min_by(|a, b| cost(a.0).total_cmp(&cost(b.0))).map_or(-1, |c| c.1)
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

/// A SACCH (I-OEMI, RS(52,30,23)) or FACCH (S-OEMI, RS(45,26,20)) MAC PDU
/// (BBAC §5.6.1, Table 5-5): the mother code RS(63,35) shortened (the first
/// 5 / 9 symbols, known zeros) and punctured (the last 6 / 9, erasures),
/// then the CRC-12 over the corrected word. `rel`: 2 per burst dibit.
pub fn decode_acch(burst: &[u8], rel: Option<&[f32]>, fast: bool) -> Option<MacPdu> {
    let runs: &[(usize, usize)] = if fast { &FACCH_RUNS } else { &SACCH_RUNS };
    let (mut bits, mut brel): (Vec<u8>, Vec<f32>) = (Vec::with_capacity(312), Vec::with_capacity(312));
    for &(s, n) in runs {
        for i in s..s + n {
            bits.extend([burst[i] >> 1 & 1, burst[i] & 1]);
            brel.extend(rel.map_or([1.0; 2], |r| [r[2 * i], r[2 * i + 1]]));
        }
    }
    let (first, info) = if fast { (9, 26) } else { (5, 30) };
    let punctured: Vec<usize> = (if fast { 54 } else { 57 }..63).collect();
    let (mut hb, mut hrel) = ([0u8; 63], [f32::INFINITY; 63]);
    for (j, i) in (0..bits.len()).step_by(6).enumerate() {
        hb[first + j] = bits[i..i + 6].iter().fold(0, |v, &b| v << 1 | b);
        hrel[first + j] = hexbit_rel(&brel[i..i + 6]);
    }
    // (The CRC-12 checks what is left.)
    if rs_decode_soft(&mut hb, 28, &punctured, first, &hrel, 0) < 0 {
        return None;
    }
    let word: Vec<u8> = hb[first..first + info].iter().flat_map(|h| (0..6).rev().map(move |b| h >> b & 1)).collect();
    let payload = 6 * info - 12;
    let rx = word[payload..].iter().fold(0u32, |v, &b| v << 1 | b as u32);
    if rx != crc12(&word, payload) {
        return None;
    }
    Some(MacPdu::new(word[..payload].chunks(8).map(|c| c.iter().fold(0u8, |v, &b| v << 1 | b)).collect()))
}

// ── ESS (4V ESS-B + 2V ESS-A, RS(44,16,29)) ──────────────────────────────────

/// A hexbit's reliability from its three dibits at `j` (`rel`: 2 per
/// burst dibit; none: all sure).
fn rel3(rel: Option<&[f32]>, j: usize) -> f32 {
    rel.map_or(1.0, |r| hexbit_rel(&r[2 * j..2 * j + 6]))
}

/// 4 hexbits of ESS-B from a 4V burst, and their reliabilities.
pub fn read_ess_b(x: &[u8], rel: Option<&[f32]>) -> ([u8; 4], [f32; 4]) {
    (std::array::from_fn(|h| (x[84 + 3 * h] & 3) << 4 | (x[85 + 3 * h] & 3) << 2 | (x[86 + 3 * h] & 3)), std::array::from_fn(|h| rel3(rel, 84 + 3 * h)))
}
/// 28 hexbits of ESS-A from a 2V burst (skipping the DUID dibit at 132), and their reliabilities.
pub fn read_ess_a(x: &[u8], rel: Option<&[f32]>) -> ([u8; 28], [f32; 28]) {
    let (mut hb, mut hr) = ([0u8; 28], [0f32; 28]);
    let mut j = 84;
    for i in 0..28 {
        hb[i] = (x[j] & 3) << 4 | (x[j + 1] & 3) << 2 | (x[j + 2] & 3);
        hr[i] = rel3(rel, j);
        j += if i == 15 { 4 } else { 3 };
    }
    (hb, hr)
}

/// The ESS (BBAC §5.6.2, RS(44,16,29): 19 symbols shortened): the 4V
/// bursts' ESS-B segments (16 information hexbits; a segment not heard: its
/// four erased), the 2V's ESS-A (28 parity), and each's reliabilities. Taken
/// if it decodes, names an ALGID there is (§5.6.2.1 step 3), and is a sync
/// that can have been sent ([`EncryptionSync::plausible`]).
pub fn decode_ess(ess_b: &[Option<([u8; 4], [f32; 4])>; 4], ess_a: &[u8; 28], ess_a_rel: &[f32; 28]) -> Option<EncryptionSync> {
    let (mut cw, mut rel) = ([0u8; 63], [f32::INFINITY; 63]);
    let mut erasures = Vec::new();
    for (k, seg) in ess_b.iter().enumerate() {
        for i in 0..4 {
            let at = 19 + 4 * k + i;
            match seg {
                Some((h, r)) => (cw[at], rel[at]) = (h[i] & 63, r[i]),
                None => erasures.push(at),
            }
        }
    }
    for i in 0..28 {
        (cw[35 + i], rel[35 + i]) = (ess_a[i] & 63, ess_a_rel[i]);
    }
    // No CRC: keep some roots to tell a wrong word by.
    if rs_decode_soft(&mut cw, 28, &erasures, 19, &rel, 6) < 0 {
        return None;
    }
    let b = &cw[19..35];
    let es = EncryptionSync {
        algid: ((b[0] as u16) << 2 | (b[1] as u16) >> 4) as u8,
        keyid: ((b[1] as u16 & 15) << 12) | (b[2] as u16) << 6 | b[3] as u16,
        // Hexbits 4..16.
        mi: hexbits_to_bytes(&cw, 23),
    };
    (algid_name(es.algid) != "unknown" && es.plausible()).then_some(es)
}

// ── Framer (p25p2_framer.cc + p25p2_sync.cc) ─────────────────────────────────

const EXPECTED_SYNC: [i32; 12] = [0, 1, -2, -2, 4, 5, -2, -2, 8, 9, -2, -2];

/// One timeslot as framed: the 180 packet dibits (ISCH first) and their bit
/// reliabilities (≥ 0; hi, lo per dibit).
#[derive(Clone, Debug)]
pub struct Packet {
    /// Superframe slot 0..11 (not the TDMA slot 0 / 1: [`SLOT_CHANNEL`] gives that).
    pub sf_slot: usize,
    /// Channel-sample instant of the packet's first dibit.
    pub sample: f64,
    pub dibits: [u8; SLOT_DIBITS],
    pub rel: Vec<f32>,
    /// The superframe's place in its ultraframe (0..3, from its I-ISCHs;
    /// BBAC §3.2.2), when one has been heard.
    pub ultraframe: Option<u8>,
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
    sf_slot: usize,
    confident: bool,
    ultraframe: Option<u8>,
    pub packets: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Framer { hist: VecDeque::new(), base: 0, n: 0, sr: 0, in_sync: 0, pkt_start: None, inverted: false, sf_slot: 0, confident: false, ultraframe: None, packets: 0 }
    }

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
        // check_confidence: an I-ISCH says where in the superframe this is.
        self.sf_slot = (self.sf_slot + 1) % 12;
        let rc = isch_lookup(&dibits);
        let info = isch_info(rc);
        let checkval = match info {
            Some((chn, loc, _)) => (loc * 4 + chn) as i32,
            None if rc == -2 => -2,
            None => -1,
        };
        if EXPECTED_SYNC[self.sf_slot] != checkval && checkval != -1 {
            self.confident = false;
            self.ultraframe = None;
        }
        if let Some((_, _, uf)) = info {
            self.confident = true;
            self.sf_slot = checkval as usize;
            self.ultraframe = Some(uf);
        }
        if self.confident && self.sf_slot < 12 {
            self.packets += 1;
            out.push(Packet { sf_slot: self.sf_slot, sample: self.hist[at].sample, dibits, rel, ultraframe: self.ultraframe });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::decode_vcw;
    use crate::p25::mac::{parse_mac_ptt, MacPtt, MAC_PTT};

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
        let pdu = decode_acch(&x, None, false).expect("PTT decodes");
        assert_eq!((pdu.opcode, pdu.offset), (MAC_PTT, 0));
        assert_eq!(parse_mac_ptt(&pdu.bytes), MacPtt { mi: [0; 9], algid: 0x80, keyid: 0, source: 1118562, group: 2207 });
        // RS(52,30) corrects symbol errors in the payload, and the CRC-12
        // (in the information symbols) is checked as corrected: an error in
        // it is no reason to drop the PDU.
        let crc_at = 48 + 3 * 16; // the CRC: information hexbits 28-29, the second run's 17th and 18th
        for i in [20, 60, 70, 150, crc_at] {
            x[i] ^= 3;
        }
        assert_eq!(decode_acch(&x, None, false).map(|p| p.bytes.clone()), Some(pdu.bytes.clone()));
        // With the shortened symbols known zeros (not erasures), 11 symbol errors come back (2·11 + 6 = 28).
        let mut y = descramble(&dibits(PTT), 10, &m);
        for h in 0..11 {
            y[11 + 3 * h] ^= 1;
        }
        assert_eq!(decode_acch(&y, None, false).map(|p| p.bytes), Some(pdu.bytes));
    }

    /// BBAC §5.5: channel 2/3 and location 3 are reserved; the ultraframe count is bits 1-0.
    #[test]
    fn isch_fields() {
        assert_eq!(isch_info(8), Some((0, 1, 0)));
        // (Reserved, channel 6-5, location 4-3, FR 2, ultraframe 1-0.)
        assert_eq!(isch_info(0b00_00_10_0_11), Some((0, 2, 3)));
        assert_eq!(isch_info(0b00_10_00_0_00), None, "channel 2");
        assert_eq!(isch_info(0b00_00_11_0_00), None, "location 3");
        assert_eq!(isch_info(-2), None);
    }

    /// BBAC §5.4.2: an unreliable DUID dibit is outvoted by the reliable ones.
    #[test]
    fn soft_duid() {
        let mut v4 = dibits(V4)[10..].to_vec();
        let mut rel = vec![1.0f32; 2 * BURST_DIBITS];
        v4[47] ^= 3; // both bits of one DUID dibit wrong…
        rel[94] = 0.01; // …and known to be unsure
        rel[95] = 0.01;
        assert_ne!(duid_decode(&v4), BURST_4V);
        assert_eq!(duid_decode_soft(&v4, &rel), BURST_4V);
    }

    /// BBAC-1 §7.2.5: a zero seed is replaced, so it still scrambles.
    #[test]
    fn a_zero_seed_scrambles() {
        assert!(xor_mask(0, 0, 0).iter().any(|&d| d != 0));
    }

    #[test]
    fn framer_needs_an_i_isch() {
        // S-ISCH slot, then an I-ISCH slot: only the second is trusted (slot 4).
        let mut f = Framer::new();
        let mut out = Vec::new();
        for d in dibits(PTT).into_iter().chain(dibits(V4)).chain(std::iter::repeat(0).take(40)) {
            f.push(&Symbol { dibit: d, ..Default::default() }, &mut out);
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sf_slot, 4);
        assert_eq!(out[0].dibits[..], dibits(V4)[..]);
    }
}
