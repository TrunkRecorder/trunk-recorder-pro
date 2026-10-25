//! Tones found in a call's audio: what P25 Phase 1 (whose vocoder has no
//! tone frames), analog FM and a half-rate radio that doesn't send tone
//! frames carry. Every 20 ms the last 40 ms is looked at:
//!
//! ```text
//! 8 kHz audio → 40 ms Hann window every 20 ms → 512-point spectrum
//!   one tone:  the strongest peak in 300–3500 Hz holds ≥ 80 % of the energy
//!              from 100 Hz up
//!   DTMF/KNOX: (looked for first) the two strongest hold ≥ 80 %, on the grid within
//!              1.5 %, the high tone at most 12 dB under the low (FM's
//!              de-emphasis takes 3–7) and not a harmonic of it (speech)
//! → runs of blocks alike (one tone within 1 %, or one key) → a tone when
//!   long enough: a key 40 ms; one tone 60 ms, steady in level and frequency
//!   (a vowel holds its pitch harmonic only roughly)
//! ```
//!
//! A frequency is the median of its blocks' (each interpolated between
//! spectrum bins): within a few tenths of a percent on clean audio, as
//! paging decoders need (±1.5 %). About 15 k flops per 20 ms.

use std::sync::{Arc, OnceLock};

use num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use super::{Chunk, Detector, Found, Origin, ToneKind, ToneSeg, DTMF_HZ, KEYS, KNOX_HZ};

/// Samples a block (20 ms), and the window looked at each block (40 ms).
pub const BLOCK: usize = 160;
const WINDOW: usize = 320;
const N: usize = 512;
const HZ_PER_BIN: f32 = 8000.0 / N as f32;
const BAND: (f32, f32) = (300.0, 3500.0);
/// A peak's share is of everything from here up: a voice's harmonic just
/// above the band stands alone in it, not beside the voice's lower ones.
const FLOOR_HZ: f32 = 100.0;
/// Quieter than this, dBFS: nothing (a tone remote's guard tone sits ~55 dB down).
const MIN_LEVEL_DB: f32 = -40.0;
const PURE: f32 = 0.8;
const KEY_TOL: f32 = 0.015;
const MIN_TWIST_DB: f32 = -12.0;
const MAX_TWIST_DB: f32 = 6.0;
const MIN_KEY_BLOCKS: usize = 2;
const MIN_TONE_BLOCKS: usize = 3;
/// A tone's blocks within this of their median frequency, and this many dB
/// apart at most (its first and last left out: the window half on it).
const STEADY_HZ: f32 = 0.006;
const STEADY_DB: f32 = 6.0;


/// What one block heard.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Heard {
    Tone { hz: f32, db: f32 },
    Key(ToneKind),
}

/// The FFT and window every call's detector shares.
fn plan() -> &'static (Arc<dyn Fft<f32>>, Vec<f32>) {
    static PLAN: OnceLock<(Arc<dyn Fft<f32>>, Vec<f32>)> = OnceLock::new();
    PLAN.get_or_init(|| {
        let window = (0..WINDOW).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / WINDOW as f32).cos()).collect();
        (FftPlanner::new().plan_fft_forward(N), window)
    })
}

pub struct AudioTones {
    buf: Vec<Complex32>,
    /// The last 40 ms (a ring: `head` is where the next sample goes), and how much of it is filled.
    hist: [f32; WINDOW],
    head: usize,
    filled: usize,
    /// Samples taken in since the last whole block.
    part: usize,
    /// The sample of the call's audio the next sample is.
    at: usize,
    /// The run of alike blocks: its first block's sample, and each block's.
    run: Vec<(usize, Heard)>,
}

impl Default for AudioTones {
    fn default() -> Self {
        AudioTones { buf: vec![Complex32::default(); N], hist: [0.0; WINDOW], head: 0, filled: 0, part: 0, at: 0, run: Vec::new() }
    }
}

