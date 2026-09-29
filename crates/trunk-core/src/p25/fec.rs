//! Forward error correction for the P25 voice layers:
//!
//! * Golay(24,12) / (23,12) — IMBE u0..u3, HDU/TDULC hexbits
//! * Hamming(15,11) — IMBE u4..u6
//! * Hamming(10,6) — LDU link-control / encryption-sync hexbits
//! * Reed-Solomon over GF(2⁶) — LC (24,12), ES (24,16), HDU (36,20), as ezpwd's RS<63,k>
//!
//! Encoders are op25's; the syndrome → error-pattern tables are built from
//! them. The soft decoders (Chase-II Golay, maximum-likelihood Hamming) cut
//! wrong IMBE codewords from 7.6 % to 2.2 % on synthetic simulcast.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fec {
    pub data: u32,
    /// Bits corrected; −1 uncorrectable (data = the raw data bits).
    pub errs: i32,
    /// Soft decoders: summed reliability of the bits overridden.
    pub cost: f32,
}

pub fn golay24_encode(data: u32) -> u32 {
    const E: [u32; 12] = [
        0o40006165, 0o20003073, 0o10007550, 0o4003664, 0o2001732, 0o1006631, 0o403315, 0o201547, 0o106706, 0o45227, 0o24476, 0o14353,
    ];
    (0..12).filter(|i| data & (1 << (11 - i)) != 0).fold(0, |o, i| o ^ E[i])
}
pub fn golay23_encode(data: u32) -> u32 {
    golay24_encode(data) >> 1
}
pub fn hamming15_encode(data: u32) -> u32 {
    const E: [u32; 11] = [0x400f, 0x200e, 0x100d, 0x080c, 0x040b, 0x020a, 0x0109, 0x0087, 0x0046, 0x0025, 0x0013];
    (0..11).filter(|i| data & (1 << (10 - i)) != 0).fold(0, |o, i| o ^ E[i])
}

fn syndrome_table(n: u32, syn_bits: u32, max_w: u32, syndrome: fn(u32) -> u32) -> Vec<i32> {
    let mut t = vec![-1i32; 1 << syn_bits];
    t[0] = 0;
    let mut visit = |pat: u32| {
        let s = syndrome(pat) as usize;
        if t[s] == -1 || (t[s] as u32).count_ones() > pat.count_ones() {
            t[s] = pat as i32;
        }
    };
    for a in 0..n {
        visit(1 << a);
        if max_w < 2 {
            continue;
        }
        for b in a + 1..n {
            visit((1 << a) | (1 << b));
            if max_w < 3 {
                continue;
            }
            for c in b + 1..n {
                visit((1 << a) | (1 << b) | (1 << c));
            }
        }
    }
    t
}
fn golay24_syn(cw: u32) -> u32 {
    (golay24_encode(cw >> 12) ^ cw) & 0xfff
}
fn golay23_syn(cw: u32) -> u32 {
    (golay23_encode(cw >> 11) ^ cw) & 0x7ff
}
fn hamming15_syn(cw: u32) -> u32 {
    (hamming15_encode(cw >> 4) ^ cw) & 0xf
}

pub fn golay24_decode(cw: u32) -> Fec {
    static T: OnceLock<Vec<i32>> = OnceLock::new();
    let t = T.get_or_init(|| syndrome_table(24, 12, 3, golay24_syn));
    let cw = cw & 0xffffff;
    let e = t[golay24_syn(cw) as usize];
    if e < 0 {
        return Fec { data: (cw >> 12) & 0xfff, errs: -1, cost: 0.0 };
    }
    Fec { data: ((cw ^ e as u32) >> 12) & 0xfff, errs: (e as u32).count_ones() as i32, cost: 0.0 }
}
pub fn golay23_decode(cw: u32) -> Fec {
    static T: OnceLock<Vec<i32>> = OnceLock::new();
    let t = T.get_or_init(|| syndrome_table(23, 11, 3, golay23_syn));
    let cw = cw & 0x7fffff;
    let e = t[golay23_syn(cw) as usize] as u32;
    Fec { data: ((cw ^ e) >> 11) & 0xfff, errs: e.count_ones() as i32, cost: 0.0 }
}
pub fn hamming15_decode(cw: u32) -> Fec {
    static T: OnceLock<Vec<i32>> = OnceLock::new();
    let t = T.get_or_init(|| syndrome_table(15, 4, 1, hamming15_syn));
    let cw = cw & 0x7fff;
    let e = t[hamming15_syn(cw) as usize];
    if e < 0 {
        return Fec { data: (cw >> 4) & 0x7ff, errs: -1, cost: 0.0 };
    }
    Fec { data: ((cw ^ e as u32) >> 4) & 0x7ff, errs: (e as u32).count_ones() as i32, cost: 0.0 }
}

