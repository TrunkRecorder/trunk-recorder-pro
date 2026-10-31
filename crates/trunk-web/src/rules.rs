//! The config's rules, for the interface (web/src/rules.ts): the recorder's
//! own ([`trunk_app::config`]), so Setup says what Start will say, places
//! sources where they will go, and reads a config as the recorder reads it.
//! Configs come and go as JSON. Used by the page in both builds (the desktop
//! app's too), not by the engine worker.

use trunk_app::config::{auto_center, plan_centers, usable_half_width, Placing};
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

/// A talkgroup file (Trunk Recorder's CSV, or one saved by a spreadsheet)
/// read as the recorder reads one: JSON `{ talkgroups: [Talkgroup], legacy,
/// skipped, unknownColumns, missing }` (trunk_core::trunk::read_rows).
#[wasm_bindgen(js_name = talkgroupsFromCsv)]
pub fn talkgroups_from_csv(text: &str) -> String {
    serde_json::to_string(&trunk_core::trunk::read_rows(text)).unwrap_or_default()
}

/// Talkgroups (JSON `[Talkgroup]`) as a talkgroup file: Trunk Recorder's
/// headed CSV, with each row's comment and columns of its own.
#[wasm_bindgen(js_name = talkgroupsToCsv)]
pub fn talkgroups_to_csv(json: &str) -> String {
    trunk_core::trunk::rows_csv(&serde_json::from_str::<Vec<trunk_core::trunk::TalkgroupRow>>(json).unwrap_or_default())
}

/// Talkgroups (JSON `[Talkgroup]`) with talkgroup `number`'s cells set
/// (JSON `[[column, value]]`, by Trunk Recorder's column names): the same
/// edit the recorder makes for `setTalkgroup`. JSON `[Talkgroup]`.
#[wasm_bindgen(js_name = talkgroupsEdit)]
pub fn talkgroups_edit(json: &str, number: u32, cells_json: &str) -> String {
    let mut rows: Vec<trunk_core::trunk::TalkgroupRow> = serde_json::from_str(json).unwrap_or_default();
    let cells: Vec<(String, String)> = serde_json::from_str(cells_json).unwrap_or_default();
    let cells: Vec<(&str, &str)> = cells.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    trunk_core::trunk::edit_rows(&mut rows, number, &cells);
    serde_json::to_string(&rows).unwrap_or_default()
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

/// Where to tune each source for a system found on the air
/// ([`trunk_app::config::plan_centers`]). JSON in `{ sources: [{ rateHz,
/// guardHz?, fixedHz? }], first, firstCenter, ccs, voice: [{ hz, weight }] }`;
/// out a centre (Hz) or null per source.
#[wasm_bindgen(js_name = planCenters)]
pub fn plan_centers_js(json: &str) -> String {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Voice {
        hz: f64,
        weight: f64,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Ask {
        sources: Vec<Placing>,
        first: usize,
        first_center: f64,
        ccs: Vec<f64>,
        voice: Vec<Voice>,
    }
    let Ok(a) = serde_json::from_str::<Ask>(json) else { return "[]".into() };
    let voice: Vec<(f64, f64)> = a.voice.iter().map(|v| (v.hz, v.weight)).collect();
    serde_json::json!(plan_centers(&a.sources, a.first, a.first_center, &a.ccs, &voice)).to_string()
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

/// What's wrong with a signal (JSON, the config's `signals` entry), or none.
#[wasm_bindgen(js_name = signalProblem)]
pub fn signal_problem(json: &str) -> Option<String> {
    serde_json::from_str::<trunk_app::config::SignalDef>(json).map_or_else(|e| Some(e.to_string()), |d| d.problem())
}

/// What the recorder counts as a signal (JSON): how near its tones have to
/// be, and what other paging codes that takes in.
#[wasm_bindgen(js_name = signalNote)]
pub fn signal_note(json: &str) -> String {
    use trunk_core::dsp::inband::{pattern::PatternKind, signal::tolerance_note};
    let Ok(d) = serde_json::from_str::<trunk_app::config::SignalDef>(json) else { return String::new() };
    let Some(kind) = PatternKind::from_name(&d.pattern) else { return String::new() };
    tolerance_note(kind, &d.tones_hz.iter().map(|&f| f as f32).collect::<Vec<_>>(), d.knox, d.extra_tolerance_pct as f32)
}

/// A signal (JSON) in words, as the recorder logs a pattern heard: "two-tone
/// 313 → 1642 Hz", "KNOX 0123", "Morse WRV713 (990 Hz)"; without `kind`,
/// less the kind ("313 → 1642 Hz"). "": not a pattern.
#[wasm_bindgen(js_name = signalText)]
pub fn signal_text(json: &str, kind: bool) -> String {
    use trunk_core::dsp::inband::pattern::{describe, describe_detail, PatternKind};
    let Ok(d) = serde_json::from_str::<trunk_app::config::SignalDef>(json) else { return String::new() };
    let Some(k) = PatternKind::from_name(&d.pattern) else { return String::new() };
    let f32s = |v: &[f64]| v.iter().map(|&x| x as f32).collect::<Vec<_>>();
    let (hz, lengths) = (f32s(&d.tones_hz), f32s(&d.lengths_s));
    if kind { describe(k, &hz, &lengths, &d.digits, d.knox, 0, None) } else { describe_detail(k, &hz, &lengths, &d.digits, 0, None) }
}

/// A signal's id made from what people call it ("Norwalk EMS page" → NORWALK_EMS_PAGE).
#[wasm_bindgen(js_name = signalId)]
pub fn signal_id(text: &str) -> String {
    trunk_app::config::SignalDef::id_from(text)
}

/// What a tone pattern sounds like (JSON `{ pattern, tones, lengths, digits,
/// knox, cycles }`, as the call JSON's `tone_patterns` and the tones log have
/// it), step by step: JSON `[{ hz: [..], s }]`.
#[wasm_bindgen(js_name = toneSteps)]
pub fn tone_steps(json: &str) -> String {
    use trunk_core::dsp::inband::pattern::{steps, PatternKind};
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Spec {
        pattern: String,
        tones: Vec<f32>,
        lengths: Vec<f32>,
        digits: String,
        knox: bool,
        cycles: u32,
    }
    let Ok(p) = serde_json::from_str::<Spec>(json) else { return "[]".into() };
    let Some(kind) = PatternKind::from_name(&p.pattern) else { return "[]".into() };
    let out: Vec<serde_json::Value> = steps(kind, &p.tones, &p.lengths, &p.digits, p.knox, p.cycles).into_iter().map(|s| serde_json::json!({ "hz": s.hz, "s": s.s })).collect();
    serde_json::Value::from(out).to_string()
}
