//! A call as the store mirrors it, once: its record's fields as typed
//! columns of `calls`, its radios' transmissions (`srcList`, the one list
//! queried) as rows of `call_transmissions`, and the lists nothing queries
//! as one JSON document (`calls.detail`). Made from a call record on the
//! way in; the record made again on the way out (the conformance test
//! checks it's the record that went in). The call's `.json` file stays the
//! truth: this is its mirror.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use trunk_recorder_plugin::{CallRecord, ErrorInterval, FreqEntry, SrcEntry, StoredCall, ToneEntry, TonePattern};

use super::sql::{Row, V};

/// One recorded call, as the store keeps it.
pub struct CallRow {
    /// The call's files' path under the capture folder, without an extension,
    /// always with `/` separators.
    pub path: String,
    /// The system it's filed under, as the record names it…
    pub short_name: String,
    /// …and as the store keeps it (`systems.id`): set by `Db::send`.
    pub system_id: i64,
    pub record: CallRecord,
    /// The call's audio file exists.
    pub audio: bool,
    /// The call's `.json` file exists.
    pub json: bool,
    /// The directory recorder that wrote the call, when known.
    pub recorder: Option<String>,
    pub indexed_ms: i64,
}

/// The row for `record`, filed under `rel`; `audio` and `json` say which
/// files exist.
pub fn call_row(rel: &str, record: &CallRecord, audio: bool, json: bool, recorder: Option<&str>, now_ms: i64) -> CallRow {
    CallRow {
        path: rel.replace('\\', "/"),
        short_name: record.short_name.clone(),
        system_id: 0,
        record: record.clone(),
        audio,
        json,
        recorder: recorder.map(str::to_string),
        indexed_ms: now_ms,
    }
}

/// The record's lists nothing queries: `calls.detail`.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Detail {
    #[serde(rename = "freqList", skip_serializing_if = "Vec::is_empty")]
    freq_list: Vec<FreqEntry>,
    #[serde(rename = "errorList", skip_serializing_if = "Vec::is_empty")]
    error_list: Vec<ErrorInterval>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tones: Vec<ToneEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tone_patterns: Vec<TonePattern>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    signals: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    patched_talkgroups: Vec<u32>,
    /// Fields of the record this version doesn't know.
    #[serde(skip_serializing_if = "Map::is_empty")]
    extra: Map<String, Value>,
}

/// `calls`' columns but its id, in the order [`call_values`] gives them and
/// [`stored_call`] reads them (the schema's, checked by a test).
pub const CALL_COLS: &[&str] = &[
    "path", "system_id", "site", "talkgroup", "talkgroup_ota", "talkgroup_tag", "talkgroup_description", "talkgroup_group_tag", "talkgroup_group", "call_num", "transmission_id", "freq_hz", "freq_error", "signal",
    "noise", "snr", "clean_voice_pct", "source_num", "recorder_num", "tdma_slot", "phase2_tdma", "start_ms", "stop_ms", "length_ms", "length_s", "emergency", "priority", "mode", "duplex", "encrypted", "algid",
    "key_id", "decrypted", "clock_off", "nac", "color_code", "ran", "tone_mode", "tone_detected", "tone_confidence", "audio_type", "system_type", "voice_mode", "audio", "json", "recorder", "indexed_ms", "detail",
];

