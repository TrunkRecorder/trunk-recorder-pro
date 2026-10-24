//! FM voice (conventional analog, SmartNet analog), from a channelizer head:
//!
//! ```text
//! channel IQ → ChannelFilter (±5.5 kHz narrow, ±8 kHz wide; also the carrier power meter)
//!   → discriminator → de-emphasis (750 µs, unity at 1 kHz; or none)
//!   → audio low-pass + resample to 8 kHz → 300 Hz high-pass (drops CTCSS, all 67–254 Hz;
//!     `push_low` also hands over the audio before it, for inband::subaudible::ToneDetector)
//!   → squelch gate (squelch::Gate: carrier power vs a threshold, held 30 ms
//!     before it opens — a neighbour keying up splatters for an instant —
//!     10 ms ramps)
//! ```
//!
//! The gate closes on a 1 ms carrier meter, 2 ms after it falls under the
//! threshold, and cuts the audio from just before the carrier went: none of
//! the discriminator noise of a dropped carrier (the squelch tail) reaches
//! the output. The 10 ms meter alone took 50–80 ms to fall from a strong
//! carrier, and all of that was noise at full scale.
//!
//! Audio is emitted only while the gate is open: a transmission's gaps are
//! cut, as Trunk Recorder's squelched analog recorder does.

use std::f64::consts::PI;

use num_complex::Complex32;

use super::filters::{lowpass, Biquad};
use super::squelch::Gate;
use super::AnalogTuning;

/// Audio output rate, Hz.
pub const AUDIO_RATE: f64 = 8000.0;
/// One-sided passband of the channel filter / carrier meter, Hz (narrowband).
pub const CHANNEL_HALF_BW: f64 = 5500.0;
/// Peak deviation mapped to full scale ±1 (before de-emphasis gain), Hz.
const FULL_SCALE_DEV: f64 = 5000.0;
pub const DEEMPH_TAU: f64 = 750e-6;
/// Carrier power smoothing, s.
const POWER_TAU: f64 = 0.010;
/// Fast carrier meter window (a boxcar), s.
const FAST_S: f64 = 0.001;

/// A channel filter that also meters carrier power. Its output passband is
/// ±`half_bw` ([`CHANNEL_HALF_BW`] unless chosen), narrower than the
/// channelizer's head filter (whose transition band widens with the source
/// rate), so a strong neighbour a channel away doesn't read as carrier.
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
        Self::with_half_bw(rate, CHANNEL_HALF_BW)
    }

    /// Passband ±`half_bw` Hz; narrower than ±5 kHz gets twice the taps (a
    /// steeper skirt for 8.33 kHz channels).
    pub fn with_half_bw(rate: f64, half_bw: f64) -> Self {
        let n = if half_bw < 5000.0 { RING } else { 64 };
        ChannelFilter {
            taps: lowpass(n, half_bw / rate),
            ring: vec![Complex32::default(); RING],
            pos: 0,
            pwr: 0.0,
            alpha: (1.0 - (-1.0 / (rate * POWER_TAU)).exp()) as f32,
        }
    }

    /// Noise-equivalent bandwidth, Hz (for converting a noise density), of [`ChannelFilter::new`]'s.
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

/// 13th-order Chebyshev II high-pass at 8 kHz: 50 dB down from 256 Hz, so every
/// CTCSS tone (67–254.1 Hz) goes; 3 dB down at 288.5 Hz (the lowest two-tone
/// paging tone), flat from 310 Hz; odd order, so DC from a mistuned carrier
/// goes too. A 300 Hz Butterworth is only 10 dB down at 250 Hz. An elliptic is
/// steeper but rings at the band edge (MDC1200 / FleetSync decode less often),
/// and a linear-phase FIR's pre-ring smears voice into an unkey burst.
/// Sections as [b0, b1, b2, a1, a2] (scipy.signal.cheby2(13, 50, 256, 'high', fs=8000)).
const HPF_SOS: [[f64; 5]; 7] = [
    [0.649971201753611, -0.649971201753611, 0.0, -0.9009186289848935, 0.0],
    [1.0, -1.9976703966479996, 0.9999999999999998, -1.804773049479563, 0.8167223030504506],
    [1.0, -1.9912294244377715, 1.0, -1.813831989552296, 0.8317473057395659],
    [1.0, -1.9821827093882347, 1.0, -1.829691361845757, 0.8561967602247423],
    [1.0, -1.9726225689301102, 0.9999999999999997, -1.8532315922396563, 0.8891899815764512],
    [1.0, -1.964731673139806, 1.0, -1.8851956447367968, 0.9294929908328667],
    [1.0, -1.9602896689698817, 1.0, -1.925772087226872, 0.9754894659150795],
];

