//! Tone sets' timings from what TRP has recorded: each call's JSON keeps the
//! tones heard in it, with how long each lasted. A tone set's A length, gap
//! and B length are filled from its pages there (by tones alone), when they
//! agree with each other.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use trunk_recorder_plugin::TonePattern;

use crate::Lists;
use crate::agencies::{Agency, Len};

/// How long each page of an agency lasted: (A, gap, B) for two tones, (A, 0, 0) for one long tone.
pub type Heard = HashMap<usize, Vec<(f64, f64, f64)>>;

/// The agency `p` is by its tones alone (not its timing; not beeps), when only one on the channel has them.
fn by_tones(l: &Lists, ch: usize, p: &TonePattern) -> Option<usize> {
    let tol = l.cfg.tolerance_pct.clamp(0.1, 10.0) / 100.0;
    let near = |got: f64, want: f64| want > 0.0 && (got - want).abs() <= want * tol;
    let t = |i: usize| p.tones.get(i).copied().unwrap_or(0.0);
    let mut hit = None;
    for (i, a) in l.cfg.agencies.iter().enumerate() {
        if a.beeps.is_some_and(|n| n > 0) || !l.on_channel(i, ch) {
            continue;
        }
        let fits = if a.single() { p.pattern == "long" && near(t(0), a.tone_a) } else { p.pattern == "two_tone" && near(t(0), a.tone_a) && a.tone_b.is_some_and(|b| near(t(1), b)) };
        if fits {
            if hit.is_some() {
                return None; // Two agencies with these tones: can't tell whose.
            }
            hit = Some(i);
        }
    }
    hit
}

/// The day folders (`<system>/<Y>/<M>/<D>`) under `sys`, newest first.
fn days(sys: &Path) -> Vec<PathBuf> {
    let sub = |p: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().chars().all(|c| c.is_ascii_digit()))).collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u32>().ok()).unwrap_or(0)));
        v
    };
    sub(sys).iter().flat_map(|y| sub(y)).flat_map(|m| sub(&m)).collect()
}

