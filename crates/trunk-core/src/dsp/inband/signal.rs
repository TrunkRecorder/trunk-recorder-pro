//! A system's signals: tone patterns it sends, by an id of its own
//! (`CSP_FIRE` on CLMRN, written `CLMRN:CSP_FIRE` where the system isn't
//! said), and which heard [`Pattern`]s are them.

use super::pattern::{Pattern, PatternKind};
use super::tables::{candidates, tolerance};
use super::Origin;
use trunk_recorder_plugin::filter::talkgroup_matches;

#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    /// Unique within its system.
    pub id: String,
    pub pattern: PatternKind,
    /// Its tones, Hz, in order (digits and Morse: none needed).
    pub hz: Vec<f32>,
    /// Digits: the string; Morse: the text.
    pub digits: String,
    /// Digits: KNOX's tones, not DTMF's.
    pub knox: bool,
    /// How much further than where it was heard allows ([`tolerance`]) a
    /// heard tone may be from `hz`, % of it (0: no further).
    pub extra_pct: f32,
    /// The talkgroups it's sent on: numbers, ranges (`1000-1099`) and
    /// patterns (`507*`, `12?45`). Empty: any.
    pub talkgroups: Vec<String>,
}

impl Signal {
    /// Whether `p`, heard on talkgroup `tg`, is this signal.
    pub fn matches(&self, p: &Pattern, tg: u32) -> bool {
        self.matches_heard(&Heard { kind: p.kind, hz: &p.hz, digits: &p.digits, knox: p.knox, frame: p.origin == Origin::Frame }, tg)
    }

    /// Whether a pattern heard on talkgroup `tg`, described by its parts, is this signal.
    pub fn matches_heard(&self, p: &Heard, tg: u32) -> bool {
        if p.kind != self.pattern || !(self.talkgroups.is_empty() || self.talkgroups.iter().any(|t| talkgroup_matches(t, tg))) {
            return false;
        }
        match p.kind {
            PatternKind::Digits => self.knox == p.knox && self.digits == p.digits,
            PatternKind::Morse => self.digits.eq_ignore_ascii_case(p.digits),
            _ => {
                let tol = |f: f32| tolerance(f, p.frame) + f * self.extra_pct / 100.0;
                self.hz.len() == p.hz.len() && self.hz.iter().zip(p.hz).all(|(&want, &got)| (want - got).abs() <= tol(want))
            }
        }
    }
}

/// What matching a pattern takes of it (a [`Pattern`], or one as a log keeps it).
pub struct Heard<'a> {
    pub kind: PatternKind,
    pub hz: &'a [f32],
    pub digits: &'a str,
    pub knox: bool,
    /// Its tones came from the vocoder's tone frames ([`tolerance`]).
    pub frame: bool,
}

/// What the recorder counts as a signal of `kind` with these tones, for a
/// person: how near each has to be when heard in the audio and from tone
/// frames ([`tolerance`]), and which other paging codes that takes in.
pub fn tolerance_note(kind: PatternKind, hz: &[f32], knox: bool, extra_pct: f32) -> String {
    match kind {
        PatternKind::Digits => return format!("Matched by its digits, exactly, sent as {}.", if knox { "KNOX tones" } else { "DTMF" }),
        PatternKind::Morse => return "Matched by its text, in either case; its pitch isn't matched.".into(),
        _ => {}
    }
    let hz: Vec<f32> = hz.iter().copied().filter(|&f| f > 0.0).collect();
    let tol = |f: f32, frame: bool| tolerance(f, frame) + f * extra_pct / 100.0;
    let r1 = |v: f32| (v * 10.0).round() / 10.0;
    let each = |frame: bool| hz.iter().map(|&f| format!("{} ± {}", r1(f), r1(tol(f, frame)))).collect::<Vec<_>>().join(", ");
    // Other paging tones a window takes in: they'd be matched too.
    let near = |frame: bool| {
        let mut v: Vec<String> = Vec::new();
        for &f in &hz {
            for c in candidates(f, tol(f, frame)).into_iter().filter(|c| (c.hz - f).abs() > 0.002 * f) {
                let t = format!("{} Hz ({} {})", c.hz, c.plan, c.code);
                if !v.contains(&t) {
                    v.push(t);
                }
            }
        }
        v
    };
    let (in_audio, in_frames) = (near(false), near(true));
    let in_frames: Vec<String> = in_frames.into_iter().filter(|t| !in_audio.contains(t)).collect();
    let plus = if extra_pct > 0.0 { format!(" + {extra_pct} %") } else { String::new() };
    let mut out = format!("A tone heard counts as one of these within ±1.5 %{plus} of it when measured in the audio (P25 Phase 1, analog)");
    if !hz.is_empty() {
        out += &format!(": {} Hz", each(false));
    }
    out += &format!("; and within 15.7 Hz + 0.5 %{plus} when a P25 Phase 2 tone frame names it, as those name tones only to the nearest 31.25 Hz");
    if !hz.is_empty() {
        out += &format!(": {} Hz", each(true));
    }
    out += ".";
    if !in_audio.is_empty() {
        out += &format!(" That also takes in {}: it would match them too.", in_audio.join(", "));
    }
    if !in_frames.is_empty() {
        out += &format!(" From Phase 2 tone frames it can't be told from {}.", in_frames.join(", "));
    }
    out + " Lengths aren't matched (pagers vary them): they're what ▶ plays."
}

