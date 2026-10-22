//! TSDU → TSBKs: block de-interleave + 1/2-rate trellis decode and the TSBK
//! CRC-16 (op25 p25p1_fdma.cc, after wireshark packet-p25cai.c).
//!
//! [`trellis_viterbi`] replaces op25's greedy walk, which gives up on a tie
//! (and then drops the rest of the TSDU): with soft bits it takes simulcast
//! control channels from 62 % to 98–99 % of TSBKs.

use super::frame::{Frame, TSDU};
use crate::bits::crc_ccitt_bytes;

pub type Tsbk = [u8; 12];

/// op25's 196-entry table: 12 rows of four 4-bit groups at 4g, 52+4g, 100+4g,
/// 148+4g, then the flush group 48..51.
static TB: [u8; 196] = {
    let mut t = [0u8; 196];
    let mut k = 0;
    let mut g = 0;
    while g < 12 {
        let bases = [0u8, 52, 100, 148];
        let mut bi = 0;
        while bi < 4 {
            let mut e = 0;
            while e < 4 {
                t[k] = bases[bi] + 4 * g as u8 + e as u8;
                k += 1;
                e += 1;
            }
            bi += 1;
        }
        g += 1;
    }
    let mut e = 0;
    while e < 4 {
        t[k] = 48 + e as u8;
        k += 1;
        e += 1;
    }
    t
};
static NEXT: [[u8; 4]; 4] = [[0x2, 0xc, 0x1, 0xf], [0xe, 0x0, 0xd, 0x3], [0x9, 0x7, 0xa, 0x4], [0x5, 0xb, 0x6, 0x8]];

/// op25's greedy decoder: `None` on a tie.
pub fn trellis_greedy(bits: &[u8]) -> Option<Tsbk> {
    let mut out = [0u8; 12];
    let mut state = 0usize;
    for b in (0..196).step_by(4) {
        let cw = (bits[TB[b] as usize] << 3) | (bits[TB[b + 1] as usize] << 2) | (bits[TB[b + 2] as usize] << 1) | bits[TB[b + 3] as usize];
        let (mut best, mut min, mut unique) = (0usize, 99u32, true);
        for j in 0..4 {
            let hd = (cw ^ NEXT[state][j]).count_ones();
            if hd < min {
                min = hd;
                best = j;
                unique = true;
            } else if hd == min {
                unique = false;
            }
        }
        if !unique {
            return None;
        }
        state = best;
        let d = b >> 2;
        if d < 48 {
            out[d >> 2] |= (state << (6 - (d % 4) * 2)) as u8;
        }
    }
    Some(out)
}

/// Viterbi decode over the 4-state trellis (49 steps, the last a flush to 0).
/// `soft` (optional, parallel to `bits`) weights each bit; none = Hamming.
pub fn trellis_viterbi(bits: &[u8], soft: Option<&[f32]>) -> Tsbk {
    const INF: f32 = f32::INFINITY;
    let mut pm = [0.0, INF, INF, INF];
    let mut from = [[0u8; 4]; 49];
    for d in 0..49 {
        let (mut c0, mut c1) = ([0f32; 4], [0f32; 4]);
        for k in 0..4 {
            let idx = TB[d * 4 + k] as usize;
            let w = soft.map_or(1.0, |s| s[idx]);
            if bits[idx] != 0 {
                c0[k] = w;
            } else {
                c1[k] = w;
            }
        }
        let mut nm = [INF; 4];
        for s in 0..4 {
            if pm[s] == INF {
                continue;
            }
            for j in 0..4 {
                if d == 48 && j != 0 {
                    continue;
                }
                let cw = NEXT[s][j];
                let m = pm[s] + (0..4).map(|k| if (cw >> (3 - k)) & 1 != 0 { c1[k] } else { c0[k] }).sum::<f32>();
                if m < nm[j] {
                    nm[j] = m;
                    from[d][j] = s as u8;
                }
            }
        }
        pm = nm;
    }
    let mut out = [0u8; 12];
    let mut state = 0usize;
    for d in (0..49).rev() {
        if d < 48 {
            out[d >> 2] |= (state << (6 - (d % 4) * 2)) as u8;
        }
        state = from[d][state] as usize;
    }
    out
}

