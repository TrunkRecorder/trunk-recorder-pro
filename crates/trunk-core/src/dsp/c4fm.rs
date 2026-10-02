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

use super::msd::{Msd, Pulse};
use super::{Receiver, Symbol};

const BLOCK: u64 = 240;
/// A channel is bursty (a mobile) when its peak power in the window is this
/// far above its floor (13 dB); its quiet samples — nothing on the air — are
/// then left out of the timing and the levels, and their symbols carry no
/// reliability.
const BURSTY: f32 = 20.0;
/// Symbols the power is smoothed over.
const ENV_SYMBOLS: f64 = 1.0;
/// The peak is the largest power in this window (a mobile's bursts come every
/// 60 ms; a new, weaker signal is followed within it), kept as block maxima.
const PEAK_WINDOW_S: f64 = 0.1;
const PEAK_BLOCKS: usize = 20;
/// Level separation (step / spread) above which MSD is switched off, and below which on.
const MSD_OFF: f32 = 11.0;
const MSD_ON: f32 = 9.0;

/// Receiver variants, for weak-signal comparisons (`tool snr --variant`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct C4fmOptions {
    /// Boxcar width after the discriminator, symbols.
    pub box_symbols: f64,
    /// Root-raised-cosine matched filter of this roll-off instead of the boxcar.
    pub rrc: Option<f64>,
    /// Clamp the discriminator at this multiple of the outer rail (click suppression).
    pub clip: Option<f32>,
    /// Levels from the four clusters' means instead of the 2 % / 98 % quantiles
    /// (noise widens the quantiles, so the outer threshold ends up too high).
    pub rail_means: bool,
    /// Symbols per timing block (one sampling phase estimated per block).
    pub block: u64,
    /// Re-decide every symbol by multi-symbol detection with this transmit pulse ([`super::msd`]).
    pub msd: Option<Pulse>,
}

/// P25: an RRC matched filter (α 0.5) and cluster-mean levels. Against the
/// boxcar and quantile levels, 3.6 dB less signal for half the IMBE codewords
/// on WMATA's C4FM voice (`tool snr`); no change on simulcast, where the
/// CQPSK receivers carry it.
impl Default for C4fmOptions {
    fn default() -> Self {
        C4fmOptions { box_symbols: 0.9, rrc: Some(0.5), clip: None, rail_means: true, block: BLOCK, msd: Some(Pulse::Rc(0.2)) }
    }
}

impl C4fmOptions {
    /// DMR: the RRC matching its transmitter's (α 0.2), cluster-mean levels —
    /// about 6 dB better than the P25-era boxcar and quantile levels.
    pub fn dmr() -> Self {
        C4fmOptions { rrc: Some(0.2), msd: Some(Pulse::Rrc(0.2)), ..Default::default() }
    }

    /// The first receiver (boxcar, quantile levels), for comparisons.
    pub fn legacy() -> Self {
        C4fmOptions { box_symbols: 0.9, rrc: None, clip: None, rail_means: false, block: BLOCK, msd: None }
    }

    /// Apply one `name[=value]` setting; false if unknown.
    pub fn set(&mut self, p: &str) -> bool {
        let (k, v) = p.split_once('=').map_or((p, None), |(k, v)| (k, v.parse::<f64>().ok()));
        match k {
            "box" => {
                self.box_symbols = v.unwrap_or(0.9);
                self.rrc = None;
            }
            "quantile" => self.rail_means = false,
            "legacy" => *self = Self::legacy(),
            "rrc" => self.rrc = Some(v.unwrap_or(0.2)),
            "clip" => self.clip = Some(v.unwrap_or(1.5) as f32),
            "means" => self.rail_means = true,
            "nomsd" => self.msd = None,
            "block" => self.block = v.unwrap_or(240.0) as u64,
            _ => return false,
        }
        true
    }
}

