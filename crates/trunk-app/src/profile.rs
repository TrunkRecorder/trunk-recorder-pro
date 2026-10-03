//! The `sourceProfile` message: how a source's band rolls off at its edges
//! ([`trunk_core::dsp::rolloff`]), for choosing its guard band in Setup.

use serde_json::{json, Value};
use trunk_core::dsp::rolloff::Rolloff;

/// Seconds of the source looked at.
pub const SECONDS: f64 = 1.5;

/// What `profileSource` asks for: source `source`, tuned to `center_hz`
/// (a capture keeps its own).
pub struct Request {
    pub source: usize,
    pub center_hz: f64,
}

impl Request {
    pub fn from_json(v: &Value) -> Self {
        Request { source: v["source"].as_u64().unwrap_or(0) as usize, center_hz: v["centerHz"].as_f64().unwrap_or(0.0) }
    }
}

/// The answer. dB values to 0.1 dB; the waterfall's rows to whole dB.
pub fn json(source: usize, center_hz: f64, r: &Rolloff) -> Value {
    let tenth = |v: &[f32]| v.iter().map(|d| (d * 10.0).round() / 10.0).collect::<Vec<f32>>();
    let rows: Vec<Vec<i16>> = r.rows.iter().map(|row| row.iter().map(|d| d.round() as i16).collect()).collect();
    json!({
        "type": "sourceProfile",
        "source": source,
        "centerHz": center_hz,
        "rateHz": r.rate_hz,
        "spectrum": tenth(&r.spectrum_db),
        "floor": tenth(&r.floor_db),
        "rows": rows,
        "referenceDb": (r.reference_db * 10.0).round() / 10.0,
        "lowHz": r.low_hz.round(),
        "highHz": r.high_hz.round(),
        "lowDropDb": (r.low_drop_db * 10.0).round() / 10.0,
        "highDropDb": (r.high_drop_db * 10.0).round() / 10.0,
        "suggestedGuardHz": r.suggested_guard_hz,
    })
}

/// It couldn't be done.
pub fn error_json(source: usize, error: &str) -> Value {
    json!({ "type": "sourceProfile", "source": source, "error": error })
}
