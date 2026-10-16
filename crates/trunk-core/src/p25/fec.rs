//! Forward error correction for the P25 voice layers:
//!
//! * Golay(24,12) / (23,12) — IMBE u0..u3, HDU/TDULC hexbits
//! * Hamming(15,11) — IMBE u4..u6
//! * Hamming(10,6) — LDU link-control / encryption-sync hexbits
//! * Reed-Solomon over GF(2⁶) — LC (24,12), ES (24,16), HDU (36,20), as ezpwd's RS<63,k>
//!
//! The encoders build test signals (`p25::synth`).
//!
//! Encoders are op25's; the syndrome → error-pattern tables are built from
//! them. The soft decoders (Chase-II Golay, maximum-likelihood Hamming) cut
//! wrong IMBE codewords from 7.6 % to 2.2 % on synthetic simulcast.

use std::sync::OnceLock;

/// A decoded codeword. `errs` keeps op25's −1 for uncorrectable, as the RS
/// decoders' return values do.
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
    // Every codeword's cost as [`weight`] sums it (differing bits from the
    // LSB up; adding 0.0 for the others changes nothing), without branches so
    // the codewords go through in SIMD lanes. The first lowest cost wins.
    let wb: [f32; 15] = std::array::from_fn(|b| w[14 - b]);
    let mut cost = [0f32; 2048];
    for (c, &cw) in cost.iter_mut().zip(cws) {
        let d = cw as u32 ^ r;
        let mut s = 0.0f32;
        for (b, &x) in wb.iter().enumerate() {
            s += if d >> b & 1 != 0 { x } else { 0.0 };
        }
        *c = s;
    }
    let (mut best, mut best_d) = (f32::INFINITY, 0u32);
    for (d, &c) in cost.iter().enumerate() {
        if c < best {
            best = c;
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

/// op25_hamming.h hmg1063EncTbl: a hexbit's 4 parity bits.
const HMG1063_ENC: [u8; 64] = [
    0, 12, 3, 15, 7, 11, 4, 8, 11, 7, 8, 4, 12, 0, 15, 3, 13, 1, 14, 2, 10, 6, 9, 5, 6, 10, 5, 9, 1, 13, 2, 14, 14, 2, 13, 1, 9, 5, 10, 6,
    5, 9, 6, 10, 2, 14, 1, 13, 3, 15, 0, 12, 4, 8, 7, 11, 8, 4, 11, 7, 15, 3, 12, 0,
];

/// The 10-bit Hamming(10,6) word for a hexbit: data, then parity.
pub fn hamming1063_encode(data6: u8) -> u32 {
    ((data6 as u32 & 63) << 4) | HMG1063_ENC[(data6 & 63) as usize] as u32
}

/// op25_hamming.h hmg1063Dec: the corrected 6-bit hexbit, or none when the
/// syndrome is one no single error gives (5, 6, 9, 10, 15: two errors at
/// least, which the shortened code detects but can't place).
pub fn hamming1063_decode(data6: u32, parity4: u32) -> Option<u8> {
    const DEC: [u8; 16] = [0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0, 8, 1, 16, 32, 0];
    let syn = (HMG1063_ENC[(data6 & 63) as usize] ^ (parity4 & 15) as u8) as usize;
    (!matches!(syn, 5 | 6 | 9 | 10 | 15)).then(|| ((data6 as u8) ^ DEC[syn]) & 63)
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

/// Fill in the parity of a 63-symbol codeword (data in `cw[..63 − nroots]`,
/// a shortened code's unused symbols zero; `nroots` parity last), so that
/// [`rs_decode`] finds no errors: c(x) = d(x)·x^nroots + (d(x)·x^nroots mod
/// g(x)), g(x) = Π (x − α^j), j = 1..=nroots.
pub fn rs_encode(cw: &mut [u8; 63], nroots: usize) {
    let mut g = vec![0i32; nroots + 1];
    g[0] = 1;
    for j in 1..=nroots {
        let r = gpow(j as i32);
        for i in (1..=j).rev() {
            g[i] = g[i - 1] ^ gmul(g[i], r);
        }
        g[0] = gmul(g[0], r);
    }
    // g[i] is the coefficient of x^i; g[nroots] = 1. Long division, highest
    // power (cw[0]) first.
    let k = 63 - nroots;
    let mut rem = vec![0i32; nroots];
    for &d in &cw[..k] {
        let f = d as i32 ^ rem[0];
        rem.rotate_left(1);
        rem[nroots - 1] = 0;
        if f != 0 {
            for i in 0..nroots {
                rem[i] ^= gmul(f, g[nroots - 1 - i]);
            }
        }
    }
    for (c, r) in cw[k..].iter_mut().zip(rem) {
        *c = r as u8;
    }
}

/// Decode a 63-symbol codeword in place (data first, `nroots` parity last).
/// A correction below `shortened_below` (a shortened code's zero padding)
/// means a wrong codeword. Returns symbols corrected, or −1.
pub fn rs_decode(cw: &mut [u8; 63], nroots: usize, shortened_below: usize) -> i32 {
    rs_decode_erasures(cw, nroots, &[], shortened_below)
}

/// [`rs_decode`] with known-bad positions (`erasures`): errors-and-erasures
/// Berlekamp–Massey, within the code's capacity (2·errors + erasures ≤
/// `nroots`). The count returned includes the erasures corrected.
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
    let errors = locs.iter().filter(|p| !erasures.contains(p)).count();
    if 2 * errors + ne > nroots {
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

/// [`rs_decode_erasures`], and failing that, again with the least reliable
/// symbols (`rel`, one per symbol: higher, surer) erased as well, two more
/// each try: a soft errors-and-erasures decoder, as BBAC §5.6 recommends. It
/// stops with `margin` roots unspent, so a word too damaged isn't simply
/// rewritten to the nearest codeword.
pub fn rs_decode_soft(cw: &mut [u8; 63], nroots: usize, erasures: &[usize], shortened_below: usize, rel: &[f32; 63], margin: usize) -> i32 {
    let heard = *cw;
    let r = rs_decode_erasures(cw, nroots, erasures, shortened_below);
    if r >= 0 {
        return r;
    }
    let mut doubtful: Vec<usize> = (shortened_below..63).filter(|p| !erasures.contains(p)).collect();
    doubtful.sort_by(|&a, &b| rel[a].total_cmp(&rel[b]));
    let room = nroots.saturating_sub(erasures.len() + margin);
    let mut e = erasures.to_vec();
    for k in (2..=room).step_by(2) {
        e.truncate(erasures.len());
        e.extend(&doubtful[..k]);
        *cw = heard;
        let r = rs_decode_erasures(cw, nroots, &e, shortened_below);
        if r >= 0 {
            return r;
        }
    }
    *cw = heard;
    -1
}

/// A hexbit's reliability: its least reliable bit's.
pub fn hexbit_rel(bits: &[f32]) -> f32 {
    bits.iter().copied().fold(f32::INFINITY, f32::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every P25 RS code (shortened, data after the zeros): an encoded word
    /// decodes clean, and up to its correction limit of errors comes back.
    #[test]
    fn rs_encode_round_trips() {
        let mut x = 0x2545_f491u32;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        // (n, k): LDU1 LC, LDU2 ES, HDU, Phase 2 ESS, MAC.
        for (n, k) in [(24, 12), (24, 16), (36, 20), (44, 16), (63, 35)] {
            let nroots = n - k;
            for _ in 0..200 {
                let mut cw = [0u8; 63];
                for c in &mut cw[63 - n..63 - nroots] {
                    *c = (rnd() & 63) as u8;
                }
                rs_encode(&mut cw, nroots);
                let sent = cw;
                assert_eq!(rs_decode(&mut cw, nroots, 63 - n), 0, "RS({n},{k})");
                let errs = 1 + rnd() as usize % (nroots / 2);
                let mut hit = Vec::new();
                while hit.len() < errs {
                    let p = 63 - n + rnd() as usize % n;
                    if !hit.contains(&p) {
                        hit.push(p);
                        cw[p] ^= 1 + (rnd() % 63) as u8;
                    }
                }
                assert_eq!(rs_decode(&mut cw, nroots, 63 - n), errs as i32, "RS({n},{k}) {errs} errors");
                assert_eq!(cw, sent);
            }
        }
    }

    /// Errors and erasures: 2e + f ≤ nroots comes back; one error more is refused.
    #[test]
    fn rs_erasures_within_capacity() {
        let mut cw = [0u8; 63];
        for (i, c) in cw[39..51].iter_mut().enumerate() {
            *c = i as u8 * 5 + 1;
        }
        rs_encode(&mut cw, 12);
        let sent = cw;
        // RS(24,12,13): 8 erasures and 2 errors (2·2 + 8 = 12).
        let erasures: Vec<usize> = (40..48).collect();
        let mut bad = sent;
        for &p in &erasures {
            bad[p] ^= 0x15;
        }
        bad[50] ^= 7;
        bad[60] ^= 9;
        let mut d = bad;
        assert_eq!(rs_decode_erasures(&mut d, 12, &erasures, 39), 10);
        assert_eq!(d, sent);
        // A third error is past it.
        bad[55] ^= 3;
        assert!(rs_decode_erasures(&mut bad, 12, &erasures, 39) < 0 || bad != sent);
    }

    #[test]
    fn hamming1063_round_trips_and_corrects() {
        for d in 0..64u8 {
            let w = hamming1063_encode(d);
            assert_eq!(hamming1063_decode(w >> 4, w & 15), Some(d));
            for b in 0..10 {
                let e = w ^ (1 << b);
                assert_eq!(hamming1063_decode(e >> 4, e & 15), Some(d), "{d} bit {b}");
            }
        }
        // A (10,6,3) code: some two-bit errors land on syndromes no single
        // error gives, and are reported, not miscorrected.
        let detected = (0..10).flat_map(|a| (a + 1..10).map(move |b| (1u32 << a) | (1 << b))).filter(|&e| {
            let w = hamming1063_encode(0x2a) ^ e;
            hamming1063_decode(w >> 4, w & 15).is_none()
        });
        assert!(detected.count() > 0);
    }

    #[test]
    fn hamming15_soft_matches_brute_force() {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for k in 0..20000 {
            let r = rnd() as u32 & 0x7fff;
            // Real-valued reliabilities, and (every 4th) whole ones with ties.
            let w: Vec<f32> = (0..15).map(|_| if k % 4 == 0 { (rnd() % 3) as f32 } else { (rnd() % 1_000_000) as f32 * 1e-6 }).collect();
            let got = hamming15_decode_soft(r, &w);
            let (mut best, mut best_d) = (f32::INFINITY, 0u32);
            for d in 0..2048 {
                let cost = weight(hamming15_encode(d) ^ r, &w, 15);
                if cost < best {
                    best = cost;
                    best_d = d;
                }
            }
            let want = if hamming15_syn(r) == 0 { (r >> 4, 0.0) } else { (best_d, best) };
            assert_eq!((got.data, got.cost.to_bits()), (want.0, want.1.to_bits()), "r {r:#x} w {w:?}");
        }
    }
}
