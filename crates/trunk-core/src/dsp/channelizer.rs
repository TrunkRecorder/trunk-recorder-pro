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

use super::filter_bank::{self, BankHead, BIN_HZ, BLOCK_HOPS};
use super::filters::blackman_sinc;

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
    /// The wideband (input) sample rate; each head puts out `output_rate`.
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
    /// Fed channel blocks from an FPGA filter bank instead of wideband IQ ([`Channelizer::external`]).
    ext: Option<Box<External>>,
}

/// Bins the FPGA sends at once (`bank_wire::MAX_BINS`).
pub const MAX_BANK_BINS: usize = super::bank_wire::MAX_BINS;

/// Input samples the bank and its host head lag the FFT path by: the
/// prototype's and the head filter's group delays, less the FFT path's own.
/// The RTL adds nothing to it: hosts time blocks by their number, and the bank's
/// output matches the model sample for sample. (On the radio, block 0 starts 51 µs
/// into the stream, while the core clears its ring: small enough to ignore.)
const BANK_EXTRA_DELAY: u64 = 2048;
/// A filter bank's bin power (mean I² + Q² of its Q15 output, at
/// [`filter_bank::RADIO_GAIN_SHIFT`]) in a wideband FFT bin's |X|². Measured
/// by `the_banks_readings_match_the_wideband_path`.
const BIN_POWER_SCALE: f64 = 2.607e-4;
/// A filter bank head's samples, to read as the wideband path's do: the gain shift undone (same test).
const HEAD_SCALE: f32 = 1.0 / (1u32 << filter_bank::RADIO_GAIN_SHIFT) as f32;

/// The external backend: per bin, its recent air for pre-roll; per head, the
/// host's end of the channel (residual NCO and filter).
struct External {
    /// Each bin sent lately: its last `ring_cap` samples, whole blocks, ending with the latest.
    rings: BTreeMap<u16, std::collections::VecDeque<Complex32>>,
    ring_cap: usize,
    heads: BTreeMap<HeadId, (BankHead, Vec<Complex32>)>,
    /// Bins always wanted (control channels).
    pinned: Vec<u16>,
    /// Bins wanted for heads, with the block each was last asked for.
    used: BTreeMap<u16, u64>,
    /// Bins evicted since the caller last asked ([`Channelizer::take_evicted`]).
    evicted: Vec<u16>,
    noise: u64,
    /// Each bin's latest power, as mean |X|² per wideband FFT bin (so it's in
    /// the same units as the spectra of a [`Channelizer`] at this rate).
    power: Vec<f32>,
}

impl External {
    /// Faint noise (about −80 dBFS) where a bin sent nothing, as the engine's gaps.
    /// A packet's power slice into `power`. A value is a sum over the frames since the scan last
    /// passed that bin; with `count` bins a block, that's a block's frames × M / `count`.
    fn take_power(&mut self, start: u16, values: &[u32]) {
        if values.is_empty() {
            return;
        }
        let frames = (BLOCK_HOPS * filter_bank::M / values.len()) as f64;
        for (k, v) in values.iter().enumerate() {
            let bin = (start as usize + k) % filter_bank::M;
            self.power[bin] = (*v as f64 * (1u64 << super::bank_wire::POWER_SHIFT) as f64 / frames * BIN_POWER_SCALE) as f32;
        }
    }

    /// The band in `n` equal cells, each its mean power, first cell at −fs/2.
    fn cells(&self, n: usize, mut each: impl FnMut(usize, &mut dyn Iterator<Item = f32>)) {
        let m = filter_bank::M;
        let per = m / n;
        for c in 0..n {
            each(c, &mut (0..per).map(|k| self.power[(c * per + k + m / 2) % m]));
        }
    }