/// The audio low-pass and resampler to 8 kHz analog demods share: one
/// channel-rate sample in, each 8 kHz sample due by then out.
pub struct AudioResampler {
    taps: Vec<f32>,
    ring: Vec<f32>,
    /// Channel samples taken so far.
    n_in: u64,
    /// Channel-sample position of the next 8 kHz output.
    t_next: f64,
    step: f64,
}

const ARING: usize = 256;

impl AudioResampler {
    /// Low-pass at `cutoff_hz` for channel rate `rate`.
    pub fn new(rate: f64, cutoff_hz: f64) -> Self {
        AudioResampler { taps: lowpass(128, cutoff_hz / rate), ring: vec![0.0; ARING], n_in: 0, t_next: 0.0, step: rate / AUDIO_RATE }
    }

    #[inline]
    fn fir_at(&self, i: u64) -> f32 {
        // Output of the audio low-pass at channel sample i (needs i < n_in).
        let n = self.taps.len() as u64;
        let mut acc = 0.0f32;
        for (k, &t) in self.taps.iter().enumerate() {
            let j = i + k as u64 + 1 - n;
            acc += t * self.ring[(j % ARING as u64) as usize];
        }
        acc
    }

    #[inline]
    pub fn push(&mut self, x: f32, mut emit: impl FnMut(f32)) {
        let i = self.n_in;
        self.ring[(i % ARING as u64) as usize] = x;
        self.n_in += 1;
        let taps = self.taps.len() as u64;
        // Resample: every output instant between samples i−1 and i.
        while self.t_next + 1.0 <= i as f64 {
            let i0 = self.t_next.floor() as u64;
            if i0 + 1 >= taps {
                let f = (self.t_next - i0 as f64) as f32;
                let a = self.fir_at(i0);
                let b = self.fir_at(i0 + 1);
                emit(a + (b - a) * f);
            }
            self.t_next += self.step;
        }
    }
}

pub struct Nbfm {
    filter: ChannelFilter,
    last: Complex32,
    disc_gain: f32,
    de_a: f32,
    de_y: f32,
    de_gain: f32,
    audio: AudioResampler,
    hpf: [Biquad; HPF_SOS.len()],
    gate: Gate,
    data: Option<DataBurst>,
}

/// Fast data on an FM voice channel (EDACS's 9600 baud signalling): loud,
/// steady deviation that sits at two levels (not speech's peaks and
/// pauses, nor noise's clicks), with much of it above the voice band.
/// Measured on the discriminator over a few ms.
pub struct DataBurst {
    a: f32,
    prev: [f32; 2],
    p2: f32,
    p4: f32,
    ph: f32,
    /// Channel samples it has looked like data (or not) in a row.
    run: u32,
    quiet: u32,
    on: bool,
    /// Samples needed to start / end a burst.
    start: u32,
    end: u32,
    /// 8 kHz samples cut back when one starts.
    pub back: usize,
}

/// Deviation (RMS, of full scale: 5 kHz) data keeps up; speech's average is well under.
const DATA_RMS: f32 = 0.3;
/// (E d²)² / E d⁴: about 0.7 for data, 0.67 for a steady tone, ~0.3 for speech, ~0.2 for noise.
const DATA_FLAT: f32 = 0.45;
/// The second difference's power over the signal's: a tone in the voice band ~0.01, data 0.4+.
const DATA_HIGH: f32 = 0.2;

impl DataBurst {
    pub fn new(rate: f64) -> Self {
        let n = |s: f64| (s * rate) as u32;
        DataBurst {
            a: (-1.0 / (0.004 * rate)).exp() as f32,
            prev: [0.0; 2],
            p2: 0.0,
            p4: 0.0,
            ph: 0.0,
            run: 0,
            quiet: 0,
            on: false,
            start: n(0.008),
            end: n(0.010),
            back: ((0.004 + 0.008 + 0.004) * 8000.0) as usize,
        }
    }

