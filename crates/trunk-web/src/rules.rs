//! The config's rules, for the interface (web/src/rules.ts): the recorder's
//! own ([`trunk_app::config`]), so Setup says what Start will say, places
//! sources where they will go, and reads a config as the recorder reads it.
//! Configs come and go as JSON. Used by the page in both builds (the desktop
//! app's too), not by the engine worker.

use trunk_app::config::usable_half_width;
use trunk_app::place::{place_sources, Placing, Targets};
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

/// The unit names files an older stored config (JSON) held, `{shortName:
/// csv}`: moved out before it is read ([`trunk_app::migrate::unit_names_of`]).
#[wasm_bindgen(js_name = configUnitNames)]
pub fn config_unit_names(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json).map_or_else(|_| "{}".into(), |v| serde_json::json!(trunk_app::migrate::unit_names_of(&v)).to_string())
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

/// Talkgroups (JSON `[Talkgroup]`) with talkgroup `number` edited as
/// `edit_json` (a `TalkgroupEdit`) says, by `origin_json` (an `Origin`):
/// the same edit the recorder makes for `setTalkgroup`. JSON `[Talkgroup]`.
#[wasm_bindgen(js_name = talkgroupsEdit)]
pub fn talkgroups_edit(json: &str, number: u32, edit_json: &str, origin_json: &str) -> String {
    let mut rows: Vec<trunk_core::trunk::TalkgroupRow> = serde_json::from_str(json).unwrap_or_default();
    let edit: trunk_app::protocol::TalkgroupEdit = serde_json::from_str(edit_json).unwrap_or_default();
    trunk_core::trunk::edit_rows(&mut rows, number, &edit.cells(), &origin(origin_json));
    serde_json::to_string(&rows).unwrap_or_default()
}

fn origin(json: &str) -> trunk_core::trunk::Origin {
    serde_json::from_str(json).unwrap_or_default()
}

/// Rows (JSON) `new_json` in place of `old_json`, as the recorder takes a
/// `setTalkgroups` or `setUnitNames` (`from_json`: its `from`, `at` set;
/// "": none): [`trunk_core::trunk::replace_rows`].
fn replace<R: trunk_core::trunk::NameRow + serde::Serialize + serde::de::DeserializeOwned>(old_json: &str, new_json: &str, from_json: &str) -> String {
    let old: Vec<R> = serde_json::from_str(old_json).unwrap_or_default();
    let new: Vec<R> = serde_json::from_str(new_json).unwrap_or_default();
    let from: Option<trunk_core::trunk::Origin> = serde_json::from_str(from_json).ok();
    serde_json::to_string(&trunk_core::trunk::replace_rows(&old, new, from.as_ref())).unwrap_or_default()
}

/// Talkgroups (JSON) in place of others ([`replace`]).
#[wasm_bindgen(js_name = talkgroupsReplace)]
pub fn talkgroups_replace(old_json: &str, new_json: &str, from_json: &str) -> String {
    replace::<trunk_core::trunk::TalkgroupRow>(old_json, new_json, from_json)
}

/// Radio names (JSON) in place of others ([`replace`]).
#[wasm_bindgen(js_name = unitNamesReplace)]
pub fn unit_names_replace(old_json: &str, new_json: &str, from_json: &str) -> String {
    replace::<trunk_core::trunk::UnitNameRow>(old_json, new_json, from_json)
}

/// A unit names file (Trunk Recorder's unitTagsFile) read as the recorder
/// reads one: JSON `{ unitNames: [UnitName], bad }` (trunk_core::trunk::read_unit_names).
#[wasm_bindgen(js_name = unitNamesFromCsv)]
pub fn unit_names_from_csv(text: &str) -> String {
    serde_json::to_string(&trunk_core::trunk::read_unit_names(text)).unwrap_or_default()
}

/// Radio names (JSON `[UnitName]`) as a unit names file.
#[wasm_bindgen(js_name = unitNamesToCsv)]
pub fn unit_names_to_csv(json: &str) -> String {
    trunk_core::trunk::unit_names_csv(&serde_json::from_str::<Vec<trunk_core::trunk::UnitNameRow>>(json).unwrap_or_default())
}

/// Radio names (JSON `[UnitName]`) with radio `unit` named `name` by
/// `origin_json`: the same edit the recorder makes for `setUnitName`.
#[wasm_bindgen(js_name = unitNamesEdit)]
pub fn unit_names_edit(json: &str, unit: u32, name: &str, origin_json: &str) -> String {
    let mut rows: Vec<trunk_core::trunk::UnitNameRow> = serde_json::from_str(json).unwrap_or_default();
    trunk_core::trunk::edit_unit_names(&mut rows, unit, name, &origin(origin_json));
    serde_json::to_string(&rows).unwrap_or_default()
}

/// A system's radio names as the recorder looks them up: by its rows, in
/// its unit names mode (`"user"`, `"ota"`, `"user_only"`, `"none"`).
#[wasm_bindgen]
pub struct UnitNamer(trunk_core::trunk::UnitTags);