/// Root-raised-cosine taps (`span` symbols, `sps` samples per symbol), unit DC gain.
fn rrc_taps(alpha: f64, sps: f64, span: usize) -> Vec<f32> {
    let n = (span as f64 * sps).round() as usize | 1;
    let m = (n - 1) as f64 / 2.0;
    let mut h: Vec<f64> = (0..n)
        .map(|i| {
            let t = (i as f64 - m) / sps;
            if t.abs() < 1e-9 {
                1.0 - alpha + 4.0 * alpha / PI
            } else if (t.abs() - 1.0 / (4.0 * alpha)).abs() < 1e-9 {
                alpha / 2f64.sqrt() * ((1.0 + 2.0 / PI) * (PI / (4.0 * alpha)).sin() + (1.0 - 2.0 / PI) * (PI / (4.0 * alpha)).cos())
            } else {
                ((PI * t * (1.0 - alpha)).sin() + 4.0 * alpha * t * (PI * t * (1.0 + alpha)).cos()) / (PI * t * (1.0 - (4.0 * alpha * t).powi(2)))
            }
        })
        .collect();
    let dc: f64 = h.iter().sum();
    for v in h.iter_mut() {
        *v /= dc;
    }
    h.into_iter().map(|v| v as f32).collect()
}

pub struct C4fm {
    opts: C4fmOptions,
    msd: Option<Msd>,
    msd_in: Vec<Symbol>,
    /// Multi-symbol detection running (the signal isn't clean enough without it).
    msd_on: bool,
    /// Matched filter taps (None: the boxcar); its delay, samples.
    taps: Option<Vec<f32>>,
    delay: f64,
    fs: f64,
    sps: f64,
    boxw: usize,
    steps: usize,
    last: Complex32,
    hist: VecDeque<f32>,
    /// Matched filter input (with taps): the last taps−1 samples already
    /// filtered (`fx_done` of them), then those waiting for [`C4fm::filter`].
    fx: Vec<f32>,
    fx_done: usize,
    acc: f64,
    y: Vec<f32>,
    y_base: u64,
    /// Per sample (parallel to `y`): its power, smoothed over a symbol. Well
    /// below the recent peak is quiet: a mobile between its bursts, or noise
    /// from before a signal came up.
    envs: Vec<f32>,
    /// The symbols the levels come from: (value, power).
    soft_env: VecDeque<f32>,
    /// Power: smoothed over a symbol, and its slowly decaying peak.
    env: f32,
    peak: f32,
    peaks: VecDeque<f32>,
    blk_max: f32,
    blk_n: usize,
    /// The quietest envelope in the same window (a mobile's gaps).
    floors: VecDeque<f32>,
    blk_min: f32,
    floor: f32,
    /// The window was bursty at the last block (a mobile on the air).
    bursty: bool,
    next_block: u64,
    next_sym: u64,
    phase_base: u64,
    phase: VecDeque<f64>,
    soft: VecDeque<f32>,
    tmp: Vec<f32>,
    since_rails: u64,
    center: f32,
    thr: f32,
    /// Level step over the symbols' spread around their levels (∞ until measured).
    separation: f32,
    pub symbols: u64,
}

impl C4fm {
    pub fn new(fs: f64) -> Self {
        Self::with_options(fs, C4fmOptions::default())
    }

    /// For DMR (4FSK at 4800 baud, RRC-shaped).
    /// Level step / spread of the symbols about their levels: ~10 and up,
    /// the decisions are clean; ~1, noise.
    pub fn separation(&self) -> f32 {
        self.separation
    }

    /// The four symbol levels (discriminator Hz: −3, −1, +1, +3), as now estimated.
    pub fn levels(&self) -> [f32; 4] {
        let o = self.thr * 1.5;
        [self.center - o, self.center - o / 3.0, self.center + o / 3.0, self.center + o]
    }

    pub fn dmr(fs: f64) -> Self {
        Self::with_options(fs, C4fmOptions::dmr())
    }

