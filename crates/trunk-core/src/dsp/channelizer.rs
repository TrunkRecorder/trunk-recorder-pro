//! Shared fast-convolution channelizer (overlap-save), after CyberEther's
//! multi-head `filter_engine`. One forward N-point FFT of each wideband block
//! serves every channel ("head"); a head costs an M-bin window × filter
//! multiply, an M-point inverse FFT (which also decimates by D = N/M) and a
//! phase rotation. 32 channels at 2.4 MSPS cost ~1 % of one core.
//!
//! Timing is absolute: block b holds input samples [b·L, (b+1)·L), and output
//! sample k of every head corresponds to input sample k·D — so a head's output
//! is the same whether it was added live or replayed from history.
//!
//! Pre-roll: the last `history_s` of spectra are kept. A head added with
//! pre-roll replays them first (only M-point work per block), so a voice
//! channel opened by a grant starts before the grant arrived.
//!
//! Use: [`Channelizer::feed_u8`] / [`Channelizer::feed`] until they report a
//! block ran, then read each head's [`Channelizer::output`]; heads may be added
//! or removed between blocks.

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::sync::Arc;

use num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

pub type HeadId = u32;

struct Head {
    bin: usize,
    /// Output-rate NCO for the sub-bin residual, rad/sample.
    residual: f64,
    filter: usize,
    buf: Vec<Complex32>,
    out: Vec<Complex32>,
}

pub struct Channelizer {
    fs: f64,
    n: usize,
    p: usize,
    m: usize,
    l: usize,
    decim: usize,
    output_rate: f64,
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
    scratch_fwd: Vec<Complex32>,
    scratch_inv: Vec<Complex32>,
    x: Vec<Complex32>,
    fft_out: Vec<Complex32>,
    hist: Vec<Complex32>,
    spectra: Vec<Complex32>,
    hist_cap: usize,
    history_count: usize,
    fill: usize,
    block: u64,
    heads: BTreeMap<HeadId, Head>,
    filters: Vec<(i64, Vec<Complex32>)>,
    next_id: HeadId,
}

/// The largest power-of-two decimation of `fs` that still delivers at least
/// `min_output_rate`, capped at N/64 (M ≥ 64 bins per head).
pub fn output_rate(fs: f64, min_output_rate: f64, fft_size: usize) -> f64 {
    let mut d = 1usize;
    while d * 2 <= fft_size / 64 && fs / (d * 2) as f64 >= min_output_rate {
        d *= 2;
    }
    fs / d as f64
}

impl Channelizer {
    /// `fs`: wideband rate. `min_output_rate`: the lowest per-channel rate any
    /// decoder needs. `history_s`: seconds of spectra kept for pre-roll.
    pub fn new(fs: f64, min_output_rate: f64, history_s: f64) -> Self {
        Self::with_size(fs, min_output_rate, history_s, 16384, 4097)
    }

    pub fn with_size(fs: f64, min_output_rate: f64, history_s: f64, n: usize, p: usize) -> Self {
        let decim = (fs / output_rate(fs, min_output_rate, n)).round() as usize;
        let m = n / decim;
        let l = n - p + 1;
        assert!((p - 1) % decim == 0 && l % decim == 0, "channelizer: taps/FFT size incompatible with decimation");
        let mut planner = FftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(m);
        let hist_cap = ((history_s * fs / l as f64).ceil() as usize).max(1);
        Channelizer {
            fs,
            n,
            p,
            m,
            l,
            decim,
            output_rate: fs / decim as f64,
            scratch_fwd: vec![Complex32::default(); fwd.get_outofplace_scratch_len()],
            scratch_inv: vec![Complex32::default(); inv.get_inplace_scratch_len()],
            fwd,
            inv,
            x: vec![Complex32::default(); n],
            fft_out: vec![Complex32::default(); n],
            hist: vec![Complex32::default(); p - 1],
            spectra: vec![Complex32::default(); hist_cap * n],
            hist_cap,
            history_count: 0,
            fill: 0,
            block: 0,
            heads: BTreeMap::new(),
            filters: Vec::new(),
            next_id: 1,
        }
    }

