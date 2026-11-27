//! Agencies (Tone Board's list: name, tones, timing, beeps, channel, alert,
//! sound, unit) and matching TRP's tone patterns against them with Tone
//! Board's rules.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use trunk_recorder_plugin::TonePattern;

/// Whether it speaks here, from Tone Board's CSV `alert` column: `yes`, `on duty`, `off duty` (all yes) or empty (no).
pub fn speaks_csv(s: &str) -> bool {
    !matches!(s.trim().to_ascii_lowercase().as_str(), "" | "no" | "n" | "false" | "0" | "off")
}

/// `alert` as saved: On/Off, or (from 0.1.x) `off`, `onDuty`, `offDuty`, `both`.
pub fn speak_flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::String(s) => speaks_csv(&s),
        _ => false,
    })
}

/// An agency: a page (tone A then tone B), a single long tone (no tone B), or beeps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[schemars(title = "Tone set", extend("x-title" = "name", "x-summary" = ["{toneA} Hz", "{toneB} Hz", "{beeps} beeps", "A {aS} s", "gap {gapS} s", "B {bS} s", "Unit {unit}", "{channel|any channel}", "{alert}", "{sound}"]))]
#[schemars(extend("x-actions" = [
    { "id": "testAgency", "label": "Test", "hint": "Speaks a test for this agency here and sends it to Rdio Scanner where its pages go" },
    { "id": "fillTimings", "label": "Fill timings", "hint": "Its A length, gap and B length, from the pages of it TRP has recorded (only those still empty)" }
]))]
pub struct Agency {
    /// Name
    pub name: String,
    /// Tone A, Hz
    pub tone_a: f64,
    /// Tone B, Hz
    ///
    /// Empty: one long tone (or beeps).
    pub tone_b: Option<f64>,
    /// Unit ID
    ///
    /// Its Rdio Scanner talkgroup.
    pub unit: Option<u32>,
    /// Channel
    ///
    /// Empty: any. MHz or a channel's name; several with `/`.
    pub channel: String,
    /// Speak
    ///
    /// Sound and name on this computer's speakers.
    #[serde(deserialize_with = "speak_flag")]
    pub alert: bool,
    /// Sound
    ///
    /// Empty, `beep`, or a file in the sounds folder.
    pub sound: String,
    /// Notes
    pub notes: String,
    /// A length, s
    ///
    /// Empty: not checked. `1+`: at least.
    pub a_s: String,
    /// Gap, s
    pub gap_s: String,
    /// B length, s
    pub b_s: String,
    /// Beeps
    ///
    /// Fewest short beeps on tone A (no tone B).
    pub beeps: Option<u32>,
}

impl Default for Agency {
    fn default() -> Self {
        Agency { name: String::new(), tone_a: 0.0, tone_b: None, unit: None, channel: String::new(), alert: true, sound: "beep".into(), notes: String::new(), a_s: String::new(), gap_s: String::new(), b_s: String::new(), beeps: None }
    }
}

/// A set length: exact (within a quarter, at least 0.15 s), or at least (`1+`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Len {
    Exact(f64),
    AtLeast(f64),
}

pub fn parse_len(s: &str) -> Option<Len> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(v) = s.strip_suffix('+') {
        return v.trim().parse().ok().filter(|v: &f64| *v > 0.0).map(Len::AtLeast);
    }
    s.parse().ok().filter(|v: &f64| *v > 0.0).map(Len::Exact)
}

impl Len {
    pub fn fits(self, got: f64) -> bool {
        match self {
            Len::Exact(v) => (got - v).abs() <= (v / 4.0).max(0.15),
            Len::AtLeast(v) => got >= v - (v / 4.0).max(0.15),
        }
    }
    pub fn fits_gap(self, got: f64) -> bool {
        match self {
            Len::Exact(v) => (got - v).abs() <= (v / 2.0).max(0.15),
            Len::AtLeast(v) => got >= v - (v / 2.0).max(0.15),
        }
    }
}