impl Detector for AudioTones {
    /// Tones that ended go to `out`; blocks the tone frames named are left alone.
    fn push(&mut self, c: &Chunk, out: &mut Vec<Found>) {
        let mut segs = Vec::new();
        self.feed(c.audio, c.at, |p| c.named.iter().any(|r| r.contains(&p)), &mut segs);
        out.extend(segs.into_iter().map(Found::Tone));
    }

    /// The tone running at the end of the call, if it is one.
    fn finish(&mut self, out: &mut Vec<Found>) {
        let mut segs = Vec::new();
        self.close(&mut segs);
        out.extend(segs.into_iter().map(Found::Tone));
    }

    /// Where the tone being heard now began.
    fn busy_since(&self) -> Option<usize> {
        self.run.first().map(|r| r.0)
    }
}

impl AudioTones {
    /// `samples` of the call's audio, starting at its sample `at`; `skip(p)`:
    /// the block at sample `p` is accounted for (a tone frame named it).
    fn feed(&mut self, samples: &[f32], at: usize, skip: impl Fn(usize) -> bool, out: &mut Vec<ToneSeg>) {
        if at != self.at {
            // A gap in the audio (or a call's first): start afresh.
            self.close(out);
            self.filled = 0;
            self.part = 0;
            self.at = at;
        }
        for &x in samples {
            self.hist[self.head] = x;
            self.head = (self.head + 1) % WINDOW;
            self.filled = (self.filled + 1).min(WINDOW);
            self.at += 1;
            self.part += 1;
            if self.part < BLOCK {
                continue;
            }
            self.part = 0;
            let block = self.at - BLOCK;
            let heard = if self.filled < WINDOW || skip(block) { None } else { self.look() };
            self.add(block, heard, out);
        }
    }

    fn add(&mut self, block: usize, heard: Option<Heard>, out: &mut Vec<ToneSeg>) {
        let alike = |a: &Heard, b: &Heard| match (a, b) {
            (Heard::Tone { hz: x, .. }, Heard::Tone { hz: y, .. }) => (x - y).abs() <= 0.01 * x.max(*y),
            (Heard::Key(x), Heard::Key(y)) => x == y,
            _ => false,
        };
        match heard {
            Some(h) if self.run.first().is_some_and(|r| alike(&r.1, &h)) => self.run.push((block, h)),
            Some(h) => {
                self.close(out);
                self.run.push((block, h));
            }
            None => self.close(out),
        }
    }

    fn close(&mut self, out: &mut Vec<ToneSeg>) {
        let run = std::mem::take(&mut self.run);
        let Some(&(start, first)) = run.first() else { return };
        let len = run.len() * BLOCK;
        match first {
            Heard::Key(kind) if run.len() >= MIN_KEY_BLOCKS => out.push(ToneSeg { kind, start, len, level: None, origin: Origin::Audio }),
            Heard::Tone { .. } if run.len() >= MIN_TONE_BLOCKS => {
                let (mut hz, mut db): (Vec<f32>, Vec<f32>) = run.iter().filter_map(|r| if let Heard::Tone { hz, db } = r.1 { Some((hz, db)) } else { None }).unzip();
                hz.sort_by(f32::total_cmp);
                let f = hz[hz.len() / 2];
                let inner = if db.len() > 2 { &mut db[1..run.len() - 1] } else { &mut db[..] };
                let (lo, hi) = inner.iter().fold((f32::MAX, f32::MIN), |(l, h), &d| (l.min(d), h.max(d)));
                let steady = hz.iter().all(|&x| (x - f).abs() <= STEADY_HZ * f) && hi - lo <= STEADY_DB;
                if steady {
                    out.push(ToneSeg { kind: ToneKind::Single(f), start, len, level: None, origin: Origin::Audio });
                }
            }
            _ => {}
        }
    }

