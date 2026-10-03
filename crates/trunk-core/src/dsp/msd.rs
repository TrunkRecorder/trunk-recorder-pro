//! Multi-symbol noncoherent detection for 4-level CPFSK (P25 C4FM, DMR):
//! each symbol re-decided from the channel IQ instead of the discriminator.
//!
//! ```text
//! C4FM receiver → symbol instants, levels (Hz), first decisions
//! for symbol k: the frequency over symbols k−1..k+1 is every nearby symbol's
//!   transmit pulse — the three in the window per hypothesis (64), the rest
//!   from decisions (earlier ones re-decided here, later ones the receiver's)
//!   → correlate the IQ with each hypothesis' phase, |Σ|² → best per level of k
//! ```
//!
//! Below the discriminator's threshold (where noise turns into clicks) the
//! correlation over three symbols holds on: at half the codewords decoded,
//! 2.8 dB less signal on P25 C4FM voice, 1.5 dB on DMR voice and 2.2 dB on
//! DMR data blocks (`tool snr`). The phase is a sum of each symbol's part, so
//! the correlations share products, and the four levels' rotations come from
//! one. Cost: ~0.5 % (DMR) to ~1 % (P25) of a core per channel while it runs;
//! the receiver switches it off on a clean signal.

use std::collections::VecDeque;
use std::f64::consts::PI;

use num_complex::Complex32;

use super::c4fm::SYMBOL_RATE;
use super::filters;
use super::Symbol;

/// Symbols either side whose pulse tails reach the window.
const SPAN: usize = 4;
/// Pulse table resolution, per sample.
const OVERSAMPLE: f64 = 16.0;

/// A transmitter's frequency pulse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pulse {
    /// Root raised cosine (DMR: α 0.2).
    Rrc(f64),
    /// Raised cosine (P25 C4FM: α 0.2).
    Rc(f64),
}

fn rc_at(t: f64, a: f64) -> f64 {
    let s = if t.abs() < 1e-9 { 1.0 } else { (PI * t).sin() / (PI * t) };
    let d = 1.0 - (2.0 * a * t).powi(2);
    if d.abs() < 1e-9 {
        PI / 4.0 * s
    } else {
        s * (PI * a * t).cos() / d
    }
}

impl Pulse {
    fn at(self, t: f64) -> f64 {
        match self {
            Pulse::Rrc(a) => filters::rrc(t, a),
            Pulse::Rc(a) => rc_at(t, a),
        }
    }
}

/// Levels −3, −1, +1, +3 ↔ dibits.
const DIBIT: [u8; 4] = [0b11, 0b10, 0b00, 0b01];
fn level_index(d: u8) -> usize {
    match d & 3 {
        0b11 => 0,
        0b10 => 1,
        0b00 => 2,
        _ => 3,
    }
}

pub struct Msd {
    /// Take the window's first symbol as decided (16 hypotheses instead of 64).
    feedback: bool,
    sps: f64,
    /// Radians per sample per Hz.
    w: f32,
    /// The pulse (scaled so a lone symbol reads its level after the
    /// receiver's filter), every 1/OVERSAMPLE sample from −(SPAN+1) symbols.
    table: Vec<f32>,
    half: f64,
    /// Channel IQ from sample `iq_base` on.
    iq: VecDeque<Complex32>,
    iq_base: u64,
    iq_end: u64,
    /// Symbols waiting (the receiver's decisions) with the levels then, and
    /// the last SPAN re-decided ones before them.
    pending: VecDeque<(Symbol, [f32; 4])>,
    done: VecDeque<(Symbol, [f32; 4])>,
    // Scratch, as separate re/im arrays padded with zeros to a multiple of
    // LANES so the correlations vectorise.
    fixed: Vec<(f64, f32)>,
    /// The fixed symbols' frequency, then the phase to remove from the IQ.
    f: Vec<f32>,
    /// The window symbols' pulses, then their rotation angles.
    pq: [Vec<f32>; 3],
    /// The IQ with the centre and fixed symbols' phase removed.
    y: [Vec<f32>; 2],
    /// Per window symbol q: u = e^{−j·o3·cq} and u³ as [u.re, u.im, u³.re, u³.im].
    e: [[Vec<f32>; 4]; 3],
    /// y times the first window symbol's hypothesis.
    y0: [Vec<f32>; 2],
}

