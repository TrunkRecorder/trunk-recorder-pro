//! AM voice (conventional: VHF and UHF airband), from a channelizer head:
//!
//! ```text
//! channel IQ → ChannelFilter (±5.5 kHz for 25 kHz channels, ±3 kHz for
//!              8.33 kHz ones, ±8 kHz wide; also the carrier power meter)
//!   → envelope |y| → over the carrier level (its mean, tracked with a
//!     50 ms or 0.5 s time constant): the modulation, 100 % = ±1, whatever
//!     the signal strength, so a fading aircraft keeps its volume
//!   → noise limiter (±1.5: impulse noise is clipped, voice isn't)
//!   → audio low-pass + resample to 8 kHz → low cut (4th-order Butterworth,
//!     200 Hz: carrier hum and flutter; or none)
//!   → squelch gate (squelch::Gate, as FM's, opened and closed by the
//!     carrier's spectral line: CarrierLine)
//! ```
//!
//! The carrier is offset-tolerant: the envelope doesn't care where in the
//! passband it sits, so a wider filter takes a transmitter that is off
//! frequency (or an offset-carrier site) as it is.
//!
//! The squelch listens for the carrier, not the channel's power. At least
//! half of an AM signal's power is its carrier, one line in the spectrum,
//! while noise spreads over the whole channel: in 26 ms FFTs (38 Hz bins) a
//! carrier stands well clear of the noise long before the channel's power
//! does — ~9 dB sooner than ±5 kHz of band power — so a distant aircraft
//! opens it. Its threshold is the band meter's, moved to one bin and set
//! above where noise alone puts the strongest bin pair
//! ([`line_noise_peak`]); the conventional detector that opens the channel
//! measures the same way ([`LineWatch`]).
//!
//! A line detector also hears what band power can't: an SDR's spurs and
//! the birdies of nearby electronics, steady unmodulated lines that sit in
//! many channels. [`LineWatch`] learns them — a line there for a minute or
//! so (a few seconds when the channel starts) — and leaves their bins out,
//! and so does the demod's own detector, so one can neither open a channel
//! nor hold its audio open. Band power still opens both, as for FM: a
//! carrier strong enough for it counts however steady, so a continuous
//! broadcast (ATIS, AWOS) that the line detector learned is still heard.

use std::sync::Arc;

use num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use super::filters::{butter_highpass4, Biquad};
use super::fm::{AudioResampler, ChannelFilter};
use super::squelch::Gate;
use super::AnalogTuning;

/// The level's time constant, Fast and Slow, s.
pub const LEVEL_FAST_S: f64 = 0.05;
pub const LEVEL_SLOW_S: f64 = 0.5;
/// The default low cut, Hz.
pub const LOW_CUT_HZ: f64 = 200.0;
/// The default filter: a 25 kHz channel's (VHF), and wider from 225 MHz, Hz.
pub const HALF_BW_VHF: f64 = 5500.0;
pub const HALF_BW_UHF: f64 = 8000.0;
/// Where UHF military air starts, Hz.
const UHF_AIR_HZ: f64 = 225e6;
/// The band meter's fast window (a boxcar), s: long enough that the troughs
/// of a fully modulated carrier don't read as its going.
const FAST_S: f64 = 0.010;
/// The carrier detector's FFT, about this long, s (a power of two in samples).
const LINE_FFT_S: f64 = 0.02;
/// The carrier is looked for this far inside the channel filter's edge, Hz.
const LINE_EDGE_HZ: f64 = 1000.0;
/// The noise limiter, in full scale.
const LIMIT: f32 = 1.5;

/// The default filter half-width for a channel at `freq_hz`.
pub fn default_half_bw(freq_hz: f64) -> f64 {
    if freq_hz >= UHF_AIR_HZ { HALF_BW_UHF } else { HALF_BW_VHF }
}