#[wasm_bindgen]
impl UnitNamer {
    #[wasm_bindgen(constructor)]
    pub fn new(unit_names_json: &str, mode: &str) -> UnitNamer {
        let rows: Vec<trunk_core::trunk::UnitNameRow> = serde_json::from_str(unit_names_json).unwrap_or_default();
        UnitNamer(trunk_core::trunk::UnitTags::new(&rows, trunk_core::trunk::UnitTagsMode::from_name(mode)))
    }
    /// Radio `unit`'s name, `heard` the talker alias heard for it, as its calls' talkers are named.
    pub fn name(&self, unit: u32, heard: Option<String>) -> Option<String> {
        trunk_core::trunk::UnitTags::name_heard(Some(&self.0), heard.as_deref(), unit)
    }
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

/// Whether config `json`, edited since `run_json` (the one recording started
/// with), waits for the next Start ([`Config::needs_restart`]): the browser
/// build's worker tells the page so, as the desktop recorder's server does.
#[wasm_bindgen(js_name = configNeedsRestart)]
pub fn config_needs_restart(json: &str, run_json: &str) -> bool {
    matches!((read(json), read(run_json)), (Ok(c), Ok(run)) if c.needs_restart(&run))
}

/// Each source's centre, Hz: as set, or where Start places it (0: nowhere it fits).
#[wasm_bindgen(js_name = configCenters)]
pub fn config_centers(json: &str) -> Vec<f64> {
    read(json).map_or_else(|_| Vec::new(), |c| c.resolved_centers())
}

/// Each source's label and title: JSON `[{ label, title }]`. The label
/// ([`trunk_app::config::Source::label`], "RTL-SDR SN 200") is the radio: what
/// a system's or channel's `receiveOn` names it by, and its status by. The
/// title ([`trunk_app::config::Source::title`], "Source 2 (Whip)") is what it's called.
#[wasm_bindgen(js_name = configSources)]
pub fn config_sources(json: &str) -> String {
    let named = |c: Config| c.sources.iter().enumerate().map(|(i, s)| serde_json::json!({ "label": s.label(), "title": s.title(i) })).collect::<Vec<_>>();
    read(json).map_or_else(|_| "[]".into(), |c| serde_json::json!(named(c)).to_string())
}

/// The source each frequency the config lists is received on, as the engine
/// will choose it ([`Config::receivers`]): JSON `[{ system, hz, source }]`
/// (`source` null: none it may be received on reaches it).
#[wasm_bindgen(js_name = configReceivers)]
pub fn config_receivers(json: &str) -> String {
    read(json).map_or_else(|_| "[]".into(), |c| serde_json::json!(c.receivers()).to_string())
}

/// Where to tune each source ([`trunk_app::place::place_sources`], as Start
/// places auto sources). JSON in `{ sources: [{ rateHz, guardHz?, fixedHz? }],
/// targets: { groups: [{ channels: [{ hz, weight }], need?: [hz], pins? }] } }`; out a centre
/// (Hz) or null per source.
#[wasm_bindgen(js_name = placeSources)]
pub fn place_sources_js(json: &str) -> String {
    #[derive(serde::Deserialize)]
    struct Ask {
        sources: Vec<Placing>,
        targets: Targets,
    }
    let Ok(a) = serde_json::from_str::<Ask>(json) else { return "[]".into() };
    serde_json::json!(place_sources(&a.sources, &a.targets)).to_string()
}

/// Better centres and rates for the sources than they have ([`trunk_app::config::Config::advice`]):
/// `found` a JSON array, each source's found radio (`{ rates?, rateRange? }`, or null); `heard`
/// JSON `{ <shortName>: [{ hz, weight }] }`, what each system was heard on (its calls). Out the
/// advice's JSON, or `null` when there's none.
#[wasm_bindgen(js_name = configAdvice)]
pub fn config_advice(json: &str, found: &str, heard: &str) -> String {
    let found: Vec<Option<trunk_app::rates::Rates>> = serde_json::from_str::<Vec<serde_json::Value>>(found).unwrap_or_default().iter().map(trunk_app::rates::Rates::from_found).collect();
    let heard: trunk_app::config::Heard = serde_json::from_str(heard).unwrap_or_default();
    read(json).ok().and_then(|c| c.advice(&found, &heard)).map_or_else(|| "null".into(), |a| serde_json::json!(a).to_string())
}

/// What a found radio's JSON (`{ rates?, rateRange? }`; "" or `null`: not known) says it runs at.
fn found_rates(found: &str) -> Option<trunk_app::rates::Rates> {
    serde_json::from_str::<serde_json::Value>(found).ok().and_then(|v| trunk_app::rates::Rates::from_found(&v))
}

/// The rates a source of kind `kind` may run at ([`trunk_app::rates::offered`]): its radio's
/// (`found`, its JSON), else its kind's, the usual first.
#[wasm_bindgen(js_name = sourceRates)]
pub fn source_rates(kind: &str, found: &str) -> Vec<u32> {
    trunk_app::rates::offered(kind, found_rates(found).as_ref())
}

/// The inputs a source of kind `kind` picks from, by number (a HydraSDR's); none for the others.
#[wasm_bindgen(js_name = sourceInputs)]
pub fn source_inputs(kind: &str) -> Vec<String> {
    if kind == "hydrasdr" { trunk_app::config::HYDRASDR_INPUTS.map(String::from).to_vec() } else { vec![] }
}

/// Whether the radio (`found`, its JSON) runs at `hz`: true when it didn't say.
#[wasm_bindgen(js_name = rateOk)]
pub fn rate_ok(hz: f64, found: &str) -> bool {
    found_rates(found).is_none_or(|r| r.allows(hz))
}

/// `hz` if the radio (`found`) runs at it or didn't say, else the nearest it does ([`trunk_app::rates::Rates::nearest`]).
#[wasm_bindgen(js_name = rateFor)]
pub fn rate_for(hz: f64, found: &str) -> f64 {
    found_rates(found).map_or(hz, |r| r.nearest(hz))
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
