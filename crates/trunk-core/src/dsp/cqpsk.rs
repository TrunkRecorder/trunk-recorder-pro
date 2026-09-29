//! Streaming π/4-DQPSK (CQPSK / LSM simulcast) receiver:
//!
//! ```text
//! carrier-offset tracker (smoothed arg Σ x[n]·x*[n−1]) → NCO derotate
//! → RRC matched filter (α 0.35, 11 symbols) → Gardner timing loop (PI, lerp)
//! → [T/2 CMA equaliser] → differential detection (4th-power residual turn)
//! → dibits with amplitude-weighted soft bits
//! ```
//!
//! It also decodes C4FM passably (π/4 phase steps are C4FM's levels
//! integrated over a symbol), though [`super::c4fm`] does that better.

use std::f32::consts::PI as PI32;
use std::f64::consts::PI;

use num_complex::Complex32;

use super::{Receiver, Symbol};

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Fractionally spaced (T/2) CMA equaliser taps (odd; 0 = off), over
    /// alternating mid-symbol / symbol samples. π/4-DQPSK has constant modulus,
    /// so CMA needs no decisions and is blind to carrier phase.
    pub eq_taps: usize,
    pub eq_mu: f32,
    /// Soft bits scaled by the symbol's amplitude (a faded symbol counts less)
    /// rather than phase only. 62 % → 98 % TSBKs on simulcast with Viterbi.
    pub soft_amplitude: bool,
    /// Experimental: decision-directed PLL + decision-feedback differential
    /// detection. Worse than differential on the simulcast tested so far.
    pub coherent: bool,
    pub pll_kp: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options { eq_taps: 0, eq_mu: 0.02, soft_amplitude: true, coherent: false, pll_kp: 0.04 }
    }
}

const REACQ_SYMBOLS: usize = 1000;

/// Dot product with 8 independent accumulators, so it vectorises (NEON / SSE /
/// AVX / wasm SIMD) without fast-math reassociation.
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let (ac, bc) = (a.chunks_exact(8), b.chunks_exact(8));
    let (ar, br) = (ac.remainder(), bc.remainder());
    let mut acc = [0f32; 8];
    for (x, y) in ac.zip(bc) {
        for k in 0..8 {
            acc[k] += x[k] * y[k];
        }
    }
    let mut s = acc.iter().sum::<f32>();
    for (x, y) in ar.iter().zip(br) {
        s += x * y;
    }
    s
}

pub struct Cqpsk {
    sps: f64,
    opt: Options,
    taps: Vec<f32>,
    // carrier
    last: Complex32,
    acc_r: f64,
    acc_i: f64,
    nco_ph: f64,
    // matched filter input (history + new) and output stream
    xi: Vec<f32>,
    xq: Vec<f32>,
    yi: Vec<f32>,
    yq: Vec<f32>,
    y_base: u64,
    // timing
    started: bool,
    pos: f64,
    rate: f64,
    power: f32,
    prev: Complex32,
    // equaliser
    eq_x: Vec<Complex32>,
    eq_w: Vec<Complex32>,
    pos_hist: [f64; 16],
    pos_n: usize,
    // detection
    dprev: Complex32,
    q4: Complex32,
    cth: f32,
    cfr: f32,
    prev_p: Complex32,
    // sync watch (for re-acquisition)
    sr: u64,
    pub symbols: u64,
    pub syncs: u64,
    pub eq_resets: u64,
    last_sync: u64,
    last_reacq: u64,
}