    /// The window now: one tone, a key, or neither.
    fn look(&mut self) -> Option<Heard> {
        let energy: f32 = self.hist.iter().map(|x| x * x).sum::<f32>() / WINDOW as f32;
        let (fft, window) = plan();
        let db = 10.0 * energy.max(1e-12).log10();
        if db < MIN_LEVEL_DB {
            return None;
        }
        // (Oldest first: the ring from `head` round.)
        let ordered = self.hist[self.head..].iter().chain(&self.hist[..self.head]);
        for (b, (x, w)) in self.buf.iter_mut().zip(ordered.zip(window)) {
            *b = Complex32::new(x * w, 0.0);
        }
        self.buf[WINDOW..].fill(Complex32::default());
        fft.process(&mut self.buf);
        let (k0, k1) = ((BAND.0 / HZ_PER_BIN) as usize, (BAND.1 / HZ_PER_BIN) as usize);
        let p: Vec<f32> = self.buf[..N / 2].iter().map(|c| c.norm_sqr()).collect();
        let total: f32 = p[(FLOOR_HZ / HZ_PER_BIN) as usize..=k1].iter().sum::<f32>().max(1e-20);
        let peak = |skip: Option<usize>| (k0..=k1).filter(|&k| skip.is_none_or(|s| k.abs_diff(s) > 6)).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap();
        // A Hann window's main lobe spans ±3 of these bins.
        let share = |k: usize| p[k.saturating_sub(3)..=(k + 3).min(N / 2 - 1)].iter().sum::<f32>() / total;
        let hz = |k: usize| {
            let (a, b, c) = (p[k - 1].max(1e-30).ln(), p[k].max(1e-30).ln(), p[k + 1].max(1e-30).ln());
            let d = a - 2.0 * b + c;
            (k as f32 + if d.abs() > 1e-12 { 0.5 * (a - c) / d } else { 0.0 }) * HZ_PER_BIN
        };
        // A key first: its low tone alone, the high one well down (FM's
        // de-emphasis), would pass for one tone.
        let k = peak(None);
        let j = peak(Some(k));
        let (s1, s2) = (share(k), share(j));
        if s1 + s2 >= PURE && s1.min(s2) >= 0.02 {
            let ((lo, plo), (hi, phi)) = if hz(k) < hz(j) { ((hz(k), p[k]), (hz(j), p[j])) } else { ((hz(j), p[j]), (hz(k), p[k])) };
            let twist = 10.0 * (phi / plo.max(1e-30)).log10();
            let ratio = hi / lo;
            let on = |grid: &[f32; 4], f: f32| grid.iter().position(|&g| (f - g).abs() <= KEY_TOL * g);
            if (MIN_TWIST_DB..=MAX_TWIST_DB).contains(&twist) && (ratio - ratio.round()).abs() >= 0.04 {
                for (grid, knox) in [(DTMF_HZ, false), (KNOX_HZ, true)] {
                    if let (Some(r), Some(c)) = (on(&grid.0, lo), on(&grid.1, hi)) {
                        let key = KEYS[r][c];
                        return Some(Heard::Key(if knox { ToneKind::Knox(key) } else { ToneKind::Dtmf(key) }));
                    }
                }
            }
        }
        (s1 >= PURE).then(|| Heard::Tone { hz: hz(k), db })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 8000.0;

    fn tone(out: &mut Vec<f32>, hz: &[f32], amp: &[f32], secs: f32) {
        let n0 = out.len();
        for n in 0..(secs * SR) as usize {
            let t = (n0 + n) as f32 / SR;
            out.push(hz.iter().zip(amp).map(|(f, a)| a * (2.0 * std::f32::consts::PI * f * t).sin()).sum());
        }
    }

    fn quiet(out: &mut Vec<f32>, secs: f32) {
        out.extend(std::iter::repeat_n(0.0, (secs * SR) as usize));
    }

    fn segs(found: Vec<Found>) -> Vec<ToneSeg> {
        found.into_iter().map(|f| if let Found::Tone(t) = f { t } else { unreachable!() }).collect()
    }

    fn run(audio: &[f32], chunk: usize) -> Vec<ToneSeg> {
        let mut d = AudioTones::default();
        let mut out = Vec::new();
        for (i, c) in audio.chunks(chunk).enumerate() {
            d.push(&Chunk { audio: c, at: i * chunk, named: &[] }, &mut out);
        }
        d.finish(&mut out);
        segs(out)
    }

    #[test]
    fn a_page_is_two_tones_measured_closely() {
        // Woodbridge FD's page as Phase 1 voice brings it: 467.2 Hz 1 s, 1032.3 Hz 3 s.
        let mut a = Vec::new();
        quiet(&mut a, 0.3);
        tone(&mut a, &[467.2], &[0.4], 1.0);
        tone(&mut a, &[1032.3], &[0.4], 3.0);
        quiet(&mut a, 0.3);
        let segs = run(&a, 333);
        assert_eq!(segs.len(), 2, "{segs:?}");
        let f = |s: &ToneSeg| if let ToneKind::Single(f) = s.kind { f } else { 0.0 };
        assert!((f(&segs[0]) - 467.2).abs() < 1.0 && (f(&segs[1]) - 1032.3).abs() < 1.0, "{segs:?}");
        assert!((segs[0].len as f32 / SR - 1.0).abs() < 0.06 && (segs[1].len as f32 / SR - 3.0).abs() < 0.06, "{segs:?}");
    }

    #[test]
    fn dtmf_with_fm_twist() {
        // Woodbridge's siren code: 0.24 s keys every 0.4 s, the high tone 6 dB down.
        let mut a = Vec::new();
        quiet(&mut a, 0.2);
        for key in [(941.0, 1336.0), (852.0, 1477.0), (697.0, 1209.0)] {
            tone(&mut a, &[key.0, key.1], &[0.3, 0.15], 0.24);
            quiet(&mut a, 0.16);
        }
        let keys: String = run(&a, 160).iter().filter_map(|s| if let ToneKind::Dtmf(c) = s.kind { Some(c) } else { None }).collect();
        assert_eq!(keys, "091");
    }

    #[test]
    fn harmonics_and_wobble_are_not_tones() {
        // A vowel: a 120 Hz voice's harmonics, its pitch wandering.
        let mut a = Vec::new();
        for n in 0..(1.0 * SR) as usize {
            let t = n as f32 / SR;
            let f0 = 120.0 + 15.0 * (2.0 * std::f32::consts::PI * 3.0 * t).sin();
            a.push((1..=8).map(|h| (0.3 / h as f32) * (2.0 * std::f32::consts::PI * f0 * h as f32 * t).sin()).sum());
        }
        let v = run(&a, 160);
        assert!(v.is_empty(), "{v:?}");
        // A pair in 2:1 (a voice's harmonic) is no key, even near the grid.
        let mut a = Vec::new();
        tone(&mut a, &[697.0, 1394.0], &[0.3, 0.3], 0.5);
        assert!(run(&a, 160).iter().all(|s| !matches!(s.kind, ToneKind::Dtmf(_))));
        // Too quiet: nothing (a tone remote's guard tone).
        let mut a = Vec::new();
        tone(&mut a, &[2175.0], &[0.003], 1.0);
        assert!(run(&a, 160).is_empty());
    }

    #[test]
    fn skipped_blocks_are_left_to_tone_frames() {
        let mut a = Vec::new();
        tone(&mut a, &[1000.0], &[0.4], 1.0);
        let mut d = AudioTones::default();
        let mut out = Vec::new();
        d.push(&Chunk { audio: &a, at: 0, named: &[4000..usize::MAX] }, &mut out);
        d.finish(&mut out);
        let out = segs(out);
        assert_eq!(out.len(), 1);
        assert!(out[0].len <= 4000);
    }
}