    /// One discriminator sample; whether a burst is on.
    #[inline]
    pub fn step(&mut self, d: f32) -> bool {
        let h = d - 2.0 * self.prev[0] + self.prev[1];
        self.prev = [d, self.prev[0]];
        let b = 1.0 - self.a;
        let d2 = d * d;
        self.p2 = self.a * self.p2 + b * d2;
        self.p4 = self.a * self.p4 + b * d2 * d2;
        self.ph = self.a * self.ph + b * h * h;
        let looks = self.p2 > DATA_RMS * DATA_RMS && self.p2 * self.p2 > DATA_FLAT * self.p4 && self.ph > DATA_HIGH * self.p2;
        if looks {
            self.run = self.run.saturating_add(1);
            self.quiet = 0;
        } else {
            self.run = 0;
            self.quiet = self.quiet.saturating_add(1);
        }
        if !self.on && self.run >= self.start {
            self.on = true;
        } else if self.on && self.quiet >= self.end {
            self.on = false;
        }
        self.on
    }
}

impl Nbfm {
    /// Narrowband, 750 µs de-emphasis, the default squelch timing.
    pub fn new(rate: f64) -> Self {
        Self::with(rate, &AnalogTuning::default())
    }

    /// With `t`'s bandwidth, de-emphasis and squelch timing.
    pub fn with(rate: f64, t: &AnalogTuning) -> Self {
        let tau = t.deemph_tau_s;
        let (de_a, de_gain) = if tau > 0.0 {
            // De-emphasis gain at 1 kHz, undone so a 1 kHz tone keeps its level.
            (1.0 - (-1.0 / (rate * tau)).exp(), (1.0 + (2.0 * PI * 1000.0 * tau).powi(2)).sqrt())
        } else {
            (1.0, 1.0)
        };
        Nbfm {
            filter: ChannelFilter::with_half_bw(rate, t.half_bw_hz),
            last: Complex32::new(1.0, 0.0),
            disc_gain: (rate / (2.0 * PI) / FULL_SCALE_DEV) as f32,
            de_a: de_a as f32,
            de_y: 0.0,
            de_gain: de_gain as f32,
            audio: AudioResampler::new(rate, 3400.0),
            hpf: HPF_SOS.map(Biquad::new),
            gate: Gate::new(rate, t.squelch, FAST_S),
            data: t.data_mute.then(|| DataBurst::new(rate)),
        }
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
        let Nbfm { filter, last, disc_gain, de_a, de_y, de_gain, audio, hpf, gate, data } = self;
        for &x in iq {
            let y = filter.step(x);
            let up = filter.power() > open_power;
            carrier |= up;
            gate.meter(y, up, open_power);
            let d = (y * last.conj()).arg() * *disc_gain;
            *last = y;
            if let Some(db) = data {
                let on = db.step(d);
                gate.mute(on, db.back);
            }
            *de_y += *de_a * (d - *de_y);
            audio.push(*de_y * *de_gain, |raw| {
                let s = hpf.iter_mut().fold(raw, |s, h| h.step(s));
                gate.audio(s, raw, out, low.as_deref_mut());
            });
        }
        carrier
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::squelch::SquelchTiming;
    use crate::dsp::AnalogTuning;

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
        assert!((out.len() as f64 - (2.0 - SquelchTiming::default().open_s) * AUDIO_RATE).abs() < 60.0, "{} samples", out.len());
        let tail = &out[out.len() / 2..];
        let p1k = tone_power(tail, AUDIO_RATE, 1000.0);
        let p100 = tone_power(tail, AUDIO_RATE, 100.0);
        // 2.5 kHz / 5 kHz full scale → amplitude 0.5 → power 0.125.
        assert!((10.0 * (p1k / 0.125).log10()).abs() < 1.0, "1 kHz level {:.1} dB", 10.0 * (p1k / 0.125).log10());
        assert!(10.0 * (p100 / p1k).log10() < -40.0, "CTCSS {:.1} dB below voice", 10.0 * (p1k / p100).log10());
    }

