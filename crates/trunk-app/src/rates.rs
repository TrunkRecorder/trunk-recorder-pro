//! The sample rates a source may run at: its kind's (what Setup offers and
//! advice tries), narrowed to what its radio said when it was opened
//! ([`Rates`]). One account of it for Setup, the radios and advice.

/// The sample rates a radio runs at, as it said when opened.
#[derive(Clone, Debug, PartialEq)]
pub enum Rates {
    /// Only these (an Airspy's), widest first.
    List(Vec<u32>),
    /// Any from `min` to `max` (a bladeRF's).
    Range { min: f64, max: f64 },
}

impl Rates {
    pub fn allows(&self, hz: f64) -> bool {
        match self {
            Rates::List(v) => v.iter().any(|&r| (r as f64 - hz).abs() < 1.0),
            Rates::Range { min, max } => hz >= *min - 1.0 && hz <= *max + 1.0,
        }
    }

    /// `hz` if it runs at it, else the nearest it does: its first rate, or the end of its range.
    pub fn nearest(&self, hz: f64) -> f64 {
        match self {
            _ if self.allows(hz) => hz,
            Rates::List(v) => v.first().map_or(hz, |&r| r as f64),
            Rates::Range { min, max } => hz.clamp(*min, *max),
        }
    }

    /// What a found radio's JSON says it runs at (`rates`, or `rateRange`), when it said.
    pub fn from_found(found: &serde_json::Value) -> Option<Rates> {
        let hz = |v: &serde_json::Value| v.as_array().map(|a| a.iter().filter_map(|r| r.as_f64()).collect::<Vec<f64>>()).unwrap_or_default();
        match (hz(&found["rates"]), hz(&found["rateRange"])) {
            (list, _) if !list.is_empty() => Some(Rates::List(list.iter().map(|&r| r as u32).collect())),
            (_, range) if range.len() == 2 => Some(Rates::Range { min: range[0], max: range[1] }),
            _ => None,
        }
    }
}

impl std::fmt::Display for Rates {
    /// "10 / 2.5" or "0.52–61.44" (MSPS).
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let msps = |hz: f64| (hz / 1e4).round() / 100.0;
        match self {
            Rates::List(v) => write!(f, "{}", v.iter().map(|&r| format!("{}", msps(r as f64))).collect::<Vec<_>>().join(" / ")),
            Rates::Range { min, max } => write!(f, "{}–{}", msps(*min), msps(*max)),
        }
    }
}

const RTL: &[u32] = &[2_400_000, 2_048_000, 2_560_000, 3_200_000, 1_920_000, 1_024_000];

/// The rates offered for a source of kind `kind` (its `type`: "rtlsdr", …),
/// the usual first. Any other can be typed for a bladeRF, HackRF, SoapySDR
/// device or capture file; an RTL-SDR, USRP or Airspy runs at these.
pub fn of_kind(kind: &str) -> &'static [u32] {
    match kind {
        "rtlsdr" => RTL,
        "usrp" => &[2_400_000, 4_000_000, 5_000_000, 6_400_000, 8_000_000, 10_000_000, 12_500_000, 16_000_000, 20_000_000],
        // Airspy R2: 10 / 2.5; Mini: 6 / 3 (and 10 with newer firmware).
        "airspy" => &[10_000_000, 6_000_000, 3_000_000, 2_500_000],
        // (Up to 61.44 can be typed; past ~40 MSPS needs USB 3 and a fast CPU.)
        "bladerf" => &[2_400_000, 4_000_000, 5_000_000, 6_000_000, 8_000_000, 10_000_000, 12_500_000, 16_000_000, 20_000_000, 25_000_000, 30_720_000],
        "hackrf" => &[8_000_000, 10_000_000, 4_000_000, 2_000_000, 16_000_000, 20_000_000],
        "soapy" => &[2_000_000, 2_400_000, 2_500_000, 3_000_000, 6_000_000, 8_000_000, 10_000_000],
        "file" => &[2_400_000, 2_048_000, 2_560_000, 3_200_000, 1_920_000, 1_024_000, 8_000_000, 10_000_000],
        _ => &[],
    }
}

/// The highest rate a source of kind `kind` runs at dependably: an RTL-SDR
/// past 2.56 MSPS drops samples on many machines. Advice goes past it only
/// when that reaches more. None: any it offers.
pub fn steady_max(kind: &str) -> Option<u32> {
    (kind == "rtlsdr").then_some(2_560_000)
}

/// The rates a source of kind `kind` may run at: what its radio said
/// (`found`: its list, or its kind's within its range), else its kind's.
pub fn offered(kind: &str, found: Option<&Rates>) -> Vec<u32> {
    match found {
        Some(Rates::List(v)) => v.clone(),
        Some(r) => of_kind(kind).iter().copied().filter(|&hz| r.allows(hz as f64)).collect(),
        None => of_kind(kind).to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_a_source_may_run_at() {
        let r2 = Rates::from_found(&json!({ "rates": [10_000_000, 2_500_000] })).unwrap();
        let blade = Rates::from_found(&json!({ "rateRange": [520_834.0, 61_440_000.0] })).unwrap();
        assert_eq!(Rates::from_found(&json!({})), None);
        assert_eq!((r2.nearest(6e6), r2.nearest(2.5e6), blade.nearest(80e6)), (10e6, 2.5e6, 61.44e6));
        assert_eq!(offered("airspy", Some(&r2)), [10_000_000, 2_500_000], "the radio's own list");
        assert_eq!(offered("bladerf", Some(&Rates::Range { min: 1e6, max: 5e6 })), [2_400_000, 4_000_000, 5_000_000], "its kind's, within its range");
        assert_eq!(offered("rtlsdr", None)[0], 2_400_000, "the usual first");
        assert_eq!((r2.to_string(), blade.to_string()), ("10 / 2.5".to_string(), "0.52–61.44".to_string()));
        assert_eq!((steady_max("rtlsdr"), steady_max("airspy")), (Some(2_560_000), None));
    }
}
