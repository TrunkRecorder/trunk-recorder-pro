//! A call's in-band signalling ([`crate::dsp::inband`]), gathered as its
//! voice comes in: the tones its vocoder played (one segment for each run of
//! the same tone), and what its detectors find in the audio: unit IDs, its
//! sub-audible tone or code, tones. The patterns its tones make are news
//! once nothing can add to them ([`News`]), and are kept for the call JSON,
//! so what was told live and what is saved are the same patterns.

use std::ops::Range;

use super::frames::VoiceFrame;
use crate::dsp::inband::pattern::{patterns, Pattern};
use crate::dsp::inband::subaudible::{ToneDetector, ToneHeard};
use crate::dsp::inband::units::{Signalling, UnitId};
use crate::dsp::inband::{audio::AudioTones, Chunk, Detector, Found, Origin, Tap, ToneKind, ToneSeg};
use crate::mbe::{Kind, FRAME_SAMPLES};

/// Clean frames that aren't the tone it carries on through: a vocoder's tone
/// detector misses a frame of a long tone now and then (seen on paging), and
/// DTMF leaves at least 40 ms (2 frames) between digits. (Damaged frames
/// are the vocoder's: it repeats the tone through up to 3.)
const BRIDGE_CLEAN: u32 = 1;
/// Tones followed by this much with none (1 s) make patterns nothing can
/// add to: a page's B is over, a DTMF string or a Morse ID has ended.
const SETTLE: usize = 8000;

/// Which detectors a call has besides the tones every call is searched for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InBandSpec {
    /// Analog FM: MDC1200 and FleetSync unit IDs, from audio de-emphasised
    /// with this time constant, s (0: none).
    pub units: Option<f64>,
    /// Analog FM: its CTCSS tone or DCS code ([`Tap::Subaudible`]).
    pub subaudible: bool,
}

/// What a call's voice newly carried.
#[derive(Debug, Default, PartialEq)]
pub struct News {
    /// Unit ID bursts: who is talking.
    pub units: Vec<UnitId>,
    /// Its sub-audible tone or code, when the reading names another.
    pub code: Option<ToneHeard>,
    /// Patterns nothing can add to any more.
    pub patterns: Vec<Pattern>,
}

pub struct InBand {
    spec: InBandSpec,
    segs: Vec<ToneSeg>,
    /// The patterns told so far ([`News::patterns`]).
    patterns: Vec<Pattern>,
    /// The vocoder's tone being played: its ID, and the clean frames since.
    open: Option<(u8, u32)>,
    detectors: Vec<Box<dyn Detector>>,
    code: Option<ToneHeard>,
    /// Tones starting before this sample have been made into patterns.
    settled_to: usize,
    finished: bool,
}

impl Default for InBand {
    fn default() -> Self {
        InBand::new(InBandSpec::default())
    }
}

/// A copy's detectors start afresh.
impl Clone for InBand {
    fn clone(&self) -> Self {
        InBand { segs: self.segs.clone(), patterns: self.patterns.clone(), open: self.open, code: self.code, settled_to: self.settled_to, finished: self.finished, ..InBand::new(self.spec) }
    }
}

impl std::fmt::Debug for InBand {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("InBand").field("segs", &self.segs).field("patterns", &self.patterns).field("code", &self.code).finish()
    }
}

impl InBand {
    pub fn new(spec: InBandSpec) -> Self {
        let mut detectors: Vec<Box<dyn Detector>> = vec![Box::<AudioTones>::default()];
        if let Some(tau) = spec.units {
            detectors.push(Box::new(Signalling::with_deemphasis(tau)));
        }
        if spec.subaudible {
            detectors.push(Box::<ToneDetector>::default());
        }
        InBand { spec, segs: Vec::new(), patterns: Vec::new(), open: None, detectors, code: None, settled_to: 0, finished: false }
    }

    /// Its tones so far, in the call's audio.
    pub fn segs(&self) -> &[ToneSeg] {
        &self.segs
    }

