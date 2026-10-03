//! P25 Phase 1 frame layer: the NID's BCH(63,16) code and a streaming framer
//! (sync → status-symbol strip → NID → frame of the DUID's length):
//! [`Framer`] (built from [`FramerOptions`]) puts out [`Frame`]s.
//!
//! `bch_decode` is op25's bch.cc::bchDec (© 2010 KA1RBI) and `decode_nid`
//! op25's p25_framer::nid_codeword. The framer adds a flywheel (a weaker sync
//! is accepted exactly where the next frame must start) and NID recovery (the
//! NID can only be the site's NAC with one of 7 DUIDs) — together they cut
//! LDUs lost on simulcast from ~15 % to ~1 %.

use crate::dsp::Symbol;

pub const HDU: u8 = 0x0;
pub const TDU: u8 = 0x3;
pub const LDU1: u8 = 0x5;
pub const TSDU: u8 = 0x7;
pub const LDU2: u8 = 0xA;
pub const PDU: u8 = 0xC;
pub const TDULC: u8 = 0xF;

pub fn duid_name(d: u8) -> &'static str {
    match d {
        HDU => "HDU",
        TDU => "TDU",
        LDU1 => "LDU1",
        TSDU => "TSDU",
        LDU2 => "LDU2",
        PDU => "PDU",
        TDULC => "TDULC",
        _ => "?",
    }
}

static GF_EXP: [i32; 64] = [
    1, 2, 4, 8, 16, 32, 3, 6, 12, 24, 48, 35, 5, 10, 20, 40, 19, 38, 15, 30, 60, 59, 53, 41, 17, 34, 7, 14, 28, 56, 51, 37, 9, 18, 36, 11,
    22, 44, 27, 54, 47, 29, 58, 55, 45, 25, 50, 39, 13, 26, 52, 43, 21, 42, 23, 46, 31, 62, 63, 61, 57, 49, 33, 0,
];
static GF_LOG: [i32; 64] = [
    -1, 0, 1, 6, 2, 12, 7, 26, 3, 32, 13, 35, 8, 48, 27, 18, 4, 24, 33, 16, 14, 52, 36, 54, 9, 45, 49, 38, 28, 41, 19, 56, 5, 62, 25, 11,
    34, 31, 17, 47, 15, 23, 53, 51, 37, 44, 55, 40, 10, 61, 46, 30, 50, 22, 39, 43, 29, 60, 42, 21, 20, 59, 57, 58,
];
static BCH_G: [u8; 48] = [
    1, 1, 0, 1, 0, 1, 0, 0, 1, 1, 0, 1, 1, 1, 0, 0, 1, 0, 1, 1, 1, 0, 1, 1, 1, 1, 0, 1, 0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 0,
    0, 1, 1,
];

#[inline]
fn gl(v: i32) -> i32 {
    GF_LOG[v as usize]
}
#[inline]
fn ge(v: i32) -> i32 {
    GF_EXP[v as usize]
}

