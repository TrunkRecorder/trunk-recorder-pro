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

use super::{c4fm, filters};
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
    /// Decision-feedback differential detection: the reference is past
    /// symbols, each turned on by its decided step, averaged with this
    /// forgetting factor (0 = plain differential detection).
    pub df_beta: f32,
    /// Symbol rate: 4800 (Phase 1, [`super::c4fm::SYMBOL_RATE`]) or 6000 (Phase 2 H-DQPSK, [`crate::p25::phase2::SYMBOL_RATE`]).
    pub baud: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options { eq_taps: 0, eq_mu: 0.02, soft_amplitude: true, coherent: false, pll_kp: 0.04, baud: c4fm::SYMBOL_RATE, df_beta: 0.0 }
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
    pub fn new(rate: f64, mut opt: Options) -> Self {
        let sps = rate / opt.baud;
        let persym = (sps.round() as usize).max(1);
        let span = (512 / persym).clamp(4, 11);
        let mut len = persym * span;
        if len % 2 == 0 {
            len += 1;
        }
        let taps = filters::rrc_taps(0.35, sps, len);
        // Odd, and at least 3: it is T/2-spaced, two inputs a symbol.
        if opt.eq_taps > 0 && opt.eq_taps < 3 {
            opt.eq_taps = 3;
        }
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
            taps,
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
            let baud = self.opt.baud as u64;
            if self.symbols - self.last_sync > baud && self.symbols - self.last_reacq > baud / 2 {
                self.reacquire(end);
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

    fn reacquire(&mut self, end: f64) {
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
        let mut shift = ((best as f64 / 16.0) * self.sps + self.sps / 2.0).rem_euclid(self.sps) - self.sps / 2.0;
        // Never past the filtered samples we have: step back a symbol instead.
        if self.pos + shift + 2.0 >= end {
            shift -= self.sps;
        }
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
            s * self.dprev.conj()
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
        if !self.opt.coherent {
            // The next reference: this symbol, plus the old reference turned on
            // by the step just decided (and the residual turn).
            let b = self.opt.df_beta;
            let step = [PI32 / 4.0, 3.0 * PI32 / 4.0, -PI32 / 4.0, -3.0 * PI32 / 4.0][dib as usize];
            self.dprev = s * (1.0 - b) + self.dprev * Complex32::from_polar(b, step + theta);
        }
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
        // Frame sync: Phase 1's 48 bits, or Phase 2's 40-bit S-ISCH.
        let (fs, inv, mask, len) = if self.opt.baud > 5000.0 {
            (0x57_5D57_F7FFu64, 0xAA_AAAA_AAAAu64, 0xFF_FFFF_FFFFu64, 20)
        } else {
            (0x5575_F5FF_77FF, 0xAAAA_AAAA_AAAA, 0xFFFF_FFFF_FFFF, 24)
        };
        let sr = self.sr & mask;
        if self.symbols - self.last_sync >= len && ((sr ^ fs).count_ones() <= 4 || (sr ^ fs ^ inv).count_ones() <= 4) {
            self.syncs += 1;
            self.last_sync = self.symbols;
        }
    }
}

impl Cqpsk {
    /// Whether `other` filters the same way (sample rate, symbol rate):
    /// given the same IQ, its matched filter output is this one's.
    pub fn same_front(&self, other: &Cqpsk) -> bool {
        self.sps == other.sps && self.taps == other.taps
    }

    /// [`Receiver::push`], also handing the matched filter output for `iq`
    /// to `share` (replacing what it held), for [`Cqpsk::push_filtered`].
    pub fn push_sharing(&mut self, iq: &[Complex32], share: &mut [Vec<f32>; 2], out: &mut Vec<Symbol>) {
        self.front(iq);
        let at = self.yi.len() - iq.len();
        for (s, y) in share.iter_mut().zip([&self.yi, &self.yq]) {
            s.clear();
            s.extend_from_slice(&y[at..]);
        }
        self.gardner(out);
    }

    /// As [`Receiver::push`], but taking the matched filter output from a
    /// receiver with the [`Cqpsk::same_front`] that was pushed the same IQ
    /// ([`Cqpsk::push_sharing`]) instead of filtering again. (A receiver
    /// must be fed one way only: this one's own front end stands still.)
    pub fn push_filtered(&mut self, yi: &[f32], yq: &[f32], out: &mut Vec<Symbol>) {
        self.yi.extend_from_slice(yi);
        self.yq.extend_from_slice(yq);
        self.gardner(out);
    }

    /// Steps 1–3: carrier, derotation, matched filter → yi / yq.
    fn front(&mut self, iq: &[Complex32]) {
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
    }
}

impl Receiver for Cqpsk {
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<Symbol>) {
        self.front(iq);
        // 4. Timing and detection.
        self.gardner(out);
    }

    /// The carrier step the front end derotates by, plus the residual turn
    /// per symbol detection takes out (the 4th-power estimate, or the PLL's
    /// frequency term), in Hz. Only while frames sync (within the last
    /// second), and not on a receiver fed another's filter output: its own
    /// carrier estimate never runs.
    fn offset_hz(&self) -> Option<f32> {
        let baud = self.opt.baud as u64;
        let synced = self.syncs > 0 && self.symbols - self.last_sync <= baud;
        if !synced || (self.acc_r == 0.0 && self.acc_i == 0.0) {
            return None;
        }
        let w = self.acc_i.atan2(self.acc_r) * self.sps;
        let theta = if self.opt.coherent { self.cfr } else { (-self.q4.im).atan2(-self.q4.re) / 4.0 };
        Some(((w + theta as f64) / (2.0 * PI) * self.opt.baud) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Receiver;

    /// A 1-tap equaliser (a tool flag) is made 3 taps, not a panic.
    #[test]
    fn a_one_tap_equaliser_is_widened() {
        let mut rx = Cqpsk::new(48_000.0, Options { eq_taps: 1, ..Default::default() });
        let mut out = Vec::new();
        let iq: Vec<Complex32> = (0..4800).map(|i| Complex32::from_polar(1.0, i as f32 * 0.7)).collect();
        rx.push(&iq, &mut out);
    }

    /// The carrier offset reads back once frames sync, and not before.
    #[test]
    fn reports_the_carrier_offset_once_it_syncs() {
        let (rate, sps, off) = (48_000.0, 10usize, 400.0);
        // Frames: the 24-dibit sync, then pseudo-random dibits to 864.
        let mut dibits = Vec::new();
        let mut lfsr = 0xACE1u16;
        for _ in 0..8 {
            dibits.extend((0..24).rev().map(|i| ((0x5575_F5FF_77FFu64 >> (2 * i)) & 3) as usize));
            for _ in 24..864 {
                for _ in 0..2 {
                    lfsr = (lfsr >> 1) ^ (-((lfsr & 1) as i16) as u16 & 0xB400);
                }
                dibits.push((lfsr & 3) as usize);
            }
        }
        let step = [PI / 4.0, 3.0 * PI / 4.0, -PI / 4.0, -3.0 * PI / 4.0];
        let taps = filters::rrc_taps(0.35, sps as f64, 8 * sps + 1);
        let mut x = vec![Complex32::default(); dibits.len() * sps + taps.len()];
        let mut ph = 0.0f64;
        for (k, &d) in dibits.iter().enumerate() {
            ph += step[d];
            let s = Complex32::from_polar(1.0, ph as f32);
            for (j, &t) in taps.iter().enumerate() {
                x[k * sps + j] += s * t;
            }
        }
        let iq: Vec<Complex32> = x.iter().enumerate().map(|(n, &v)| v * Complex32::from_polar(1.0, (2.0 * PI * off * n as f64 / rate) as f32)).collect();

        let mut rx = Cqpsk::new(rate, Options::default());
        assert_eq!(rx.offset_hz(), None);
        let mut out = Vec::new();
        for c in iq.chunks(4800) {
            rx.push(c, &mut out);
        }
        assert!(rx.syncs > 2, "syncs {}", rx.syncs);
        let got = rx.offset_hz().expect("an offset once synced");
        assert!((got - off as f32).abs() < 30.0, "offset {got}");
    }
}