    /// The patterns its tones made, as told.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// Its sub-audible tone or code, as now read.
    pub fn code(&self) -> Option<ToneHeard> {
        self.code
    }

    /// The call's `k`th frame (its audio starts at sample `k`·160).
    pub fn frame(&mut self, f: &VoiceFrame, k: usize) {
        let start = k * FRAME_SAMPLES;
        match (f.tone, self.open) {
            // The tone goes on (a damaged frame the vocoder repeated it through too).
            (Some((id, level)), Some((open, _))) if id == open => {
                let s = self.segs.last_mut().unwrap();
                s.len = start + FRAME_SAMPLES - s.start;
                s.level = s.level.max(Some(level));
                self.open = Some((id, 0));
            }
            // Another tone, played from its own frame (ID 255, silence: none).
            (Some((id, level)), _) if f.kind == Kind::Tone => {
                self.open = ToneKind::from_tone_id(id).map(|kind| {
                    self.segs.push(ToneSeg { kind, start, len: FRAME_SAMPLES, level: Some(level), origin: Origin::Frame });
                    (id, 0)
                });
            }
            (None, Some((id, c))) if f.kind == Kind::Voice && c < BRIDGE_CLEAN => self.open = Some((id, c + 1)),
            _ => self.open = None,
        }
    }

    /// `samples` of the call's audio, from its sample `at` (after the frames
    /// they came from, if any).
    pub fn audio(&mut self, samples: &[f32], at: usize) -> News {
        let mut news = self.detect(Tap::Voice, samples, at);
        news.patterns = self.settle(at + samples.len());
        news
    }

    /// FM: the same audio before the voice high-pass.
    pub fn subaudible(&mut self, samples: &[f32], at: usize) -> News {
        self.detect(Tap::Subaudible, samples, at)
    }

    fn detect(&mut self, tap: Tap, samples: &[f32], at: usize) -> News {
        let named: Vec<Range<usize>> = self.segs.iter().filter(|s| s.origin == Origin::Frame).map(|s| s.start..s.start + s.len).collect();
        let chunk = Chunk { audio: samples, at, named: &named };
        let mut found = Vec::new();
        for d in self.detectors.iter_mut().filter(|d| d.tap() == tap) {
            d.push(&chunk, &mut found);
        }
        self.take(found)
    }

    fn take(&mut self, found: Vec<Found>) -> News {
        let mut news = News::default();
        for f in found {
            match f {
                Found::Unit(u) => news.units.push(u),
                Found::Code(h) => {
                    if self.code.map(|c| c.tone) != Some(h.tone) {
                        news.code = Some(h);
                    }
                    self.code = Some(h);
                }
                Found::Tone(t) => self.segs.push(t),
            }
        }
        news
    }

    /// The call's audio is all in: what its detectors were in the middle of,
    /// and the patterns its last tones made. (Once: again, nothing.)
    pub fn finish(&mut self) -> News {
        if std::mem::replace(&mut self.finished, true) {
            return News::default();
        }
        let mut found = Vec::new();
        for d in &mut self.detectors {
            d.finish(&mut found);
        }
        let mut news = self.take(found);
        self.segs.sort_by_key(|s| s.start);
        news.patterns = self.settle(usize::MAX);
        news
    }

    /// The patterns that nothing can add to any more, each once: the tones
    /// before a second with none, `now` being the audio's length (or
    /// usize::MAX at the call's end: all the rest).
    fn settle(&mut self, now: usize) -> Vec<Pattern> {
        // (A tone being heard still keeps what came just before it open: a page's A, while B sounds.)
        let barrier = self.detectors.iter().filter_map(|d| d.busy_since()).fold(now, usize::min);
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
        let made = patterns(&v[..=i]);
        self.patterns.extend(made.iter().cloned());
        made
    }