/// The ids of the signals `p` is, heard on talkgroup `tg`.
pub fn matching<'a>(signals: &'a [Signal], p: &Pattern, tg: u32) -> impl Iterator<Item = &'a str> {
    signals.iter().filter(move |s| s.matches(p, tg)).map(|s| s.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(hz: [f32; 2], origin: Origin) -> Pattern {
        Pattern { kind: PatternKind::TwoTone, hz: hz.to_vec(), lengths: vec![1.0, 3.0], digits: String::new(), knox: false, cycles: 0, format: None, start: 0, len: 32000, origin }
    }

    fn signal(id: &str, kind: PatternKind, hz: &[f32], digits: &str, tgs: &[&str]) -> Signal {
        Signal { id: id.into(), pattern: kind, hz: hz.to_vec(), digits: digits.into(), knox: false, extra_pct: 0.0, talkgroups: tgs.iter().map(|t| t.to_string()).collect() }
    }

    #[test]
    fn notes_say_what_counts_and_what_else_it_takes_in() {
        let n = tolerance_note(PatternKind::TwoTone, &[788.5, 879.0], false, 0.0);
        assert!(n.contains("788.5 ± 11.8, 879 ± 13.2 Hz"), "{n}");
        // QC2 126 is GE Type 99 B2's neighbour: other plans' tones are named too, not only Quick Call II's.
        assert!(n.contains("GE99"), "{n}");
        assert_eq!(tolerance_note(PatternKind::Digits, &[], true, 0.0), "Matched by its digits, exactly, sent as KNOX tones.");
    }

    #[test]
    fn pages_digits_and_morse() {
        // The chart's QC2 126/128 against Phase 2's 781.25 / 875; and the audio's 788 / 880.
        let norwalk = signal("NORWALK_EMS", PatternKind::TwoTone, &[788.5, 879.0], "", &["20341"]);
        assert!(norwalk.matches(&page([781.25, 875.0], Origin::Frame), 20341));
        assert!(norwalk.matches(&page([788.0, 880.0], Origin::Audio), 20341));
        assert!(!norwalk.matches(&page([781.25, 875.0], Origin::Frame), 20342), "another talkgroup");
        assert!(!norwalk.matches(&page([875.0, 781.25], Origin::Frame), 20341), "B then A");
        assert!(!norwalk.matches(&page([760.0, 880.0], Origin::Audio), 20341), "A 3.6 % off in the audio");
        assert!(!norwalk.matches(&page([775.0, 880.0], Origin::Audio), 20341), "A 1.7 % off");
        let wider = Signal { extra_pct: 2.5, ..norwalk.clone() };
        assert!(wider.matches(&page([760.0, 880.0], Origin::Audio), 20341), "within 1.5 % + 2.5 %");
        let mut dtmf = page([0.0, 0.0], Origin::Audio);
        (dtmf.kind, dtmf.hz, dtmf.digits) = (PatternKind::Digits, vec![], "072864".into());
        assert!(signal("SIREN_SIGNAL_2", PatternKind::Digits, &[], "072864", &[]).matches(&dtmf, 3));
        dtmf.knox = true;
        assert!(!signal("SIREN_SIGNAL_2", PatternKind::Digits, &[], "072864", &[]).matches(&dtmf, 3), "the same keys on KNOX");
        assert!(Signal { knox: true, ..signal("SIREN_KNOX", PatternKind::Digits, &[], "072864", &[]) }.matches(&dtmf, 3));
        let mut cw = page([0.0, 0.0], Origin::Audio);
        (cw.kind, cw.hz, cw.digits) = (PatternKind::Morse, vec![990.0], "WRV713".into());
        let id = [signal("PW_ID", PatternKind::Morse, &[], "wrv713", &[]), signal("OTHER", PatternKind::Morse, &[], "WRV714", &[])];
        assert_eq!(matching(&id, &cw, 3).collect::<Vec<_>>(), ["PW_ID"]);
    }
}
