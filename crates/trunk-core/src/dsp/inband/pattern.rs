//! A call's tones as the signals they make up: a two-tone page, a long
//! alert tone, a hi-low warble, a pulsed tone, a DTMF / KNOX string, Morse
//! (a repeater's CW ID), a run of short tones (selective calling, a chirp),
//! or a lone beep.
//!
//! Thresholds follow icad_tone_detection's (two tones at least 40 Hz apart,
//! at most 0.35 s between A and B, 6 alternations or pulses, digits joined
//! across 0.3 s), with the durations widened to the paging formats seen on
//! the air: Quick Call II's battery-save page is 2.7 s + 0.8 s, and some
//! dispatch consoles send 1 s + 2 s.

use super::{Origin, ToneKind, ToneSeg};
use crate::mbe::SAMPLE_RATE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternKind {
    TwoTone,
    Long,
    HiLow,
    Pulsed,
    Digits,
    Morse,
    Sequence,
    Beep,
}

impl PatternKind {
    /// The call JSON's `pattern`.
    pub fn name(self) -> &'static str {
        match self {
            PatternKind::TwoTone => "two_tone",
            PatternKind::Long => "long",
            PatternKind::HiLow => "hi_low",
            PatternKind::Pulsed => "pulsed",
            PatternKind::Digits => "digits",
            PatternKind::Morse => "morse",
            PatternKind::Sequence => "sequence",
            PatternKind::Beep => "beep",
        }
    }

    pub fn from_name(s: &str) -> Option<PatternKind> {
        [PatternKind::TwoTone, PatternKind::Long, PatternKind::HiLow, PatternKind::Pulsed, PatternKind::Digits, PatternKind::Morse, PatternKind::Sequence, PatternKind::Beep].into_iter().find(|k| k.name() == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    /// Its tones, Hz: two-tone A then B; hi-low its two, lower first; a
    /// sequence each in turn; otherwise its one. (Digits: none.)
    pub hz: Vec<f32>,
    /// How long, s: two-tone A and B; long and beep its one; hi-low its two
    /// tones' typical; pulsed on then off; Morse its dot; a sequence each in turn.
    pub lengths: Vec<f32>,
    /// DTMF / KNOX: the digits (`knox` when they're KNOX); Morse: the text.
    pub digits: String,
    pub knox: bool,
    /// Hi-low: tones; pulsed: pulses.
    pub cycles: u32,
    /// A two-tone page's format, when its timing is one ([`FORMATS`]).
    pub format: Option<&'static str>,
    /// Where it starts in the call's audio, and how long it lasts: samples.
    pub start: usize,
    pub len: usize,
    pub origin: Origin,
}

/// A two-tone paging format's timing (Midian's paging timings table).
pub struct Format {
    pub name: &'static str,
    /// The tone plan its pages use ([`super::tables::plans`]), when one does.
    pub plan: Option<&'static str>,
    /// A, the gap, and B, s.
    pub a: f32,
    pub gap: f32,
    pub b: f32,
}

const fn fmt(name: &'static str, plan: Option<&'static str>, a: f32, gap: f32, b: f32) -> Format {
    Format { name, plan, a, gap, b }
}

/// The gap tells some apart (Quick Call I's 1 s, 0.2 s, 1 s from NEC C's 1 s,
/// 1 s). NEC's B and M are Quick Call II's timings, so they read as those.
pub const FORMATS: &[Format] = &[
    fmt("Quick Call II", Some("QC2"), 1.0, 0.0, 3.0),
    fmt("Quick Call II tone only", Some("QC2"), 0.4, 0.0, 0.8),
    fmt("Quick Call II battery save", Some("QC2"), 2.7, 0.0, 0.8),
    fmt("Quick Call I", Some("QC1"), 1.0, 0.2, 1.0),
    fmt("GE Type 99", Some("GE99"), 1.0, 0.0, 1.5),
    fmt("Reach slow", Some("Reach"), 2.0, 0.025, 0.7),
    fmt("AvCall", Some("AvCall"), 1.25, 0.2, 1.0),
    fmt("Plectron fast", None, 0.75, 0.0, 0.25),
    fmt("Plectron slow", None, 3.0, 0.0, 0.25),
    fmt("NEC A", None, 1.0, 0.25, 3.0),
    fmt("NEC C", None, 1.0, 0.0, 1.0),
    fmt("NEC D", None, 0.4, 0.0, 0.4),
    fmt("NEC L", None, 0.5, 0.0, 0.5),
];

/// The plan a format's pages use.
pub fn format_plan(name: &str) -> Option<&'static str> {
    FORMATS.iter().find(|f| f.name == name).and_then(|f| f.plan)
}