/// Where pure noise puts the strongest of `pairs` adjacent-bin pairs, as
/// the pair's mean over the noise per bin: a carrier-line detector's 0 dB.
/// Two exponential bins sum to Gamma(2), P(S > x) = (1 + x)·e^−x; the
/// strongest of n sits near the x where n·P = 1, plus Euler's γ (its
/// spread is about exponential).
pub fn line_noise_peak(pairs: usize) -> f64 {
    let n = pairs.max(1) as f64;
    let mut x = n.ln().max(1.0);
    for _ in 0..30 {
        x = (n * (1.0 + x)).ln();
    }
    (x + std::f64::consts::EULER_GAMMA) / 2.0
}

/// Each bin's power smoothing, and its background's rising (and, in the
/// channel's first seconds, while it doesn't open, faster), s.
const BIN_TAU_S: f64 = 0.5;
const STEADY_TAU_S: f64 = 60.0;
const WARM_TAU_S: f64 = 2.0;
const WARM_S: f64 = 3.0;
/// A bin whose background is this far over the noise, and half its power, is a steady line.
const STEADY_OVER_NOISE: f64 = 4.0;

/// A conventional AM channel's carrier detector over the channelizer's
/// spectrum (its [`crate::dsp::Channelizer::band_bins`]): the strongest
/// pair of adjacent bins, against where noise alone puts it, leaving out
/// the steady lines it has learned. Each bin's power is smoothed, and a
/// background follows it down at once and up slowly; a bin whose
/// background has caught up with it (a line there for most of a minute)
/// and stands over the noise is steady, and with its neighbours left out.
/// For its first 3 s it learns fast and reads nothing, so the spurs there
/// from the start never open the channel.
#[derive(Default)]
pub struct LineWatch {
    smooth: Vec<f64>,
    background: Vec<f64>,
    steady: Vec<bool>,
    first_hz: f64,
    bin_hz: f64,
    age_s: f64,
    peak: f64,
}

impl LineWatch {
    /// One block: its `bins` (the first `first_hz` from the channel's
    /// centre, each `bin_hz` wide), the noise per bin, the block's length,
    /// and `a` the smoothing of the result. Returns the carrier's SNR over
    /// noise alone, dB.
    pub fn update(&mut self, bins: &[f64], first_hz: f64, bin_hz: f64, floor: f64, block_s: f64, a: f64) -> f64 {
        if self.smooth.len() != bins.len() {
            *self = LineWatch { smooth: bins.to_vec(), background: vec![floor; bins.len()], steady: vec![false; bins.len()], age_s: self.age_s, ..Default::default() };
        }
        (self.first_hz, self.bin_hz) = (first_hz, bin_hz);
        let rate = |tau: f64| 1.0 - (-block_s / tau).exp();
        let (fast, slow) = (rate(BIN_TAU_S), rate(if self.age_s < WARM_S { WARM_TAU_S } else { STEADY_TAU_S }));
        self.age_s += block_s;
        let mut line = vec![false; bins.len()];
        for (k, &b) in bins.iter().enumerate() {
            let s = &mut self.smooth[k];
            *s += fast * (b - *s);
            let bg = &mut self.background[k];
            *bg += if *s > *bg { slow } else { fast } * (*s - *bg);
            line[k] = *bg > STEADY_OVER_NOISE * floor && *bg > 0.5 * *s;
        }
        let n = bins.len();
        for k in 0..n {
            self.steady[k] = line[k] || (k > 0 && line[k - 1]) || (k + 1 < n && line[k + 1]);
        }
        let (mut best, mut pairs) = (0.0f64, 0);
        for k in 0..n.saturating_sub(1) {
            if !self.steady[k] && !self.steady[k + 1] {
                best = best.max(bins[k] + bins[k + 1]);
                pairs += 1;
            }
        }
        let best = best / 2.0;
        self.peak = if self.peak == 0.0 { best } else { self.peak + a * (best - self.peak) };
        if pairs == 0 || self.age_s < WARM_S {
            return f64::NEG_INFINITY;
        }
        10.0 * (self.peak / floor / line_noise_peak(pairs)).log10()
    }