    fn quiet(&mut self) -> Complex32 {
        let mut r = || {
            self.noise ^= self.noise << 13;
            self.noise ^= self.noise >> 7;
            self.noise ^= self.noise << 17;
            (self.noise >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        Complex32::new(r(), r()) * 2e-4
    }
}

/// Narrowest noise-floor slice ([`floor_slices`]), Hz: a 12.5 kHz channel
/// fills under half of one, so a slice's median stays on the noise.
const MIN_FLOOR_SLICE_HZ: f64 = 30_000.0;

/// How many slices to measure the noise floor of a band `fs` wide in
/// ([`Channelizer::noise_profile`]): 64, or fewer (a power of two) where 64
/// would make them narrower than [`MIN_FLOOR_SLICE_HZ`]. A slice filled by a
/// signal reads the signal as the floor (64 slices at 480 kS/s are 7.5 kHz:
/// no conventional channel ever opened).
pub fn floor_slices(fs: f64) -> usize {
    let mut n = 64;
    while n > 1 && fs / (n as f64) < MIN_FLOOR_SLICE_HZ {
        n /= 2;
    }
    n
}

/// Which of an `n`-slice noise profile's slices ([`Channelizer::noise_profile`],
/// the first at −fs/2) a frequency `off_hz` from the centre falls in: the
/// noise floor under it.
pub fn floor_slice(n: usize, fs: f64, off_hz: f64) -> usize {
    ((off_hz + fs / 2.0) / fs * n as f64).floor().clamp(0.0, n as f64 - 1.0) as usize
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
            ext: None,
        }
    }

    /// Fed by an FPGA filter bank ([`Channelizer::feed_bank`]) at 8 MS/s, with
    /// the same rates, block and clock as [`Channelizer::new`] there. No
    /// spectrum: the power and noise readings come from the bank's own power
    /// slices ([`Channelizer::feed_bank`]), in the same units.
    pub fn external(min_output_rate: f64, history_s: f64) -> Self {
        let mut c = Self::new(filter_bank::FS, min_output_rate, 0.0);
        assert_eq!(c.output_rate, filter_bank::OUTPUT_RATE, "external channelizer: the bank's rate is fixed");
        assert_eq!(c.l, BLOCK_HOPS * filter_bank::HOP);
        c.spectra = Vec::new();
        c.ext = Some(Box::new(External {
            rings: BTreeMap::new(),
            ring_cap: (history_s * c.output_rate).ceil() as usize,
            heads: BTreeMap::new(),
            pinned: Vec::new(),
            used: BTreeMap::new(),
            evicted: Vec::new(),
            power: vec![0.0; filter_bank::M],
            noise: 0x9e37_79b9_7f4a_7c15,
        }));
        c
    }

    pub fn is_external(&self) -> bool {
        self.ext.is_some()
    }

    /// External: bins to keep on whatever heads come and go (control channels).
    pub fn pin_bins(&mut self, offsets_hz: &[f64]) {
        if let Some(e) = &mut self.ext {
            e.pinned = offsets_hz.iter().map(|&f| filter_bank::bin_for(f).0 as u16).collect();
            e.pinned.sort_unstable();
            e.pinned.dedup();
            e.pinned.truncate(MAX_BANK_BINS);
        }
    }

    /// External: the bins the FPGA should send, at most [`MAX_BANK_BINS`]:
    /// the pinned ones, then those heads used most recently.
    pub fn wanted_bins(&self) -> Vec<u16> {
        let Some(e) = &self.ext else { return Vec::new() };
        let mut v = e.pinned.clone();
        v.extend(e.used.keys().filter(|b| !e.pinned.contains(b)));
        v
    }

    /// External: bins dropped to make room since the last call, to log.
    pub fn take_evicted(&mut self) -> Vec<u16> {
        self.ext.as_mut().map_or_else(Vec::new, |e| std::mem::take(&mut e.evicted))
    }