    pub fn output_rate(&self) -> f64 {
        self.output_rate
    }
    pub fn fs(&self) -> f64 {
        self.fs
    }
    pub fn decim(&self) -> usize {
        self.decim
    }
    /// Seconds of air one block represents.
    pub fn block_seconds(&self) -> f64 {
        self.l as f64 / self.fs
    }
    /// Absolute input sample index at which the next block begins.
    pub fn sample_position(&self) -> u64 {
        self.block * self.l as u64 + self.fill as u64
    }
    /// Seconds of pre-roll currently available.
    pub fn history_seconds(&self) -> f64 {
        (self.history_count * self.l) as f64 / self.fs
    }
    pub fn fft_size(&self) -> usize {
        self.n
    }
    pub fn head_ids(&self) -> Vec<HeadId> {
        self.heads.keys().copied().collect()
    }
    /// A head's output for the block that just ran (empty before the first).
    pub fn output(&self, id: HeadId) -> Option<&[Complex32]> {
        self.heads.get(&id).map(|h| h.out.as_slice())
    }

    /// Start a channel at `offset_hz` from the tuned centre. Replays up to
    /// `preroll_s` of stored air, returned here for the caller to process
    /// before the head's live output. Also returns the absolute input sample
    /// the head's first output sample (replayed or live) corresponds to.
    pub fn add_head(&mut self, offset_hz: f64, cutoff_hz: f64, preroll_s: f64) -> (HeadId, Vec<Complex32>, u64) {
        let n = self.n as i64;
        let bin = (offset_hz / self.fs * self.n as f64).round() as i64;
        let residual_hz = offset_hz - bin as f64 * self.fs / self.n as f64;
        let filter = self.filter_for(cutoff_hz);
        let mut head = Head {
            bin: bin.rem_euclid(n) as usize,
            residual: -2.0 * PI * residual_hz / self.output_rate,
            filter,
            buf: vec![Complex32::default(); self.m],
            out: vec![Complex32::default(); self.l / self.decim],
        };
        // Blocks [block − replay, block − 1] are stored; the next block is live.
        let replay = self.history_count.min((preroll_s * self.fs / self.l as f64).ceil() as usize);
        let start_block = self.block - replay as u64;
        let mut pre = Vec::with_capacity(replay * head.out.len());
        for b in start_block..self.block {
            self.run_head(&mut head, b);
            pre.extend_from_slice(&head.out);
        }
        head.out.clear();
        let id = self.next_id;
        self.next_id += 1;
        self.heads.insert(id, head);
        (id, pre, start_block * self.l as u64)
    }

    pub fn remove_head(&mut self, id: HeadId) {
        self.heads.remove(&id);
    }

    /// RTL-SDR native unsigned 8-bit interleaved IQ. Consumes until a block
    /// completes (and runs it) or the input runs out; returns (bytes consumed,
    /// whether a block ran).
    pub fn feed_u8(&mut self, u8: &[u8]) -> (usize, bool) {
        let total = u8.len() / 2;
        let take = (self.l - self.fill).min(total);
        let base = self.p - 1 + self.fill;
        for (k, pair) in u8[..2 * take].chunks_exact(2).enumerate() {
            self.x[base + k] = Complex32::new((pair[0] as f32 - 127.5) * (1.0 / 127.5), (pair[1] as f32 - 127.5) * (1.0 / 127.5));
        }
        self.fill += take;
        let ran = self.fill == self.l;
        if ran {
            self.run_block();
        }
        (2 * take, ran)
    }

    /// Float IQ; as [`Channelizer::feed_u8`] but counting complex samples.
    pub fn feed(&mut self, iq: &[Complex32]) -> (usize, bool) {
        let take = (self.l - self.fill).min(iq.len());
        let base = self.p - 1 + self.fill;
        self.x[base..base + take].copy_from_slice(&iq[..take]);
        self.fill += take;
        let ran = self.fill == self.l;
        if ran {
            self.run_block();
        }
        (take, ran)
    }