/// SIMD width the correlation loops are written for (NEON / SSE: 4 × f32).
const LANES: usize = 4;

impl Msd {
    /// `rx_alpha`: the roll-off of the receiver's RRC filter whose output the
    /// levels are in. `feedback`: take the window's first symbol as already
    /// decided (16 hypotheses, not 64): better on DMR voice, ~0.8 dB worse on
    /// P25 C4FM (`tool snr`).
    pub fn new(rate: f64, tx: Pulse, rx_alpha: f64, feedback: bool) -> Self {
        let sps = rate / SYMBOL_RATE;
        let n = (8.0 * sps).round() as i64 | 1;
        let m = n / 2;
        let rx: Vec<f64> = (-m..=m).map(|i| filters::rrc(i as f64 / sps, rx_alpha)).collect();
        let sum: f64 = rx.iter().sum();
        let k_norm: f64 = (-m..=m).map(|i| tx.at(i as f64 / sps) * rx[(i + m) as usize] / sum).sum();
        let half = (SPAN + 1) as f64 * sps;
        let len = (2.0 * half * OVERSAMPLE).ceil() as usize + 2;
        let table = (0..len).map(|i| (tx.at((i as f64 / OVERSAMPLE - half) / sps) / k_norm) as f32).collect();
        Msd {
            feedback,
            sps,
            w: (2.0 * PI / rate) as f32,
            table,
            half,
            iq: VecDeque::new(),
            iq_base: 0,
            iq_end: 0,
            pending: VecDeque::new(),
            done: VecDeque::new(),
            fixed: Vec::new(),
            f: Vec::new(),
            pq: Default::default(),
            y: Default::default(),
            e: Default::default(),
            y0: Default::default(),
        }
    }

    /// The pulse `dt` samples from its symbol's instant (for one sample;
    /// [`Msd::add_pulse`] for a window).
    #[cfg(test)]
    fn p(&self, dt: f64) -> f32 {
        let x = (dt + self.half) * OVERSAMPLE;
        if x < 0.0 || x as usize + 1 >= self.table.len() {
            return 0.0;
        }
        let i = x as usize;
        let f = (x - i as f64) as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * f
    }

    /// Channel IQ (in order, every sample) and the receiver's symbols decided
    /// from it so far, with its levels; re-decided symbols go to `out`, SPAN
    /// symbols behind.
    pub fn push(&mut self, iq: &[Complex32], syms: &[Symbol], levels: [f32; 4], out: &mut Vec<Symbol>) {
        self.iq.extend(iq.iter().copied());
        self.iq_end += iq.len() as u64;
        self.pending.extend(syms.iter().map(|s| (*s, levels)));
        while self.pending.len() > SPAN + 1 {
            let (s, lv) = self.pending[0];
            // The window must be in the buffer (the receiver's latency is ~100 ms, so it is).
            let end = s.sample + 1.5 * self.sps + 1.0;
            if end >= self.iq_end as f64 {
                break;
            }
            let r = self.decide();
            self.pending.pop_front();
            out.push(r);
            self.done.push_back((r, lv));
            if self.done.len() > SPAN {
                self.done.pop_front();
            }
        }
        // Keep IQ from SPAN+2 symbols before the oldest waiting symbol.
        if let Some((s, _)) = self.done.front().or(self.pending.front()) {
            let keep = (s.sample - 2.0 * self.sps).max(0.0) as u64;
            while self.iq_base < keep && !self.iq.is_empty() {
                self.iq.pop_front();
                self.iq_base += 1;
            }
        }
    }

    /// Hand back the symbols still waiting, as the receiver decided them, and
    /// start afresh (the receiver's decisions are good enough on their own).
    pub fn flush(&mut self, out: &mut Vec<Symbol>) {
        out.extend(self.pending.drain(..).map(|(s, _)| s));
        self.done.clear();
        self.iq.clear();
        self.iq_base = self.iq_end;
    }