/// Tones this close are one, Hz: a Phase 2 tone frame's step is 31.25 Hz.
const SAME_HZ: f32 = 16.0;
/// Two tones of a pattern this far apart at least: icad_tone_detection
/// asks 40 Hz, but Quick Call II's low tones are 20–35 Hz apart, a step
/// of a Phase 2 tone frame's grid (593.75 → 562.5 Hz is a page on the air).
const MIN_SEP_HZ: f32 = 25.0;
const MAX_AB_GAP_S: f32 = 0.35;
const TWO_TONE_A_S: f32 = 0.3;
const TWO_TONE_B_S: f32 = 0.2;
/// Together at least this long (no pair of beeps is a page).
const TWO_TONE_S: f32 = 0.9;
const LONG_S: f32 = 1.0;
const HI_LOW_STEP_S: f32 = 0.6;
const HI_LOW_GAP_S: f32 = 0.2;
const MIN_CYCLES: usize = 6;
/// A warble's steps at least: high, low, high is one already (Woodbridge's
/// console sends 3 or 4).
const MIN_HI_LOW: usize = 3;
const PULSE_ON_S: (f32, f32) = (0.08, 0.9);
const PULSE_OFF_S: (f32, f32) = (0.025, 0.35);
const SEQUENCE_STEP_S: f32 = 0.25;
const SEQUENCE_GAP_S: f32 = 0.05;
const DIGIT_GAP_S: f32 = 0.3;
/// Shorter than this, alone: not a signal (a vocoder misreading speech).
const MIN_S: f32 = 0.06;
/// From the audio, alone, a beep and a key this long at least: speech
/// holds a harmonic steady for 60–80 ms now and then (about one call in
/// eight on CLMRN, 2026-10-21). Short tones still count inside a pattern
/// (a Morse dot, a warble's step).
const MIN_AUDIO_BEEP_S: f32 = 0.15;
const MIN_AUDIO_KEY_S: f32 = 0.1;
/// A tone this short between two others, touching both: the vocoder's frame
/// between them, not a tone of its own.
const BETWEEN_S: f32 = 0.045;

fn secs(n: usize) -> f32 {
    n as f32 / SAMPLE_RATE as f32
}

fn end(s: &ToneSeg) -> usize {
    s.start + s.len
}

fn gap(a: &ToneSeg, b: &ToneSeg) -> f32 {
    secs(b.start.saturating_sub(end(a)))
}

fn hz(s: &ToneSeg) -> f32 {
    match s.kind {
        ToneKind::Single(f) => f,
        _ => 0.0,
    }
}

fn same(a: f32, b: f32) -> bool {
    (a - b).abs() <= SAME_HZ
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// The format a two-tone page's timing is: each tone within 15 % (at least
/// 60 ms) and the gap within 80 ms (a frame or two of either tone lost).
/// A short A is let through: a call heard from a moment after A began.
pub fn format_of(a: f32, gap: f32, b: f32) -> Option<&'static str> {
    let near = |x: f32, t: f32| (x - t).abs() <= (0.15 * t).max(0.06);
    // (A down to half its length: a call is often picked up a moment into its first tone.)
    let near_a = |x: f32, t: f32| near(x, t) || (x < t && x >= 0.5 * t);
    FORMATS.iter().find(|f| near_a(a, f.a) && near(b, f.b) && (gap - f.gap).abs() <= 0.08).map(|f| f.name)
}