    /// The steady lines' bands, Hz from the channel's centre (low, high).
    pub fn ignored(&self) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = Vec::new();
        for k in (0..self.steady.len()).filter(|&k| self.steady[k]) {
            let (lo, hi) = (self.first_hz + (k as f64 - 0.5) * self.bin_hz, self.first_hz + (k as f64 + 0.5) * self.bin_hz);
            match out.last_mut() {
                Some(r) if (r.1 - lo).abs() < 1e-6 => r.1 = hi,
                _ => out.push((lo, hi)),
            }
        }
        out
    }
}

/// The carrier's line in the channel: a rectangular FFT of the last `len`
/// channel samples every `len / 2`, its strongest adjacent-bin pair within
/// the search band against a threshold.
struct CarrierLine {
    fft: Arc<dyn Fft<f32>>,
    ring: Vec<Complex32>,
    pos: usize,
    since: usize,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// The bins searched, from the lowest frequency up (wrapping through 0),
    /// and which of them to leave out (a steady line's).
    lo: i64,
    hi: i64,
    skip: Vec<bool>,
    bin_hz: f64,
    /// The band meter's: density per unit of its power, × len × rate.
    per_bin: f64,
    /// The threshold per unit of the band meter's: the band's noise moved
    /// to one bin, times noise's strongest pair.
    scale: f64,
    up: bool,
}

impl CarrierLine {
    /// `ignore`: bands (Hz from the centre) with a steady line in them.
    fn new(rate: f64, half_bw: f64, ignore: &[(f64, f64)]) -> Self {
        let len = 2f64.powf((rate * LINE_FFT_S).log2().round()).max(64.0) as usize;
        let search = (half_bw - LINE_EDGE_HZ).max(half_bw / 2.0);
        let k = (search / rate * len as f64).round().max(1.0) as i64;
        let fft = FftPlanner::new().plan_fft_forward(len);
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        // Band power P through the meter (noise bandwidth 2·half_bw) is a
        // density P / (2·half_bw), a variance per sample of that × rate, and
        // E|X|² = len × that per bin.
        let per_bin = len as f64 * rate / (2.0 * half_bw);
        let mut c = CarrierLine {
            fft,
            ring: vec![Complex32::default(); len],
            pos: 0,
            since: 0,
            buf: vec![Complex32::default(); len],
            scratch,
            lo: -k,
            hi: k,
            skip: Vec::new(),
            bin_hz: rate / len as f64,
            per_bin,
            scale: 0.0,
            up: false,
        };
        c.set_ignore(ignore);
        c
    }

    /// Leave out the bins in (or a bin from) these bands, Hz from the centre.
    fn set_ignore(&mut self, ignore: &[(f64, f64)]) {
        let w = self.bin_hz;
        self.skip = (self.lo..=self.hi).map(|j| ignore.iter().any(|&(lo, hi)| j as f64 * w > lo - w && (j as f64 * w) < hi + w)).collect();
        let pairs = self.skip.windows(2).filter(|p| !p[0] && !p[1]).count();
        self.scale = self.per_bin * line_noise_peak(pairs);
    }

    /// How late it can tell the carrier has gone, s.
    fn lag_s(&self, rate: f64) -> f64 {
        self.ring.len() as f64 / rate
    }

    /// One filtered sample; `open_power`: the band meter's threshold. Whether a carrier is there.
    #[inline]
    fn push(&mut self, y: Complex32, open_power: f32) -> bool {
        let len = self.ring.len();
        self.ring[self.pos] = y;
        self.pos = (self.pos + 1) % len;
        self.since += 1;
        if self.since >= len / 2 {
            self.since = 0;
            for (i, b) in self.buf.iter_mut().enumerate() {
                *b = self.ring[(self.pos + i) % len];
            }
            self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            let bin = |k: i64| self.buf[k.rem_euclid(len as i64) as usize].norm_sqr() as f64;
            let mut best = 0.0f64;
            for k in self.lo..self.hi {
                let i = (k - self.lo) as usize;
                if !self.skip[i] && !self.skip[i + 1] {
                    best = best.max(bin(k) + bin(k + 1));
                }
            }
            self.up = best / 2.0 > open_power as f64 * self.scale;
        }
        self.up
    }
}

