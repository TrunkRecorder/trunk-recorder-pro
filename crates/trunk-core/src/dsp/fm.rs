//! Narrowband FM voice (conventional analog), from a channelizer head:
//!
//! ```text
//! channel IQ → ChannelFilter (±5.5 kHz; also the carrier power meter)
//!   → discriminator → de-emphasis (750 µs, unity at 1 kHz)
//!   → audio low-pass + resample to 8 kHz → 300 Hz high-pass (drops CTCSS;
//!     `push_low` also hands over the audio before it, for tones::ToneDetector)
//!   → squelch gate (carrier power vs a threshold, held 30 ms before it
//!     opens — a neighbour keying up splatters for an instant — 10 ms ramps)
//! ```
//!
//! Audio is emitted only while the gate is open: a transmission's gaps are
//! cut, as Trunk Recorder's squelched analog recorder does.

use std::f64::consts::PI;

use num_complex::Complex32;

use super::filters::lowpass;

/// Audio output rate, Hz.
pub const AUDIO_RATE: f64 = 8000.0;
/// One-sided passband of the channel filter / carrier meter, Hz.
pub const CHANNEL_HALF_BW: f64 = 5500.0;
/// Peak deviation mapped to full scale ±1 (before de-emphasis gain), Hz.
const FULL_SCALE_DEV: f64 = 5000.0;
pub const DEEMPH_TAU: f64 = 750e-6;
/// Carrier power smoothing, s.
const POWER_TAU: f64 = 0.010;
/// Gate ramp, s.
const RAMP_S: f64 = 0.010;
/// Carrier must hold this long before the gate opens, s.
const ATTACK_S: f64 = 0.030;

/// A narrow channel filter that also meters carrier power. Its output
/// passband is ±[`CHANNEL_HALF_BW`], narrower than the channelizer's head
/// filter (whose transition band widens with the source rate), so a strong
/// neighbour 12.5 kHz away doesn't read as carrier.
pub struct ChannelFilter {
    taps: Vec<f32>,
    ring: Vec<Complex32>,
    pos: usize,
    pwr: f32,
    alpha: f32,
}

const RING: usize = 128;

impl ChannelFilter {
    pub fn new(rate: f64) -> Self {
        ChannelFilter {
            taps: lowpass(64, CHANNEL_HALF_BW / rate),
            ring: vec![Complex32::default(); RING],
            pos: 0,
            pwr: 0.0,
            alpha: (1.0 - (-1.0 / (rate * POWER_TAU)).exp()) as f32,
        }
    }

    /// Noise-equivalent bandwidth, Hz (for converting a noise density).
    pub fn noise_bandwidth() -> f64 {
        2.0 * CHANNEL_HALF_BW
    }

    /// One sample in, the filtered sample out; updates the power meter.
    #[inline]
    pub fn step(&mut self, x: Complex32) -> Complex32 {
        self.ring[self.pos] = x;
        let mut acc = Complex32::default();
        let n = self.taps.len();
        for (k, &t) in self.taps.iter().enumerate() {
            acc += self.ring[(self.pos + RING - (n - 1 - k)) % RING] * t;
        }
        self.pos = (self.pos + 1) % RING;
        self.pwr += self.alpha * (acc.norm_sqr() - self.pwr);
        acc
    }

    /// Smoothed carrier power (same units as the input's |x|²).
    pub fn power(&self) -> f32 {
        self.pwr
    }

    /// Filter a block for its power only; returns the highest smoothed power seen.
    pub fn meter(&mut self, iq: &[Complex32]) -> f32 {
        let mut peak = 0.0f32;
        for &x in iq {
            self.step(x);
            peak = peak.max(self.pwr);
        }
        peak
    }
}