    fn add_bank_head(&mut self, offset_hz: f64, cutoff_hz: f64, preroll_s: f64) -> (HeadId, Vec<Complex32>, u64) {
        let block = self.block;
        let e = self.ext.as_mut().unwrap();
        // The nearest bin, or a neighbour already sending that still covers the
        // channel: a head AutoTune moves across a bin edge keeps its bin.
        let (near, _) = filter_bank::bin_for(offset_hz);
        let m = filter_bank::M;
        let covers = |b: usize| {
            let signed = if b >= m / 2 { b as f64 - m as f64 } else { b as f64 };
            (offset_hz - signed * BIN_HZ).abs() <= BIN_HZ / 2.0 + 1000.0
        };
        let bin = [(near + m - 1) % m, (near + 1) % m]
            .into_iter()
            .find(|&b| covers(b) && !e.rings.contains_key(&(near as u16)) && e.rings.contains_key(&(b as u16)))
            .unwrap_or(near) as u16;
        let (first_block, pre_src): (u64, Vec<Complex32>) = match e.rings.get(&bin) {
            Some(ring) => {
                let want = ((preroll_s * self.output_rate).ceil() as usize).min(ring.len()) / BLOCK_HOPS * BLOCK_HOPS;
                (block - (want / BLOCK_HOPS) as u64, ring.range(ring.len() - want..).copied().collect())
            }
            None => (block, Vec::new()),
        };
        let mut head = BankHead::on_bin(bin as usize, offset_hz, cutoff_hz, first_block * BLOCK_HOPS as u64);
        let pre: Vec<Complex32> = pre_src.into_iter().map(|x| head.push(x)).collect();
        // Make room: the least recently used bin that no head is on and isn't pinned.
        if !e.pinned.contains(&bin) && !e.used.contains_key(&bin) && e.pinned.len() + e.used.len() >= MAX_BANK_BINS {
            let busy: Vec<u16> = e.heads.values().map(|(h, _)| h.bin as u16).collect();
            if let Some((&old, _)) = e.used.iter().filter(|(b, _)| !busy.contains(b)).min_by_key(|(_, t)| **t) {
                e.used.remove(&old);
                e.evicted.push(old);
            }
        }
        if !e.pinned.contains(&bin) {
            e.used.insert(bin, block);
        }
        let id = self.next_id;
        self.next_id += 1;
        e.heads.insert(id, (head, Vec::new()));
        (id, pre, (first_block * self.l as u64).saturating_sub(BANK_EXTRA_DELAY))
    }

    /// External: one block from the bank, `bins` (the table it was made with)
    /// × [`BLOCK_HOPS`] hops, hop by hop, Q15. Always a whole block: runs it.
    pub fn feed_bank(&mut self, bins: &[u16], iq: &[[i16; 2]], power_start: u16, power: &[u32]) {
        assert_eq!(iq.len(), bins.len() * BLOCK_HOPS, "feed_bank: a block is {BLOCK_HOPS} hops of every bin");
        self.ext.as_mut().expect("feed_bank: not an external channelizer").take_power(power_start, power);
        self.run_bank(Some((bins, iq)));
    }

    /// External: a block that never came; every head and ring gets quiet.
    pub fn feed_bank_gap(&mut self) {
        self.run_bank(None);
    }

    fn run_bank(&mut self, data: Option<(&[u16], &[[i16; 2]])>) {
        let block = self.block;
        let e = self.ext.as_mut().expect("feed_bank: not an external channelizer");
        let sent: &[u16] = data.map_or(&[], |d| d.0);
        e.rings.retain(|b, _| data.is_none() || sent.contains(b));
        let mut col = vec![Complex32::default(); BLOCK_HOPS];
        let column = |e: &mut External, i: Option<usize>, col: &mut Vec<Complex32>| {
            for (h, c) in col.iter_mut().enumerate() {
                *c = match (i, data) {
                    (Some(i), Some((bins, iq))) => filter_bank::q15(iq[h * bins.len() + i]) * HEAD_SCALE,
                    _ => e.quiet(),
                };
            }
        };
        let cap = e.ring_cap;
        for (i, &b) in sent.iter().enumerate() {
            column(e, Some(i), &mut col);
            let ring = e.rings.entry(b).or_default();
            ring.extend(col.iter().copied());
            while ring.len() > cap {
                ring.drain(..BLOCK_HOPS);
            }
        }
        if data.is_none() {
            let bins: Vec<u16> = e.rings.keys().copied().collect();
            for b in bins {
                column(e, None, &mut col);
                let ring = e.rings.get_mut(&b).unwrap();
                ring.extend(col.iter().copied());
                while ring.len() > cap {
                    ring.drain(..BLOCK_HOPS);
                }
            }
        }
        let ids: Vec<HeadId> = e.heads.keys().copied().collect();
        for id in ids {
            let bin = e.heads[&id].0.bin as u16;
            column(e, sent.iter().position(|&b| b == bin), &mut col);
            let (head, out) = e.heads.get_mut(&id).unwrap();
            out.clear();
            out.extend(col.iter().map(|&x| head.push(x)));
            if let Some(t) = e.used.get_mut(&bin) {
                *t = block;
            }
        }
        self.block += 1;
        self.fill = 0;
    }