    /// Off: count `n` samples of IQ without keeping them (symbol instants are
    /// in samples since the start, so the count must go on).
    pub fn skip(&mut self, n: usize) {
        self.iq_end += n as u64;
        self.iq_base = self.iq_end;
        self.iq.clear();
    }

    /// Add `level` × the pulse of the symbol at `t` to `out[i]`, for samples
    /// n0 + i, interpolating the table. Its position steps by exactly
    /// OVERSAMPLE per sample, so the fraction is worked out once.
    fn add_pulse(&self, t: f64, n0: u64, level: f32, out: &mut [f32]) {
        let x0 = ((n0 as f64 - t) + self.half) * OVERSAMPLE;
        let step = OVERSAMPLE as usize;
        // The samples whose position lies in [0, len − 1): the pulse is zero outside.
        let first = if x0 < 0.0 { ((-x0) / OVERSAMPLE).ceil() as usize } else { 0 };
        let x = x0 + (first as f64) * OVERSAMPLE;
        if first >= out.len() || x as usize + 1 >= self.table.len() {
            return;
        }
        let (mut k, fr) = (x as usize, (x - (x as usize) as f64) as f32);
        for o in &mut out[first..] {
            if k + 1 >= self.table.len() {
                break;
            }
            *o += level * (self.table[k] + (self.table[k + 1] - self.table[k]) * fr);
            k += step;
        }
    }