/// `row`'s values for [`CALL_COLS`].
pub fn call_values(row: &CallRow) -> Vec<V> {
    let r = &row.record;
    let detail = Detail {
        freq_list: r.freq_list.clone(),
        error_list: r.error_list.clone(),
        tones: r.tones.clone(),
        tone_patterns: r.tone_patterns.clone(),
        signals: r.signals.clone(),
        patched_talkgroups: r.patched_talkgroups.clone(),
        extra: r.extra.clone(),
    };
    vec![
        row.path.clone().into(),
        row.system_id.into(),
        r.site.clone().into(),
        r.talkgroup.into(),
        r.talkgroup_ota.into(),
        r.talkgroup_tag.clone().into(),
        r.talkgroup_description.clone().into(),
        r.talkgroup_group_tag.clone().into(),
        r.talkgroup_group.clone().into(),
        (r.call_num as i64).into(),
        r.transmission_id.clone().into(),
        (r.freq as i64).into(),
        r.freq_error.into(),
        r.signal.into(),
        r.noise.into(),
        r.snr.into(),
        r.clean_voice_pct.into(),
        r.source_num.into(),
        r.recorder_num.into(),
        r.tdma_slot.into(),
        r.phase2_tdma.into(),
        r.start_time_ms.into(),
        r.stop_time_ms.into(),
        r.call_length_ms.into(),
        r.call_length.into(),
        r.emergency.into(),
        r.priority.into(),
        r.mode.into(),
        r.duplex.into(),
        r.encrypted.into(),
        r.algid.into(),
        r.key_id.into(),
        r.decrypted.into(),
        r.clock_off.into(),
        r.nac.clone().into(),
        r.color_code.into(),
        r.ran.into(),
        r.tone_mode.clone().into(),
        r.tone_detected.clone().into(),
        r.tone_confidence.into(),
        r.audio_type.clone().into(),
        r.system_type.clone().into(),
        r.voice_mode.clone().into(),
        row.audio.into(),
        row.json.into(),
        row.recorder.clone().into(),
        row.indexed_ms.into(),
        serde_json::to_string(&detail).unwrap_or_else(|_| "{}".into()).into(),
    ]
}

/// `call_transmissions`' columns, in the order [`tx_values`] gives them and
/// [`src_entry`] reads them.
pub const TX_COLS: &[&str] = &[
    "call_id", "seq", "system_id", "call_start_ms", "unit", "first", "time", "pos", "emergency", "encrypted", "duplex", "mode", "priority", "signal_system", "tag", "tag_ota", "lat", "lon", "extra",
];

/// Transmission `seq` of call `id` (`row`'s): its values for [`TX_COLS`].
/// `first`: its radio's first in the call (what a radio's page walks).
pub fn tx_values(id: i64, seq: usize, row: &CallRow, s: &SrcEntry, first: bool) -> Vec<V> {
    vec![
        id.into(),
        seq.into(),
        row.system_id.into(),
        row.record.start_time_ms.into(),
        s.src.into(),
        first.into(),
        s.time.into(),
        s.pos.into(),
        s.emergency.into(),
        s.encrypted.into(),
        s.duplex.into(),
        s.mode.into(),
        s.priority.into(),
        s.signal_system.clone().into(),
        s.tag.clone().into(),
        s.tag_ota.clone().into(),
        s.lat.into(),
        s.lon.into(),
        (!s.extra.is_empty()).then(|| serde_json::to_string(&s.extra).unwrap_or_default()).into(),
    ]
}

/// A transmission read back (columns [`TX_COLS`] from `at`).
pub fn src_entry(r: &Row, at: usize) -> SrcEntry {
    let i = |k: usize| at + k;
    SrcEntry {
        src: r.int(i(4)),
        time: r.int(i(6)),
        pos: r.real(i(7)),
        emergency: r.bool(i(8)),
        encrypted: r.bool(i(9)),
        duplex: r.bool(i(10)),
        mode: r.bool(i(11)),
        priority: r.int(i(12)),
        signal_system: r.text(i(13)),
        tag: r.text(i(14)),
        tag_ota: r.text(i(15)),
        lat: r.opt_real(i(16)),
        lon: r.opt_real(i(17)),
        extra: r.opt_text(i(18)).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default(),
    }
}

