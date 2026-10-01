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

fn rrc_at(t: f64, a: f64) -> f64 {
    if t.abs() < 1e-9 {
        1.0 - a + 4.0 * a / PI
    } else if (t.abs() - 1.0 / (4.0 * a)).abs() < 1e-9 {
        a / 2f64.sqrt() * ((1.0 + 2.0 / PI) * (PI / (4.0 * a)).sin() + (1.0 - 2.0 / PI) * (PI / (4.0 * a)).cos())
    } else {
        ((PI * t * (1.0 - a)).sin() + 4.0 * a * t * (PI * t * (1.0 + a)).cos()) / (PI * t * (1.0 - (4.0 * a * t).powi(2)))
    }
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
            Pulse::Rrc(a) => rrc_at(t, a),
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
    // Scratch.
    y: Vec<Complex32>,
    e: Vec<Vec<Complex32>>,
    z: Vec<Complex32>,
}

impl Msd {
    /// `rx_alpha`: the roll-off of the receiver's RRC filter whose output the
    /// levels are in. `feedback`: take the window's first symbol as already
    /// decided (16 hypotheses, not 64): better on DMR voice, ~0.8 dB worse on
    /// P25 C4FM (`tool snr`).
    pub fn new(rate: f64, tx: Pulse, rx_alpha: f64, feedback: bool) -> Self {
        let sps = rate / 4800.0;
        let n = (8.0 * sps).round() as i64 | 1;
        let m = n / 2;
        let rx: Vec<f64> = (-m..=m).map(|i| rrc_at(i as f64 / sps, rx_alpha)).collect();
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
            y: Vec::new(),
            e: vec![Vec::new(); 12],
            z: Vec::new(),
        }
    }

    /// The pulse `dt` samples from its symbol's instant.
    #[inline]
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
        let at = |n: u64| (n - self.iq_base) as usize;
        // The fixed symbols: re-decided before `prev`, the receiver's after `next`.
        let fixed: Vec<(f64, f32)> = self
            .done
            .iter()
            .take(self.done.len() - 1)
            .map(|(s, l)| (s.sample, l[level_index(s.dibit)]))
            .chain(self.pending.iter().skip(2).take(SPAN).map(|(s, l)| (s.sample, l[level_index(s.dibit)])))
            .collect();
        // Levels are symmetric about the centre c: c ± o/3, c ± o. The centre's
        // share of every hypothesis' phase is the same, so it goes into y with
        // the fixed symbols'; each window symbol then needs one rotation u (at
        // o/3), the others being conj(u), u³ and conj(u³).
        let c = (lv[0] + lv[3]) / 2.0;
        let o3 = (lv[3] - lv[0]) / 6.0;
        let inst = [prev.sample, sk.sample, next.sample];
        self.y.clear();
        for e in self.e.iter_mut() {
            e.clear();
        }
        let (mut ph, mut cq) = (0f32, [0f32; 3]);
        for i in 0..len {
            let n = (n0 + i as u64) as f64;
            let f: f32 = fixed.iter().map(|&(t, l)| l * self.p(n - t)).sum();
            let pq = [self.p(n - inst[0]), self.p(n - inst[1]), self.p(n - inst[2])];
            ph += self.w * (f + c * (pq[0] + pq[1] + pq[2]));
            self.y.push(self.iq[at(n0 + i as u64)] * Complex32::from_polar(1.0, -ph));
            for q in 0..3 {
                cq[q] += self.w * pq[q];
                let u = Complex32::from_polar(1.0, -o3 * cq[q]);
                let u3 = u * u * u;
                // Level order −3, −1, +1, +3.
                self.e[q * 4].push(u3.conj());
                self.e[q * 4 + 1].push(u.conj());
                self.e[q * 4 + 2].push(u);
                self.e[q * 4 + 3].push(u3);
            }
        }
        let mut best = [0f32; 4];
        let one = [level_index(prev.dibit)];
        let l0s: &[usize] = if self.feedback { &one } else { &[0, 1, 2, 3] };
        for &l0 in l0s {
            for l1 in 0..4 {
                self.z.clear();
                for i in 0..len {
                    self.z.push(self.y[i] * self.e[l0][i] * self.e[4 + l1][i]);
                }
                for l2 in 0..4 {
                    let e2 = &self.e[8 + l2];
                    let acc: Complex32 = self.z.iter().zip(e2).map(|(z, e)| z * e).sum();
                    let m = acc.norm_sqr();
                    if m > best[l1] {
                        best[l1] = m;
                    }
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