    /// Re-decide pending[0].
    fn decide(&mut self) -> Symbol {
        let (sk, lv) = self.pending[0];
        let Some(&(prev, _)) = self.done.back() else { return sk };
        let next = self.pending[1].0;
        let (a, b) = (prev.sample - self.sps / 2.0, next.sample + self.sps / 2.0);
        if a < self.iq_base as f64 {
            return sk;
        }
        let (n0, n1) = (a.ceil() as u64, b.floor() as u64);
        let len = (n1 - n0 + 1) as usize;
        let padded = len.div_ceil(LANES) * LANES;
        // The fixed symbols: re-decided before `prev`, the receiver's after `next`.
        let mut fixed = std::mem::take(&mut self.fixed);
        fixed.clear();
        fixed.extend(self.done.iter().take(self.done.len() - 1).map(|(s, l)| (s.sample, l[level_index(s.dibit)])));
        fixed.extend(self.pending.iter().skip(2).take(SPAN).map(|(s, l)| (s.sample, l[level_index(s.dibit)])));
        // Levels are symmetric about the centre c: c ± o/3, c ± o. The centre's
        // share of every hypothesis' phase is the same, so it goes into y with
        // the fixed symbols'; each window symbol then needs one rotation u (at
        // o/3), the others being conj(u), u³ and conj(u³).
        let c = (lv[0] + lv[3]) / 2.0;
        let o3 = (lv[3] - lv[0]) / 6.0;
        let inst = [prev.sample, sk.sample, next.sample];
        let (mut f, mut pq) = (std::mem::take(&mut self.f), std::mem::take(&mut self.pq));
        for v in std::iter::once(&mut f).chain(pq.iter_mut()) {
            v.clear();
            v.resize(padded, 0.0);
        }
        for &(t, l) in &fixed {
            self.add_pulse(t, n0, l, &mut f);
        }
        for (q, v) in pq.iter_mut().enumerate() {
            self.add_pulse(inst[q], n0, 1.0, v);
        }
        self.fixed = fixed;
        // Running phases: the one to remove from the IQ (into f), and each
        // window symbol's rotation angle (into pq). Zero past the window.
        let (mut ph, mut cq) = (0f32, [0f32; 3]);
        for i in 0..len {
            ph += self.w * (f[i] + c * (pq[0][i] + pq[1][i] + pq[2][i]));
            f[i] = -ph;
            for q in 0..3 {
                cq[q] += self.w * pq[q][i];
                pq[q][i] = -o3 * cq[q];
            }
        }
        for v in std::iter::once(&mut f).chain(pq.iter_mut()) {
            v[len..].fill(0.0);
        }
        // y = IQ · e^{−j·ph}; zero padding (y = 0) adds nothing to any correlation.
        let [yr, yi] = &mut self.y;
        sincos(&f, yi, yr);
        let (iq0, iq1) = self.iq.as_slices();
        let k0 = (n0 - self.iq_base) as usize;
        for i in 0..len {
            let k = k0 + i;
            let x = if k < iq0.len() { iq0[k] } else { iq1[k - iq0.len()] };
            let (c, s) = (yr[i], yi[i]);
            (yr[i], yi[i]) = (x.re * c - x.im * s, x.re * s + x.im * c);
        }
        yr[len..].fill(0.0);
        yi[len..].fill(0.0);
        for (q, e) in self.e.iter_mut().enumerate() {
            let [ur, ui, vr, vi] = e;
            sincos(&pq[q], ui, ur);
            for v in [&mut *vr, &mut *vi] {
                v.resize(padded, 0.0);
            }
            for i in 0..padded {
                // u³
                let (a, b) = (ur[i], ui[i]);
                let (a2, b2) = (a * a - b * b, 2.0 * a * b);
                (vr[i], vi[i]) = (a2 * a - b2 * b, a2 * b + b2 * a);
            }
        }
        self.f = f;
        self.pq = pq;
        // Hypothesis (l0, l1, l2) correlates y with e0[l0]·e1[l1]·e2[l2]. Level
        // order −3, −1, +1, +3 is conj(u³), conj(u), u, u³: (rotation, sign of
        // its imaginary part). For z = y·e0·e1 = a + jb and e2's u = c + jd,
        // Σ z·u and Σ z·conj(u) share Σac, Σbd, Σad, Σbc — so one pass of eight
        // real sums gives all four l2.
        const LEVEL: [(usize, f32); 4] = [(2, -1.0), (0, -1.0), (0, 1.0), (2, 1.0)];
        let mut best = [0f32; 4];
        let one = [level_index(prev.dibit)];
        let l0s: &[usize] = if self.feedback { &one } else { &[0, 1, 2, 3] };
        let [e0, e1, e2] = &self.e;
        let [yr, yi] = &self.y;
        for &l0 in l0s {
            let (k, s) = LEVEL[l0];
            let (er, ei) = (&e0[k], &e0[k + 1]);
            let [y0r, y0i] = &mut self.y0;
            y0r.resize(padded, 0.0);
            y0i.resize(padded, 0.0);
            for i in 0..padded {
                let (a, b, c, d) = (yr[i], yi[i], er[i], s * ei[i]);
                y0r[i] = a * c - b * d;
                y0i[i] = a * d + b * c;
            }
            for (l1, best) in best.iter_mut().enumerate() {
                let (k, s) = LEVEL[l1];
                let m = sums(y0r, y0i, &e1[k], &e1[k + 1], s, e2);
                // u: (Σac − Σbd, Σad + Σbc); conj(u): (Σac + Σbd, Σbc − Σad); likewise u³.
                for (ac, bd, ad, bc) in [(m[0], m[1], m[2], m[3]), (m[4], m[5], m[6], m[7])] {
                    *best = best.max((ac - bd) * (ac - bd) + (ad + bc) * (ad + bc)).max((ac + bd) * (ac + bd) + (bc - ad) * (bc - ad));
                }
            }
        }
        let top = best.iter().cloned().fold(0.0, f32::max).max(1e-30);
        let d = (0..4).max_by(|&i, &j| best[i].total_cmp(&best[j])).unwrap();
        let (pos, neg) = (best[2].max(best[3]), best[0].max(best[1]));
        let (outer, inner) = (best[0].max(best[3]), best[1].max(best[2]));
        Symbol { dibit: DIBIT[d], sample: sk.sample, rel_hi: (pos - neg).abs() / top, rel_lo: (outer - inner).abs() / top }
    }
}