#[derive(Clone, Copy)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn highpass(rate: f64, f0: f64, q: f64) -> Self {
        let w0 = 2.0 * PI * f0 / rate;
        let (c, alpha) = (w0.cos(), w0.sin() / (2.0 * q));
        let a0 = 1.0 + alpha;
        Biquad {
            b: [((1.0 + c) / 2.0 / a0) as f32, (-(1.0 + c) / a0) as f32, ((1.0 + c) / 2.0 / a0) as f32],
            a: [(-2.0 * c / a0) as f32, ((1.0 - alpha) / a0) as f32],
            z: [0.0; 2],
        }
    }
    #[inline]
    fn step(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

pub struct Nbfm {
    filter: ChannelFilter,
    last: Complex32,
    disc_gain: f32,
    de_a: f32,
    de_y: f32,
    de_gain: f32,
    audio_taps: Vec<f32>,
    ring: Vec<f32>,
    /// Channel samples taken so far.
    n_in: u64,
    /// Channel-sample position of the next 8 kHz output.
    t_next: f64,
    step: f64,
    hpf: [Biquad; 3],
    gate: f32,
    ramp: f32,
    /// Channel samples the carrier has been up for, and how many opens the gate.
    up_run: u32,
    attack: u32,
}

const ARING: usize = 256;

impl Nbfm {
    pub fn new(rate: f64) -> Self {
        let de_a = 1.0 - (-1.0 / (rate * DEEMPH_TAU)).exp();
        // De-emphasis gain at 1 kHz, undone so a 1 kHz tone keeps its level.
        let de_gain = (1.0 + (2.0 * PI * 1000.0 * DEEMPH_TAU).powi(2)).sqrt();
        // 6th-order Butterworth high-pass at 300 Hz: CTCSS (67–254 Hz) out.
        // Section k's Q is 1 / (2 sin((2k − 1)π / 12)).
        let hpf = [1.0, 3.0, 5.0].map(|k: f64| Biquad::highpass(AUDIO_RATE, 300.0, 1.0 / (2.0 * (k * PI / 12.0).sin())));
        Nbfm {
            filter: ChannelFilter::new(rate),
            last: Complex32::new(1.0, 0.0),
            disc_gain: (rate / (2.0 * PI) / FULL_SCALE_DEV) as f32,
            de_a: de_a as f32,
            de_y: 0.0,
            de_gain: de_gain as f32,
            audio_taps: lowpass(128, 3400.0 / rate),
            ring: vec![0.0; ARING],
            n_in: 0,
            t_next: 0.0,
            step: rate / AUDIO_RATE,
            hpf,
            gate: 0.0,
            ramp: (1.0 / (RAMP_S * AUDIO_RATE)) as f32,
            up_run: 0,
            attack: (ATTACK_S * rate) as u32,
        }
    }

    #[inline]
    fn fir_at(&self, i: u64) -> f32 {
        // Output of the audio low-pass at channel sample i (needs i < n_in).
        let n = self.audio_taps.len() as u64;
        let mut acc = 0.0f32;
        for (k, &t) in self.audio_taps.iter().enumerate() {
            let j = i + k as u64 + 1 - n;
            acc += t * self.ring[(j % ARING as u64) as usize];
        }
        acc
    }

    /// Demodulate `iq`, appending gated 8 kHz audio to `out`. The gate opens
    /// while the carrier meter reads above `open_power`. Returns true if the
    /// carrier was up at any point.
    pub fn push(&mut self, iq: &[Complex32], open_power: f32, out: &mut Vec<f32>) -> bool {
        self.push_low(iq, open_power, out, None)
    }

    /// [`Nbfm::push`], also appending to `low` each output sample as it was
    /// before the high-pass and the gate's ramp (CTCSS and DCS still in it).
    pub fn push_low(&mut self, iq: &[Complex32], open_power: f32, out: &mut Vec<f32>, mut low: Option<&mut Vec<f32>>) -> bool {
        let mut carrier = false;
        let taps = self.audio_taps.len() as u64;
        for &x in iq {
            let y = self.filter.step(x);
            let up = self.filter.power() > open_power;
            carrier |= up;
            self.up_run = if up { self.up_run.saturating_add(1) } else { 0 };
            let open = self.up_run >= self.attack;
            let d = (y * self.last.conj()).arg() * self.disc_gain;
            self.last = y;
            self.de_y += self.de_a * (d - self.de_y);
            let i = self.n_in;
            self.ring[(i % ARING as u64) as usize] = self.de_y * self.de_gain;
            self.n_in += 1;
            // Resample: every output instant between samples i−1 and i.
            while self.t_next + 1.0 <= i as f64 {
                let i0 = self.t_next.floor() as u64;
                if i0 + 1 >= taps {
                    let f = (self.t_next - i0 as f64) as f32;
                    let a = self.fir_at(i0);
                    let b = self.fir_at(i0 + 1);
                    let mut s = a + (b - a) * f;
                    let raw = s;
                    for h in &mut self.hpf {
                        s = h.step(s);
                    }
                    let target = if open { 1.0 } else { 0.0 };
                    self.gate = if self.gate < target { (self.gate + self.ramp).min(1.0) } else { (self.gate - self.ramp).max(0.0) };
                    if self.gate > 0.0 {
                        out.push(s * self.gate);
                        if let Some(l) = low.as_deref_mut() {
                            l.push(raw);
                        }
                    }
                }
                self.t_next += self.step;
            }
        }
        carrier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm_tone(rate: f64, tone_hz: f64, dev_hz: f64, n: usize, amp: f32) -> Vec<Complex32> {
        let mut ph = 0.0f64;
        (0..n)
            .map(|i| {
                let f = dev_hz * (2.0 * PI * tone_hz * i as f64 / rate).sin();
                ph += 2.0 * PI * f / rate;
                Complex32::from_polar(amp, ph as f32)
            })
            .collect()
    }

    /// Goertzel power of `hz` in `x` at `fs`, normalised to a unit sine → 0.5.
    fn tone_power(x: &[f32], fs: f64, hz: f64) -> f64 {
        let w = 2.0 * PI * hz / fs;
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s = v as f64 + 2.0 * w.cos() * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2) * 2.0 / (x.len() as f64).powi(2)
    }

    #[test]
    fn tone_level_rate_and_ctcss_rejection() {
        let rate = 39_062.5;
        // 1 kHz voice tone at 2.5 kHz deviation (pre-emphasised: de-emphasis
        // is unity at 1 kHz) plus a 100 Hz CTCSS tone at 500 Hz deviation.
        let n = (rate * 2.0) as usize;
        let mut ph = 0.0f64;
        let iq: Vec<Complex32> = (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                let f = 2500.0 * (2.0 * PI * 1000.0 * t).sin() + 500.0 * (2.0 * PI * 100.0 * t).sin();
                ph += 2.0 * PI * f / rate;
                Complex32::from_polar(0.5, ph as f32)
            })
            .collect();
        let mut fm = Nbfm::new(rate);
        let mut out = Vec::new();
        assert!(fm.push(&iq, 1e-4, &mut out));
        // All but the 30 ms attack.
        assert!((out.len() as f64 - (2.0 - ATTACK_S) * AUDIO_RATE).abs() < 60.0, "{} samples", out.len());
        let tail = &out[out.len() / 2..];
        let p1k = tone_power(tail, AUDIO_RATE, 1000.0);
        let p100 = tone_power(tail, AUDIO_RATE, 100.0);
        // 2.5 kHz / 5 kHz full scale → amplitude 0.5 → power 0.125.
        assert!((10.0 * (p1k / 0.125).log10()).abs() < 1.0, "1 kHz level {:.1} dB", 10.0 * (p1k / 0.125).log10());
        assert!(10.0 * (p100 / p1k).log10() < -40.0, "CTCSS {:.1} dB below voice", 10.0 * (p1k / p100).log10());
    }

    #[test]
    fn gate_closes_without_carrier() {
        let rate = 37_500.0;
        let mut fm = Nbfm::new(rate);
        let mut out = Vec::new();
        let on = fm_tone(rate, 1000.0, 2500.0, (rate * 0.5) as usize, 0.5);
        fm.push(&on, 1e-3, &mut out);
        let n_on = out.len();
        let off = vec![Complex32::new(1e-3, 0.0); (rate * 0.5) as usize];
        fm.push(&off, 1e-3, &mut out);
        // The meter decays from 24 dB above the threshold (~55 ms) plus the ramp.
        assert!(out.len() - n_on < 800, "gate stayed open: {} extra samples", out.len() - n_on);
        let n_off = out.len();
        assert!(!fm.push(&off, 1e-3, &mut out));
        assert_eq!(out.len(), n_off);
        assert!(n_on > 3500);
    }
}