/// The patterns `segs` (one call's, in order) make.
pub fn patterns(segs: &[ToneSeg]) -> Vec<Pattern> {
    let mut out = digits(segs);
    let singles = singles(segs);
    let mut i = 0;
    while i < singles.len() {
        let (p, n) = hi_low(&singles[i..]).or_else(|| morse(&singles[i..])).or_else(|| pulsed(&singles[i..])).or_else(|| two_tone(&singles[i..])).or_else(|| sequence(&singles[i..])).unwrap_or_else(|| {
            let s = &singles[i];
            let l = secs(s.len);
            let kind = if l >= LONG_S { PatternKind::Long } else { PatternKind::Beep };
            (pattern(kind, vec![hz(s)], vec![l], &singles[i..=i]), 1)
        });
        let min = if p.origin == Origin::Audio { MIN_AUDIO_BEEP_S } else { MIN_S };
        if p.kind != PatternKind::Beep || secs(p.len) >= min {
            out.push(p);
        }
        i += n;
    }
    out.sort_by_key(|p| p.start);
    out
}

impl Pattern {
    /// For a log line: "two-tone 781.25 → 875 Hz, 0.98 + 2.98 s (Quick Call II)",
    /// "DTMF 072864", "Morse WRV713 (990 Hz)".
    pub fn describe(&self) -> String {
        let hz = |sep: &str| self.hz.iter().map(|f| format!("{}", (f * 100.0).round() / 100.0)).collect::<Vec<_>>().join(sep);
        let s = |l: &f32| format!("{}", (l * 100.0).round() / 100.0);
        match self.kind {
            PatternKind::Digits => format!("{} {}", if self.knox { "KNOX" } else { "DTMF" }, self.digits),
            PatternKind::Morse => format!("Morse {} ({} Hz)", self.digits, hz("")),
            PatternKind::TwoTone => {
                let format = self.format.map_or(String::new(), |f| format!(" ({f})"));
                format!("two-tone {} Hz, {} s{format}", hz(" → "), self.lengths.iter().map(s).collect::<Vec<_>>().join(" + "))
            }
            PatternKind::HiLow => format!("hi-low {} Hz", hz(" / ")),
            PatternKind::Pulsed => format!("pulsed {} Hz ×{}", hz(""), self.cycles),
            PatternKind::Sequence => format!("sequence {} Hz", hz(" · ")),
            PatternKind::Long | PatternKind::Beep => format!("{} {} Hz, {} s", if self.kind == PatternKind::Long { "long tone" } else { "beep" }, hz(""), self.lengths.first().map(s).unwrap_or_default()),
        }
    }
}

fn pattern(kind: PatternKind, hz: Vec<f32>, lengths: Vec<f32>, run: &[ToneSeg]) -> Pattern {
    let (start, last) = (run[0].start, run.last().map_or(0, end));
    let origin = if run.iter().all(|s| s.origin == Origin::Frame) { Origin::Frame } else { Origin::Audio };
    Pattern { kind, hz, lengths, digits: String::new(), knox: false, cycles: 0, format: None, start, len: last - start, origin }
}

/// The single tones, the vocoder's frames between two tones left out and a
/// tone broken by one frame made whole, or a long one (half a second or
/// more) by up to 100 ms: pages heard on the air drop out that long. (A
/// pulsed tone's gaps are longer than a frame, its pulses mostly shorter.)
fn singles(segs: &[ToneSeg]) -> Vec<ToneSeg> {
    let all: Vec<ToneSeg> = segs.iter().filter(|s| matches!(s.kind, ToneKind::Single(_))).copied().collect();
    let mut out: Vec<ToneSeg> = Vec::new();
    for (k, s) in all.iter().enumerate() {
        let touches = |o: Option<&ToneSeg>, before: bool| o.is_some_and(|o| if before { gap(o, s) <= 0.025 && secs(o.len) > secs(s.len) } else { gap(s, o) <= 0.025 && secs(o.len) > secs(s.len) });
        if secs(s.len) <= BETWEEN_S && touches(k.checked_sub(1).and_then(|j| all.get(j)), true) && touches(all.get(k + 1), false) {
            continue;
        }
        match out.last_mut() {
            Some(p) if same(hz(p), hz(s)) && (gap(p, s) <= 0.021 || (gap(p, s) <= 0.1 && secs(p.len) >= 0.5)) => p.len = end(s) - p.start,
            _ => out.push(*s),
        }
    }
    out
}