/// List Viterbi (the parallel algorithm): the `k` best paths into each
/// state at every step, and so the `k` best whole decodes, best first, each
/// with its path metric. The first is [`trellis_viterbi`]'s (ties go the
/// same way).
pub fn trellis_list(bits: &[u8], soft: Option<&[f32]>, k: usize) -> Vec<(Tsbk, f32)> {
    // Per state: its paths' (metric, previous state, that state's rank), best first.
    let mut pm: [Vec<(f32, u8, u8)>; 4] = [vec![(0.0, 0, 0)], vec![], vec![], vec![]];
    let mut back: Vec<[Vec<(u8, u8)>; 4]> = Vec::with_capacity(49);
    for d in 0..49 {
        let (mut c0, mut c1) = ([0f32; 4], [0f32; 4]);
        for b in 0..4 {
            let idx = TB[d * 4 + b] as usize;
            let w = soft.map_or(1.0, |s| s[idx]);
            if bits[idx] != 0 {
                c0[b] = w;
            } else {
                c1[b] = w;
            }
        }
        let mut next: [Vec<(f32, u8, u8)>; 4] = Default::default();
        for (j, nj) in next.iter_mut().enumerate() {
            if d == 48 && j != 0 {
                continue;
            }
            for s in 0..4 {
                let cw = NEXT[s][j];
                let br: f32 = (0..4).map(|b| if (cw >> (3 - b)) & 1 != 0 { c1[b] } else { c0[b] }).sum();
                nj.extend(pm[s].iter().enumerate().map(|(r, p)| (p.0 + br, s as u8, r as u8)));
            }
            // Stable: equal metrics keep the lower state first, as trellis_viterbi's strict < does.
            nj.sort_by(|a, b| a.0.total_cmp(&b.0));
            nj.truncate(k);
        }
        back.push(std::array::from_fn(|j| next[j].iter().map(|p| (p.1, p.2)).collect()));
        pm = next;
    }
    pm[0]
        .iter()
        .enumerate()
        .map(|(rank, p)| {
            let mut out = [0u8; 12];
            let (mut state, mut r) = (0usize, rank);
            for d in (0..49).rev() {
                if d < 48 {
                    out[d >> 2] |= (state << (6 - (d % 4) * 2)) as u8;
                }
                let (ps, pr) = back[d][state][r];
                (state, r) = (ps as usize, pr as usize);
            }
            (out, p.0)
        })
        .collect()
}