pub struct Am {
    filter: ChannelFilter,
    line: CarrierLine,
    /// The band meter's fast window: |y|² over the last `fast.len()` samples, and their sum.
    fast: Vec<f32>,
    fast_pos: usize,
    fast_sum: f64,
    was_up: bool,
    /// The carrier's mean envelope, and its smoothing per channel sample.
    level: f32,
    level_a: f32,
    audio: AudioResampler,
    hpf: Option<[Biquad; 2]>,
    gate: Gate,
}

impl Am {
    /// `ignore`: bands (Hz from the centre) with a steady line in them, which
    /// the squelch doesn't listen to ([`LineWatch::ignored`]).
    pub fn new(rate: f64, t: &AnalogTuning, ignore: &[(f64, f64)]) -> Self {
        let line = CarrierLine::new(rate, t.half_bw_hz, ignore);
        let gate = Gate::detected(rate, t.squelch, line.lag_s(rate).max(FAST_S));
        Am {
            filter: ChannelFilter::with_half_bw(rate, t.half_bw_hz),
            line,
            fast: vec![0.0; ((FAST_S * rate).round() as usize).max(1)],
            fast_pos: 0,
            fast_sum: 0.0,
            was_up: false,
            level: 0.0,
            level_a: (1.0 - (-1.0 / (rate * t.level_tau_s.max(0.005))).exp()) as f32,
            // Voice to 3.4 kHz, less in a narrow channel (8.33 kHz: 2.5 kHz).
            audio: AudioResampler::new(rate, (t.half_bw_hz - 500.0).clamp(2500.0, 3400.0)),
            hpf: (t.low_cut_hz > 0.0).then(|| butter_highpass4(t.low_cut_hz, super::fm::AUDIO_RATE)),
            gate,
        }
    }

    /// The steady lines changed ([`LineWatch::ignored`]).
    pub fn set_ignore(&mut self, ignore: &[(f64, f64)]) {
        self.line.set_ignore(ignore);
    }