    pub fn with_options(fs: f64, opts: C4fmOptions) -> Self {
        let sps = fs / 4800.0;
        let boxw = ((sps * opts.box_symbols).round() as usize).max(1);
        let taps = opts.rrc.map(|a| rrc_taps(a, sps, 8));
        let delay = taps.as_ref().map_or((boxw - 1) as f64 / 2.0, |t| (t.len() - 1) as f64 / 2.0);
        C4fm {
            opts,
            // DMR (RRC pulse): decision feedback; P25 (RC): the full search.
            msd: opts.msd.map(|p| Msd::new(fs, p, opts.rrc.unwrap_or(0.5), matches!(p, Pulse::Rrc(_)))),
            msd_in: Vec::new(),
            msd_on: true,
            taps,
            delay,
            fs,
            sps,
            boxw,
            steps: ((sps * 2.0).round() as usize).max(8),
            last: Complex32::default(),
            hist: VecDeque::new(),
            fx: Vec::new(),
            fx_done: 0,
            acc: 0.0,
            y: Vec::new(),
            y_base: 0,
            envs: Vec::new(),
            soft_env: VecDeque::new(),
            env: 0.0,
            peak: 0.0,
            peaks: VecDeque::new(),
            blk_max: 0.0,
            blk_n: 0,
            floors: VecDeque::new(),
            blk_min: f32::INFINITY,
            floor: 0.0,
            bursty: false,
            next_block: 0,
            next_sym: 0,
            phase_base: 0,
            phase: VecDeque::new(),
            soft: VecDeque::new(),
            tmp: Vec::new(),
            since_rails: 0,
            center: 0.0,
            thr: 1200.0,
            separation: f32::INFINITY,
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

    /// Nothing on the air at channel sample `t` (the symbol instant, before the filter's delay).
    #[inline]
    fn quiet_at(&self, t: f64) -> bool {
        self.is_quiet(self.env_at(t))
    }

    /// A mobile came up out of nothing: the levels so far are noise's. Keep
    /// the history's strong symbols; with too few for levels, start afresh.
    fn burst_began(&mut self) {
        let gate = (self.peak * self.floor.max(1e-30)).sqrt();
        let keep: Vec<(f32, f32)> = self.soft.iter().zip(&self.soft_env).filter(|(_, &e)| e >= gate).map(|(&v, &e)| (v, e)).collect();
        self.soft = keep.iter().map(|k| k.0).collect();
        self.soft_env = keep.iter().map(|k| k.1).collect();
        if self.soft.len() < 240 {
            self.center = 0.0;
            self.thr = 1200.0;
            self.separation = f32::INFINITY;
        }
        self.since_rails = self.opts.block;
        // The timing of blocks not emitted yet was found with the noise in:
        // again, without it.
        let b0 = (self.next_sym / self.opts.block).max(self.phase_base + 1);
        let keep = (b0 - self.phase_base) as usize;
        if keep < self.phase.len() {
            self.phase.truncate(keep);
            for b in b0..self.next_block {
                self.block_phase(b);
            }
        }
    }

    /// Only a bursty channel has quiet: its gaps at least BURSTY below its
    /// bursts (a continuous carrier's noise never dips that far for a whole
    /// window); then quiet is below the geometric middle of the two.
    #[inline]
    fn is_quiet(&self, env: f32) -> bool {
        self.peak > BURSTY * self.floor && env * env < self.peak * self.floor.max(1e-30)
    }

    #[inline]
    fn env_at(&self, t: f64) -> f32 {
        let i = (t - self.delay - self.y_base as f64).round();
        if i >= 0.0 && (i as usize) < self.envs.len() {
            self.envs[i as usize]
        } else {
            self.peak
        }
    }

    fn block_phase(&mut self, b: u64) {
        let (mut best, mut ph) = (-1.0f64, 0.0f64);
        for p in 0..self.steps {
            let cand = p as f64 / self.steps as f64 * self.sps;
            let mut e = 0.0f64;
            for s in b * self.opts.block..(b + 1) * self.opts.block {
                let t = cand + s as f64 * self.sps;
                if t < self.y_base as f64 || self.quiet_at(t) {
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

    /// Phase at symbol s: linear between block centres (b + 0.5)·block.
    fn phase_at(&self, s: u64) -> f64 {
        let x = s as f64 / self.opts.block as f64 - 0.5 - self.phase_base as f64;
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
        let up_to = (self.phase_base + self.phase.len() as u64 - 1) * self.opts.block + self.opts.block / 2;
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
            let env = self.env_at(t);
            self.slice(v, t - self.delay, env, out);
            self.next_sym += 1;
        }
    }

    fn slice(&mut self, v: f32, t: f64, env: f32, out: &mut Vec<Symbol>) {
        if self.is_quiet(env) {
            // Nothing on the air: a decision for the framer's count, worth nothing.
            let x = v - self.center;
            let dibit = if x >= self.thr { 0b01 } else if x >= 0.0 { 0b00 } else if x >= -self.thr { 0b10 } else { 0b11 };
            self.symbols += 1;
            out.push(Symbol { dibit, sample: t, rel_hi: 0.0, rel_lo: 0.0 });
            return;
        }
        // Rails from the last ~0.5 s of symbols, refreshed every block.
        self.soft.push_back(v);
        self.soft_env.push_back(env);
        if self.soft.len() > 2400 {
            self.soft.pop_front();
            self.soft_env.pop_front();
        }
        self.since_rails += 1;
        if self.since_rails >= self.opts.block && self.soft.len() >= 240 {
            self.since_rails = 0;
            // A bursty channel (a mobile coming up): what came before at noise
            // level is no help with the levels — out of the history for good.
            if self.peak > BURSTY * self.floor {
                let gate = (self.peak * self.floor.max(1e-30)).sqrt();
                let keep: Vec<(f32, f32)> = self.soft.iter().zip(&self.soft_env).filter(|(_, &e)| e >= gate).map(|(&v, &e)| (v, e)).collect();
                if keep.len() >= 240 {
                    self.soft = keep.iter().map(|k| k.0).collect();
                    self.soft_env = keep.iter().map(|k| k.1).collect();
                }
            }
            self.tmp.clear();
            if self.bursty {
                // A mobile: levels from the bursts' steady middles (their edges
                // ring through the filter from the quiet before and after).
                let full = 0.5 * self.peak;
                self.tmp.extend(self.soft.iter().zip(&self.soft_env).filter(|(_, &e)| e >= full).map(|(&v, _)| v));
            }
            if self.tmp.len() < 120 {
                self.tmp.clear();
                self.tmp.extend(self.soft.iter());
            }
            let n = self.tmp.len();
            let (lo, hi) = (n * 2 / 100, (n * 98 / 100).min(n - 1));
            let q_lo = *self.tmp.select_nth_unstable_by(lo, f32::total_cmp).1;
            let q_hi = *self.tmp.select_nth_unstable_by(hi, f32::total_cmp).1;
            self.center = (q_hi + q_lo) / 2.0;
            let outer = (q_hi - q_lo) / 2.0;
            self.thr = if outer > 300.0 { outer * 2.0 / 3.0 } else { 1200.0 };
            if self.opts.rail_means && outer > 300.0 {
                // Refine from the clusters: a few rounds of assigning symbols to
                // the nearest level and taking each level's mean.
                for _ in 0..3 {
                    let (mut s, mut n) = ([0.0f64; 4], [0u32; 4]);
                    for &v in &self.tmp {
                        let x = v - self.center;
                        let k = if x >= self.thr { 3 } else if x >= 0.0 { 2 } else if x >= -self.thr { 1 } else { 0 };
                        s[k] += v as f64;
                        n[k] += 1;
                    }
                    if n.iter().any(|&c| c < 8) {
                        break;
                    }
                    let m: Vec<f32> = (0..4).map(|k| (s[k] / n[k] as f64) as f32).collect();
                    // Spread of the symbols about their levels, against the level step.
                    let mut var = 0f64;
                    for &v in &self.tmp {
                        let k = (0..4).min_by(|&a, &b| (v - m[a]).abs().total_cmp(&(v - m[b]).abs())).unwrap();
                        var += ((v - m[k]) as f64).powi(2);
                    }
                    let sd = (var / self.tmp.len() as f64).sqrt() as f32;
                    self.separation = (m[3] - m[0]) / 3.0 / sd.max(1e-3);
                    self.center = (m[0] + m[3]) / 2.0;
                    // Thresholds halfway between the inner and outer levels.
                    self.thr = ((m[2] + m[3]) / 2.0 - self.center + self.center - (m[0] + m[1]) / 2.0) / 2.0;
                }
            }
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
        let keep_from = self.next_sym as f64 * self.sps + front - 2.0 * self.opts.block as f64 * self.sps;
        if keep_from > self.y_base as f64 + 8192.0 {
            let drop = (keep_from - self.y_base as f64) as usize;
            self.y.drain(..drop);
            self.envs.drain(..drop);
            self.y_base += drop as u64;
        }
        while self.phase.len() > 4 && (self.phase_base + 2) * self.opts.block + self.opts.block / 2 < self.next_sym {
            self.phase.pop_front();
            self.phase_base += 1;
        }
    }
}

impl C4fm {
    /// Matched filter (with taps) over the samples waiting in `fx`, onto `y`:
    /// each output Σ x·tap from the oldest sample, as a plain sum would
    /// (from −0.0, unfused), so the result is exact — but eight outputs at
    /// a time, one per SIMD lane, instead of one long dependent chain.
    fn filter(&mut self) {
        let Some(t) = &self.taps else { return };
        let (tl, x) = (t.len(), &self.fx);
        let mut j = self.fx_done;
        // Until there are taps−1 samples before it, an output sums fewer.
        while j < x.len() && j + 1 < tl {
            let off = tl - (j + 1);
            self.y.push(x[..=j].iter().zip(&t[off..]).map(|(a, b)| a * b).sum());
            j += 1;
        }
        const W: usize = 8;
        while j + W <= x.len() {
            let mut acc = [-0.0f32; W];
            for (k, &tk) in t.iter().enumerate() {
                let xs = &x[j + 1 + k - tl..][..W];
                for l in 0..W {
                    acc[l] += xs[l] * tk;
                }
            }
            self.y.extend_from_slice(&acc);
            j += W;
        }
        for j in j..x.len() {
            self.y.push(x[j + 1 - tl..=j].iter().zip(t).map(|(a, b)| a * b).sum());
        }
        let keep = (tl - 1).min(x.len());
        self.fx.drain(..x.len() - keep);
        self.fx_done = keep;
    }
}

impl Receiver for C4fm {
    fn push(&mut self, iq: &[Complex32], out: &mut Vec<Symbol>) {
        let k = (self.fs / (2.0 * PI)) as f32;
        // Clicks: a noise-driven phase wrap gives a spike far outside the rails.
        let lim = self.opts.clip.map(|c| c * (self.thr * 1.5).max(1500.0));
        let a_env = (1.0 / (self.sps * ENV_SYMBOLS)) as f32;
        let blk = ((self.fs * PEAK_WINDOW_S / PEAK_BLOCKS as f64) as usize).max(1);
        for &x in iq {
            let p = x.norm_sqr();
            self.env += a_env * (p - self.env);
            // The peak: the largest envelope in the last PEAK_WINDOW_S (block maxima).
            self.blk_max = self.blk_max.max(self.env);
            self.blk_min = self.blk_min.min(self.env);
            self.blk_n += 1;
            if self.blk_n >= blk {
                self.peaks.push_back(self.blk_max);
                self.floors.push_back(self.blk_min);
                if self.peaks.len() > PEAK_BLOCKS {
                    self.peaks.pop_front();
                    self.floors.pop_front();
                }
                self.peak = self.peaks.iter().copied().fold(0.0, f32::max);
                self.floor = self.floors.iter().copied().fold(f32::INFINITY, f32::min);
                let bursty = self.peak > BURSTY * self.floor;
                if bursty && !self.bursty {
                    // (It re-times blocks from the filtered signal so far.)
                    self.filter();
                    self.burst_began();
                }
                self.bursty = bursty;
                self.blk_max = 0.0;
                self.blk_min = f32::INFINITY;
                self.blk_n = 0;
            }
            self.envs.push(self.env);
            let mut f = (x * self.last.conj()).arg() * k;
            self.last = x;
            if let Some(l) = lim {
                f = (f - self.center).clamp(-l, l) + self.center;
            }
            // Nothing on the air: the discriminator is pure noise, which the
            // filter would smear into the next and last symbols of a burst.
            if self.is_quiet(self.env) {
                f = self.center;
            }
            if self.taps.is_some() {
                self.fx.push(f);
                continue;
            }
            self.hist.push_back(f);
            self.acc += f as f64;
            if self.hist.len() > self.boxw {
                self.acc -= self.hist.pop_front().unwrap() as f64;
            }
            self.y.push((self.acc / self.hist.len() as f64) as f32);
        }
        self.filter();
        while ((self.y_base + self.y.len() as u64) as f64) > ((self.next_block + 1) * self.opts.block) as f64 * self.sps + self.sps + 2.0 {
            self.block_phase(self.next_block);
            self.next_block += 1;
        }
        // A clean signal (level step ≥ ~10 spreads, ~15 dB) needs no MSD: off
        // above 11, on again below 9.
        if self.msd_on && self.separation > MSD_OFF {
            self.msd_on = false;
            if let Some(m) = self.msd.as_mut() {
                m.flush(out);
            }
        } else if !self.msd_on && self.separation < MSD_ON {
            self.msd_on = true;
        }
        if !self.msd_on {
            if let Some(m) = self.msd.as_mut() {
                m.skip(iq.len());
            }
        }
        match self.msd.as_mut().filter(|_| self.msd_on) {
            None => self.emit(out),
            Some(_) => {
                let mut syms = std::mem::take(&mut self.msd_in);
                syms.clear();
                self.emit(&mut syms);
                let lv = self.levels();
                self.msd.as_mut().unwrap().push(iq, &syms, lv, out);
                self.msd_in = syms;
            }
        }
        self.compact();
    }
}