/// sin and cos of every angle (|x| up to 10³ rad), ~1e-7 absolute: Cephes'
/// sinf / cosf polynomials after a three-part π/2 reduction, without
/// branches so the loop vectorises. `sin` and `cos` are resized to `x`.
fn sincos(x: &[f32], sin: &mut Vec<f32>, cos: &mut Vec<f32>) {
    const P1: f32 = 1.5703125;
    const P2: f32 = 4.837_513e-4;
    const P3: f32 = 7.549_79e-8;
    sin.resize(x.len(), 0.0);
    cos.resize(x.len(), 0.0);
    for ((&x, s), c) in x.iter().zip(sin.iter_mut()).zip(cos.iter_mut()) {
        let t = x * std::f32::consts::FRAC_2_PI;
        let j = (t + if t >= 0.0 { 0.5 } else { -0.5 }) as i32;
        let jf = j as f32;
        let r = ((x - jf * P1) - jf * P2) - jf * P3;
        let z = r * r;
        let sp = ((-1.951_529_6e-4 * z + 8.332_161e-3) * z - 1.666_665_5e-1) * z * r + r;
        let cp = ((2.443_315_7e-5 * z - 1.388_731_6e-3) * z + 4.166_664_6e-2) * z * z - 0.5 * z + 1.0;
        // Quadrant j: (sin, cos) = (S, C), (C, −S), (−S, −C), (−C, S).
        let (a, b) = if j & 1 == 0 { (sp, cp) } else { (cp, sp) };
        *s = if j & 2 == 0 { a } else { -a };
        *c = if (j + 1) & 2 == 0 { b } else { -b };
    }
}

/// For z = w·e1 (e1's imaginary part times `s`) = a + jb, and e2's rotations
/// u = c + jd and u³ = c₃ + jd₃: [Σac, Σbd, Σad, Σbc, Σac₃, Σbd₃, Σad₃, Σbc₃].
/// Every slice is padded to a multiple of LANES.
#[cfg(target_arch = "aarch64")]
fn sums(wr: &[f32], wi: &[f32], e1r: &[f32], e1i: &[f32], s: f32, e2: &[Vec<f32>; 4]) -> [f32; 8] {
    use std::arch::aarch64::*;
    let n = wr.len();
    assert!(n.is_multiple_of(LANES) && [wi, e1r, e1i, &e2[0], &e2[1], &e2[2], &e2[3]].iter().all(|v| v.len() >= n));
    // SAFETY: NEON is part of aarch64; every load is of LANES floats below n, checked above.
    unsafe {
        let mut acc = [vdupq_n_f32(0.0); 8];
        for i in (0..n).step_by(LANES) {
            let ld = |v: &[f32]| vld1q_f32(v.as_ptr().add(i));
            let (x, y, c, d) = (ld(wr), ld(wi), ld(e1r), vmulq_n_f32(ld(e1i), s));
            let a = vfmsq_f32(vmulq_f32(x, c), y, d);
            let b = vfmaq_f32(vmulq_f32(x, d), y, c);
            let (ur, ui, vr, vi) = (ld(&e2[0]), ld(&e2[1]), ld(&e2[2]), ld(&e2[3]));
            acc[0] = vfmaq_f32(acc[0], a, ur);
            acc[1] = vfmaq_f32(acc[1], b, ui);
            acc[2] = vfmaq_f32(acc[2], a, ui);
            acc[3] = vfmaq_f32(acc[3], b, ur);
            acc[4] = vfmaq_f32(acc[4], a, vr);
            acc[5] = vfmaq_f32(acc[5], b, vi);
            acc[6] = vfmaq_f32(acc[6], a, vi);
            acc[7] = vfmaq_f32(acc[7], b, vr);
        }
        acc.map(|v| vaddvq_f32(v))
    }
}