    /// Demodulate `iq`, appending gated 8 kHz audio to `out`. `open_power`:
    /// the band power a carrier must have; its line, as far above the noise,
    /// will do too. Returns true if the carrier was up at any point.
    pub fn push(&mut self, iq: &[Complex32], open_power: f32, out: &mut Vec<f32>) -> bool {
        let mut carrier = false;
        let Am { filter, line, fast, fast_pos, fast_sum, was_up, level, level_a, audio, hpf, gate } = self;
        let n = fast.len();
        for &x in iq {
            let y = filter.step(x);
            let line_up = line.push(y, open_power);
            let p = y.norm_sqr();
            *fast_sum += (p - fast[*fast_pos]) as f64;
            fast[*fast_pos] = p;
            *fast_pos = (*fast_pos + 1) % n;
            if *fast_pos == 0 {
                // Re-sum now and then so rounding can't drift.
                *fast_sum = fast.iter().map(|&v| v as f64).sum();
            }
            let up = line_up || filter.power() > open_power;
            carrier |= up;
            gate.decide(up, line_up || *fast_sum > open_power as f64 * n as f64);
            let env = y.norm();
            let rising = up && !*was_up;
            *was_up = up;
            if rising {
                // A carrier from nothing: the level from its power, not from the noise's.
                *level = level.max(filter.power().sqrt());
            } else {
                *level += *level_a * (env - *level);
            }
            let m = ((env - *level) / level.max(1e-20)).clamp(-LIMIT, LIMIT);
            audio.push(m, |raw| {
                let s = match hpf {
                    Some(h) => h.iter_mut().fold(raw, |s, b| b.step(s)),
                    None => raw,
                };
                gate.audio(s, raw, out, None);
            });
        }
        carrier
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;
    use crate::dsp::fm::AUDIO_RATE;
    use crate::dsp::squelch::SquelchTiming;

    const RATE: f64 = 39_062.5;

    /// AM at `amp`, modulated `m` by `hz`, `off_hz` from the centre; `fade(t)` scales the carrier.
    fn am(n: usize, amp: f32, m: f64, hz: f64, off_hz: f64, fade: impl Fn(f64) -> f64) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let t = i as f64 / RATE;
                let a = amp as f64 * fade(t) * (1.0 + m * (2.0 * PI * hz * t).sin());
                Complex32::from_polar(a as f32, (2.0 * PI * off_hz * t) as f32)
            })
            .collect()
    }

    /// Goertzel power of `hz` in `x`, normalised to a unit sine → 0.5.
    fn tone_power(x: &[f32], hz: f64) -> f64 {
        let w = 2.0 * PI * hz / AUDIO_RATE;
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s = v as f64 + 2.0 * w.cos() * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2) * 2.0 / (x.len() as f64).powi(2)
    }

    fn db(p: f64, q: f64) -> f64 {
        10.0 * (p / q).log10()
    }

    fn noise(n: usize, sd: f32, seed: &mut u32) -> Vec<Complex32> {
        let mut rnd = || {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (*seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        (0..n).map(|_| Complex32::new(rnd(), rnd()) * sd).collect()
    }

    #[test]
    fn level_follows_modulation_not_strength() {
        // 50 % at 1 kHz → amplitude 0.5 (power 0.125), 40 dB apart in strength
        // and 2 kHz off frequency.
        for amp in [0.01f32, 1.0] {
            let mut d = Am::new(RATE, &AnalogTuning::am(120e6), &[]);
            let mut out = Vec::new();
            assert!(d.push(&am((RATE * 2.0) as usize, amp, 0.5, 1000.0, 2000.0, |_| 1.0), 1e-7, &mut out));
            let tail = &out[out.len() / 2..];
            let p = tone_power(tail, 1000.0);
            assert!(db(p, 0.125).abs() < 1.0, "amp {amp}: 1 kHz at {:.1} dB", db(p, 0.125));
        }
    }

    #[test]
    fn fading_keeps_the_volume() {
        // The carrier swings 12 dB at 0.5 Hz: the audio's level stays within ±1.5 dB.
        let fade = |t: f64| 10f64.powf(0.3 * (PI * t).sin());
        let mut d = Am::new(RATE, &AnalogTuning::am(120e6), &[]);
        let mut out = Vec::new();
        d.push(&am((RATE * 3.0) as usize, 0.1, 0.5, 1000.0, 0.0, fade), 1e-7, &mut out);
        let mut levels = Vec::new();
        for w in out[8000..].chunks(800) {
            levels.push(db(tone_power(w, 1000.0), 0.125));
        }
        let (lo, hi) = levels.iter().fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(lo > -1.5 && hi < 1.5, "level {lo:.1} … {hi:.1} dB");
    }

    #[test]
    fn low_cut_takes_hum() {
        // 60 Hz hum at 20 % beside 1 kHz voice at 50 %.
        let n = (RATE * 2.0) as usize;
        let iq: Vec<Complex32> = (0..n)
            .map(|i| {
                let t = i as f64 / RATE;
                Complex32::new((0.5 * (1.0 + 0.2 * (2.0 * PI * 60.0 * t).sin() + 0.5 * (2.0 * PI * 1000.0 * t).sin())) as f32, 0.0)
            })
            .collect();
        let run = |low_cut_hz: f64| {
            let mut d = Am::new(RATE, &AnalogTuning { low_cut_hz, ..AnalogTuning::am(120e6) }, &[]);
            let mut out = Vec::new();
            d.push(&iq, 1e-4, &mut out);
            let tail = &out[out.len() / 2..];
            db(tone_power(tail, 60.0), tone_power(tail, 1000.0))
        };
        assert!(run(200.0) < -40.0, "hum {:.1} dB under voice", -run(200.0));
        // With none, the hum stays (8 dB under: 20 % against 50 %).
        assert!((run(0.0) + 8.0).abs() < 1.5, "hum {:.1} dB with no low cut", run(0.0));
    }

    #[test]
    fn neighbours_dont_open_it() {
        // A strong carrier a channel away, 8.33 kHz in a narrow filter and 25 kHz
        // in the standard one, against a squelch set for a carrier 40 dB weaker.
        for (half, off) in [(3000.0, 8333.0), (HALF_BW_VHF, 25_000.0)] {
            let mut d = Am::new(RATE, &AnalogTuning { half_bw_hz: half, ..AnalogTuning::am(120e6) }, &[]);
            let mut out = Vec::new();
            // (Inside the channelizer's head this neighbour would already be
            // filtered.) The filter's start-up splatter is no test: skip it.
            let iq = am((RATE * 0.6) as usize, 1.0, 0.8, 1000.0, off, |_| 1.0);
            d.push(&iq[..(RATE * 0.1) as usize], 1e-4, &mut Vec::new());
            let carrier = d.push(&iq[(RATE * 0.1) as usize..], 1e-4, &mut out);
            assert!(!carrier && out.is_empty(), "±{half} Hz filter: a carrier {off} Hz away opened it");
        }
    }

    #[test]
    fn no_squelch_tail() {
        // A strong carrier (40 dB over the threshold) drops to noise 10 dB
        // under it: none of the noise may reach the output, at any hang.
        for hang_s in [0.002, 0.2] {
            let t = AnalogTuning { squelch: SquelchTiming { hang_s, ..Default::default() }, ..AnalogTuning::am(120e6) };
            let mut d = Am::new(RATE, &t, &[]);
            let mut out = Vec::new();
            d.push(&am((RATE * 0.5) as usize, 1.0, 0.5, 1000.0, 0.0, |_| 1.0), 1e-4, &mut out);
            let n_on = out.len();
            d.push(&noise((RATE * 0.5) as usize, 0.011, &mut 1), 1e-4, &mut out);
            assert!(out.len() > (0.4 * AUDIO_RATE) as usize, "hang {hang_s}: only {} samples", out.len());
            // The last 30 ms before the cut: voice at 0.5 and nothing louder.
            let peak = out[n_on.saturating_sub(240)..].iter().fold(0.0f32, |m, &v| m.max(v.abs()));
            assert!(peak < 0.6, "hang {hang_s}: squelch tail, peak {peak:.2}");
            let n = out.len();
            d.push(&noise((RATE * 0.5) as usize, 0.011, &mut 2), 1e-4, &mut out);
            assert_eq!(out.len(), n, "hang {hang_s}: noise let through");
        }
    }

    #[test]
    fn hang_bridges_a_fade() {
        // 1 s of carrier with a 40 ms dropout in the middle: a 100 ms hang
        // keeps it one stretch of audio, the 2 ms default cuts it in two.
        let n = (RATE * 1.0) as usize;
        let dropout = |t: f64| if (0.5..0.54).contains(&t) { 0.0 } else { 1.0 };
        let run = |hang_s: f64| {
            let t = AnalogTuning { squelch: SquelchTiming { hang_s, ..Default::default() }, ..AnalogTuning::am(120e6) };
            let mut d = Am::new(RATE, &t, &[]);
            let mut out = Vec::new();
            let mut iq = am(n, 0.5, 0.5, 1000.0, 0.0, dropout);
            iq.extend(vec![Complex32::default(); (RATE * 0.3) as usize]);
            d.push(&iq, 1e-4, &mut out);
            out.len() as f64 / AUDIO_RATE
        };
        let (long, short) = (run(0.1), run(0.002));
        assert!(long > 0.95, "100 ms hang: {long:.3} s of audio");
        // (Most of it: the carrier detector tells the carrier has gone a block late.)
        assert!(long - short > 0.025, "the 2 ms hang kept the fade too ({short:.3} s vs {long:.3} s)");
    }

    #[test]
    fn full_modulation_keeps_the_gate_open() {
        // 100 % at 300 Hz, the carrier only 10 dB over the threshold: the
        // troughs mustn't chop it.
        let mut d = Am::new(RATE, &AnalogTuning::am(120e6), &[]);
        let mut out = Vec::new();
        let amp = (1e-4f32 * 10.0).sqrt();
        d.push(&am((RATE * 1.0) as usize, amp, 1.0, 300.0, 0.0, |_| 1.0), 1e-4, &mut out);
        let got = out.len() as f64 / AUDIO_RATE;
        assert!(got > 0.93, "{got:.3} s of 1 s");
    }

    fn gauss(n: usize, sigma: f32, seed: &mut u64) -> Vec<Complex32> {
        let mut iq = vec![Complex32::default(); n];
        crate::nxdn::synth::add_noise(&mut iq, sigma, seed);
        iq
    }

    #[test]
    fn noise_alone_reads_zero_db() {
        // The detector's statistic over pure noise, averaged: where line_noise_peak says, at any rate.
        use crate::dsp::channelizer::floor_slices;
        use crate::dsp::Channelizer;
        for fs in [240_000.0, 2_400_000.0, 8_000_000.0] {
            let mut chz = Channelizer::new(fs, 24_000.0, 0.01);
            let (off, n) = (fs * 0.1, floor_slices(fs));
            let mut prof = vec![0.0; n];
            let (mut sum, mut blocks) = (0.0, 0);
            let mut bins = Vec::new();
            let mut seed = 7;
            let iq = gauss(60 * 16_384, 0.1, &mut seed);
            let mut at = 0;
            while at < iq.len() {
                let (took, ran) = chz.feed(&iq[at..]);
                at += took;
                if ran {
                    chz.noise_profile(&mut prof);
                    let slice = (((off + fs / 2.0) / fs * n as f64) as usize).min(n - 1);
                    let first = chz.band_bins(off, 5000.0, &mut bins);
                    // (A fresh watch each block, past its warm-up: the statistic unsmoothed, nothing learned.)
                    sum += 10f64.powf(LineWatch { age_s: WARM_S, ..Default::default() }.update(&bins, first, chz.bin_hz(), prof[slice], chz.block_seconds(), 1.0) / 10.0);
                    blocks += 1;
                }
            }
            let db = 10.0 * (sum / blocks as f64).log10();
            assert!(blocks >= 30 && db.abs() < 0.7, "{fs} S/s: noise reads {db:+.2} dB ({blocks} blocks)");
        }
    }

    #[test]
    fn a_weak_carrier_opens_the_gate() {
        // Noise σ² per sample: band power nb through the ±5.5 kHz meter. A
        // carrier with as much power as that band's noise (0 dB: 8 dB under
        // what the band meter would open at) opens it by its line; noise
        // alone doesn't.
        let t = AnalogTuning::am(120e6);
        let sigma = 0.05f32;
        let nb = (sigma as f64).powi(2) * 2.0 * t.half_bw_hz / RATE;
        let open_power = (nb * 10f64.powf(5.0 / 10.0)) as f32;
        let n = (RATE * 2.0) as usize;
        let mut seed = 3;
        let mut iq = am(n, (nb.sqrt() / 1.06) as f32, 0.5, 1000.0, 1500.0, |_| 1.0);
        for (x, v) in iq.iter_mut().zip(gauss(n, sigma / 2f32.sqrt(), &mut seed)) {
            *x += v;
        }
        let mut d = Am::new(RATE, &t, &[]);
        let mut out = Vec::new();
        assert!(d.push(&iq, open_power, &mut out));
        let got = out.len() as f64 / AUDIO_RATE;
        assert!(got > 1.85, "{got:.2} s of 2 s");
        // The tone is in it, well over the hiss around it.
        let tail = &out[out.len() / 2..];
        assert!(db(tone_power(tail, 1000.0), tone_power(tail, 1300.0)) > 15.0);
        let mut d = Am::new(RATE, &t, &[]);
        let mut out = Vec::new();
        let carrier = d.push(&gauss(n, sigma / 2f32.sqrt(), &mut seed), open_power, &mut out);
        assert!(!carrier && out.is_empty(), "noise alone: {} samples", out.len());
    }
}