/// The block's last 16 bits are the CRC-CCITT of the 80 before them.
pub fn tsbk_ok(t: &Tsbk) -> bool {
    crc_ccitt_bytes(&t[..10]) == u16::from_be_bytes([t[10], t[11]])
}
pub fn tsbk_last(t: &Tsbk) -> bool {
    t[0] >> 7 != 0
}
pub fn tsbk_opcode(t: &Tsbk) -> u8 {
    t[0] & 0x3f
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trellis {
    Greedy,
    Viterbi,
}

#[derive(Default, Debug)]
pub struct TsduResult {
    pub good: Vec<Tsbk>,
    pub bad: usize,
    pub trellis_fail: usize,
    pub crc_fail: usize,
    /// Blocks never tried after a greedy trellis failure.
    pub blocks_skipped: usize,
}

/// Every block of a TSDU frame. Greedy: a trellis failure ends the frame.
/// Viterbi: every block is decoded and the CRC alone judges. Either way the
/// last-block flag stops.
pub fn decode_tsdu(f: &Frame, mode: Trellis) -> TsduResult {
    let mut r = TsduResult::default();
    if f.nid.duid != TSDU || f.bits.len() < 112 {
        return r;
    }
    let blocks = ((f.bits.len() - 112) / 196).min(3);
    for b in 0..blocks {
        let at = 112 + b * 196;
        let bits = &f.bits[at..at + 196];
        let t = match mode {
            Trellis::Viterbi => trellis_viterbi(bits, (!f.soft.is_empty()).then(|| &f.soft[at..at + 196])),
            Trellis::Greedy => match trellis_greedy(bits) {
                Some(t) => t,
                None => {
                    r.bad += 1;
                    r.trellis_fail += 1;
                    r.blocks_skipped = blocks - b - 1;
                    break;
                }
            },
        };
        if !tsbk_ok(&t) {
            r.bad += 1;
            r.crc_fail += 1;
            continue;
        }
        r.good.push(t);
        if tsbk_last(&t) {
            break;
        }
    }
    r
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::mbt::tests::encode_block;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
        fn gauss(&mut self) -> f64 {
            let (a, b) = (self.unit().max(1e-300), self.unit());
            (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()
        }
        fn tsbk(&mut self) -> Tsbk {
            let mut t = [0u8; 12];
            for b in &mut t[..10] {
                *b = self.next() as u8;
            }
            let c = crc_ccitt_bytes(&t[..10]);
            t[10..12].copy_from_slice(&c.to_be_bytes());
            t
        }
    }

    /// A block through antipodal bits in Gaussian noise of deviation `sd`:
    /// hard bits and their reliabilities |y|.
    fn noisy(r: &mut Rng, t: &Tsbk, sd: f64) -> (Vec<u8>, Vec<f32>) {
        let (mut bits, mut soft) = (Vec::new(), Vec::new());
        for b in encode_block(t) {
            let y = if b != 0 { 1.0 } else { -1.0 } + sd * r.gauss();
            bits.push((y > 0.0) as u8);
            soft.push(y.abs() as f32);
        }
        (bits, soft)
    }

    #[test]
    fn list_viterbi_starts_with_viterbi() {
        let mut r = Rng(0x2545_f491_4f6c_dd1d);
        for _ in 0..2000 {
            let t = r.tsbk();
            let (bits, soft) = noisy(&mut r, &t, 0.9);
            let l = trellis_list(&bits, Some(&soft), 8);
            assert_eq!(l[0].0, trellis_viterbi(&bits, Some(&soft)));
            assert!(l.windows(2).all(|w| w[0].1 <= w[1].1));
            assert_eq!(trellis_list(&bits, None, 4)[0].0, trellis_viterbi(&bits, None));
        }
    }

    /// `cargo test -p trunk-core --release list_viterbi_false_accepts -- --ignored --nocapture`:
    /// per noise level, of the blocks whose best path fails its CRC, how many
    /// a list of K finds right, and how many it passes wrong (CRC-16 by
    /// chance) — the false-accept rate list decoding costs.
    #[test]
    #[ignore]
    fn list_viterbi_false_accepts() {
        let mut r = Rng(0x9e37_79b9_7f4a_7c15);
        let n = 200_000;
        for sd in [0.9, 1.0] {
            let mut failed = 0u64;
            let mut right = [0u64; 3];
            let mut wrong = [0u64; 3];
            let mut tried = [0u64; 3];
            let mut dist = Vec::new();
            for _ in 0..n {
                let t = r.tsbk();
                let (bits, soft) = noisy(&mut r, &t, sd);
                let l = trellis_list(&bits, Some(&soft), 64);
                if tsbk_ok(&l[0].0) {
                    continue;
                }
                failed += 1;
                for (i, k) in [8usize, 32, 64].into_iter().enumerate() {
                    for c in l.iter().take(k).skip(1) {
                        tried[i] += 1;
                        if tsbk_ok(&c.0) {
                            if c.0 == t {
                                right[i] += 1;
                            } else {
                                wrong[i] += 1;
                                if k == 32 {
                                    let d: u32 = c.0.iter().zip(&t).map(|(a, b)| (a ^ b).count_ones()).sum();
                                    let head = c.0[..2] == t[..2];
                                    dist.push((d, head));
                                }
                            }
                            break;
                        }
                    }
                }
            }
            println!("sd {sd}: {failed} of {n} blocks fail the best path's CRC");
            dist.sort();
            println!("  K 32 wrong: bits from the block sent {:?}; opcode + MFID octets kept in {} of {}", dist.iter().map(|d| d.0).collect::<Vec<_>>(), dist.iter().filter(|d| d.1).count(), dist.len());
            for (i, k) in [8, 32, 64].into_iter().enumerate() {
                println!(
                    "  K {k:>2}: {:>5.1} % of them found right, {} wrong ({:.2e} per failed block, {:.2e} per candidate tried)",
                    100.0 * right[i] as f64 / failed as f64,
                    wrong[i],
                    wrong[i] as f64 / failed as f64,
                    wrong[i] as f64 / tried[i] as f64
                );
            }
        }
    }
}
