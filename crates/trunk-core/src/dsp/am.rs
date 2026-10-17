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
//!   → squelch gate (squelch::Gate, as FM's; its fast meter averages 10 ms,
//!     so the troughs of a fully modulated carrier don't close it)
//! ```
//!
//! The carrier is offset-tolerant: the envelope doesn't care where in the
//! passband it sits, so a wider filter takes a transmitter that is off
//! frequency (or an offset-carrier site) as it is.

use num_complex::Complex32;

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
/// The fast carrier meter's window, s.
const FAST_S: f64 = 0.010;
/// The noise limiter, in full scale.
const LIMIT: f32 = 1.5;

/// The default filter half-width for a channel at `freq_hz`.
pub fn default_half_bw(freq_hz: f64) -> f64 {
    if freq_hz >= UHF_AIR_HZ { HALF_BW_UHF } else { HALF_BW_VHF }
}

pub struct Am {
    filter: ChannelFilter,
    /// The carrier's mean envelope, and its smoothing per channel sample.
    level: f32,
    level_a: f32,
    audio: AudioResampler,
    hpf: Option<[Biquad; 2]>,
    gate: Gate,
}

impl Am {
    pub fn new(rate: f64, t: &AnalogTuning) -> Self {
        Am {
            filter: ChannelFilter::with_half_bw(rate, t.half_bw_hz),
            level: 0.0,
            level_a: (1.0 - (-1.0 / (rate * t.level_tau_s.max(0.005))).exp()) as f32,
            // Voice to 3.4 kHz, less in a narrow channel (8.33 kHz: 2.5 kHz).
            audio: AudioResampler::new(rate, (t.half_bw_hz - 500.0).clamp(2500.0, 3400.0)),
            hpf: (t.low_cut_hz > 0.0).then(|| butter_highpass4(t.low_cut_hz, super::fm::AUDIO_RATE)),
            gate: Gate::new(rate, t.squelch, FAST_S),
        }
    }

    /// Demodulate `iq`, appending gated 8 kHz audio to `out`. The gate opens
    /// while the carrier meter reads above `open_power`. Returns true if the
    /// carrier was up at any point.
    pub fn push(&mut self, iq: &[Complex32], open_power: f32, out: &mut Vec<f32>) -> bool {
        let mut carrier = false;
        // The carrier's amplitude at the threshold: the level never reads under it.
        let floor = open_power.max(1e-30).sqrt();
        let Am { filter, level, level_a, audio, hpf, gate } = self;
        for &x in iq {
            let y = filter.step(x);
            let up = filter.power() > open_power;
            carrier |= up;
            gate.meter(y, up, open_power);
            let env = y.norm();
            if up && *level < floor {
                // A carrier from nothing: start the level at its power, not from zero.
                *level = filter.power().sqrt();
            } else {
                *level += *level_a * (env - *level);
            }
            let m = ((env - *level) / level.max(floor)).clamp(-LIMIT, LIMIT);
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
            let mut d = Am::new(RATE, &AnalogTuning::am(120e6));
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
        let mut d = Am::new(RATE, &AnalogTuning::am(120e6));
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
            let mut d = Am::new(RATE, &AnalogTuning { low_cut_hz, ..AnalogTuning::am(120e6) });
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
            let mut d = Am::new(RATE, &AnalogTuning { half_bw_hz: half, ..AnalogTuning::am(120e6) });
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
            let mut d = Am::new(RATE, &t);
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
            let mut d = Am::new(RATE, &t);
            let mut out = Vec::new();
            let mut iq = am(n, 0.5, 0.5, 1000.0, 0.0, dropout);
            iq.extend(vec![Complex32::default(); (RATE * 0.3) as usize]);
            d.push(&iq, 1e-4, &mut out);
            out.len() as f64 / AUDIO_RATE
        };
        let (long, short) = (run(0.1), run(0.002));
        assert!(long > 0.95, "100 ms hang: {long:.3} s of audio");
        assert!(long - short > 0.04, "the 2 ms hang kept the fade too ({short:.3} s vs {long:.3} s)");
    }

    #[test]
    fn full_modulation_keeps_the_gate_open() {
        // 100 % at 300 Hz, the carrier only 10 dB over the threshold: the
        // troughs mustn't chop it.
        let mut d = Am::new(RATE, &AnalogTuning::am(120e6));
        let mut out = Vec::new();
        let amp = (1e-4f32 * 10.0).sqrt();
        d.push(&am((RATE * 1.0) as usize, amp, 1.0, 300.0, 0.0, |_| 1.0), 1e-4, &mut out);
        let got = out.len() as f64 / AUDIO_RATE;
        assert!(got > 0.93, "{got:.3} s of 1 s");
    }
}