impl Cqpsk {
    pub fn new(fs: f64, mut opt: Options) -> Self {
        let sps = fs / 4800.0;
        let alpha = 0.35;
        let persym = (sps.round() as usize).max(1);
        let span = (512 / persym).clamp(4, 11);
        let mut len = persym * span;
        if len % 2 == 0 {
            len += 1;
        }
        let mid = (len - 1) as f64 / 2.0;
        let mut taps: Vec<f64> = (0..len)
            .map(|i| {
                let t = (i as f64 - mid) / sps;
                if t.abs() < 1e-8 {
                    1.0 - alpha + 4.0 * alpha / PI
                } else if (t.abs() - 1.0 / (4.0 * alpha)).abs() < 1e-8 {
                    let a = PI / (4.0 * alpha);
                    alpha / std::f64::consts::SQRT_2 * ((1.0 + 2.0 / PI) * a.sin() + (1.0 - 2.0 / PI) * a.cos())
                } else {
                    let pt = PI * t;
                    ((pt * (1.0 - alpha)).sin() + 4.0 * alpha * t * (pt * (1.0 + alpha)).cos()) / (pt * (1.0 - (4.0 * alpha * t).powi(2)))
                }
            })
            .collect();
        let sum: f64 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        if opt.eq_taps > 0 && opt.eq_taps % 2 == 0 {
            opt.eq_taps += 1;
        }
        let mut eq_w = vec![Complex32::default(); opt.eq_taps];
        if opt.eq_taps > 0 {
            eq_w[opt.eq_taps / 2] = Complex32::new(1.0, 0.0);
        }
        Cqpsk {
            sps,
            opt,
            taps: taps.iter().map(|&t| t as f32).collect(),
            last: Complex32::default(),
            acc_r: 0.0,
            acc_i: 0.0,
            nco_ph: 0.0,
            xi: vec![0.0; len - 1],
            xq: vec![0.0; len - 1],
            yi: Vec::new(),
            yq: Vec::new(),
            y_base: 0,
            started: false,
            pos: 0.0,
            rate: 0.0,
            power: 0.0,
            prev: Complex32::default(),
            eq_x: vec![Complex32::default(); opt.eq_taps],
            eq_w,
            pos_hist: [0.0; 16],
            pos_n: 0,
            dprev: Complex32::default(),
            q4: Complex32::default(),
            cth: 0.0,
            cfr: 0.0,
            prev_p: Complex32::new(1.0, 0.0),
            sr: 0,
            symbols: 0,
            syncs: 0,
            eq_resets: 0,
            last_sync: 0,
            last_reacq: 0,
        }
    }

    #[inline]
    fn at(y: &[f32], base: u64, pos: f64) -> f32 {
        let p = pos - base as f64;
        let i = p as usize;
        let f = (p - i as f64) as f32;
        y[i] + (y[i + 1] - y[i]) * f
    }

    fn gardner(&mut self, out: &mut Vec<Symbol>) {
        let end = (self.y_base + self.yi.len() as u64) as f64;
        if !self.started {
            if end - (self.y_base as f64) < (self.taps.len() + 4) as f64 {
                return;
            }
            self.pos = self.y_base as f64 + self.taps.len() as f64;
            self.prev = Complex32::new(Self::at(&self.yi, self.y_base, self.pos), Self::at(&self.yq, self.y_base, self.pos));
            self.started = true;
        }
        let (kp, ki) = (0.02f64, 0.02f64 * 0.02 * 0.25);
        let sps = self.sps;
        while self.pos + sps + self.rate + 2.0 < end {
            let next = self.pos + sps + self.rate;
            let mid = self.pos + (next - self.pos) / 2.0;
            let c = Complex32::new(Self::at(&self.yi, self.y_base, next), Self::at(&self.yq, self.y_base, next));
            let m = Complex32::new(Self::at(&self.yi, self.y_base, mid), Self::at(&self.yq, self.y_base, mid));
            let e = (c.re - self.prev.re) * m.re + (c.im - self.prev.im) * m.im;
            let p = c.norm_sqr();
            self.power = if self.power == 0.0 { p } else { self.power + 0.01 * (p - self.power) };
            let en = if self.power > 0.0 { (e / self.power).clamp(-1.0, 1.0) } else { 0.0 } as f64;
            self.rate = (self.rate - ki * en * sps).clamp(-sps * 0.05, sps * 0.05);
            self.pos = next - kp * en * sps;
            let s = Complex32::new(Self::at(&self.yi, self.y_base, self.pos), Self::at(&self.yq, self.y_base, self.pos));
            if self.opt.eq_taps > 0 {
                self.equalise(m, s, out);
            } else {
                self.symbol(s, self.pos, out);
            }
            self.prev = s;
            // No sync for ~1 s: the loop may sit on a wrong equilibrium. Re-pick
            // the sampling phase by energy.
            if self.symbols - self.last_sync > 4800 && self.symbols - self.last_reacq > 2400 {
                self.reacquire();
            }
        }
        let keep = (REACQ_SYMBOLS as f64 * sps) as u64 + 8;
        let drop = ((self.pos - keep as f64).floor() - self.y_base as f64).max(0.0) as usize;
        if drop > 4096 {
            self.yi.drain(..drop);
            self.yq.drain(..drop);
            self.y_base += drop as u64;
        }
    }