fn digits(segs: &[ToneSeg]) -> Vec<Pattern> {
    let mut out: Vec<Pattern> = Vec::new();
    let mut last: Option<ToneSeg> = None;
    for s in segs {
        let (c, knox) = match s.kind {
            ToneKind::Dtmf(c) => (c, false),
            ToneKind::Knox(c) => (c, true),
            _ => continue,
        };
        match (out.last_mut(), last) {
            (Some(p), Some(l)) if p.knox == knox && gap(&l, s) <= DIGIT_GAP_S => {
                p.digits.push(c);
                p.len = end(s) - p.start;
                p.lengths.push(secs(s.len));
            }
            _ => {
                let mut p = pattern(PatternKind::Digits, vec![], vec![secs(s.len)], std::slice::from_ref(s));
                (p.digits, p.knox) = (c.to_string(), knox);
                out.push(p);
            }
        }
        last = Some(*s);
    }
    // One digit alone, shorter than a key press: speech misread.
    out.retain(|p| p.digits.len() > 1 || p.lengths[0] >= if p.origin == Origin::Audio { MIN_AUDIO_KEY_S } else { MIN_S });
    out
}

fn hi_low(run: &[ToneSeg]) -> Option<(Pattern, usize)> {
    let (a, b) = (hz(run.first()?), hz(run.get(1)?));
    if (a - b).abs() < MIN_SEP_HZ {
        return None;
    }
    let mut n = 0;
    while let Some(s) = run.get(n) {
        let want = if n % 2 == 0 { a } else { b };
        if !same(hz(s), want) || secs(s.len) > HI_LOW_STEP_S || (n > 0 && gap(&run[n - 1], s) > HI_LOW_GAP_S) {
            break;
        }
        n += 1;
    }
    if n < MIN_HI_LOW {
        return None;
    }
    let typical = |k: usize| median(run[..n].iter().skip(k).step_by(2).map(|s| secs(s.len)).collect());
    let (lo, hi) = if a < b { ((a, typical(0)), (b, typical(1))) } else { ((b, typical(1)), (a, typical(0))) };
    let mut p = pattern(PatternKind::HiLow, vec![lo.0, hi.0], vec![lo.1, hi.1], &run[..n]);
    p.cycles = n as u32;
    Some((p, n))
}

fn pulsed(run: &[ToneSeg]) -> Option<(Pattern, usize)> {
    let f = hz(run.first()?);
    let on = |s: &ToneSeg| same(hz(s), f) && (PULSE_ON_S.0..=PULSE_ON_S.1).contains(&secs(s.len));
    let mut n = 0;
    while run.get(n).is_some_and(on) && (n == 0 || (PULSE_OFF_S.0..=PULSE_OFF_S.1).contains(&gap(&run[n - 1], &run[n]))) {
        n += 1;
    }
    if n < MIN_CYCLES {
        return None;
    }
    let ons = median(run[..n].iter().map(|s| secs(s.len)).collect());
    let offs = median(run[..n].windows(2).map(|w| gap(&w[0], &w[1])).collect());
    let mut p = pattern(PatternKind::Pulsed, vec![f], vec![ons, offs], &run[..n]);
    p.cycles = n as u32;
    Some((p, n))
}

fn two_tone(run: &[ToneSeg]) -> Option<(Pattern, usize)> {
    let (a, b) = (run.first()?, run.get(1)?);
    let (la, lb) = (secs(a.len), secs(b.len));
    if la < TWO_TONE_A_S || lb < TWO_TONE_B_S || la + lb < TWO_TONE_S || gap(a, b) > MAX_AB_GAP_S || (hz(a) - hz(b)).abs() < MIN_SEP_HZ {
        return None;
    }
    let mut p = pattern(PatternKind::TwoTone, vec![hz(a), hz(b)], vec![la, lb], &run[..2]);
    p.format = format_of(la, gap(a, b), lb);
    Some((p, 2))
}