/// Summed reliability of the bits set in `diff` (bit b of an N-bit word ↔ w[N−1−b]).
#[inline]
fn weight(mut diff: u32, w: &[f32], nbits: usize) -> f32 {
    let mut c = 0.0;
    while diff != 0 {
        let b = diff.trailing_zeros() as usize;
        c += w[nbits - 1 - b];
        diff &= diff - 1;
    }
    c
}

/// Hamming(15,11), maximum likelihood over all 2048 codewords. `w[i]`: the
/// reliability of bit i counted from the MSB.
pub fn hamming15_decode_soft(r: u32, w: &[f32]) -> Fec {
    static CW: OnceLock<Vec<u16>> = OnceLock::new();
    let r = r & 0x7fff;
    // Already a codeword: it is its own nearest, at zero cost.
    if hamming15_syn(r) == 0 {
        return Fec { data: r >> 4, errs: 0, cost: 0.0 };
    }
    let cws = CW.get_or_init(|| (0..2048).map(|d| hamming15_encode(d) as u16).collect());
    let (mut best, mut best_d) = (f32::INFINITY, 0u32);
    for (d, &c) in cws.iter().enumerate() {
        let cost = weight(c as u32 ^ r, w, 15);
        if cost < best {
            best = cost;
            best_d = d as u32;
        }
    }
    Fec { data: best_d, errs: (cws[best_d as usize] as u32 ^ r).count_ones() as i32, cost: best }
}

/// Golay(23,12), Chase-II: flip every subset of the 5 least reliable bits,
/// hard-decode each, keep the lowest-cost codeword.
pub fn golay23_decode_soft(r: u32, w: &[f32]) -> Fec {
    let r = r & 0x7fffff;
    if golay23_syn(r) == 0 {
        return Fec { data: r >> 11, errs: 0, cost: 0.0 };
    }
    let mut idx: [usize; 23] = std::array::from_fn(|i| i);
    idx.select_nth_unstable_by(5, |&a, &b| w[a].total_cmp(&w[b]));
    let lrb = &idx[..5];
    let (mut best, mut best_cw) = (f32::INFINITY, r);
    for m in 0..32u32 {
        let mut t = r;
        for (k, &i) in lrb.iter().enumerate() {
            if m & (1 << k) != 0 {
                t ^= 1 << (22 - i);
            }
        }
        let c = golay23_encode(golay23_decode(t).data);
        let cost = weight((c ^ r) & 0x7fffff, w, 23);
        if cost < best {
            best = cost;
            best_cw = c;
        }
    }
    Fec { data: best_cw >> 11, errs: ((best_cw ^ r) & 0x7fffff).count_ones() as i32, cost: best }
}

/// op25_hamming.h hmg1063EncTbl / hmg1063Dec: the corrected 6-bit hexbit.
pub fn hamming1063_decode(data6: u32, parity4: u32) -> u8 {
    const ENC: [u8; 64] = [
        0, 12, 3, 15, 7, 11, 4, 8, 11, 7, 8, 4, 12, 0, 15, 3, 13, 1, 14, 2, 10, 6, 9, 5, 6, 10, 5, 9, 1, 13, 2, 14, 14, 2, 13, 1, 9, 5, 10,
        6, 5, 9, 6, 10, 2, 14, 1, 13, 3, 15, 0, 12, 4, 8, 7, 11, 8, 4, 11, 7, 15, 3, 12, 0,
    ];
    const DEC: [u8; 16] = [0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0, 8, 1, 16, 32, 0];
    ((data6 as u8) ^ DEC[(ENC[(data6 & 63) as usize] ^ (parity4 & 15) as u8) as usize]) & 63
}

// ── Reed-Solomon over GF(64): primitive 0x43, first root α¹ (ezpwd) ──────────

