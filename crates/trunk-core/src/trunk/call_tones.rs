//! A call's in-band tones ([`crate::dsp::inband`]), placed in its audio as
//! it comes: half-rate tone frames (one segment for each run of the same
//! tone), and what the audio holds besides ([`AudioTones`]: P25 Phase 1,
//! analog, a half-rate radio that sends no tone frames).

use std::ops::Range;

use super::frames::{Codec, VoiceFrame};
use crate::dsp::inband::audio::AudioTones;
use crate::dsp::inband::pattern::{patterns, Pattern};
use crate::dsp::inband::{Origin, ToneKind, ToneSeg};
use crate::mbe::{self, FRAME_SAMPLES};

/// Damaged frames a tone carries on through (the vocoder repeats a damaged
/// tone frame up to 3 times as well).
const BRIDGE: u32 = 3;
/// Clean frames that aren't the tone it carries on through: a vocoder's tone
/// detector misses a frame of a long tone now and then (seen on paging), and
/// DTMF leaves at least 40 ms (2 frames) between digits.
const BRIDGE_CLEAN: u32 = 1;
/// Tones followed by this much with none (1 s) make patterns nothing can
/// add to: a page's B is over, a DTMF string or a Morse ID has ended.
const SETTLE: usize = 8000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallTones {
    pub segs: Vec<ToneSeg>,
    /// The last segment is still going: its tone ID, and the damaged and
    /// the clean frames since.
    open: Option<(u8, u32, u32)>,
    audio: Detector,
    /// Tones starting before this sample have been made into patterns ([`CallTones::settled`]).
    settled_to: usize,
}

/// The audio's detector, made with the call's first audio; a copy starts afresh.
#[derive(Default)]
struct Detector(Option<Box<AudioTones>>);

impl Clone for Detector {
    fn clone(&self) -> Self {
        Detector(None)
    }
}

impl PartialEq for Detector {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for Detector {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "AudioTones" } else { "-" })
    }
}

impl CallTones {
    /// The call's `k`th frame (its audio starts at sample `k`·160).
    pub fn frame(&mut self, f: &VoiceFrame, k: usize) {
        let tone = match f.codec {
            Codec::Ambe if !f.erased => f.bits[..].try_into().ok().and_then(mbe::tone_frame),
            _ => None,
        };
        let damaged = f.erased || f.errs > 3;
        match (tone, self.open) {
            // The tone goes on; a damaged frame's ID counts too when it names
            // the same tone (its four copies agree), but not its level.
            (Some((id, level)), Some((open, ..))) if id == open => {
                let s = self.segs.last_mut().unwrap();
                s.len = (k + 1) * FRAME_SAMPLES - s.start;
                if !damaged {
                    s.level = s.level.max(Some(level));
                }
                self.open = Some((id, 0, 0));
            }
            // Another tone, or none (ID 255), from a frame the FEC got right.
            (Some((id, level)), _) if !damaged => {
                self.open = ToneKind::from_tone_id(id).map(|kind| {
                    self.segs.push(ToneSeg { kind, start: k * FRAME_SAMPLES, len: FRAME_SAMPLES, level: Some(level), origin: Origin::Frame });
                    (id, 0, 0)
                });
            }
            (_, Some((id, n, c))) if damaged && n < BRIDGE => self.open = Some((id, n + 1, c)),
            (None, Some((id, n, c))) if !damaged && c < BRIDGE_CLEAN => self.open = Some((id, n, c + 1)),
            _ => self.open = None,
        }
    }

    /// `samples` of the call's audio, from its sample `at` (after the frames
    /// they came from, if any): what tone frames didn't name, looked for.
    pub fn audio(&mut self, samples: &[f32], at: usize) {
        let named: Vec<Range<usize>> = self.segs.iter().filter(|s| s.origin == Origin::Frame).map(|s| s.start..s.start + s.len).collect();
        let d = self.audio.0.get_or_insert_with(Default::default);
        d.push(samples, at, |p| named.iter().any(|r| r.contains(&p)), &mut self.segs);
    }

    /// The call's audio is all in: the tone it ends on, if any.
    pub fn finish(&mut self) {
        if let Some(mut d) = self.audio.0.take() {
            d.finish(&mut self.segs);
        }
        self.segs.sort_by_key(|s| s.start);
    }