    /// Each head's sample rate.
    pub fn output_rate(&self) -> f64 {
        self.output_rate
    }
    /// The wideband sample rate.
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
    pub fn fft_size(&self) -> usize {
        self.n
    }
    /// A head's output for the block that just ran (empty before the first).
    pub fn output(&self, id: HeadId) -> Option<&[Complex32]> {
        if let Some(e) = &self.ext {
            return e.heads.get(&id).map(|h| h.1.as_slice());
        }
        self.heads.get(&id).map(|h| h.out.as_slice())
    }

    /// Start a channel at `offset_hz` from the tuned centre. Replays up to
    /// `preroll_s` of stored air, returned here for the caller to process
    /// before the head's live output. Also returns the absolute input sample
    /// the head's first output sample (replayed or live) corresponds to.
    pub fn add_head(&mut self, offset_hz: f64, cutoff_hz: f64, preroll_s: f64) -> (HeadId, Vec<Complex32>, u64) {
        if self.ext.is_some() {
            return self.add_bank_head(offset_hz, cutoff_hz, preroll_s);
        }
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

    /// Removes the head. External: its bin stays wanted until evicted.
    pub fn remove_head(&mut self, id: HeadId) {
        if let Some(e) = &mut self.ext {
            e.heads.remove(&id);
        }
        self.heads.remove(&id);
    }

    /// RTL-SDR native unsigned 8-bit interleaved IQ. Consumes until a block
    /// completes (and runs it) or the input runs out; returns (bytes consumed,
    /// whether a block ran).
    pub fn feed_u8(&mut self, u8: &[u8]) -> (usize, bool) {
        assert!(self.ext.is_none(), "an external channelizer takes channel blocks");
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
        assert!(self.ext.is_none(), "an external channelizer takes channel blocks");
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
        if let Some(e) = &self.ext {
            if bins == 0 || bins > filter_bank::M {
                return out;
            }
            let norm = 1.0 / (self.n as f64 * self.n as f64);
            e.cells(bins, |c, p| {
                let (mut acc, mut n) = (0.0f64, 0usize);
                p.for_each(|v| {
                    acc += v as f64;
                    n += 1;
                });
                out[c] = (10.0 * ((acc / n as f64) * norm + 1e-14).log10()) as f32;
            });
            return out;
        }
        // (No more cells than bins.)
        if self.history_count == 0 || bins == 0 || bins > self.n {
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
        if self.history_count == 0 || out.is_empty() || out.len() > self.n {
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
        if let Some(e) = &self.ext {
            let c = (offset_hz / BIN_HZ).round() as i64;
            let hw = ((half_width_hz / BIN_HZ).round() as i64).max(0);
            let acc: f64 = (c - hw..=c + hw).map(|k| e.power[k.rem_euclid(filter_bank::M as i64) as usize] as f64).sum();
            return acc / (2 * hw + 1) as f64;
        }
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

    /// Each bin's |X|² within ±`half_width_hz` of `offset_hz` in the latest
    /// block, lowest frequency first, into `out` (same units as
    /// [`Channelizer::band_power`]); returns the first one's offset from
    /// `offset_hz`, Hz. Empty before the first block.
    pub fn band_bins(&self, offset_hz: f64, half_width_hz: f64, out: &mut Vec<f64>) -> f64 {
        out.clear();
        if let Some(e) = &self.ext {
            let c = (offset_hz / BIN_HZ).round() as i64;
            let hw = ((half_width_hz / BIN_HZ).round() as i64).max(1);
            out.extend((c - hw..=c + hw).map(|k| e.power[k.rem_euclid(filter_bank::M as i64) as usize] as f64));
            return (c - hw) as f64 * BIN_HZ - offset_hz;
        }
        if self.history_count == 0 {
            return 0.0;
        }
        let n = self.n as i64;
        let slot = ((self.block - 1) as usize % self.hist_cap) * self.n;
        let c = (offset_hz / self.fs * self.n as f64).round() as i64;
        let hw = ((half_width_hz / self.fs * self.n as f64).round() as i64).max(1);
        out.extend((c - hw..=c + hw).map(|k| self.spectra[slot + k.rem_euclid(n) as usize].norm_sqr() as f64));
        (c - hw) as f64 * self.fs / self.n as f64 - offset_hz
    }

    /// The width of a bin, Hz.
    pub fn bin_hz(&self) -> f64 {
        if self.ext.is_some() {
            return BIN_HZ;
        }
        self.fs / self.n as f64
    }

    /// The latest block's noise floor, as mean noise |X|² per bin in each of
    /// `out.len()` equal slices of the band (first slice at −fs/2). Each is
    /// the slice's median / ln 2 — noise bins are exponential, whose median is
    /// ln 2 × the mean — so signals filling under half a slice don't lift it,
    /// and the profile follows the SDR's passband shape.
    pub fn noise_profile(&self, out: &mut [f64]) {
        if let Some(e) = &self.ext {
            if out.is_empty() || out.len() > filter_bank::M {
                out.fill(0.0);
                return;
            }
            let mut seg = Vec::new();
            e.cells(out.len(), |c, p| {
                seg.clear();
                seg.extend(p);
                // A quarter of the way up: a 7.8 kHz bin is about as wide as a channel, so
                // busy slices have few bins of plain noise. No ln 2 here: a sum over
                // hundreds of frames is close to its mean, not exponential.
                let at = seg.len() / 4;
                let (_, m, _) = seg.select_nth_unstable_by(at, |a, b| a.total_cmp(b));
                out[c] = *m as f64;
            });
            return;
        }
        if self.history_count == 0 || out.is_empty() || out.len() > self.n {
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

    /// A carrier threshold: `db` above the noise in a band (as
    /// [`Channelizer::noise_in_band`]), the level a head's squelch or
    /// carrier meter compares against.
    pub fn threshold(&self, bin_noise: f64, bandwidth_hz: f64, db: f64) -> f32 {
        (self.noise_in_band(bin_noise, bandwidth_hz) * 10f64.powf(db / 10.0)) as f32
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
        let h = blackman_sinc(p, fc);
        let sum: f64 = h.iter().sum();
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

    #[test]
    fn floor_slices_stay_wide_enough() {
        for (fs, n) in [(3_200_000.0, 64), (2_400_000.0, 64), (2_048_000.0, 64), (1_024_000.0, 32), (960_000.0, 32), (480_000.0, 16), (240_000.0, 8), (20_000.0, 1)] {
            assert_eq!(floor_slices(fs), n, "{fs}");
        }
    }

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

    /// The external backend: bins shared, evicted, kept across an edge, and a
    /// head on a bin not sent yet gets quiet, then the bin once it comes, with
    /// pre-roll from a bin already sent.
    #[test]
    fn external_bins_are_shared_evicted_and_filled_in() {
        use super::super::filter_bank::{BIN_HZ, BLOCK_HOPS};
        let mut c = Channelizer::external(24_000.0, 0.1);
        c.pin_bins(&[10.0 * BIN_HZ]);
        let block = |c: &mut Channelizer, bins: &[u16], v: i16| c.feed_bank(bins, &vec![[v, 0]; bins.len() * BLOCK_HOPS], 0, &[]);
        // Two channels in one bin: wanted once, two heads.
        let (a, _, _) = c.add_head(20.0 * BIN_HZ + 1000.0, 7000.0, 0.0);
        let (b, _, _) = c.add_head(20.0 * BIN_HZ - 1000.0, 7000.0, 0.0);
        assert_eq!(c.wanted_bins(), vec![10, 20]);
        // Not sent yet: quiet, still a whole block.
        block(&mut c, &[10], 16384);
        let quiet = c.output(a).unwrap();
        assert_eq!(quiet.len(), BLOCK_HOPS);
        assert!(quiet.iter().all(|v| v.norm() < 1e-3));
        // Sent: the bin's level comes through, to both heads (after the head filter fills).
        for _ in 0..3 {
            block(&mut c, &[10, 20], 16384);
        }
        for h in [a, b] {
            // (A quarter of the Q15 level: the radio's gain shift undone, by HEAD_SCALE.)
            assert!((c.output(h).unwrap()[BLOCK_HOPS - 1].norm() - 0.125).abs() < 0.0125);
        }
        // Pre-roll replays the bin's stored blocks.
        let (_, pre, start) = c.add_head(20.0 * BIN_HZ, 7000.0, 1.0);
        assert_eq!(pre.len(), 3 * BLOCK_HOPS);
        assert_eq!(start, (12288 * 1u64).saturating_sub(BANK_EXTRA_DELAY));
        // A channel nudged past the bin edge stays on the bin being sent.
        let (edge, _, _) = c.add_head(20.5 * BIN_HZ + 200.0, 7000.0, 0.0);
        assert_eq!(c.ext.as_ref().unwrap().heads[&edge].0.bin, 20);
        // Fill the table: the oldest bin with no head goes; pinned and busy ones stay.
        c.remove_head(edge);
        let (old, _, _) = c.add_head(30.0 * BIN_HZ, 7000.0, 0.0);
        c.remove_head(old);
        block(&mut c, &[10, 20], 0);
        for k in 0..MAX_BANK_BINS - 3 {
            c.add_head((100 + k) as f64 * BIN_HZ, 7000.0, 0.0);
        }
        assert_eq!(c.wanted_bins().len(), MAX_BANK_BINS);
        assert!(c.take_evicted().is_empty());
        c.add_head(400.0 * BIN_HZ, 7000.0, 0.0);
        assert_eq!(c.take_evicted(), vec![30]);
        let w = c.wanted_bins();
        assert_eq!(w.len(), MAX_BANK_BINS);
        assert!(w.contains(&10) && w.contains(&20) && w.contains(&400) && !w.contains(&30));
    }

    /// The same white noise through the wideband path and through the bank
    /// (the fixed-point model, at the radio's gain, as packets): the floor and
    /// a channel's power must read alike, which is what the two scales are for.
    #[test]
    fn the_banks_readings_match_the_wideband_path() {
        use super::super::bank_wire::{parse, Body, Packer};
        use super::super::filter_bank::{Design, FixedBank, FS, OUTPUT_RATE, RADIO_GAIN_SHIFT};
        let mut x = 0x2545_f491_u64;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut gauss = || (-2.0 * (rnd() + 1e-12).ln()).sqrt() * (2.0 * std::f64::consts::PI * rnd()).cos();
        let block = 12288;
        let noise: Vec<Complex32> = (0..block * 40).map(|_| Complex32::new(0.08 * gauss() as f32, 0.08 * gauss() as f32)).collect();
        let offset = 100_000.0;

        let mut wide = Channelizer::new(FS, OUTPUT_RATE, 1.0);
        let (head, _, _) = wide.add_head(offset, 7000.0, 0.0);
        let mut wide_power = (0.0f64, 0usize);
        for b in noise.chunks(block).skip(10) {
            wide.feed(b);
            for v in wide.output(head).unwrap() {
                wide_power.0 += v.norm_sqr() as f64;
                wide_power.1 += 1;
            }
        }

        let mut ext = Channelizer::external(OUTPUT_RATE, 1.0);
        let (bank_head, _, _) = ext.add_head(offset, 7000.0, 0.0);
        let table = ext.wanted_bins();
        let mut bank = FixedBank::new(&Design { gain_shift: RADIO_GAIN_SHIFT, ..Default::default() });
        let mut packer = Packer::new(table.clone(), 1);
        let input: Vec<[i16; 2]> = noise.iter().map(|v| [(v.re * 2048.0).round() as i16, (v.im * 2048.0).round() as i16]).collect();
        let mut packets = Vec::new();
        bank.push(&input, |f| packets.extend(packer.push(f)));
        let mut bank_power = (0.0f64, 0usize);
        for (n, p) in packets.iter().enumerate() {
            let (got, _) = parse(&p.encode()).unwrap();
            let Body::Channels { iq, power_start, power, .. } = &got.body;
            ext.feed_bank(&table, iq, *power_start, power);
            if n >= 20 {
                for v in ext.output(bank_head).unwrap() {
                    bank_power.0 += v.norm_sqr() as f64;
                    bank_power.1 += 1;
                }
            }
        }

        let floor = |c: &Channelizer| {
            let mut p = vec![0.0; 16];
            c.noise_profile(&mut p);
            p.iter().sum::<f64>() / p.len() as f64
        };
        let floor_ratio = floor(&wide) / floor(&ext);
        let head_ratio = (wide_power.0 / wide_power.1 as f64) / (bank_power.0 / bank_power.1 as f64);
        let db = |r: f64| 10.0 * r.log10();
        eprintln!("floor ratio {floor_ratio} ({:.2} dB), head power ratio {head_ratio} ({:.2} dB, amplitude {})", db(floor_ratio), db(head_ratio), head_ratio.sqrt());
        assert!(db(floor_ratio).abs() < 0.5, "floor reads {:.2} dB off", db(floor_ratio));
        assert!(db(head_ratio).abs() < 0.5, "channel power reads {:.2} dB off", db(head_ratio));
    }
}