fn sequence(run: &[ToneSeg]) -> Option<(Pattern, usize)> {
    let mut n = 0;
    while run.get(n).is_some_and(|s| secs(s.len) <= SEQUENCE_STEP_S) && (n == 0 || gap(&run[n - 1], &run[n]) <= SEQUENCE_GAP_S) {
        n += 1;
    }
    // (One tone over and over is pulsed, or Morse: not a sequence.)
    if n < 3 || run[..n].iter().all(|s| same(hz(s), hz(&run[0]))) {
        return None;
    }
    let p = pattern(PatternKind::Sequence, run[..n].iter().map(hz).collect(), run[..n].iter().map(|s| secs(s.len)).collect(), &run[..n]);
    Some((p, n))
}

/// One step of a pattern played back: its tones together (none: silence), for `s` seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub hz: Vec<f32>,
    pub s: f32,
}

/// Longest a played tone runs, s (an 8 s group call is heard well enough in 4).
const MAX_PLAY_S: f32 = 4.0;

/// What a pattern sounds like, step by step: its tones at their lengths, s
/// (one not known: the usual, a 1 s + 3 s page, 0.1 s digits); `digits` a
/// digit string's keys (on KNOX's pad when `knox`) or Morse's text.
pub fn steps(kind: PatternKind, hz: &[f32], lengths: &[f32], digits: &str, knox: bool, cycles: u32) -> Vec<Step> {
    let len = |k: usize, usual: f32| lengths.get(k).copied().filter(|&l| l > 0.0).unwrap_or(usual).min(MAX_PLAY_S);
    let tone = |f: f32, s: f32| Step { hz: vec![f], s };
    let quiet = |s: f32| Step { hz: Vec::new(), s };
    let (a, b) = (hz.first().copied().unwrap_or(0.0), hz.get(1).copied().unwrap_or(0.0));
    let times = |usual: u32, most: u32| if cycles > 0 { cycles } else { usual }.min(most);
    match kind {
        PatternKind::TwoTone => vec![tone(a, len(0, 1.0)), tone(b, len(1, 3.0))],
        PatternKind::Long => vec![tone(a, len(0, 3.0))],
        PatternKind::Beep => vec![tone(a, len(0, 0.2))],
        PatternKind::HiLow => (0..times(12, 24)).map(|i| if i % 2 == 0 { tone(a, len(0, 0.25)) } else { tone(b, len(1, 0.25)) }).collect(),
        PatternKind::Pulsed => (0..times(6, 12)).flat_map(|_| [tone(a, len(0, 0.3)), quiet(len(1, 0.2))]).collect(),
        PatternKind::Sequence => hz.iter().enumerate().map(|(i, &f)| tone(f, len(i, 0.07))).collect(),
        PatternKind::Morse => morse_steps(digits, if a > 0.0 { a } else { 1000.0 }, len(0, 0.06)),
        PatternKind::Digits => {
            let grid = if knox { &super::KNOX_HZ } else { &super::DTMF_HZ };
            let key = |c: char| super::key_hz(grid, c).map_or_else(Vec::new, |(lo, hi)| vec![lo, hi]);
            digits.chars().enumerate().flat_map(|(i, c)| [Step { hz: key(c), s: len(i, 0.1) }, quiet(0.1)]).collect()
        }
    }
}

/// Text keyed as Morse on `hz` with a dot of `dot` s (a dash 3, gaps 1, 3 and 7).
fn morse_steps(text: &str, hz: f32, dot: f32) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(Step { hz: Vec::new(), s: 7.0 * dot });
        }
        for c in word.chars() {
            let Some((code, _)) = MORSE.iter().find(|m| m.1 == c.to_ascii_uppercase()) else { continue };
            if out.last().is_some_and(|s| !s.hz.is_empty()) {
                out.push(Step { hz: Vec::new(), s: 3.0 * dot });
            }
            for (i, e) in code.chars().enumerate() {
                if i > 0 {
                    out.push(Step { hz: Vec::new(), s: dot });
                }
                out.push(Step { hz: vec![hz], s: if e == '-' { 3.0 * dot } else { dot } });
            }
        }
    }
    out
}

