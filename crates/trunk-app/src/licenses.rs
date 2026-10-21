//! FCC license lookups: who is licensed on some frequencies near a location,
//! from the trunkrecorder.pro API (`POST /api/v1/fcc/identify`). For the
//! survey's signals, or a system already set up. Only when the user asks,
//! with the location they gave; the platform makes the request.

use serde::Deserialize;
use serde_json::{json, Value};

/// At most this many frequencies in one lookup (the API's limit).
pub const MAX_SIGNALS: usize = 64;
/// The farthest a lookup reaches (the API's limit), m.
pub const MAX_RADIUS_M: f64 = 150_000.0;

/// A frequency to look up, and how wide the signal on it is (Hz).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub freq_hz: f64,
    #[serde(default)]
    pub width_hz: Option<f64>,
}

/// The API's request body: the frequencies on the 6.25 kHz raster, each
/// once, at most [`MAX_SIGNALS`]; an error (for the user) when there is
/// nothing to ask or the location isn't one.
pub fn request(lat: f64, lon: f64, radius_m: f64, signals: &[Signal]) -> Result<Value, String> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err("That location isn't a latitude and longitude.".into());
    }
    let mut out: Vec<(f64, f64)> = Vec::new();
    for s in signals {
        let f = (s.freq_hz / 6250.0).round() * 6250.0;
        if f > 0.0 && out.len() < MAX_SIGNALS && !out.iter().any(|&(g, _)| (g - f).abs() < 1.0) {
            out.push((f, s.width_hz.unwrap_or(12_500.0).clamp(6_250.0, 25_000.0)));
        }
    }
    if out.is_empty() {
        return Err("No frequencies to look up.".into());
    }
    let signals: Vec<Value> = out.iter().map(|(f, w)| json!({ "freqHz": f, "widthHz": w })).collect();
    Ok(json!({ "lat": lat, "lon": lon, "radiusM": radius_m.clamp(1_000.0, MAX_RADIUS_M), "signals": signals }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_on_the_raster_each_once_and_bounded() {
        let s = |f: f64| Signal { freq_hz: f, width_hz: None };
        let r = request(38.9, -77.03, 500_000.0, &[s(857_987_000.0), s(857_987_600.0), s(489_087_500.0)]).unwrap();
        assert_eq!(r["radiusM"], 150_000.0);
        assert_eq!(r["signals"], json!([{ "freqHz": 857_987_500.0, "widthHz": 12_500.0 }, { "freqHz": 489_087_500.0, "widthHz": 12_500.0 }]));
        let many: Vec<Signal> = (0..100).map(|i| s(851e6 + i as f64 * 12_500.0)).collect();
        assert_eq!(request(38.9, -77.03, 50_000.0, &many).unwrap()["signals"].as_array().unwrap().len(), MAX_SIGNALS);
        assert!(request(95.0, 0.0, 50_000.0, &[s(851e6)]).is_err());
        assert!(request(38.9, -77.03, 50_000.0, &[]).unwrap_err().contains("No frequencies"));
    }
}
