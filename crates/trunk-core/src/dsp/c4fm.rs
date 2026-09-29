//! Streaming C4FM receiver (freq-finder's demodC4fmTracked without windows):
//!
//! ```text
//! FM discriminator (Hz) → boxcar (0.9 symbol) → per-block best sampling phase
//! (max Σ|deviation| over 240 symbols, 2·sps candidates), unwrapped across
//! blocks and interpolated between block centres → slice on the rails:
//! centre = midpoint of the 2 %/98 % quantiles of the last ~0.5 s of symbols,
//! inner/outer threshold = 2/3 of the outer rail.
//! ```
//!
//! Latency is two blocks (100 ms): block b's symbols need block b+1's phase.

use std::collections::VecDeque;
use std::f64::consts::PI;

use num_complex::Complex32;

use super::{Receiver, Symbol};

const BLOCK: u64 = 240;

pub struct C4fm {
    fs: f64,
    sps: f64,
    boxw: usize,
    steps: usize,
    last: Complex32,
    hist: VecDeque<f32>,
    acc: f64,
    y: Vec<f32>,
    y_base: u64,
    next_block: u64,
    next_sym: u64,
    phase_base: u64,
    phase: VecDeque<f64>,
    soft: VecDeque<f32>,
    tmp: Vec<f32>,
    since_rails: u64,
    center: f32,
    thr: f32,
    pub symbols: u64,
}

impl C4fm {
    pub fn new(fs: f64) -> Self {
        let sps = fs / 4800.0;
        C4fm {
            fs,
            sps,
            boxw: ((sps * 0.9).round() as usize).max(1),
            steps: ((sps * 2.0).round() as usize).max(8),
            last: Complex32::default(),
            hist: VecDeque::new(),
            acc: 0.0,
            y: Vec::new(),
            y_base: 0,
            next_block: 0,
            next_sym: 0,
            phase_base: 0,
            phase: VecDeque::new(),
            soft: VecDeque::new(),
            tmp: Vec::new(),
            since_rails: 0,
            center: 0.0,
            thr: 1200.0,
            symbols: 0,
        }
    }

    #[inline]
    fn at(&self, t: f64) -> f32 {
        let p = t - self.y_base as f64;
        let i = p as usize;
        let f = (p - i as f64) as f32;
        self.y[i] + (self.y[i + 1] - self.y[i]) * f
    }

    fn block_phase(&mut self, b: u64) {
        let (mut best, mut ph) = (-1.0f64, 0.0f64);
        for p in 0..self.steps {
            let cand = p as f64 / self.steps as f64 * self.sps;
            let mut e = 0.0f64;
            for s in b * BLOCK..(b + 1) * BLOCK {
                let t = cand + s as f64 * self.sps;
                if t < self.y_base as f64 {
                    continue;
                }
                e += (self.at(t) - self.center).abs() as f64;
            }
            if e > best {
                best = e;
                ph = cand;
            }
        }
        if let Some(&prev) = self.phase.back() {
            while ph - prev > self.sps / 2.0 {
                ph -= self.sps;
            }
            while ph - prev < -self.sps / 2.0 {
                ph += self.sps;
            }
        }
        self.phase.push_back(ph);
    }

    /// Phase at symbol s: linear between block centres (b + 0.5)·BLOCK.
    fn phase_at(&self, s: u64) -> f64 {
        let x = s as f64 / BLOCK as f64 - 0.5 - self.phase_base as f64;
        if x <= 0.0 {
            return self.phase[0];
        }
        let b = x as usize;
        if b + 1 >= self.phase.len() {
            return *self.phase.back().unwrap();
        }
        self.phase[b] + (self.phase[b + 1] - self.phase[b]) * (x - b as f64)
    }

    fn emit(&mut self, out: &mut Vec<Symbol>) {
        if self.phase.len() < 2 {
            return;
        }
        // Symbols up to the centre of the newest block with a phase are final.
        let up_to = (self.phase_base + self.phase.len() as u64 - 1) * BLOCK + BLOCK / 2;
        while self.next_sym < up_to {
            let t = self.phase_at(self.next_sym) + self.next_sym as f64 * self.sps;
            if t < self.y_base as f64 {
                self.next_sym += 1;
                continue;
            }
            if t + 1.0 >= (self.y_base + self.y.len() as u64) as f64 {
                break;
            }
            let v = self.at(t);
            self.slice(v, t - (self.boxw - 1) as f64 / 2.0, out);
            self.next_sym += 1;
        }
    }

    fn slice(&mut self, v: f32, t: f64, out: &mut Vec<Symbol>) {
        // Rails from the last ~0.5 s of symbols, refreshed every block.
        self.soft.push_back(v);
        if self.soft.len() > 2400 {
            self.soft.pop_front();
        }
        self.since_rails += 1;
        if self.since_rails >= BLOCK && self.soft.len() >= 480 {
            self.since_rails = 0;
            self.tmp.clear();
            self.tmp.extend(self.soft.iter());
            let n = self.tmp.len();
            let (lo, hi) = (n * 2 / 100, (n * 98 / 100).min(n - 1));
            let q_lo = *self.tmp.select_nth_unstable_by(lo, f32::total_cmp).1;
            let q_hi = *self.tmp.select_nth_unstable_by(hi, f32::total_cmp).1;
            self.center = (q_hi + q_lo) / 2.0;
            let outer = (q_hi - q_lo) / 2.0;
            self.thr = if outer > 300.0 { outer * 2.0 / 3.0 } else { 1200.0 };
        }
        let x = v - self.center;
        let dibit = if x >= self.thr { 0b01 } else if x >= 0.0 { 0b00 } else if x >= -self.thr { 0b10 } else { 0b11 };
        self.symbols += 1;
        // Reliabilities in units of the outer rail: sign bit |x|, outer bit ||x| − thr|.
        let scale = if self.thr > 0.0 { 1.0 / (self.thr * 1.5) } else { 1.0 };
        out.push(Symbol { dibit, sample: t, rel_hi: x.abs() * scale, rel_lo: (x.abs() - self.thr).abs() * scale });
    }

    fn compact(&mut self) {
        let front = self.phase.front().copied().unwrap_or(0.0);
        let keep_from = self.next_sym as f64 * self.sps + front - 2.0 * BLOCK as f64 * self.sps;
        if keep_from > self.y_base as f64 + 8192.0 {
            let drop = (keep_from - self.y_base as f64) as usize;
            self.y.drain(..drop);
            self.y_base += drop as u64;
        }
        while self.phase.len() > 4 && (self.phase_base + 2) * BLOCK + BLOCK / 2 < self.next_sym {
            self.phase.pop_front();
            self.phase_base += 1;
        }
    }
}

impl Receiver for C4fm {
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<Symbol>) {
        let k = (self.fs / (2.0 * PI)) as f32;
        for &x in iq {
            let f = (x * self.last.conj()).arg() * k;
            self.last = x;
            self.hist.push_back(f);
            self.acc += f as f64;
            if self.hist.len() > self.boxw {
                self.acc -= self.hist.pop_front().unwrap() as f64;
            }
            self.y.push((self.acc / self.hist.len() as f64) as f32);
        }
        while ((self.y_base + self.y.len() as u64) as f64) > ((self.next_block + 1) * BLOCK) as f64 * self.sps + self.sps + 2.0 {
            self.block_phase(self.next_block);
            self.next_block += 1;
        }
        self.emit(out);
        self.compact();
    }
}