struct Gf {
    exp: [u8; 126],
    log: [i16; 64],
}
fn gf() -> &'static Gf {
    static G: OnceLock<Gf> = OnceLock::new();
    G.get_or_init(|| {
        let mut g = Gf { exp: [0; 126], log: [-1; 64] };
        let mut x = 1u32;
        for i in 0..63 {
            g.exp[i] = x as u8;
            g.log[x as usize] = i as i16;
            x <<= 1;
            if x & 64 != 0 {
                x ^= 0x43;
            }
        }
        for i in 63..126 {
            g.exp[i] = g.exp[i - 63];
        }
        g
    })
}
#[inline]
fn gmul(a: i32, b: i32) -> i32 {
    if a == 0 || b == 0 {
        0
    } else {
        let g = gf();
        g.exp[(g.log[a as usize] + g.log[b as usize]) as usize] as i32
    }
}
#[inline]
fn gdiv(a: i32, b: i32) -> i32 {
    if a == 0 {
        0
    } else {
        let g = gf();
        g.exp[((g.log[a as usize] - g.log[b as usize] + 63) % 63) as usize] as i32
    }
}
#[inline]
fn gpow(e: i32) -> i32 {
    gf().exp[e.rem_euclid(63) as usize] as i32
}

/// Decode a 63-symbol codeword in place (data first, `nroots` parity last).
/// A correction below `shortened_below` (a shortened code's zero padding)
/// means a wrong codeword. Returns symbols corrected, or −1.
pub fn rs_decode(cw: &mut [u8; 63], nroots: usize, shortened_below: usize) -> i32 {
    rs_decode_erasures(cw, nroots, &[], shortened_below)
}

/// [`rs_decode`] with known-bad positions (`erasures`): errors-and-erasures
/// Berlekamp–Massey. The count returned includes the erasures corrected.
pub fn rs_decode_erasures(cw: &mut [u8; 63], nroots: usize, erasures: &[usize], shortened_below: usize) -> i32 {
    let n = 63i32;
    let s: Vec<i32> = (0..nroots)
        .map(|j| {
            let a = gpow(j as i32 + 1);
            cw.iter().fold(0, |acc, &c| gmul(acc, a) ^ c as i32)
        })
        .collect();
    if s.iter().all(|&v| v == 0) {
        return 0;
    }
    let ne = erasures.len();
    if ne > nroots {
        return -1;
    }
    // Erasure locator Γ(x) = Π (1 − X_k x), X_k = α^(n−1−p).
    let mut lambda = vec![0i32; nroots + 1];
    lambda[0] = 1;
    for &p in erasures {
        let x = gpow(n - 1 - p as i32);
        for j in (1..=nroots).rev() {
            lambda[j] ^= gmul(lambda[j - 1], x);
        }
    }
    let mut b_poly = lambda.clone();
    let (mut l, mut m, mut b) = (ne, 1usize, 1i32);
    for r in ne..nroots {
        let mut d = s[r];
        for i in 1..=l {
            d ^= gmul(lambda[i], s[r - i]);
        }
        if d == 0 {
            m += 1;
            continue;
        }
        let t = lambda.clone();
        let coef = gdiv(d, b);
        for i in m..=nroots {
            lambda[i] ^= gmul(coef, b_poly[i - m]);
        }
        if 2 * l <= r + ne {
            l = r + 1 + ne - l;
            b_poly = t;
            b = d;
            m = 1;
        } else {
            m += 1;
        }
    }
    let deg = (0..=nroots).rev().find(|&i| lambda[i] != 0).unwrap_or(0);
    if deg == 0 {
        return -1;
    }
    let locs: Vec<usize> = (0..63usize)
        .filter(|&p| {
            let xinv = gpow(-(n - 1 - p as i32));
            let (mut v, mut xp) = (0, 1);
            for &c in &lambda[..=deg] {
                v ^= gmul(c, xp);
                xp = gmul(xp, xinv);
            }
            v == 0
        })
        .collect();
    if locs.len() != deg {
        return -1;
    }
    let omega: Vec<i32> = (0..nroots)
        .map(|i| (0..=i.min(deg)).fold(0, |v, j| v ^ gmul(lambda[j], s[i - j])))
        .collect();
    for &p in &locs {
        if p < shortened_below {
            return -1;
        }
        let x = gpow(n - 1 - p as i32);
        let xinv = gdiv(1, x);
        let (mut num, mut xp) = (0, 1);
        for &o in &omega {
            num ^= gmul(o, xp);
            xp = gmul(xp, xinv);
        }
        let mut den = 0;
        for i in (1..=deg).step_by(2) {
            den ^= gmul(lambda[i], gpow(-(n - 1 - p as i32) * (i as i32 - 1)));
        }
        if den == 0 {
            return -1;
        }
        cw[p] ^= gdiv(num, den) as u8;
    }
    locs.len() as i32
}