    /// Keep only what's in `ranges` of the call's audio, closed up as the
    /// audio is: its tones and patterns move with it (those left out go).
    pub fn keep(&mut self, ranges: &[Range<usize>]) {
        self.finish();
        let moved = |p: usize| ranges.iter().map(|r| p.clamp(r.start, r.end) - r.start).sum::<usize>();
        let keep = |start: &mut usize, len: &mut usize| {
            let (a, b) = (moved(*start), moved(*start + *len));
            (*start, *len) = (a, b - a);
            b > a
        };
        self.segs.retain_mut(|s| keep(&mut s.start, &mut s.len));
        self.patterns.retain_mut(|p| keep(&mut p.start, &mut p.len));
        self.open = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::inband::pattern::PatternKind;
    use crate::mbe::{lcg, Decoder, Profile};
    use crate::trunk::frames::Codec;

    /// ambe_d for a tone frame (TIA-102.BABA-A Table 20).
    fn tone_bits(id: u8, level: u8) -> [u8; 49] {
        let mut d = [0u8; 49];
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

    /// Frames through the vocoder, as a tracker makes them: (bits, errors).
    fn run(frames: &[([u8; 49], u32)]) -> InBand {
        let mut dec = Decoder::new(lcg(1), Profile::Enhanced);
        let mut t = InBand::default();
        let mut buf = [0f32; FRAME_SAMPLES];
        for (k, (bits, errs)) in frames.iter().enumerate() {
            let kind = dec.ambe(bits, *errs, &mut buf);
            let f = VoiceFrame { codec: Codec::Ambe, bits: bits.to_vec(), e0: 0, errs: *errs, erased: false, kind, tone: dec.tone_played(kind) };
            t.frame(&f, k);
        }
        t
    }

    const VOICE: [u8; 49] = [0; 49];

    #[test]
    fn a_run_of_tone_frames_is_one_tone() {
        let five = (tone_bits(133, 100), 0);
        let t = run(&[(VOICE, 0), five, five, five, (tone_bits(133, 110), 0), (VOICE, 0), (VOICE, 0), (tone_bits(143, 90), 1), (VOICE, 0)]);
        assert_eq!(t.segs.len(), 2);
        let s = t.segs[0];
        assert_eq!((s.kind, s.start, s.len, s.level, s.origin), (ToneKind::Dtmf('5'), 160, 4 * 160, Some(110), Origin::Frame));
        assert_eq!((t.segs[1].kind, t.segs[1].start, t.segs[1].len), (ToneKind::Dtmf('#'), 7 * 160, 160));
    }

    #[test]
    fn the_vocoders_repeats_carry_a_tone_through_damage() {
        let five = (tone_bits(133, 100), 0);
        let bad = ([1; 49], 9);
        // Damaged frames, one naming another tone: the vocoder repeats the tone. Their levels aren't taken.
        let t = run(&[five, bad, (tone_bits(140, 120), 7), (tone_bits(133, 120), 5), five]);
        assert_eq!(t.segs.len(), 1);
        assert_eq!((t.segs[0].len, t.segs[0].level), (5 * 160, Some(100)));
        // Past 3 repeats the vocoder mutes: the tone ends, and comes back as another.
        let t = run(&[five, bad, bad, bad, bad, five]);
        assert_eq!(t.segs.iter().map(|s| (s.start, s.len)).collect::<Vec<_>>(), vec![(0, 4 * 160), (5 * 160, 160)]);
        // One clean frame that isn't the tone is bridged; two end it.
        assert_eq!(run(&[five, (VOICE, 0), five]).segs.len(), 1);
        assert_eq!(run(&[five, (VOICE, 0), (VOICE, 0), five]).segs.len(), 2);
        // Damage alone never makes one.
        assert!(run(&[(tone_bits(133, 100), 6)]).segs.is_empty());
        // ID 255 (silence) and reserved IDs end a tone and make none.
        let t = run(&[five, (tone_bits(255, 0), 0), (tone_bits(200, 50), 0), five]);
        assert_eq!(t.segs.len(), 2);
    }

    #[test]
    fn imbe_frames_carry_no_tones() {
        let f = VoiceFrame { codec: Codec::Imbe, bits: vec![1; 88], e0: 0, errs: 0, erased: false, kind: Kind::Voice, tone: None };
        let mut t = InBand::default();
        t.frame(&f, 0);
        t.frame(&f, 1);
        assert!(t.segs.is_empty());
    }

    /// A page heard in the audio: nothing until a second after B, then the page once, kept for the call.
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
        let mut t = InBand::default();
        let mut heard = Vec::new();
        for (k, c) in audio.chunks(FRAME_SAMPLES).enumerate() {
            for p in t.audio(c, k * FRAME_SAMPLES).patterns {
                heard.push(((k + 1) * FRAME_SAMPLES, p));
            }
        }
        assert_eq!(heard.len(), 1, "{heard:?}");
        let (at, p) = &heard[0];
        assert_eq!(p.kind, PatternKind::TwoTone);
        assert!(*at >= b_end + 8000 && *at <= b_end + 8000 + 2 * FRAME_SAMPLES, "{at} vs B's end {b_end}");
        assert!(t.finish().patterns.is_empty(), "nothing twice");
        assert_eq!(t.patterns().len(), 1, "kept for the call JSON");
    }

    #[test]
    fn trimming_moves_tones_and_patterns_with_the_audio() {
        let mut t = InBand::default();
        let seg = |start, len| ToneSeg { kind: ToneKind::Single(1000.0), start, len, level: None, origin: Origin::Frame };
        t.segs = vec![seg(1000, 500), seg(20_000, 9000), seg(40_000, 9000)];
        for k in 1..3 {
            t.patterns.extend(patterns(&t.segs[k..k + 1]));
        }
        assert_eq!(t.patterns.len(), 2, "two long tones");
        (t.settled_to, t.finished) = (usize::MAX, true);
        t.keep(&[0..2000, 25_000..30_000]);
        assert_eq!(t.segs.iter().map(|s| (s.start, s.len)).collect::<Vec<_>>(), vec![(1000, 500), (2000, 4000)]);
        assert_eq!(t.patterns.iter().map(|p| (p.start, p.len)).collect::<Vec<_>>(), vec![(2000, 4000)], "the one in the cut part goes");
    }

    /// An FM call's sub-audible tone, through the same detectors as its tones.
    #[test]
    fn codes_are_found_with_the_tones() {
        use crate::dsp::inband::subaudible::Tone;
        let mut t = InBand::new(InBandSpec { units: None, subaudible: true });
        let tone: Vec<f32> = (0..16000).map(|n| 0.1 * (2.0 * std::f32::consts::PI * 151.4 * n as f32 / 8000.0).sin()).collect();
        let mut code = None;
        for (k, c) in tone.chunks(FRAME_SAMPLES).enumerate() {
            code = code.or(t.subaudible(c, k * FRAME_SAMPLES).code);
        }
        assert_eq!(code.map(|h| h.tone), Some(Tone::Ctcss(1514)));
        assert_eq!(t.code().map(|h| h.tone), Some(Tone::Ctcss(1514)));
    }
}

/// `cargo test -p trunk-core --release tone_cost -- --ignored --nocapture`:
/// what finding tones costs a call, per second of its audio.
#[cfg(test)]
mod cost {
    use super::*;
    use std::time::Instant;

    fn audio(secs: usize) -> Vec<f32> {
        // Speech-like: a wandering voice's harmonics in noise, with a page now and then.
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
        let mut t = InBand::default();
        let start = Instant::now();
        let mut found = 0;
        for (k, c) in a.chunks(FRAME_SAMPLES).enumerate() {
            found += t.audio(c, k * FRAME_SAMPLES).patterns.len();
        }
        t.finish();
        let took = start.elapsed().as_secs_f64();
        println!("{secs} s of audio: {:.1} ms ({:.3} % of one core per call), {found} patterns, {} tones", took * 1000.0, 100.0 * took / secs as f64, t.segs.len());
    }
}