    fn equalise(&mut self, m: Complex32, s: Complex32, out: &mut Vec<Symbol>) {
        let g = if self.power > 0.0 { 1.0 / self.power.sqrt() } else { 1.0 };
        let n = self.eq_x.len();
        self.eq_x.copy_within(0..n - 2, 2);
        self.eq_x[1] = m * g;
        self.eq_x[0] = s * g;
        let y: Complex32 = self.eq_w.iter().zip(&self.eq_x).map(|(w, x)| w * x).sum();
        // Clipped CMA error, and a reset to a plain centre tap on divergence
        // (noise before a call must not throw the taps about).
        let e = (y.norm_sqr() - 1.0).clamp(-1.0, 1.0);
        let k = y * (self.opt.eq_mu * e);
        let mut wn = 0.0f32;
        for (w, x) in self.eq_w.iter_mut().zip(&self.eq_x) {
            *w -= k * x.conj();
            wn += w.norm_sqr();
        }
        if !(wn < 4.0 && wn > 0.05) {
            self.eq_w.iter_mut().for_each(|w| *w = Complex32::default());
            self.eq_w[n / 2] = Complex32::new(1.0, 0.0);
            self.eq_resets += 1;
        }
        // The centre tap is a symbol sample, N/4 symbols back.
        self.pos_hist[self.pos_n % 16] = self.pos;
        self.pos_n += 1;
        let lag = n / 4;
        let at = if self.pos_n > lag { self.pos_hist[(self.pos_n - 1 - lag) % 16] } else { self.pos };
        self.symbol(y, at, out);
    }

    fn reacquire(&mut self) {
        self.last_reacq = self.symbols;
        let from = self.pos - REACQ_SYMBOLS as f64 * self.sps;
        if from < self.y_base as f64 + 1.0 {
            return;
        }
        let (mut best, mut best_e) = (0usize, -1.0f64);
        for ph in 0..16 {
            let mut e = 0.0f64;
            for k in 0..REACQ_SYMBOLS {
                let p = from + (ph as f64 / 16.0 + k as f64) * self.sps;
                let (i, q) = (Self::at(&self.yi, self.y_base, p), Self::at(&self.yq, self.y_base, p));
                e += (i * i + q * q) as f64;
            }
            if e > best_e {
                best_e = e;
                best = ph;
            }
        }
        // `from` is a whole number of symbols behind pos, so phase 0 ≡ pos.
        let shift = ((best as f64 / 16.0) * self.sps + self.sps / 2.0).rem_euclid(self.sps) - self.sps / 2.0;
        self.pos += shift;
        self.rate = 0.0;
        self.prev = Complex32::new(Self::at(&self.yi, self.y_base, self.pos), Self::at(&self.yq, self.y_base, self.pos));
    }