    #[test]
    fn every_ctcss_tone_is_rejected() {
        let rate = 39_062.5;
        for tone in super::super::inband::subaudible::CTCSS.map(|t| t as f64 / 10.0) {
            let n = (rate * 2.0) as usize;
            let mut ph = 0.0f64;
            let iq: Vec<Complex32> = (0..n)
                .map(|i| {
                    let t = i as f64 / rate;
                    let f = 2500.0 * (2.0 * PI * 1000.0 * t).sin() + 700.0 * (2.0 * PI * tone * t).sin();
                    ph += 2.0 * PI * f / rate;
                    Complex32::from_polar(0.5, ph as f32)
                })
                .collect();
            let mut fm = Nbfm::new(rate);
            let mut out = Vec::new();
            fm.push(&iq, 1e-4, &mut out);
            let tail = &out[out.len() / 2..];
            let p1k = tone_power(tail, AUDIO_RATE, 1000.0);
            let pt = tone_power(tail, AUDIO_RATE, tone);
            let db = 10.0 * (pt / p1k).log10();
            assert!(db < -40.0, "{tone} Hz CTCSS only {:.1} dB below voice", -db);
        }
    }

    #[test]
    fn high_pass_edges() {
        // The filter alone at 8 kHz: gain at `hz`, dB.
        let gain = |hz: f64| {
            let mut hpf = HPF_SOS.map(Biquad::new);
            let x: Vec<f32> = (0..16_000).map(|i| (2.0 * PI * hz * i as f64 / AUDIO_RATE).sin() as f32).collect();
            let y: Vec<f32> = x.iter().map(|&v| hpf.iter_mut().fold(v, |s, h| h.step(s))).collect();
            10.0 * (tone_power(&y[8000..], AUDIO_RATE, hz) / 0.5).log10()
        };
        for hz in super::super::inband::subaudible::CTCSS.map(|t| t as f64 / 10.0) {
            assert!(gain(hz) < -45.0, "{hz} Hz only {:.1} dB down", -gain(hz));
        }
        for hz in [288.5, 313.0, 400.0, 1000.0, 3000.0] {
            assert!(gain(hz) > -3.5, "{hz} Hz {:.1} dB down", -gain(hz));
        }
    }

    /// FM IQ of instantaneous frequencies `f` (Hz) at `rate`.
    fn fm_of(rate: f64, f: impl Iterator<Item = f64>) -> Vec<Complex32> {
        let mut ph = 0.0f64;
        f.map(|f| {
            ph = (ph + 2.0 * PI * f / rate) % (2.0 * PI);
            Complex32::from_polar(0.5, ph as f32)
        })
        .collect()
    }

    /// EDACS's 9600 baud burst at a transmission's start is left out; speech
    /// (syllables of 1 and 2.5 kHz) and a steady tone aren't touched.
    #[test]
    fn data_bursts_are_left_out_and_voice_is_not() {
        let rate = 24_000.0;
        let burst = |n: usize| {
            let mut x = 0x1234_5678u32;
            let bits: Vec<f64> = (0..n)
                .map(|k| {
                    if k < 40 {
                        return if k % 2 == 0 { 1.0 } else { -1.0 };
                    }
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    if x & 1 == 1 { 1.0 } else { -1.0 }
                })
                .collect();
            let sps = rate / 9600.0;
            fm_of(rate, (0..(n as f64 * sps) as usize).map(move |i| 3000.0 * bits[(i as f64 / sps) as usize]))
        };
        let speech = |secs: f64| {
            fm_of(
                rate,
                (0..(secs * rate) as usize).map(|i| {
                    let t = i as f64 / rate;
                    let env = (0.5 - 0.5 * (2.0 * PI * 4.0 * t).cos()).powi(2);
                    env * (2000.0 * (2.0 * PI * 1000.0 * t).sin() + 1000.0 * (2.0 * PI * 2500.0 * t).sin())
                }),
            )
        };
        let run = |iq: &[Complex32], mute: bool| {
            let mut fm = Nbfm::with(rate, &AnalogTuning { data_mute: mute, ..Default::default() });
            let mut out = Vec::new();
            fm.push(iq, 1e-4, &mut out);
            out
        };
        let voice = speech(1.0);
        let mut with_burst = burst(1440); // 150 ms
        with_burst.extend(&voice);
        let plain = run(&voice, false).len();
        let cut = run(&with_burst, true).len();
        let kept = run(&with_burst, false).len();
        assert!(kept > plain + 1000, "without muting the burst is heard: {kept} vs {plain}");
        assert!(cut.abs_diff(plain) < 120, "the burst left out: {cut} samples, {plain} without it");
        // Voice alone, and a steady tone, are never taken for data.
        assert_eq!(run(&voice, true).len(), plain);
        let tone = fm_tone(rate, 1000.0, 3000.0, rate as usize, 0.5);
        assert_eq!(run(&tone, true).len(), run(&tone, false).len());
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
        // The delay, the fast meter's close and the ramp: ~28 ms.
        assert!(out.len() - n_on < 300, "gate stayed open: {} extra samples", out.len() - n_on);
        let n_off = out.len();
        assert!(!fm.push(&off, 1e-3, &mut out));
        assert_eq!(out.len(), n_off);
        assert!(n_on > 3500);
    }