fn near(got: f64, want: f64, tol: f64) -> Option<f64> {
    if want <= 0.0 {
        return None;
    }
    let e = (got - want).abs() / want;
    (e <= tol).then_some(e)
}

impl Agency {
    /// A single tone: no tone B, or tone B the same as A.
    pub fn single(&self) -> bool {
        self.tone_b.is_none_or(|b| (b - self.tone_a).abs() <= self.tone_a * 0.005)
    }

    /// How far off `p` is (the worst tone's relative error), when it's this agency's
    /// page, alert tone or beeps. `beeps`: the beeps on its tone heard so far in the
    /// call, this pattern's included.
    pub fn fits(&self, p: &TonePattern, tol: f64, beeps: u32) -> Option<f64> {
        let f = |i: usize| p.tones.get(i).copied().unwrap_or(0.0);
        let l = |i: usize| p.lengths.get(i).copied().unwrap_or(0.0);
        if let Some(n) = self.beeps.filter(|n| *n > 0) {
            if !matches!(p.pattern.as_str(), "beep" | "pulsed" | "sequence" | "long") {
                return None;
            }
            let e = p.tones.iter().map(|t| near(*t, self.tone_a, tol)).try_fold(0.0f64, |m, e| e.map(|e| m.max(e)))?;
            return (beeps >= n && !p.tones.is_empty()).then_some(e);
        }
        if self.single() {
            let a = parse_len(&self.a_s);
            let total = if self.tone_b.is_some() { a.zip(parse_len(&self.b_s)) } else { None };
            match p.pattern.as_str() {
                "long" => {}
                // A short alert tone ("0.3+"): TRP calls a lone tone under a second a beep.
                "beep" if a.is_some() => {}
                _ => return None,
            }
            let e = near(f(0), self.tone_a, tol)?;
            let len = l(0).max(p.len);
            let ok = match (a, total) {
                (Some(Len::Exact(x)), Some((_, Len::Exact(y)))) => Len::Exact(x + y).fits(len),
                (Some(a), _) => a.fits(len),
                (None, _) => true,
            };
            return ok.then_some(e);
        }
        if p.pattern != "two_tone" {
            return None;
        }
        let ea = near(f(0), self.tone_a, tol)?;
        let eb = near(f(1), self.tone_b?, tol)?;
        if parse_len(&self.a_s).is_some_and(|x| !x.fits(l(0))) || parse_len(&self.b_s).is_some_and(|x| !x.fits(l(1))) {
            return None;
        }
        let gap = (p.len - l(0) - l(1)).max(0.0);
        if parse_len(&self.gap_s).is_some_and(|x| !x.fits_gap(gap)) {
            return None;
        }
        Some(ea.max(eb))
    }
}