    /// The patterns that nothing can add to any more, each once: the tones
    /// before a second with none, `now` being the audio's length (or the
    /// call's end, after [`CallTones::finish`]: all the rest).
    pub fn settled(&mut self, now: usize) -> Vec<Pattern> {
        // (A tone being heard still keeps what came just before it open: a page's A, while B sounds.)
        let barrier = self.audio.0.as_ref().and_then(|d| d.running_since()).map_or(now, |r| r.min(now));
        let mut v: Vec<ToneSeg> = self.segs.iter().filter(|s| s.start >= self.settled_to).copied().collect();
        v.sort_by_key(|s| s.start);
        let (mut end, mut cut) = (0, None);
        for (i, s) in v.iter().enumerate() {
            end = end.max(s.start + s.len);
            let next = v.get(i + 1).map_or(barrier, |n| n.start.min(barrier));
            if end.saturating_add(SETTLE) <= next {
                cut = Some((i, end));
            }
        }
        let Some((i, end)) = cut else { return Vec::new() };
        self.settled_to = end;
        patterns(&v[..=i])
    }

    /// Keep only what's in `ranges` of the call's audio, closed up as the audio is.
    pub fn keep(&mut self, ranges: &[Range<usize>]) {
        self.finish();
        let moved = |p: usize| ranges.iter().map(|r| p.clamp(r.start, r.end) - r.start).sum::<usize>();
        self.segs.retain_mut(|s| {
            let (a, b) = (moved(s.start), moved(s.start + s.len));
            (s.start, s.len) = (a, b - a);
            b > a
        });
        self.open = None;
        // (Trimmed when the call is saved: what it had to say, it said.)
        self.settled_to = usize::MAX;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mbe::Kind;

    /// ambe_d for a tone frame (TIA-102.BABA-A Table 20).
    fn tone_bits(id: u8, level: u8) -> Vec<u8> {
        let mut d = vec![0u8; 49];
        let mut put = |at: usize, v: u32, n: usize| {
            for i in 0..n {
                d[at + i] = (v >> (n - 1 - i) & 1) as u8;
            }
        };
        let id = id as u32;
        put(0, 63, 6);
        put(6, level as u32 >> 1, 6);
        put(12, id, 8);
        put(20, id >> 4, 4);
        put(24, id & 15, 4);
        put(28, id >> 1, 7);
        put(35, id & 1, 1);
        put(36, id, 8);
        put(44, level as u32 & 1, 1);
        d
    }

    fn frame(bits: Vec<u8>, errs: u32) -> VoiceFrame {
        VoiceFrame { codec: Codec::Ambe, bits, e0: 0, errs, erased: false, kind: Kind::Voice }
    }

    fn run(frames: &[VoiceFrame]) -> CallTones {
        let mut t = CallTones::default();
        for (k, f) in frames.iter().enumerate() {
            t.frame(f, k);
        }
        t
    }

    #[test]
    fn a_run_of_tone_frames_is_one_tone() {
        let voice = || frame(vec![0; 49], 0);
        let five = || frame(tone_bits(133, 100), 0);
        let t = run(&[voice(), five(), five(), five(), frame(tone_bits(133, 110), 0), voice(), frame(tone_bits(143, 90), 1), voice()]);
        assert_eq!(t.segs.len(), 2);
        let s = t.segs[0];
        assert_eq!((s.kind, s.start, s.len, s.level, s.origin), (ToneKind::Dtmf('5'), 160, 4 * 160, Some(110), Origin::Frame));
        assert_eq!((t.segs[1].kind, t.segs[1].start, t.segs[1].len), (ToneKind::Dtmf('#'), 6 * 160, 160));
    }

    #[test]
    fn damage_is_bridged_but_never_starts_a_tone() {
        let five = || frame(tone_bits(133, 100), 0);
        let bad = || frame(vec![1; 49], 9);
        // A damaged frame naming another tone: damage, not a new tone. Its level isn't taken.
        let t = run(&[five(), bad(), frame(tone_bits(140, 100), 7), frame(tone_bits(133, 120), 5), five()]);
        assert_eq!(t.segs.len(), 1);
        assert_eq!((t.segs[0].len, t.segs[0].level), (5 * 160, Some(100)));
        // Past BRIDGE damaged frames the tone ends, and comes back as another.
        let t = run(&[five(), bad(), bad(), bad(), bad(), five()]);
        assert_eq!(t.segs.iter().map(|s| (s.start, s.len)).collect::<Vec<_>>(), vec![(0, 160), (5 * 160, 160)]);
        // One clean frame that isn't the tone is bridged; two end it.
        let voice = || frame(vec![0; 49], 0);
        assert_eq!(run(&[five(), voice(), five()]).segs.len(), 1);
        assert_eq!(run(&[five(), voice(), voice(), five()]).segs.len(), 2);
        // Damage alone never makes one.
        assert!(run(&[frame(tone_bits(133, 100), 6)]).segs.is_empty());
        // ID 255 (silence) and reserved IDs end a tone and make none.
        let t = run(&[five(), frame(tone_bits(255, 0), 0), frame(tone_bits(200, 50), 0), five()]);
        assert_eq!(t.segs.len(), 2);
    }

    #[test]
    fn imbe_frames_carry_no_tones() {
        let f = VoiceFrame { codec: Codec::Imbe, bits: vec![1; 88], e0: 0, errs: 0, erased: false, kind: Kind::Voice };
        assert!(run(&[f.clone(), f]).segs.is_empty());
    }

    /// A page heard in the audio: nothing until a second after B, then the page once.
    #[test]
    fn a_page_settles_a_second_after_it_ends() {
        let sr = 8000.0;
        let mut audio = vec![0f32; 2400];
        for (f, secs) in [(467.2f32, 1.0f32), (1032.3, 3.0)] {
            let n0 = audio.len();
            audio.extend((0..(secs * sr) as usize).map(|n| 0.4 * (2.0 * std::f32::consts::PI * f * (n0 + n) as f32 / sr).sin()));
        }
        let b_end = audio.len();
        audio.extend(std::iter::repeat_n(0.0, 16000));
        let mut t = CallTones::default();
        let mut heard = Vec::new();
        for (k, c) in audio.chunks(FRAME_SAMPLES).enumerate() {
            t.audio(c, k * FRAME_SAMPLES);
            for p in t.settled((k + 1) * FRAME_SAMPLES) {
                heard.push(((k + 1) * FRAME_SAMPLES, p));
            }
        }
        assert_eq!(heard.len(), 1, "{heard:?}");
        let (at, p) = &heard[0];
        assert_eq!(p.kind, crate::dsp::inband::pattern::PatternKind::TwoTone);
        assert!(*at >= b_end + 8000 && *at <= b_end + 8000 + 2 * FRAME_SAMPLES, "{at} vs B's end {b_end}");
        t.finish();
        assert!(t.settled(usize::MAX).is_empty(), "nothing twice");
    }

    #[test]
    fn trimming_moves_tones_with_the_audio() {
        let mut t = CallTones::default();
        let seg = |start, len| ToneSeg { kind: ToneKind::Dtmf('1'), start, len, level: None, origin: Origin::Frame };
        t.segs = vec![seg(100, 50), seg(400, 300), seg(1000, 100)];
        t.keep(&[0..200, 500..900]);
        assert_eq!(t.segs.iter().map(|s| (s.start, s.len)).collect::<Vec<_>>(), vec![(100, 50), (200, 200)]);
    }
}

/// `cargo test -p trunk-core --release tone_cost -- --ignored --nocapture`:
/// what finding tones costs a call, per second of its audio.
#[cfg(test)]
mod cost {
    use super::*;
    use std::time::Instant;

