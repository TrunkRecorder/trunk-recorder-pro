//! Per-call loudness normalisation: every call's speech brought to one level,
//! as Trunk Recorder's uploads were (it runs ffmpeg's two-pass loudnorm, I −16
//! LUFS, on every call, whatever `audioPostprocess.enabled` says — about −18.6
//! LUFS measured on its WMATA M4As). The vocoder's own level follows the
//! talker: 13 dB between a quiet and a loud one is common.
//!
//! ```text
//! 20 ms frames → speech level: mean power of the frames above −50 dBFS and
//! within 10 dB of their mean (BS.1770's gates) → gain to TARGET_DB (−18 dB
//! up to +24 dB) → look-ahead peak limiter at −2.5 dBFS (headroom for the
//! overshoot between samples once resampled for AAC)
//! ```
//!
//! A call with under 0.2 s of speech-level frames — a keyed radio sending
//! IMBE silence, a squelch tail — is left alone: loudnorm brings dead air up
//! 50 dB into a steady hum.

const FRAME: usize = 160;
/// Speech level the gain aims for, dBFS (≈ −18.5 LUFS, Trunk Recorder's level).
pub const TARGET_DB: f64 = -16.5;
const MAX_GAIN_DB: f64 = 24.0;
const MIN_GAIN_DB: f64 = -18.0;
const ABS_GATE_DB: f64 = -50.0;
const REL_GATE_DB: f64 = 10.0;
const MIN_FRAMES: usize = 10;
/// Limiter: ceiling, look-ahead (samples either side of a peak) and release.
const CEILING: f32 = 0.75;
const LOOKAHEAD: usize = 16;
const RELEASE_S: f32 = 0.05;

/// The call's speech level, dBFS (None: too little speech to judge).
pub fn speech_level_db(x: &[f32]) -> Option<f64> {
    let p: Vec<f64> = x.chunks_exact(FRAME).map(|f| f.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / FRAME as f64).collect();
    let db = |p: f64| 10.0 * p.max(1e-20).log10();
    let loud: Vec<f64> = p.into_iter().filter(|&p| db(p) > ABS_GATE_DB).collect();
    if loud.len() < MIN_FRAMES {
        return None;
    }
    let gate = db(loud.iter().sum::<f64>() / loud.len() as f64) - REL_GATE_DB;
    let kept: Vec<f64> = loud.into_iter().filter(|&p| db(p) > gate).collect();
    (kept.len() >= MIN_FRAMES).then(|| db(kept.iter().sum::<f64>() / kept.len() as f64))
}

/// Normalise `x` (8 kHz, [−1, 1]) in place; returns the gain applied, dB.
pub fn normalize(x: &mut [f32], rate: u32) -> f64 {
    let Some(level) = speech_level_db(x) else { return 0.0 };
    let gain_db = (TARGET_DB - level).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    let g = 10f64.powf(gain_db / 20.0) as f32;
    for v in x.iter_mut() {
        *v *= g;
    }
    limit(x, rate);
    gain_db
}

/// Peak limiter: the gain each sample needs to stay under the ceiling, its
/// minimum over ±LOOKAHEAD (so the gain is down before a peak arrives), then
/// a release back up; a backward pass ramps the attack in over LOOKAHEAD.
fn limit(x: &mut [f32], rate: u32) {
    let n = x.len();
    if n == 0 || x.iter().all(|v| v.abs() <= CEILING) {
        return;
    }
    let need: Vec<f32> = x.iter().map(|v| if v.abs() > CEILING { CEILING / v.abs() } else { 1.0 }).collect();
    let mut g: Vec<f32> = (0..n).map(|i| need[i.saturating_sub(LOOKAHEAD)..(i + LOOKAHEAD + 1).min(n)].iter().copied().fold(1.0, f32::min)).collect();
    let rel = 1.0 - (-1.0 / (RELEASE_S * rate as f32)).exp();
    for i in 1..n {
        g[i] = g[i].min(g[i - 1] + (1.0 - g[i - 1]) * rel);
    }
    let att = 1.0 / LOOKAHEAD as f32;
    for i in (0..n - 1).rev() {
        g[i] = g[i].min(g[i + 1] + att);
    }
    for (v, g) in x.iter_mut().zip(g) {
        *v = (*v * g).clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32, secs: f64) -> Vec<f32> {
        (0..(8000.0 * secs) as usize).map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 8000.0).sin()).collect()
    }

    #[test]
    fn brings_quiet_and_loud_calls_to_the_target() {
        for amp in [0.03, 0.1, 0.5] {
            let mut x = tone(amp, 2.0);
            normalize(&mut x, 8000);
            let l = speech_level_db(&x).unwrap();
            assert!((l - TARGET_DB).abs() < 0.5, "amp {amp}: {l}");
        }
    }

    #[test]
    fn leaves_dead_air_alone() {
        let mut x = tone(0.0005, 3.0);
        let before = x.clone();
        assert_eq!(normalize(&mut x, 8000), 0.0);
        assert_eq!(x, before);
    }

    #[test]
    fn limits_peaks_without_hard_clipping() {
        // A quiet call with one loud burst: the burst is limited to the ceiling.
        let mut x = tone(0.02, 2.0);
        x.extend(tone(0.4, 0.1));
        x.extend(tone(0.02, 1.0));
        normalize(&mut x, 8000);
        let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak <= CEILING + 1e-3, "{peak}");
    }

    #[test]
    fn gain_is_capped() {
        let mut x = tone(0.006, 2.0);
        assert_eq!(normalize(&mut x, 8000), MAX_GAIN_DB);
    }
}