/// One CSV field list from a line, with quotes ("a ""b"", c").
fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, q) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', _) => q = !q,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Tone Board's agency CSV (`name,tone_a,tone_b,notes,alert,sound,channel,unit,a_s,gap_s,b_s,beeps`).
/// The agencies, and the lines it couldn't read.
pub fn read_csv(text: &str) -> (Vec<Agency>, Vec<String>) {
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let v = csv_fields(line);
        let g = |i: usize| v.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
        if n == 0 && g(0).eq_ignore_ascii_case("name") {
            continue;
        }
        let Ok(tone_a) = g(1).parse::<f64>() else {
            bad.push(format!("line {}: no tone A ({})", n + 1, g(0)));
            continue;
        };
        out.push(Agency {
            name: g(0),
            tone_a,
            tone_b: g(2).parse().ok().filter(|b: &f64| *b > 0.0),
            notes: g(3),
            alert: speaks_csv(&g(4)),
            sound: g(5),
            channel: g(6),
            unit: g(7).parse().ok(),
            a_s: g(8),
            gap_s: g(9),
            b_s: g(10),
            beeps: g(11).parse().ok().filter(|b: &u32| *b > 0),
        });
    }
    (out, bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two(a: f64, b: f64, la: f64, lb: f64, len: f64) -> TonePattern {
        TonePattern { pattern: "two_tone".into(), tones: vec![a, b], lengths: vec![la, lb], len, ..Default::default() }
    }

    #[test]
    fn reads_tone_boards_csv() {
        let csv = "name,tone_a,tone_b,notes,alert,sound,channel,unit,a_s,gap_s,b_s,beeps\n\
Empress EMS,1180,2575,Empress EMS,yes,beep,453.9,202,,,,\n\
City of Newburgh Fire Department Alert,1000,,City of Newburgh Single tone,yes,beep,460.4625,14999,1+,,,\n\
\"City of Poughkeepsie \"\"Hot Job\"\"\",1000,,City PD Hot Job Alert,yes,beep,155.440,149112,,0.614,1.2,2\n\
Howels FD -Pagers,1926,1201,\"From survey: heard 1 times, A 1.00 s\",,,154.205,,,,,\n";
        let (a, bad) = read_csv(csv);
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(a.len(), 4);
        assert_eq!((a[0].tone_b, a[0].unit, a[0].alert), (Some(2575.0), Some(202), true));
        assert_eq!(a[1].tone_b, None);
        assert_eq!(parse_len(&a[1].a_s), Some(Len::AtLeast(1.0)));
        assert_eq!(a[2].name, "City of Poughkeepsie \"Hot Job\"");
        assert_eq!(a[2].beeps, Some(2));
        assert!(!a[3].alert);
        assert_eq!(a[3].notes, "From survey: heard 1 times, A 1.00 s");
    }

    #[test]
    fn pages_match_by_tones_and_timing() {
        let (a, _) = read_csv("Empress EMS,1180,2575,,yes,,453.9,202,,,,\nCrash,599,799,,yes,,154.325,1,1.8,0.1,3.8,\n");
        // As TRP heard it on DC911: 1179.98 → 2574.93 Hz, 2 + 1 s.
        assert!(a[0].fits(&two(1179.98, 2574.93, 2.0, 1.0, 3.0), 0.015, 0).is_some());
        assert!(a[0].fits(&two(1179.98, 2650.0, 2.0, 1.0, 3.0), 0.015, 0).is_none(), "B too far off");
        assert!(a[0].fits(&two(2574.93, 1179.98, 1.0, 2.0, 3.0), 0.015, 0).is_none(), "the wrong way round");
        // Timing set: 1.8 s, 0.1 s gap, 3.8 s.
        assert!(a[1].fits(&two(599.5, 799.0, 1.8, 3.8, 5.7), 0.015, 0).is_some());
        assert!(a[1].fits(&two(599.5, 799.0, 1.0, 3.0, 4.0), 0.015, 0).is_none(), "QC2 timing isn't this agency's");
    }

    #[test]
    fn long_tones_and_beeps() {
        let (a, _) = read_csv("Newburgh,1000,,,yes,,460.4625,1,1+,,,\nBOLO,1004,,,yes,,155.415,2,,,,2\nFDNY,1000,,,yes,,482.256,3,0.3+,,,\n");
        let long = |f: f64, l: f64| TonePattern { pattern: "long".into(), tones: vec![f], lengths: vec![l], len: l, ..Default::default() };
        let beep = |f: f64| TonePattern { pattern: "beep".into(), tones: vec![f], lengths: vec![0.25], len: 0.25, ..Default::default() };
        assert!(a[0].fits(&long(999.8, 2.9), 0.015, 0).is_some());
        assert!(a[0].fits(&beep(999.8), 0.015, 1).is_none(), "under its 1 s");
        assert!(a[1].fits(&beep(1003.0), 0.015, 1).is_none(), "one beep of two");
        assert!(a[1].fits(&beep(1003.0), 0.015, 2).is_some(), "the second");
        assert!(a[2].fits(&beep(1000.0), 0.015, 1).is_some(), "0.3+ takes a short tone");
    }
}