    fn audio(secs: usize) -> Vec<f32> {
        // Speech-like: a wandering voice's harmonics in noise, with a page and DTMF now and then.
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut noise = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        (0..secs * 8000)
            .map(|n| {
                let t = n as f32 / 8000.0;
                let s = t % 30.0;
                let f0 = 120.0 + 20.0 * (6.0 * t).sin();
                let voice: f32 = (1..8).map(|h| 0.05 / h as f32 * (2.0 * std::f32::consts::PI * f0 * h as f32 * t).sin()).sum();
                let page = if (5.0..6.0).contains(&s) { 0.4 * (2.0 * std::f32::consts::PI * 467.2 * t).sin() } else if (6.0..9.0).contains(&s) { 0.4 * (2.0 * std::f32::consts::PI * 1032.3 * t).sin() } else { 0.0 };
                voice + page + 0.01 * noise()
            })
            .collect()
    }

    #[test]
    #[ignore]
    fn tone_cost() {
        let secs = 600;
        let a = audio(secs);
        let mut t = CallTones::default();
        let start = Instant::now();
        let mut found = 0;
        for (k, c) in a.chunks(FRAME_SAMPLES).enumerate() {
            t.audio(c, k * FRAME_SAMPLES);
            found += t.settled((k + 1) * FRAME_SAMPLES).len();
        }
        t.finish();
        let took = start.elapsed().as_secs_f64();
        println!("{secs} s of audio: {:.1} ms ({:.3} % of one core per call), {found} patterns, {} tones", took * 1000.0, 100.0 * took / secs as f64, t.segs.len());
        // The settle check alone, with many tones on the call.
        let start = Instant::now();
        for k in 0..30_000 {
            t.settled(k * FRAME_SAMPLES);
        }
        println!("settled() with {} tones: {:.2} µs a frame", t.segs.len(), start.elapsed().as_secs_f64() * 1e6 / 30_000.0);
    }
}
