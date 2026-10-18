//! The config's rules, for the interface (web/src/rules.ts): the recorder's
//! own ([`trunk_app::config`]), so Setup says what Start will say, places
//! sources where they will go, and reads a config as the recorder reads it.
//! Configs come and go as JSON. Used by the page in both builds (the desktop
//! app's too), not by the engine worker.

use trunk_app::config::{auto_center, usable_half_width};
use trunk_app::Config;
use wasm_bindgen::prelude::*;

fn read(json: &str) -> Result<Config, String> {
    Config::from_json(json).map_err(|e| format!("Bad config: {e}"))
}

/// A config as the recorder reads it: every setting it lacks at its
/// default, an older one upgraded (`trunk_app::migrate`). JSON
/// `{ config, notes }` (what the upgrade changed), or `{ error }`.
#[wasm_bindgen(js_name = configNormalize)]
pub fn config_normalize(json: &str) -> String {
    match read(json) {
        Ok(c) => serde_json::json!({ "notes": c.notes.clone(), "config": c }).to_string(),
        Err(e) => serde_json::json!({ "error": e }).to_string(),
    }
}

/// The talkgroup files an older stored config (JSON) held, `{shortName:
/// csv}`: moved out before it is read (the upgrade: [`trunk_app::migrate`]).
#[wasm_bindgen(js_name = configTalkgroups)]
pub fn config_talkgroups(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json).map_or_else(|_| "{}".into(), |v| serde_json::json!(trunk_app::migrate::talkgroups_of(&v)).to_string())
}

/// Why Start would refuse this config, or none.
#[wasm_bindgen(js_name = configProblem)]
pub fn config_problem(json: &str) -> Option<String> {
    read(json).map_or_else(Some, |c| c.problem())
}

/// Why its short names don't work (two systems share one, one is empty), or none.
#[wasm_bindgen(js_name = configNameProblem)]
pub fn config_name_problem(json: &str) -> Option<String> {
    read(json).map_or_else(Some, |c| c.name_problem())
}

/// Each source's centre, Hz: as set, or where Start places it (0: nowhere it fits).
#[wasm_bindgen(js_name = configCenters)]
pub fn config_centers(json: &str) -> Vec<f64> {
    read(json).map_or_else(|_| Vec::new(), |c| c.resolved_centers())
}

/// A centre that puts every one of `hz` inside one source of `rate_hz` (off the DC spike), or none.
#[wasm_bindgen(js_name = autoCenter)]
pub fn auto_center_js(hz: Vec<f64>, rate_hz: f64, guard_hz: f64) -> Option<f64> {
    auto_center(&hz, rate_hz, guard_hz)
}

/// How far from a source's centre a channel may be, Hz.
#[wasm_bindgen(js_name = usableHalfWidth)]
pub fn usable_half_width_js(rate_hz: f64, guard_hz: f64) -> f64 {
    usable_half_width(rate_hz, guard_hz)
}

/// A conventional channel CSV read: JSON `{ channels, notes }`, or `{ error }`.
#[wasm_bindgen(js_name = channelsParse)]
pub fn channels_parse(text: &str) -> String {
    match trunk_app::channels::parse(text) {
        Ok(p) => serde_json::json!({ "channels": p.channels, "notes": p.notes }).to_string(),
        Err(e) => serde_json::json!({ "error": e }).to_string(),
    }
}

/// Conventional channels (JSON, a list) as the CSV [`channels_parse`] reads back.
#[wasm_bindgen(js_name = channelsWrite)]
pub fn channels_write(json: &str) -> String {
    serde_json::from_str::<Vec<trunk_app::config::Channel>>(json).map_or_else(|_| String::new(), |c| trunk_app::channels::write(&c))
}

/// Why a SmartNet band plan can't be used, or none.
#[wasm_bindgen(js_name = bandplanProblem)]
pub fn bandplan_problem(name: &str, base_hz: f64, spacing_hz: f64, offset: u16, high_hz: f64) -> Option<String> {
    trunk_core::smartnet::Bandplan::from_config(name, base_hz, spacing_hz, offset, high_hz).err()
}