    #[test]
    fn no_squelch_tail() {
        // A strong carrier (40 dB over the threshold) drops to noise: none of
        // the discriminator noise may reach the output.
        let rate = 37_500.0;
        let mut fm = Nbfm::new(rate);
        let mut out = Vec::new();
        let on = fm_tone(rate, 1000.0, 2500.0, (rate * 0.5) as usize, 1.0);
        fm.push(&on, 1e-4, &mut out);
        let n_on = out.len();
        let mut seed = 1u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        // Noise at ~1e-5, 10 dB under the threshold.
        let off: Vec<Complex32> = (0..(rate * 0.5) as usize).map(|_| Complex32::new(rnd(), rnd()) * 0.011).collect();
        fm.push(&off, 1e-4, &mut out);
        // The 1 kHz tone peaks at 0.5; discriminator noise runs to several times full scale.
        let peak = out[n_on..].iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        assert!(peak < 0.6, "squelch tail: peak {peak:.2} after the carrier dropped");
    }

    #[test]
    fn deemphasis_off_is_flat() {
        // Tones at 1 and 2 kHz, equal deviation and not pre-emphasised: off
        // keeps them level; 750 µs takes 2 kHz ~6 dB under 1 kHz.
        let rate = 39_062.5;
        let n = (rate * 2.0) as usize;
        let mut ph = 0.0f64;
        let iq: Vec<Complex32> = (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                let f = 1500.0 * (2.0 * PI * 1000.0 * t).sin() + 1500.0 * (2.0 * PI * 2000.0 * t).sin();
                ph += 2.0 * PI * f / rate;
                Complex32::from_polar(0.5, ph as f32)
            })
            .collect();
        let tilt = |tau: f64| {
            let mut fm = Nbfm::with(rate, &AnalogTuning { deemph_tau_s: tau, ..Default::default() });
            let mut out = Vec::new();
            fm.push(&iq, 1e-4, &mut out);
            let tail = &out[out.len() / 2..];
            10.0 * (tone_power(tail, AUDIO_RATE, 2000.0) / tone_power(tail, AUDIO_RATE, 1000.0)).log10()
        };
        assert!(tilt(0.0).abs() < 1.0, "off: 2 kHz {:.1} dB from 1 kHz", tilt(0.0));
        assert!(tilt(DEEMPH_TAU) < -4.5, "750 µs: 2 kHz only {:.1} dB", tilt(DEEMPH_TAU));
    }

    #[test]
    fn wide_filter_keeps_wide_deviation_clean() {
        // 25 kHz FM: 1 kHz at 5 kHz deviation spreads past the narrow filter
        // (distortion ~22 dB under the tone); the wide one keeps it clean.
        let rate = 39_062.5;
        let iq = fm_tone(rate, 1000.0, 5000.0, (rate * 2.0) as usize, 0.5);
        let thd = |half_bw_hz: f64| {
            let mut fm = Nbfm::with(rate, &AnalogTuning { half_bw_hz, deemph_tau_s: 0.0, ..Default::default() });
            let mut out = Vec::new();
            fm.push(&iq, 1e-4, &mut out);
            let tail = &out[out.len() / 2..];
            // All but the tone, against the tone.
            let tone = tone_power(tail, AUDIO_RATE, 1000.0);
            let all = tail.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / tail.len() as f64;
            10.0 * ((all - tone) / tone).log10()
        };
        let (narrow, wide) = (thd(CHANNEL_HALF_BW), thd(8000.0));
        assert!(wide < -40.0, "wide: distortion {wide:.1} dB");
        assert!(narrow > wide + 15.0, "narrow {narrow:.1} dB vs wide {wide:.1} dB");
    }
}
