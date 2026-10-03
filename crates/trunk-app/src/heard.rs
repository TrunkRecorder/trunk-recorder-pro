//! What codes each conventional frequency has carried — CTCSS tones and DCS
//! codes, NACs, DMR colour codes / slots / talkgroups — counted across runs,
//! so a frequency's codes can be found by recording it without any and
//! looking. Kept as JSON beside the config (`conventional.heard.json`):
//!
//! ```text
//! { "154325000": [ { "code": "151.4", "calls": 42, "skipped": 0, "lastMs": 1790683195000 },
//!                  { "code": "",      "calls": 5,  "skipped": 2, "lastMs": … } ] }
//! ```
//!
//! `code` is in the form a row's Tone takes (`151.4`, `D023N`, `NAC 293`,
//! `CC 1 TS 2 TG 201`; "" for none), so the interface can add a row for one.
//! `calls` were recorded; `skipped` transmissions no row took.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Codes kept per frequency (the least recently heard go first).
const MAX_CODES: usize = 40;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Code {
    pub code: String,
    pub calls: u32,
    pub skipped: u32,
    pub last_ms: f64,
}

#[derive(Default)]
pub struct HeardCodes {
    freqs: BTreeMap<u64, Vec<Code>>,
    /// Changed since last saved / since last shown.
    unsaved: bool,
    unshown: bool,
}

impl HeardCodes {
    /// What [`HeardCodes::json`] saved (anything unreadable: nothing).
    pub fn load(text: &str) -> Self {
        let freqs: BTreeMap<String, Vec<Code>> = serde_json::from_str(text).unwrap_or_default();
        let freqs = freqs.into_iter().filter_map(|(f, v)| Some((f.parse().ok()?, v))).collect();
        HeardCodes { freqs, unsaved: false, unshown: true }
    }

    /// `{ "<freq Hz>": [codes, most heard first] }`.
    pub fn json(&self) -> Value {
        Value::Object(self.freqs.iter().map(|(f, v)| (f.to_string(), serde_json::to_value(v).unwrap_or_default())).collect())
    }

    /// A call (`recorded`) or a transmission no row took, carrying `code`.
    pub fn note(&mut self, freq_hz: u64, code: &str, recorded: bool, at_ms: f64) {
        let v = self.freqs.entry(freq_hz).or_default();
        let i = match v.iter().position(|c| c.code == code) {
            Some(i) => i,
            None => {
                if v.len() >= MAX_CODES {
                    let old = (0..v.len()).min_by(|&a, &b| v[a].last_ms.total_cmp(&v[b].last_ms)).unwrap();
                    v.remove(old);
                }
                v.push(Code { code: code.to_string(), ..Default::default() });
                v.len() - 1
            }
        };
        let c = &mut v[i];
        if recorded {
            c.calls += 1;
        } else {
            c.skipped += 1;
        }
        c.last_ms = c.last_ms.max(at_ms);
        v.sort_by(|a, b| (b.calls + b.skipped).cmp(&(a.calls + a.skipped)).then(b.last_ms.total_cmp(&a.last_ms)));
        self.unsaved = true;
        self.unshown = true;
    }

    /// The JSON to save, when there is news since the last call.
    pub fn take_unsaved(&mut self) -> Option<String> {
        std::mem::take(&mut self.unsaved).then(|| self.json().to_string())
    }

    /// Whether there is news for the interface since the last call.
    pub fn take_unshown(&mut self) -> bool {
        std::mem::take(&mut self.unshown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_sorts_saves_and_loads() {
        let mut h = HeardCodes::default();
        h.note(154_325_000, "151.4", true, 1.0);
        h.note(154_325_000, "D023N", false, 2.0);
        h.note(154_325_000, "D023N", false, 3.0);
        h.note(154_325_000, "", true, 4.0);
        let saved = h.take_unsaved().unwrap();
        assert!(h.take_unsaved().is_none());
        let back = HeardCodes::load(&saved);
        let v = &back.freqs[&154_325_000];
        assert_eq!(v[0], Code { code: "D023N".into(), calls: 0, skipped: 2, last_ms: 3.0 });
        assert_eq!(v.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(), ["D023N", "", "151.4"]);
        assert_eq!(back.json()["154325000"][0]["skipped"], 2);
        // A full frequency forgets the code heard longest ago.
        let mut h = HeardCodes::default();
        for i in 0..=MAX_CODES {
            h.note(1, &format!("NAC {i:03X}"), true, i as f64);
        }
        assert_eq!(h.freqs[&1].len(), MAX_CODES);
        assert!(!h.freqs[&1].iter().any(|c| c.code == "NAC 000"));
        assert!(HeardCodes::load("not json").freqs.is_empty());
    }
}