    fn symbol(&mut self, s: Complex32, at: f64, out: &mut Vec<Symbol>) {
        let d = if self.opt.coherent {
            // Decision-directed PLL on the 8 π/4-DQPSK phases; the previous
            // symbol enters as its clean decided point.
            let mut z = s;
            if self.opt.eq_taps == 0 && self.power > 0.0 {
                z /= self.power.sqrt();
            }
            z *= Complex32::from_polar(1.0, -self.cth);
            let m8 = ((z.arg() / (PI32 / 4.0)).round() as i32) & 7;
            let p = Complex32::from_polar(1.0, m8 as f32 * PI32 / 4.0);
            let err = (z * p.conj()).arg();
            let ki = self.opt.pll_kp * self.opt.pll_kp / 4.0;
            self.cfr = (self.cfr + ki * err).clamp(-0.2, 0.2);
            self.cth = (self.cth + self.opt.pll_kp * err + self.cfr + PI32).rem_euclid(2.0 * PI32) - PI32;
            let d = z * self.prev_p.conj();
            self.prev_p = p;
            d
        } else {
            let d = s * self.dprev.conj();
            self.dprev = s;
            d
        };
        let m = d.norm();
        if m > 0.0 && !self.opt.coherent {
            // Residual turn from the 4th power (every ideal step⁴ = e^{jπ}).
            let u = d / m;
            let u2 = u * u;
            self.q4 = self.q4 * 0.995 + u2 * u2 * m;
        }
        let theta = if self.opt.coherent { 0.0 } else { (-self.q4.im).atan2(-self.q4.re) / 4.0 };
        let r = d * Complex32::from_polar(1.0, -theta);
        // +45° = 00, +135° = 01, −45° = 10, −135° = 11 (P25's +1 +3 −1 −3).
        let dib = if r.im >= 0.0 { if r.re > 0.0 { 0b00 } else { 0b01 } } else if r.re > 0.0 { 0b10 } else { 0b11 };
        self.symbols += 1;
        // hi bit = (im < 0), lo bit = (re ≤ 0): the reliabilities are |im|, |re|.
        let amp_norm = if self.opt.coherent { 1.0 } else if self.power > 0.0 { 1.0 / self.power } else { 1.0 };
        let norm = if self.opt.soft_amplitude { amp_norm } else if m > 0.0 { 1.0 / m } else { 0.0 };
        out.push(Symbol {
            dibit: dib,
            sample: at - (self.taps.len() - 1) as f64 / 2.0,
            rel_hi: r.im.abs() * norm,
            rel_lo: r.re.abs() * norm,
        });
        self.sr = ((self.sr << 2) | dib as u64) & 0xFFFF_FFFF_FFFF;
        const FS: u64 = 0x5575_F5FF_77FF;
        const INV: u64 = 0xAAAA_AAAA_AAAA;
        if self.symbols - self.last_sync >= 24 && ((self.sr ^ FS).count_ones() <= 4 || (self.sr ^ FS ^ INV).count_ones() <= 4) {
            self.syncs += 1;
            self.last_sync = self.symbols;
        }
    }
}

impl Receiver for Cqpsk {
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<Symbol>) {
        let n = iq.len();
        // 1. Carrier offset: smoothed power-weighted mean phase increment.
        let (mut br, mut bi) = (0.0f64, 0.0f64);
        for &x in iq {
            let d = x * self.last.conj();
            br += d.re as f64;
            bi += d.im as f64;
            self.last = x;
        }
        self.acc_r = 0.97 * self.acc_r + br;
        self.acc_i = 0.97 * self.acc_i + bi;
        let w = if self.acc_r == 0.0 && self.acc_i == 0.0 { 0.0 } else { self.acc_i.atan2(self.acc_r) };
        // 2. Derotate into the matched filter's input.
        let rot = Complex32::new((-w).cos() as f32, (-w).sin() as f32);
        let mut c = Complex32::new(self.nco_ph.cos() as f32, self.nco_ph.sin() as f32);
        for &x in iq {
            let v = x * c;
            self.xi.push(v.re);
            self.xq.push(v.im);
            c *= rot;
        }
        self.nco_ph = (self.nco_ph - w * n as f64 + PI).rem_euclid(2.0 * PI) - PI;
        // 3. RRC matched filter.
        let t = self.taps.len();
        self.yi.reserve(n);
        self.yq.reserve(n);
        for j in 0..n {
            self.yi.push(dot(&self.xi[j..j + t], &self.taps));
            self.yq.push(dot(&self.xq[j..j + t], &self.taps));
        }
        self.xi.drain(..n);
        self.xq.drain(..n);
        // 4. Timing and detection.
        self.gardner(out);
    }
}