    /// Power spectrum of the latest block, fft-shifted (first bin = −fs/2),
    /// averaged down to `bins`, in dBFS — free for a waterfall.
    pub fn power_spectrum(&self, bins: usize) -> Vec<f32> {
        let mut out = vec![-120.0f32; bins];
        if self.history_count == 0 || bins == 0 {
            return out;
        }
        let n = self.n;
        let slot = ((self.block - 1) as usize % self.hist_cap) * n;
        let per = n / bins;
        let norm = 1.0 / (n as f64 * n as f64);
        for (b, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for k in 0..per {
                let i = (b * per + k + n / 2) % n;
                acc += self.spectra[slot + i].norm_sqr() as f64;
            }
            *o = (10.0 * ((acc / per as f64) * norm + 1e-14).log10()) as f32;
        }
        out
    }

    /// Mean |X|² per bin of the latest block in `out.len()` equal cells
    /// (a divisor of the FFT size), fft-shifted (first cell starts at −fs/2);
    /// zeros before the first block. The survey's raw spectrum.
    pub fn cell_powers(&self, out: &mut [f32]) {
        if self.history_count == 0 || out.is_empty() {
            out.fill(0.0);
            return;
        }
        let n = self.n;
        let slot = ((self.block - 1) as usize % self.hist_cap) * n;
        let per = n / out.len();
        for (c, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for k in 0..per {
                acc += self.spectra[slot + (c * per + k + n / 2) % n].norm_sqr();
            }
            *o = acc / per as f32;
        }
    }

    /// Mean |X|² per bin over the bins within ±`half_width_hz` of `offset_hz`
    /// in the latest block (0 before the first). Same units as
    /// [`Channelizer::noise_profile`]: the energy detector for a channel,
    /// with no head running.
    pub fn band_power(&self, offset_hz: f64, half_width_hz: f64) -> f64 {
        if self.history_count == 0 {
            return 0.0;
        }
        let n = self.n as i64;
        let slot = ((self.block - 1) as usize % self.hist_cap) * self.n;
        let c = (offset_hz / self.fs * self.n as f64).round() as i64;
        let hw = ((half_width_hz / self.fs * self.n as f64).round() as i64).max(0);
        let mut acc = 0.0f64;
        for k in c - hw..=c + hw {
            acc += self.spectra[slot + k.rem_euclid(n) as usize].norm_sqr() as f64;
        }
        acc / (2 * hw + 1) as f64
    }

    /// The latest block's noise floor, as mean noise |X|² per bin in each of
    /// `out.len()` equal slices of the band (first slice at −fs/2). Each is
    /// the slice's median / ln 2 — noise bins are exponential, whose median is
    /// ln 2 × the mean — so signals filling under half a slice don't lift it,
    /// and the profile follows the SDR's passband shape.
    pub fn noise_profile(&self, out: &mut [f64]) {
        if self.history_count == 0 || out.is_empty() {
            out.fill(0.0);
            return;
        }
        let n = self.n;
        let slot = ((self.block - 1) as usize % self.hist_cap) * n;
        let per = n / out.len();
        let mut seg = vec![0.0f32; per];
        for (s, o) in out.iter_mut().enumerate() {
            for (k, v) in seg.iter_mut().enumerate() {
                *v = self.spectra[slot + (s * per + k + n / 2) % n].norm_sqr();
            }
            let (_, m, _) = seg.select_nth_unstable_by(per / 2, |a, b| a.total_cmp(b));
            *o = *m as f64 / std::f64::consts::LN_2;
        }
    }

    /// The noise power a filter of noise bandwidth `bandwidth_hz` passes
    /// (on a head's output, in |x|² units), given the per-bin noise from
    /// [`Channelizer::noise_profile`]: white noise of variance σ² gives
    /// E|X|² = N·σ² per bin, of which a band B keeps B/fs.
    pub fn noise_in_band(&self, bin_noise: f64, bandwidth_hz: f64) -> f64 {
        bin_noise / self.n as f64 * bandwidth_hz / self.fs
    }

