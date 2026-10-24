//! A system's signals: tone patterns it sends, by an id of its own
//! (`CSP_FIRE` on CLMRN, written `CLMRN:CSP_FIRE` where the system isn't
//! said), and which heard [`Pattern`]s are them.

use super::pattern::{Pattern, PatternKind};
use super::tables::tolerance;
use super::Origin;

#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    /// Unique within its system.
    pub id: String,
    pub pattern: PatternKind,
    /// Its tones, Hz, in order (digits and Morse: none needed).
    pub hz: Vec<f32>,
    /// Digits: the string (`KNOX ` first for KNOX tones); Morse: the text.
    pub digits: String,
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
        if p.kind != self.pattern || !(self.talkgroups.is_empty() || self.talkgroups.iter().any(|t| talkgroup_matches(t, tg))) {
            return false;
        }
        match p.kind {
            PatternKind::Digits => self.digits == if p.knox { format!("KNOX {}", p.digits) } else { p.digits.clone() },
            PatternKind::Morse => self.digits.eq_ignore_ascii_case(&p.digits),
            _ => {
                let frame = p.origin == Origin::Frame;
                let tol = |f: f32| tolerance(f, frame) + f * self.extra_pct / 100.0;
                self.hz.len() == p.hz.len() && self.hz.iter().zip(&p.hz).all(|(&want, &got)| (want - got).abs() <= tol(want))
            }
        }
    }
}

/// The ids of the signals `p` is, heard on talkgroup `tg`.
pub fn matching<'a>(signals: &'a [Signal], p: &Pattern, tg: u32) -> impl Iterator<Item = &'a str> {
    signals.iter().filter(move |s| s.matches(p, tg)).map(|s| s.id.as_str())
}

/// A talkgroup list's entry names `tg`: a number, a range `1000-1099`, or a
/// pattern where `*` is any run of digits and `?` any one.
pub fn talkgroup_matches(entry: &str, tg: u32) -> bool {
    let e = entry.trim();
    if let Some((a, b)) = e.split_once('-')
        && let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>())
    {
        return (a..=b).contains(&tg);
    }
    glob(e.as_bytes(), tg.to_string().as_bytes())
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob(&p[1..], s) || (!s.is_empty() && glob(p, &s[1..])),
        (Some(b'?'), Some(_)) => glob(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => glob(&p[1..], &s[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(hz: [f32; 2], origin: Origin) -> Pattern {
        Pattern { kind: PatternKind::TwoTone, hz: hz.to_vec(), lengths: vec![1.0, 3.0], digits: String::new(), knox: false, cycles: 0, format: None, start: 0, len: 32000, origin }
    }

    fn signal(id: &str, kind: PatternKind, hz: &[f32], digits: &str, tgs: &[&str]) -> Signal {
        Signal { id: id.into(), pattern: kind, hz: hz.to_vec(), digits: digits.into(), extra_pct: 0.0, talkgroups: tgs.iter().map(|t| t.to_string()).collect() }
    }

    #[test]
    fn talkgroup_lists() {
        assert!(talkgroup_matches("401", 401) && !talkgroup_matches("401", 4010));
        assert!(talkgroup_matches("400-499", 401) && !talkgroup_matches("400-499", 500));
        assert!(talkgroup_matches("40*", 401) && talkgroup_matches("4?1", 401) && !talkgroup_matches("5*", 401));
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
        let mut cw = page([0.0, 0.0], Origin::Audio);
        (cw.kind, cw.hz, cw.digits) = (PatternKind::Morse, vec![990.0], "WRV713".into());
        let id = [signal("PW_ID", PatternKind::Morse, &[], "wrv713", &[]), signal("OTHER", PatternKind::Morse, &[], "WRV714", &[])];
        assert_eq!(matching(&id, &cw, 3).collect::<Vec<_>>(), ["PW_ID"]);
    }
}