/// Morse, letters and figures.
const MORSE: &[(&str, char)] = &[
    (".-", 'A'), ("-...", 'B'), ("-.-.", 'C'), ("-..", 'D'), (".", 'E'), ("..-.", 'F'), ("--.", 'G'), ("....", 'H'), ("..", 'I'), (".---", 'J'),
    ("-.-", 'K'), (".-..", 'L'), ("--", 'M'), ("-.", 'N'), ("---", 'O'), (".--.", 'P'), ("--.-", 'Q'), (".-.", 'R'), ("...", 'S'), ("-", 'T'),
    ("..-", 'U'), ("...-", 'V'), (".--", 'W'), ("-..-", 'X'), ("-.--", 'Y'), ("--..", 'Z'), ("-----", '0'), (".----", '1'), ("..---", '2'),
    ("...--", '3'), ("....-", '4'), (".....", '5'), ("-....", '6'), ("--...", '7'), ("---..", '8'), ("----.", '9'), ("-..-.", '/'), ("-...-", '='),
];

/// Morse: one tone keyed in dots and dashes (a dash three dots, a letter's
/// gap three, a word's seven), read as text. At least four elements, both
/// lengths among them, and text that reads (two letters or more, a fifth
/// at most not Morse).
fn morse(run: &[ToneSeg]) -> Option<(Pattern, usize)> {
    let f = hz(run.first()?);
    let short = |s: &ToneSeg| secs(s.len) <= 0.6;
    let mut n = 0;
    while run.get(n).is_some_and(|s| same(hz(s), f) && short(s) && (n == 0 || gap(&run[n - 1], s) <= 0.8)) {
        n += 1;
    }
    if n < 4 {
        return None;
    }
    let lens: Vec<f32> = run[..n].iter().map(|s| secs(s.len)).collect();
    let min = lens.iter().copied().fold(f32::MAX, f32::min);
    let dot = median(lens.iter().copied().filter(|&l| l < 2.0 * min).collect());
    if !lens.iter().any(|&l| l > 2.0 * dot) || lens.iter().any(|&l| l > 4.5 * dot) {
        return None;
    }
    let (mut text, mut sym, mut unknown) = (String::new(), String::new(), 0);
    let mut letter = |sym: &mut String, text: &mut String| {
        if !sym.is_empty() {
            let c = MORSE.iter().find(|m| m.0 == sym.as_str()).map_or('?', |m| m.1);
            unknown += (c == '?') as usize;
            text.push(c);
            sym.clear();
        }
    };
    for k in 0..n {
        if k > 0 {
            let g = gap(&run[k - 1], &run[k]);
            if g > 2.0 * dot {
                letter(&mut sym, &mut text);
            }
            if g > 5.0 * dot {
                text.push(' ');
            }
        }
        sym.push(if lens[k] > 2.0 * dot { '-' } else { '.' });
    }
    letter(&mut sym, &mut text);
    let letters = text.chars().filter(|c| *c != ' ').count();
    if letters < 2 || unknown * 5 > letters {
        return None;
    }
    let mut p = pattern(PatternKind::Morse, vec![f], vec![dot], &run[..n]);
    p.digits = text;
    p.cycles = n as u32;
    Some((p, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: f32 = SAMPLE_RATE as f32;

    fn tone(f: f32, at: f32, len: f32) -> ToneSeg {
        ToneSeg { kind: ToneKind::Single(f), start: (at * S) as usize, len: (len * S) as usize, level: None, origin: Origin::Frame }
    }

    fn kinds(p: &[Pattern]) -> Vec<&'static str> {
        p.iter().map(|p| p.kind.name()).collect()
    }

    /// The pages heard on CLMRN's Phase 2 dispatch talkgroups, 2026-10-21.
    #[test]
    fn pages_heard_on_the_air() {
        // Norwalk EMS: Quick Call II, with the vocoder's frame between A and B.
        let p = patterns(&[tone(781.25, 3.92, 0.92), tone(750.0, 4.84, 0.02), tone(875.0, 4.86, 3.0)]);
        assert_eq!(kinds(&p), ["two_tone"]);
        assert_eq!((p[0].hz.clone(), p[0].format), (vec![781.25, 875.0], Some("Quick Call II")));
        // Valley EMS: battery save, then a hi-low warble of 10 + 10.
        let mut segs = vec![tone(531.25, 1.0, 2.7), tone(593.75, 3.7, 0.8)];
        for k in 0..20 {
            segs.push(tone(if k % 2 == 0 { 812.5 } else { 1500.0 }, 5.0 + k as f32 * 0.1, if k % 2 == 0 { 0.1 } else { 0.08 }));
        }
        let p = patterns(&segs);
        assert_eq!(kinds(&p), ["two_tone", "hi_low"]);
        assert_eq!(p[0].format, Some("Quick Call II battery save"));
        assert_eq!((p[1].hz.clone(), p[1].cycles), (vec![812.5, 1500.0], 20));
        // VS Dispatch: two pages of 1 s + 2 s back to back (no format of Midian's).
        let p = patterns(&[tone(2000.0, 1.0, 1.0), tone(1906.25, 2.0, 2.0), tone(2000.0, 4.1, 1.0), tone(906.25, 5.1, 2.0)]);
        assert_eq!(kinds(&p), ["two_tone", "two_tone"]);
        assert_eq!(p[1].format, None);
        // Wilton EMS: Quick Call II one grid step apart.
        let p = patterns(&[tone(593.75, 0.76, 1.0), tone(562.5, 1.76, 3.0)]);
        assert_eq!((kinds(&p), p[0].format), (vec!["two_tone"], Some("Quick Call II")));
        // Valley EMS, A dropping out for 80 ms; North Branford, B.
        let p = patterns(&[tone(531.25, 1.84, 2.44), tone(531.25, 4.36, 0.14), tone(593.75, 4.5, 0.8)]);
        assert_eq!((kinds(&p), p[0].lengths.clone()), (vec!["two_tone"], vec![2.66, 0.8]));
        let p = patterns(&[tone(968.75, 0.08, 0.88), tone(1687.5, 0.96, 0.56), tone(1687.5, 1.6, 2.36)]);
        assert_eq!((kinds(&p), p[0].format), (vec!["two_tone"], Some("Quick Call II")));
        // Westport FD: 1000 Hz alert; Wilton Fire: an 8 s group call.
        assert_eq!(kinds(&patterns(&[tone(1000.0, 0.5, 3.5)])), ["long"]);
        assert_eq!(kinds(&patterns(&[tone(812.5, 0.5, 7.96)])), ["long"]);
    }

    #[test]
    fn short_tones() {
        // A descending chirp: a sequence.
        let p = patterns(&[tone(750.0, 1.0, 0.04), tone(718.75, 1.04, 0.04), tone(687.5, 1.08, 0.04), tone(656.25, 1.12, 0.04)]);
        assert_eq!(kinds(&p), ["sequence"]);
        assert_eq!(p[0].hz.len(), 4);
        // A beep, and a frame or two alone (dropped).
        assert_eq!(kinds(&patterns(&[tone(2000.0, 7.38, 0.2), tone(343.75, 9.0, 0.04)])), ["beep"]);
        // From the audio a lone beep needs 150 ms (speech holds a harmonic 60–80 ms).
        let heard = |f, at, len| ToneSeg { origin: Origin::Audio, ..tone(f, at, len) };
        assert_eq!(kinds(&patterns(&[heard(497.7, 1.0, 0.08), heard(2000.0, 3.0, 0.2)])), ["beep"]);
        // A tone broken by a dropped frame is one.
        assert_eq!(kinds(&patterns(&[tone(1593.75, 1.0, 0.5), tone(1593.75, 1.52, 0.46), tone(1468.75, 2.0, 3.0)])), ["two_tone"]);
        // Pulsed: 6 beeps of one tone.
        let segs: Vec<ToneSeg> = (0..6).map(|k| tone(1000.0, k as f32 * 0.4, 0.2)).collect();
        let p = patterns(&segs);
        assert_eq!((kinds(&p), p[0].cycles), (vec!["pulsed"], 6));
    }

    /// Woodbridge PW's repeater ID, 992 Hz at ~24 WPM (dot 60 ms as 20 ms blocks see it).
    #[test]
    fn patterns_play_as_they_sound() {
        let page = steps(PatternKind::TwoTone, &[781.25, 875.0], &[0.98], "", false, 0);
        assert_eq!(page, [Step { hz: vec![781.25], s: 0.98 }, Step { hz: vec![875.0], s: 3.0 }], "B's length not known: the usual 3 s");
        assert_eq!(steps(PatternKind::Digits, &[], &[], "1#", false, 0)[0].hz, [697.0, 1209.0]);
        assert_eq!(steps(PatternKind::Digits, &[], &[], "1", true, 0)[0].hz, [606.0, 1052.0], "KNOX's pad");
        let k = steps(PatternKind::Morse, &[990.0], &[0.06], "K", false, 0);
        assert_eq!(k.iter().map(|s| (s.hz.len(), (s.s / 0.06).round() as u32)).collect::<Vec<_>>(), [(1, 3), (0, 1), (1, 1), (0, 1), (1, 3)], "-.-");
        assert_eq!(steps(PatternKind::Long, &[1000.0], &[8.0], "", false, 0)[0].s, 4.0, "held 4 s at most");
    }

    #[test]
    fn morse_ids_read() {
        let (dot, mut at, mut segs) = (0.06f32, 0.5f32, Vec::new());
        for (i, word) in ["WRV713"].iter().enumerate() {
            if i > 0 {
                at += 4.0 * dot;
            }
            for c in word.chars() {
                let code = MORSE.iter().find(|m| m.1 == c).unwrap().0;
                for e in code.chars() {
                    let l = if e == '-' { 3.0 * dot } else { dot };
                    segs.push(tone(990.0, at, l));
                    at += l + dot;
                }
                at += 2.0 * dot;
            }
        }
        let p = patterns(&segs);
        assert_eq!(kinds(&p), ["morse"]);
        assert_eq!((p[0].digits.as_str(), p[0].hz[0]), ("WRV713", 990.0));
        // A pulsed alert isn't Morse: one length only.
        let segs: Vec<ToneSeg> = (0..8).map(|k| tone(1000.0, k as f32 * 0.4, 0.2)).collect();
        assert_eq!(kinds(&patterns(&segs)), ["pulsed"]);
    }

    #[test]
    fn digit_strings() {
        let d = |c, at, len| ToneSeg { kind: ToneKind::Dtmf(c), start: (at * S) as usize, len: (len * S) as usize, level: None, origin: Origin::Frame };
        let p = patterns(&[d('1', 0.0, 0.1), d('2', 0.2, 0.1), d('#', 0.4, 0.1), d('9', 2.0, 0.1), d('4', 5.0, 0.04)]);
        assert_eq!(p.iter().map(|p| p.digits.as_str()).collect::<Vec<_>>(), ["12#", "9"], "a lone 40 ms digit is left out");
    }

    #[test]
    fn formats_by_timing() {
        assert_eq!(format_of(0.98, 0.0, 2.98), Some("Quick Call II"));
        assert_eq!(format_of(2.44, 0.0, 0.8), Some("Quick Call II battery save"));
        assert_eq!(format_of(1.0, 0.0, 2.0), None);
        // A call picked up 160 ms into its A tone (CLMRN, 2026-10-22).
        assert_eq!(format_of(0.84, 0.0, 3.0), Some("Quick Call II"));
        assert_eq!(format_of(0.4, 0.0, 3.0), None, "under half an A");
        // The gap tells them apart.
        assert_eq!(format_of(1.0, 0.2, 1.0), Some("Quick Call I"));
        assert_eq!(format_of(1.0, 0.0, 1.0), Some("NEC C"));
        assert_eq!(format_of(1.0, 0.24, 3.0), Some("NEC A"));
        assert_eq!((format_plan("Quick Call I"), format_plan("NEC A")), (Some("QC1"), None));
    }
}