    fn filter_for(&mut self, cutoff_hz: f64) -> usize {
        let key = cutoff_hz.round() as i64;
        if let Some(i) = self.filters.iter().position(|(k, _)| *k == key) {
            return i;
        }
        // Blackman windowed sinc → its N-point spectrum, keeping the M bins
        // around DC, with the overlap-save 1/N folded in. A direct f64 DFT of
        // just those bins keeps the stopband exact.
        let (n, p, m) = (self.n, self.p, self.m);
        let fc = cutoff_hz / self.fs;
        let mut h = vec![0.0f64; p];
        let mut sum = 0.0;
        for (i, v) in h.iter_mut().enumerate() {
            let k = i as f64 - (p - 1) as f64 / 2.0;
            let sinc = if k == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * k).sin() / (PI * k) };
            let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (p - 1) as f64).cos() + 0.08 * (4.0 * PI * i as f64 / (p - 1) as f64).cos();
            *v = sinc * w;
            sum += *v;
        }
        let f: Vec<Complex32> = (0..m)
            .map(|k| {
                let src = if k < m / 2 { k } else { n - m + k };
                let (mut ar, mut ai) = (0.0f64, 0.0f64);
                for (i, v) in h.iter().enumerate() {
                    let ph = -2.0 * PI * ((src * i) % n) as f64 / n as f64;
                    ar += v / sum * ph.cos();
                    ai += v / sum * ph.sin();
                }
                Complex32::new((ar / n as f64) as f32, (ai / n as f64) as f32)
            })
            .collect();
        self.filters.push((key, f));
        self.filters.len() - 1
    }

    fn run_block(&mut self) {
        let (n, p1) = (self.n, self.p - 1);
        // Overlap-save: the first P−1 samples are the previous block's tail.
        self.x[..p1].copy_from_slice(&self.hist);
        self.hist.copy_from_slice(&self.x[n - p1..]);
        // Out of place (x is refilled before the next block), then one
        // sequential copy into the history ring: the FFT's scattered writes
        // stay in cache, the ring (megabytes) is only streamed to.
        self.fwd.process_outofplace_with_scratch(&mut self.x, &mut self.fft_out, &mut self.scratch_fwd);
        let slot = (self.block as usize % self.hist_cap) * n;
        self.spectra[slot..slot + n].copy_from_slice(&self.fft_out);
        if self.history_count < self.hist_cap {
            self.history_count += 1;
        }
        let b = self.block;
        let mut heads = std::mem::take(&mut self.heads);
        for h in heads.values_mut() {
            self.run_head(h, b);
        }
        self.heads = heads;
        self.block += 1;
        self.fill = 0;
    }

    fn run_head(&mut self, h: &mut Head, b: u64) {
        let (n, m, l, decim) = (self.n, self.m, self.l, self.decim);
        let slot = (b as usize % self.hist_cap) * n;
        let s = &self.spectra[slot..slot + n];
        let f = &self.filters[h.filter].1;
        let half = m / 2;
        for k in 0..m {
            let src = if k < half { (h.bin + k) % n } else { (h.bin + n + k - m) % n };
            h.buf[k] = s[src] * f[k];
        }
        self.inv.process_with_scratch(&mut h.buf, &mut self.scratch_inv);
        // Overlap-save shift e^{−j2π·bin·L·b/N} (integer mod N: exact forever)
        // plus the residual NCO from the absolute output index.
        let shift_turns = (((h.bin as u64 * l as u64) % n as u64) * (b % n as u64) % n as u64) as f64 / n as f64;
        let n_out = l / decim;
        let first_out = b as f64 * n_out as f64;
        let keep_from = (self.p - 1) / decim;
        let ph = -2.0 * PI * shift_turns + (h.residual * first_out) % (2.0 * PI);
        let rot = Complex32::new(h.residual.cos() as f32, h.residual.sin() as f32);
        let mut c = Complex32::new(ph.cos() as f32, ph.sin() as f32);
        h.out.resize(n_out, Complex32::default());
        for j in 0..n_out {
            h.out[j] = h.buf[keep_from + j] * c;
            c *= rot;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(fs: f64, hz: f64, n: usize) -> Vec<Complex32> {
        (0..n).map(|i| {
            let ph = 2.0 * PI * hz * i as f64 / fs;
            Complex32::new(ph.cos() as f32, ph.sin() as f32)
        }).collect()
    }

    #[test]
    fn tone_lands_on_its_head_and_not_the_neighbour() {
        let fs = 2_400_000.0;
        let mut c = Channelizer::new(fs, 24_000.0, 0.1);
        let (on, _, _) = c.add_head(312_500.0, 7000.0, 0.0);
        let (off, _, _) = c.add_head(-600_000.0, 7000.0, 0.0);
        let x = tone(fs, 312_550.0, 12288 * 20);
        let (mut p_on, mut p_off, mut cnt) = (0.0f64, 0.0f64, 0);
        let mut i = 0;
        while i < x.len() {
            let (used, ran) = c.feed(&x[i..]);
            i += used;
            if ran && c.block > 4 {
                p_on += c.output(on).unwrap().iter().map(|v| v.norm_sqr() as f64).sum::<f64>();
                p_off += c.output(off).unwrap().iter().map(|v| v.norm_sqr() as f64).sum::<f64>();
                cnt += c.output(on).unwrap().len();
            }
        }
        let on_db = 10.0 * (p_on / cnt as f64).log10();
        let off_db = 10.0 * (p_off / cnt as f64 + 1e-30).log10();
        assert!(on_db.abs() < 0.5, "passband {on_db} dB");
        assert!(off_db < -100.0, "neighbour {off_db} dB");
    }

    fn gauss_noise(n: usize, sigma: f32) -> Vec<Complex32> {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut u = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        (0..n)
            .map(|_| {
                // Box–Muller: complex Gaussian with E|x|² = σ².
                let (a, b) = (u(), u());
                let r = (-a.ln()).sqrt() as f32 * sigma;
                Complex32::from_polar(r, (2.0 * PI * b) as f32)
            })
            .collect()
    }

    #[test]
    fn noise_floor_band_power_and_head_noise_agree() {
        let fs = 2_400_000.0;
        let mut c = Channelizer::new(fs, 24_000.0, 0.1);
        let (h, _, _) = c.add_head(100_000.0, 7000.0, 0.0);
        let mut x = gauss_noise(12288 * 40, 0.1);
        // An on-bin tone at −300 kHz adds N²·P to one bin; over the ±5 kHz
        // band's 69 bins that reads 1 + N·P/(69·σ²) above the floor: 20 dB.
        let bin_noise = 0.01 * 16384.0;
        let tone_pwr = 99.0 * 69.0 * 0.01 / 16384.0;
        for (i, v) in x.iter_mut().enumerate() {
            let ph = -2.0 * PI * 300_000.0 * i as f64 / fs;
            *v += Complex32::from_polar((tone_pwr as f32).sqrt(), ph as f32);
        }
        let (mut floor, mut head_p, mut quiet, mut loud, mut blocks) = (0.0, 0.0, 0.0, 0.0, 0);
        let mut prof = vec![0.0; 64];
        let mut i = 0;
        while i < x.len() {
            let (used, ran) = c.feed(&x[i..]);
            i += used;
            if ran && c.block > 2 {
                c.noise_profile(&mut prof);
                floor += prof.iter().sum::<f64>() / prof.len() as f64;
                quiet += c.band_power(500_000.0, 5000.0);
                loud += c.band_power(-300_000.0, 5000.0);
                let o = c.output(h).unwrap();
                head_p += o.iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / o.len() as f64;
                blocks += 1;
            }
        }
        let b = blocks as f64;
        let (floor, quiet, loud, head_p) = (floor / b, quiet / b, loud / b, head_p / b);
        let db = |r: f64| 10.0 * r.log10();
        assert!(db(floor / bin_noise).abs() < 0.3, "floor vs N·σ²: {:.2} dB", db(floor / bin_noise));
        assert!(db(quiet / floor).abs() < 0.3, "quiet band vs floor: {:.2} dB", db(quiet / floor));
        let snr = db(loud / floor);
        assert!((snr - 20.0).abs() < 0.5, "tone band {snr:.2} dB above floor");
        // A 7 kHz-cutoff head passes about 14 kHz of noise.
        let expect = c.noise_in_band(floor, 14_000.0);
        assert!(db(head_p / expect).abs() < 0.7, "head noise vs expected: {:.2} dB", db(head_p / expect));
    }
}