/// A call read back: columns [`CALL_COLS`] from `at`, its system's short
/// name, and its transmissions.
pub fn stored_call(r: &Row, at: usize, short_name: String, src_list: Vec<SrcEntry>) -> StoredCall {
    let i = |k: usize| at + k;
    let detail: Detail = r.opt_text(i(47)).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let (start_ms, stop_ms) = (r.int(i(21)), r.int(i(22)));
    let record = CallRecord {
        call_num: r.int(i(9)) as u64,
        transmission_id: r.text(i(10)),
        freq: r.int(i(11)) as u64,
        freq_error: r.int(i(12)),
        signal: r.opt_real(i(13)),
        noise: r.opt_real(i(14)),
        snr: r.opt_real(i(15)),
        clean_voice_pct: r.opt_real(i(16)),
        source_num: r.int(i(17)),
        recorder_num: r.int(i(18)),
        tdma_slot: r.int(i(19)),
        phase2_tdma: r.bool(i(20)),
        start_time: start_ms.div_euclid(1000),
        stop_time: stop_ms.div_euclid(1000),
        start_time_ms: start_ms,
        stop_time_ms: stop_ms,
        call_length_ms: r.int(i(23)),
        call_length: r.real(i(24)),
        emergency: r.bool(i(25)),
        priority: r.int(i(26)),
        mode: r.bool(i(27)),
        duplex: r.bool(i(28)),
        encrypted: r.bool(i(29)),
        algid: r.int(i(30)),
        key_id: r.int(i(31)),
        decrypted: r.bool(i(32)),
        clock_off: r.bool(i(33)),
        talkgroup: r.int(i(3)) as u32,
        talkgroup_ota: r.opt_int(i(4)).map(|t| t as u32),
        talkgroup_tag: r.text(i(5)),
        talkgroup_description: r.text(i(6)),
        talkgroup_group_tag: r.text(i(7)),
        talkgroup_group: r.text(i(8)),
        nac: r.text(i(34)),
        color_code: r.int(i(35)),
        ran: r.int(i(36)),
        tone_mode: r.text(i(37)),
        tone_detected: r.text(i(38)),
        tone_confidence: r.real(i(39)),
        audio_type: r.text(i(40)),
        system_type: r.text(i(41)),
        voice_mode: r.text(i(42)),
        short_name,
        site: r.text(i(2)),
        patched_talkgroups: detail.patched_talkgroups,
        freq_list: detail.freq_list,
        error_list: detail.error_list,
        src_list,
        tones: detail.tones,
        tone_patterns: detail.tone_patterns,
        signals: detail.signals,
        extra: detail.extra,
    };
    StoredCall { path: r.text(i(0)), record, audio: r.bool(i(43)), json: r.bool(i(44)) }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn record(tg: u32, start_ms: i64) -> CallRecord {
        CallRecord {
            short_name: "ohmarcs".into(),
            talkgroup: tg,
            start_time_ms: start_ms,
            start_time: start_ms.div_euclid(1000),
            stop_time_ms: start_ms + 4200,
            stop_time: (start_ms + 4200).div_euclid(1000),
            call_length_ms: 4200,
            call_length: 4.0,
            freq: 851_350_000,
            nac: "35C".into(),
            site: "ohmarcs-1-13".into(),
            src_list: vec![
                SrcEntry { src: 9001644, time: start_ms / 1000, pos: 0.0, emergency: false, ..Default::default() },
                SrcEntry { src: 8344139, time: start_ms / 1000 + 2, pos: 2.5, emergency: true, ..Default::default() },
            ],
            freq_list: vec![FreqEntry { freq: 851_350_000, time: start_ms / 1000, pos: 0.0, len: 4.2, error_count: 3, spike_count: 1, ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn paths_are_stored_with_forward_slashes() {
        let r = record(1, 1000);
        let row = call_row("ohmarcs\\2026\\10\\26\\1-1_851350000", &r, true, true, None, 0);
        assert_eq!(row.path, "ohmarcs/2026/10/26/1-1_851350000");
    }

    /// The columns are the schema's, in its order.
    #[test]
    fn the_columns_are_the_schemas() {
        let cols = |name: &str| super::super::schema::TABLES.iter().find(|t| t.name == name).unwrap().cols.iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(cols("calls")[1..], *CALL_COLS);
        assert_eq!(cols("call_transmissions"), TX_COLS);
    }
}