/// Every page of an agency in the calls saved under `capture`, over the last `max_days` days (of each system's).
pub fn heard(l: &Lists, capture: &Path, max_days: usize) -> (Heard, usize) {
    let mut out: Heard = HashMap::new();
    let mut calls = 0;
    let systems: Vec<PathBuf> = std::fs::read_dir(capture).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    for sys in systems {
        let name = sys.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // The channels on this system, by talkgroup.
        let tgs: HashMap<u32, usize> = l.cfg.channels.iter().enumerate().filter_map(|(k, _)| l.follows(k).filter(|(s, _)| s.is_empty() || s.eq_ignore_ascii_case(&name)).map(|(_, tg)| (tg, k))).collect();
        if tgs.is_empty() {
            continue;
        }
        for day in days(&sys).into_iter().take(max_days) {
            for e in std::fs::read_dir(&day).into_iter().flatten().flatten() {
                let f = e.path();
                let fname = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let Some(ch) = fname.strip_suffix(".json").and_then(|n| n.split('-').next()).and_then(|tg| tg.parse::<u32>().ok()).and_then(|tg| tgs.get(&tg).copied()) else { continue };
                let Ok(v) = std::fs::read(&f).map_err(|_| ()).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).map_err(|_| ())) else { continue };
                let Some(ps) = v.get("tone_patterns").cloned().and_then(|t| serde_json::from_value::<Vec<TonePattern>>(t).ok()) else { continue };
                calls += 1;
                for p in ps {
                    let Some(i) = by_tones(l, ch, &p) else { continue };
                    let len = |k: usize| p.lengths.get(k).copied().unwrap_or(0.0);
                    let s = if p.pattern == "two_tone" { (len(0), (p.len - len(0) - len(1)).max(0.0), len(1)) } else { (len(0).max(p.len), 0.0, 0.0) };
                    out.entry(i).or_default().push(s);
                }
            }
        }
    }
    (out, calls)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn secs(v: f64) -> String {
    let s = format!("{:.1}", (v * 10.0).round() / 10.0);
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// A typical length that every page heard fits (`gap`: by a gap's looser rule), else none.
fn steady(v: Vec<f64>, gap: bool) -> Option<String> {
    let m = (median(v.clone()) * 10.0).round() / 10.0;
    let l = Len::Exact(m);
    v.iter().all(|&x| if gap { l.fits_gap(x) } else { l.fits(x) }).then(|| secs(m))
}

/// Fills the empty timings of `a` from `pages`. What it filled, as words ("" for nothing).
pub fn fill(a: &mut Agency, pages: &[(f64, f64, f64)]) -> String {
    if pages.is_empty() || a.beeps.is_some_and(|n| n > 0) {
        return String::new();
    }
    let mut did = Vec::new();
    let col = |k: usize| pages.iter().map(|p| [p.0, p.1, p.2][k]).collect::<Vec<f64>>();
    if a.single() {
        // A long tone runs as long as it's held: at least the shortest heard.
        if a.a_s.trim().is_empty() {
            let min = col(0).into_iter().fold(f64::MAX, f64::min);
            if min >= 0.5 {
                a.a_s = format!("{}+", secs((min * 10.0).floor() / 10.0));
                did.push(format!("A {} s", a.a_s));
            }
        }
    } else {
        if a.a_s.trim().is_empty()
            && let Some(v) = steady(col(0), false)
        {
            did.push(format!("A {v} s"));
            a.a_s = v;
        }
        if a.gap_s.trim().is_empty()
            && median(col(1)) >= 0.1
            && let Some(v) = steady(col(1), true)
        {
            did.push(format!("gap {v} s"));
            a.gap_s = v;
        }
        if a.b_s.trim().is_empty()
            && let Some(v) = steady(col(2), false)
        {
            did.push(format!("B {v} s"));
            a.b_s = v;
        }
    }
    did.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, Config};

    fn two(a: f64, b: f64, la: f64, gap: f64, lb: f64) -> serde_json::Value {
        serde_json::json!({ "pattern": "two_tone", "tones": [a, b], "lengths": [la, lb], "pos": 0.0, "len": la + gap + lb })
    }

    #[test]
    fn fills_from_recorded_calls() {
        let dir = trunk_recorder_plugin::testing::temp_dir("page-timings");
        let day = dir.join("UHF/2026/11/23");
        std::fs::create_dir_all(&day).unwrap();
        let call = |n: u32, tg: u32, ps: Vec<serde_json::Value>| std::fs::write(day.join(format!("{tg}-17640{n}_453900000.json")), serde_json::json!({ "talkgroup": tg, "tone_patterns": ps }).to_string()).unwrap();
        call(1, 1, vec![two(1179.98, 2574.93, 1.0, 0.05, 3.0)]);
        call(2, 1, vec![two(1180.1, 2575.2, 1.02, 0.04, 2.96), two(950.0, 2575.0, 2.0, 0.6, 1.0)]);
        call(3, 1, vec![two(950.0, 2575.0, 1.0, 0.6, 5.0)]);
        call(4, 2, vec![two(731.9, 1285.9, 2.0, 0.0, 1.0)]); // another talkgroup: not a channel
        call(5, 1, vec![serde_json::json!({ "pattern": "long", "tones": [1082.0], "lengths": [7.6], "pos": 0.0, "len": 7.6 })]);
        let agencies = vec![
            Agency { name: "Empress EMS".into(), tone_a: 1180.0, tone_b: Some(2575.0), channel: "453.9".into(), ..Default::default() },
            Agency { name: "Countywide".into(), tone_a: 950.0, tone_b: Some(2575.0), channel: "453.9".into(), ..Default::default() },
            Agency { name: "8 second".into(), tone_a: 1082.0, channel: "453.9".into(), ..Default::default() },
            Agency { name: "Set already".into(), tone_a: 731.9, tone_b: Some(1285.9), a_s: "2".into(), ..Default::default() },
        ];
        let l = Lists::new(Config { channels: vec![Channel { name: "DC 911".into(), freq_mhz: 453.9, follow: "UHF/1".into(), talkgroup: None, rdio_system: None, send: false }], agencies, ..Config::default() });
        let (h, calls) = heard(&l, &dir, 90);
        assert_eq!(calls, 4);
        let mut a = l.cfg.agencies.clone();
        assert_eq!(fill(&mut a[0], &h[&0]), "A 1 s, B 3 s", "a gap under 0.1 s isn't set");
        // Countywide's A and B were held differently each time: left alone.
        assert_eq!(fill(&mut a[1], &h[&1]), "gap 0.6 s");
        assert!(a[1].a_s.is_empty() && a[1].b_s.is_empty());
        assert_eq!(fill(&mut a[2], &h[&2]), "A 7.6+ s");
        assert!(!h.contains_key(&3));
    }
}