/// op25 bchDec. `cw[0..63]` is the codeword (cw[i] = coefficient of x^i),
/// corrected in place. Returns bits corrected, or < 0 if undecodable.
pub fn bch_decode(cw: &mut [u8; 64]) -> i32 {
    let mut elp = [[0i32; 22]; 24];
    let (mut s, mut d, mut l, mut ulu) = ([0i32; 23], [0i32; 23], [0i32; 24], [0i32; 24]);
    let (mut locn, mut reg) = ([0usize; 11], [0i32; 12]);
    let mut syn_error = false;
    for i in 1..=22 {
        s[i] = 0;
        for j in 0..=62 {
            if cw[j] != 0 {
                s[i] ^= GF_EXP[(i * j) % 63];
            }
        }
        if s[i] != 0 {
            syn_error = true;
        }
        s[i] = gl(s[i]);
    }
    if !syn_error {
        return 0;
    }
    l[0] = 0;
    ulu[0] = -1;
    d[0] = 0;
    elp[0][0] = 0;
    l[1] = 0;
    ulu[1] = 0;
    d[1] = s[1];
    elp[1][0] = 1;
    for i in 1..=21 {
        elp[0][i] = -1;
        elp[1][i] = 0;
    }
    let mut u: usize = 0;
    loop {
        u += 1;
        if d[u] == -1 {
            l[u + 1] = l[u];
            for i in 0..=l[u] as usize {
                elp[u + 1][i] = elp[u][i];
                elp[u][i] = gl(elp[u][i]);
            }
        } else {
            let mut q = u - 1;
            while d[q] == -1 && q > 0 {
                q -= 1;
            }
            if q > 0 {
                let mut j = q;
                loop {
                    j -= 1;
                    if d[j] != -1 && ulu[q] < ulu[j] {
                        q = j;
                    }
                    if j == 0 {
                        break;
                    }
                }
            }
            l[u + 1] = if l[u] > l[q] + u as i32 - q as i32 { l[u] } else { l[q] + u as i32 - q as i32 };
            for i in 0..=21 {
                elp[u + 1][i] = 0;
            }
            for i in 0..=l[q] as usize {
                if elp[q][i] != -1 {
                    elp[u + 1][i + u - q] = ge((d[u] + 63 - d[q] + elp[q][i]) % 63);
                }
            }
            for i in 0..=l[u] as usize {
                elp[u + 1][i] ^= elp[u][i];
                elp[u][i] = gl(elp[u][i]);
            }
        }
        ulu[u + 1] = u as i32 - l[u + 1];
        if u < 22 {
            d[u + 1] = if s[u + 1] != -1 { ge(s[u + 1]) } else { 0 };
            for i in 1..=l[u + 1] as usize {
                if s[u + 1 - i] != -1 && elp[u + 1][i] != 0 {
                    d[u + 1] ^= ge((s[u + 1 - i] + gl(elp[u + 1][i])) % 63);
                }
            }
            d[u + 1] = gl(d[u + 1]);
        }
        if !(u < 22 && l[u + 1] <= 11) {
            break;
        }
    }
    u += 1;
    if l[u] > 11 {
        return -2;
    }
    for i in 0..=l[u] as usize {
        elp[u][i] = gl(elp[u][i]);
    }
    for i in 1..=l[u] as usize {
        reg[i] = elp[u][i];
    }
    let mut count = 0usize;
    for i in 1..=63usize {
        let mut q = 1;
        for j in 1..=l[u] as usize {
            if reg[j] != -1 {
                reg[j] = (reg[j] + j as i32) % 63;
                q ^= ge(reg[j]);
            }
        }
        if q == 0 {
            if count < 11 {
                locn[count] = 63 - i;
            }
            count += 1;
        }
    }
    if count as i32 != l[u] {
        return -1;
    }
    for &p in &locn[..count] {
        cw[p] ^= 1;
    }
    count as i32
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Nid {
    pub nac: u16,
    pub duid: u8,
    /// Bits the BCH decoder corrected.
    pub errors: u8,
}

/// The 64-bit NID word (first bit on air = MSB) for {nac, duid}, with parity.
pub fn encode_nid(nac: u16, duid: u8) -> u64 {
    let v = ((nac as u32 & 0xfff) << 4) | (duid as u32 & 0xf);
    let mut c = [0u8; 63];
    for k in 0..16 {
        c[62 - k] = ((v >> (15 - k)) & 1) as u8;
    }
    for pos in (47..=62).rev() {
        if c[pos] != 0 {
            for k in 0..=47 {
                c[k + pos - 47] ^= BCH_G[k];
            }
        }
    }
    for k in 0..16 {
        c[62 - k] = ((v >> (15 - k)) & 1) as u8;
    }
    let mut word = 0u64;
    for (i, &b) in c.iter().enumerate() {
        if b != 0 {
            word |= 1 << i;
        }
    }
    (word << 1) | (duid == LDU1 || duid == LDU2) as u64
}

/// Decode a NID word; `None` if BCH fails (> 4 errors, as op25) or the
/// DUID/parity check fails.
pub fn decode_nid(acc: u64) -> Option<Nid> {
    let parity = (acc & 1) as u8;
    let mut a = acc >> 1;
    let mut cw = [0u8; 64];
    for b in cw.iter_mut() {
        *b = (a & 1) as u8;
        a >>= 1;
    }
    let ec = bch_decode(&mut cw);
    if !(0..=4).contains(&ec) {
        return None;
    }
    let mut word = 0u64;
    for i in (0..=62).rev() {
        word = (word << 1) | cw[i] as u64;
    }
    word = (word << 1) | parity as u64;
    if word >> 1 == 0 {
        return None;
    }
    let nac = ((word >> 52) & 0xfff) as u16;
    let duid = ((word >> 48) & 0xf) as u8;
    if matches!(duid, 0 | 3 | 7 | 12 | 15) && parity != 0 {
        return None;
    }
    if matches!(duid, 5 | 10) && parity == 0 {
        return None;
    }
    Some(Nid { nac, duid, errors: ec as u8 })
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub nid: Nid,
    /// Symbol index of the sync's first symbol, in this receiver's count.
    pub symbol: u64,
    /// Channel-sample instant of that symbol — common to every receiver on the channel.
    pub sample: f64,
    pub inverted: bool,
    /// Status symbols removed: [0,48) sync, [48,112) NID, then the body.
    pub bits: Vec<u8>,
    /// op25's frame body: status symbols left in place (the voice tables index this).
    pub raw: Vec<u8>,
    /// Per-bit reliability (≥ 0, unsigned: the bit is in `bits`), parallel
    /// to `bits` / `raw`; empty when the symbols carried none.
    pub soft: Vec<f32>,
    pub raw_soft: Vec<f32>,
    /// Reached its full length (false: cut short by the next sync).
    pub complete: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct FramerOptions {
    pub flywheel: bool,
    pub flywheel_errs: u32,
    pub nid_recover: bool,
    pub nid_max_errs: u32,
}

impl Default for FramerOptions {
    fn default() -> Self {
        FramerOptions { flywheel: true, flywheel_errs: 12, nid_recover: true, nid_max_errs: 11 }
    }
}

const SYNC: u64 = 0x5575_F5FF_77FF;
const INV: u64 = 0xAAAA_AAAA_AAAA;
static SYNC_BITS: [u8; 48] = [
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 1, 0, 1, 0, 1, 1, 1, 1, 1, 0, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1,
];

/// Frame length in dibits including status symbols; TSDU at its 3-TSBK maximum.
fn frame_dibits(duid: u8) -> usize {
    match duid {
        HDU => 396,
        TDU => 72,
        LDU1 | LDU2 => 864,
        TSDU => 360,
        TDULC => 216,
        _ => 0,
    }
}

#[derive(Default)]
pub struct FramerStats {
    pub syncs: u64,
    pub nid_fails: u64,
    pub flywheels: u64,
    pub nid_recovered: u64,
}

pub struct Framer {
    opt: FramerOptions,
    sr: u64,
    count: u64,
    start: u64,
    expect_at: u64,
    expecting: bool,
    expect_inv: bool,
    last_nac: Option<u16>,
    times: [f64; 24],
    active: bool,
    inv: bool,
    pos: usize,
    target: usize,
    f: Frame,
    pub stats: FramerStats,
}

impl Framer {
    pub fn new(opt: FramerOptions) -> Self {
        Framer {
            opt,
            sr: 0,
            count: 0,
            start: 0,
            expect_at: 0,
            expecting: false,
            expect_inv: false,
            last_nac: None,
            times: [0.0; 24],
            active: false,
            inv: false,
            pos: 0,
            target: 0,
            f: Frame::default(),
            stats: FramerStats::default(),
        }
    }

    pub fn push(&mut self, s: &Symbol, frames: &mut Vec<Frame>) {
        self.sr = ((self.sr << 2) | s.dibit as u64) & 0xFFFF_FFFF_FFFF;
        let n = self.count;
        self.count += 1;
        self.times[(n % 24) as usize] = s.sample;
        let soft = s.rel_hi >= 0.0;
        // A new sync ends whatever was running (ignoring matches inside the sync
        // we are already in).
        if !(self.active && self.pos < 48) {
            let e = (self.sr ^ SYNC).count_ones();
            let ei = (self.sr ^ SYNC ^ INV).count_ones();
            // (Not before a whole sync's 24 symbols have come: the register's
            // empty top would pass for the sync's leading dibits.)
            let strict = n >= 23 && (e <= 4 || ei <= 4);
            let fly = !strict
                && self.opt.flywheel
                && self.expecting
                && n == self.expect_at
                && (if self.expect_inv { ei } else { e }) <= self.opt.flywheel_errs;
            if n >= self.expect_at {
                self.expecting = false;
            }
            if strict || fly {
                self.finish(false, frames);
                self.stats.syncs += 1;
                if fly {
                    self.stats.flywheels += 1;
                }
                self.expecting = false;
                self.active = true;
                self.inv = if fly { self.expect_inv } else { ei < e };
                self.pos = 24;
                self.start = n - 23;
                let f = &mut self.f;
                f.sample = self.times[((n + 1) % 24) as usize];
                f.bits.clear();
                f.bits.extend_from_slice(&SYNC_BITS);
                f.raw.clear();
                f.raw.extend_from_slice(&SYNC_BITS);
                f.soft.clear();
                f.raw_soft.clear();
                if soft {
                    f.soft.resize(48, 1.0);
                    f.raw_soft.resize(48, 1.0);
                }
                self.target = 0;
                return;
            }
        }
        if !self.active {
            return;
        }
        let p = self.pos;
        self.pos += 1;
        let d = if self.inv { s.dibit ^ 0b10 } else { s.dibit };
        self.f.raw.push(d >> 1);
        self.f.raw.push(d & 1);
        if soft {
            self.f.raw_soft.push(s.rel_hi);
            self.f.raw_soft.push(s.rel_lo);
        }
        if (p + 1) % 36 == 0 {
            // status symbol
            if self.target != 0 && p + 1 >= self.target {
                self.finish(true, frames);
            }
            return;
        }
        self.f.bits.push(d >> 1);
        self.f.bits.push(d & 1);
        if soft {
            self.f.soft.push(s.rel_hi);
            self.f.soft.push(s.rel_lo);
        }
        if self.f.bits.len() == 112 {
            let acc = self.f.bits[48..112].iter().fold(0u64, |a, &b| (a << 1) | b as u64);
            match decode_nid(acc).or_else(|| self.recover_nid(acc)) {
                Some(nid) => {
                    self.f.nid = nid;
                    self.last_nac = Some(nid.nac);
                }
                None => {
                    self.stats.nid_fails += 1;
                    self.active = false;
                    return;
                }
            }
            self.target = frame_dibits(self.f.nid.duid);
            if self.target == 0 {
                // PDU or unknown: report the header, don't try to follow it.
                self.finish(false, frames);
                return;
            }
        }
        if self.target != 0 && p + 1 >= self.target {
            self.finish(true, frames);
        }
    }

    fn recover_nid(&mut self, acc: u64) -> Option<Nid> {
        let nac = self.last_nac.filter(|_| self.opt.nid_recover)?;
        let soft = self.f.soft.len() >= 112;
        let mut best: Option<(f32, u8, u32)> = None;
        for d in [HDU, TDU, LDU1, TSDU, LDU2, PDU, TDULC] {
            let diff = acc ^ encode_nid(nac, d);
            let hard = diff.count_ones();
            let cost = if soft {
                (0..64).filter(|i| (diff >> (63 - i)) & 1 != 0).map(|i| self.f.soft[48 + i]).sum()
            } else {
                hard as f32
            };
            if best.is_none_or(|(c, _, _)| cost < c) {
                best = Some((cost, d, hard));
            }
        }
        let (_, duid, hard) = best?;
        if hard > self.opt.nid_max_errs {
            return None;
        }
        self.stats.nid_recovered += 1;
        Some(Nid { nac, duid, errors: hard as u8 })
    }

    fn finish(&mut self, complete: bool, frames: &mut Vec<Frame>) {
        if !self.active {
            return;
        }
        self.active = false;
        if self.f.bits.len() < 112 {
            return;
        }
        if complete {
            // The next frame's sync must end exactly target + 23 dibits after this start.
            self.expecting = true;
            self.expect_at = self.start + self.target as u64 + 23;
            self.expect_inv = self.inv;
        }
        self.f.symbol = self.start;
        self.f.inverted = self.inv;
        self.f.complete = complete;
        frames.push(self.f.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nid_round_trips_and_corrects() {
        let mut seed = 1u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 33
        };
        for nac in [0x443u16, 0x4d8, 0x001, 0xfff] {
            for d in [HDU, TDU, LDU1, TSDU, LDU2, PDU, TDULC] {
                let w = encode_nid(nac, d);
                assert_eq!(decode_nid(w), Some(Nid { nac, duid: d, errors: 0 }));
                for _ in 0..50 {
                    let k = 1 + rnd() % 4;
                    let mut e = 0u64;
                    while (e.count_ones() as u64) < k {
                        e |= 1 << (1 + rnd() % 63);
                    }
                    let got = decode_nid(w ^ e).expect("≤ 4 errors must decode");
                    assert_eq!((got.nac, got.duid), (nac, d));
                }
            }
        }
    }

    /// A receiver whose first symbols land inside a frame sync: the tail
    /// of a sync isn't taken for one (it read before the stream's start).
    #[test]
    fn a_sync_cut_by_the_stream_start_is_not_a_sync() {
        for skip in 1..=4 {
            let mut fr = Framer::new(FramerOptions::default());
            let mut frames = Vec::new();
            for k in skip..24 {
                let dibit = ((SYNC >> (2 * (23 - k))) & 3) as u8;
                fr.push(&Symbol { dibit, sample: k as f64, rel_hi: 1.0, rel_lo: 1.0 }, &mut frames);
            }
            assert_eq!(fr.stats.syncs, 0, "{skip} symbols cut");
        }
    }
}