/// [`sums`] for other targets: lane-wise accumulators, so the loop vectorises
/// without reassociating.
#[cfg(not(target_arch = "aarch64"))]
fn sums(wr: &[f32], wi: &[f32], e1r: &[f32], e1i: &[f32], s: f32, e2: &[Vec<f32>; 4]) -> [f32; 8] {
    fn lanes(v: &[f32]) -> &[[f32; LANES]] {
        v.as_chunks::<LANES>().0
    }
    let (wr, wi, e1r, e1i) = (lanes(wr), lanes(wi), lanes(e1r), lanes(e1i));
    let n = wr.len();
    let (wi, e1r, e1i) = (&wi[..n], &e1r[..n], &e1i[..n]);
    let [ur, ui, vr, vi] = [&e2[0], &e2[1], &e2[2], &e2[3]].map(|v| &lanes(v)[..n]);
    let mut acc = [[0f32; LANES]; 8];
    for ch in 0..n {
        for j in 0..LANES {
            let (c, d) = (e1r[ch][j], s * e1i[ch][j]);
            let a = wr[ch][j] * c - wi[ch][j] * d;
            let b = wr[ch][j] * d + wi[ch][j] * c;
            acc[0][j] += a * ur[ch][j];
            acc[1][j] += b * ui[ch][j];
            acc[2][j] += a * ui[ch][j];
            acc[3][j] += b * ur[ch][j];
            acc[4][j] += a * vr[ch][j];
            acc[5][j] += b * vi[ch][j];
            acc[6][j] += a * vi[ch][j];
            acc[7][j] += b * vr[ch][j];
        }
    }
    acc.map(|a| a.iter().sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Msd {
        /// The straightforward form of [`Msd::decide`] (complex arrays, every
        /// hypothesis correlated on its own), which the fast one must match.
        fn decide_reference(&self) -> Symbol {
            let (sk, lv) = self.pending[0];
            let Some(&(prev, _)) = self.done.back() else { return sk };
            let next = self.pending[1].0;
            let (a, b) = (prev.sample - self.sps / 2.0, next.sample + self.sps / 2.0);
            if a < self.iq_base as f64 {
                return sk;
            }
            let (n0, n1) = (a.ceil() as u64, b.floor() as u64);
            let len = (n1 - n0 + 1) as usize;
            let fixed: Vec<(f64, f32)> = self
                .done
                .iter()
                .take(self.done.len() - 1)
                .map(|(s, l)| (s.sample, l[level_index(s.dibit)]))
                .chain(self.pending.iter().skip(2).take(SPAN).map(|(s, l)| (s.sample, l[level_index(s.dibit)])))
                .collect();
            let c = (lv[0] + lv[3]) / 2.0;
            let o3 = (lv[3] - lv[0]) / 6.0;
            let inst = [prev.sample, sk.sample, next.sample];
            let (mut y, mut e) = (Vec::new(), vec![Vec::new(); 12]);
            let (mut ph, mut cq) = (0f32, [0f32; 3]);
            for i in 0..len {
                let n = (n0 + i as u64) as f64;
                let f: f32 = fixed.iter().map(|&(t, l)| l * self.p(n - t)).sum();
                let pq = [self.p(n - inst[0]), self.p(n - inst[1]), self.p(n - inst[2])];
                ph += self.w * (f + c * (pq[0] + pq[1] + pq[2]));
                y.push(self.iq[(n0 + i as u64 - self.iq_base) as usize] * Complex32::from_polar(1.0, -ph));
                for q in 0..3 {
                    cq[q] += self.w * pq[q];
                    let u = Complex32::from_polar(1.0, -o3 * cq[q]);
                    let u3 = u * u * u;
                    e[q * 4].push(u3.conj());
                    e[q * 4 + 1].push(u.conj());
                    e[q * 4 + 2].push(u);
                    e[q * 4 + 3].push(u3);
                }
            }
            let mut best = [0f32; 4];
            let one = [level_index(prev.dibit)];
            let l0s: &[usize] = if self.feedback { &one } else { &[0, 1, 2, 3] };
            for &l0 in l0s {
                for l1 in 0..4 {
                    let z: Vec<Complex32> = (0..len).map(|i| y[i] * e[l0][i] * e[4 + l1][i]).collect();
                    for l2 in 0..4 {
                        let acc: Complex32 = z.iter().zip(&e[8 + l2]).map(|(z, e)| z * e).sum();
                        best[l1] = best[l1].max(acc.norm_sqr());
                    }
                }
            }
            let top = best.iter().cloned().fold(0.0, f32::max).max(1e-30);
            let d = (0..4).max_by(|&i, &j| best[i].total_cmp(&best[j])).unwrap();
            let (pos, neg) = (best[2].max(best[3]), best[0].max(best[1]));
            let (outer, inner) = (best[0].max(best[3]), best[1].max(best[2]));
            Symbol { dibit: DIBIT[d], sample: sk.sample, rel_hi: (pos - neg).abs() / top, rel_lo: (outer - inner).abs() / top }
        }
    }

    /// Noisy 4-level CPFSK and a receiver's (partly wrong) decisions through
    /// both forms of the detector: the same symbols, the same reliabilities.
    fn matches_reference(pulse: Pulse, feedback: bool) {
        let rate = 37_500.0;
        let mut m = Msd::new(rate, pulse, 0.5, feedback);
        let levels = [-1800f32, -600.0, 600.0, 1800.0];
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let nsym = 3000;
        let tx: Vec<usize> = (0..nsym).map(|_| (rnd() * 4.0) as usize).collect();
        let t = |k: usize| 5.3 + k as f64 * m.sps;
        let nsamp = (t(nsym) + 10.0) as usize;
        let mut ph = 0f64;
        let iq: Vec<Complex32> = (0..nsamp)
            .map(|n| {
                let k0 = ((n as f64 - 5.3) / m.sps) as i64;
                let f: f64 = (k0 - 6..=k0 + 6).filter(|&k| k >= 0 && (k as usize) < nsym).map(|k| levels[tx[k as usize]] as f64 * m.p(n as f64 - t(k as usize)) as f64).sum();
                ph += 2.0 * PI * f / rate;
                let noise = Complex32::new(rnd() as f32 - 0.5, rnd() as f32 - 0.5) * 0.9;
                Complex32::from_polar(1.0, ph as f32) + noise
            })
            .collect();
        // One decision in five wrong, as from a discriminator below threshold.
        let syms: Vec<Symbol> = (0..nsym)
            .map(|k| {
                let l = if rnd() < 0.2 { (tx[k] + 1 + (rnd() * 3.0) as usize) % 4 } else { tx[k] };
                Symbol { dibit: DIBIT[l], sample: t(k), rel_hi: 1.0, rel_lo: 1.0 }
            })
            .collect();
        m.iq.extend(iq.iter().copied());
        m.iq_end = iq.len() as u64;
        m.pending.extend(syms.iter().map(|s| (*s, levels)));
        let (mut checked, mut worst) = (0, 0f32);
        while m.pending.len() > SPAN + 1 {
            let want = m.decide_reference();
            let got = m.decide();
            assert_eq!(got.dibit, want.dibit, "symbol {checked}");
            worst = worst.max((got.rel_hi - want.rel_hi).abs()).max((got.rel_lo - want.rel_lo).abs());
            m.pending.pop_front();
            m.done.push_back((want, levels));
            if m.done.len() > SPAN {
                m.done.pop_front();
            }
            checked += 1;
        }
        assert!(checked > 2900);
        assert!(worst < 1e-4, "reliabilities differ by {worst}");
    }

    #[test]
    fn sincos_is_accurate() {
        let x: Vec<f32> = (-400_000..400_000).map(|i| i as f32 * 2.5e-3).collect();
        let (mut s, mut c) = (Vec::new(), Vec::new());
        sincos(&x, &mut s, &mut c);
        for (i, &x) in x.iter().enumerate() {
            let e = ((s[i] as f64 - (x as f64).sin()).abs()).max((c[i] as f64 - (x as f64).cos()).abs());
            assert!(e < 2e-7, "x {x}: error {e}");
        }
    }

    #[test]
    fn fast_correlation_matches_reference_p25() {
        matches_reference(Pulse::Rc(0.2), false);
    }

    #[test]
    fn fast_correlation_matches_reference_dmr() {
        matches_reference(Pulse::Rrc(0.2), true);
    }
}
