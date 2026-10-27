//! A call as the store keeps it: the columns of its row and its two child
//! lists, made from a call record on the way in, and the call entry the
//! protocol carries made from a stored row on the way out.

use serde_json::{Value, json};
use trunk_recorder_plugin::CallRecord;

/// One recorded call, as the store keeps it.
pub struct CallRow {
    /// The call's files' path under the capture folder, without an extension,
    /// always with `/` separators.
    pub path: String,
    pub short_name: String,
    pub nac: Option<String>,
    pub site: Option<String>,
    pub talkgroup: i64,
    pub start_ms: i64,
    pub stop_ms: i64,
    pub length_ms: i64,
    pub freq_hz: i64,
    pub emergency: bool,
    pub encrypted: bool,
    pub decrypted: bool,
    pub duplex: bool,
    pub priority: Option<i64>,
    pub audio_type: Option<String>,
    pub signal: Option<f64>,
    pub noise: Option<f64>,
    pub snr: Option<f64>,
    pub clean_voice_pct: Option<f64>,
    /// The call's audio file exists.
    pub audio: bool,
    /// The call's `.json` file exists.
    pub json: bool,
    /// The directory recorder that wrote the call, when known.
    pub recorder: Option<String>,
    /// The call record as its `.json` file has it.
    pub record: String,
    pub indexed_ms: i64,
    pub transmissions: Vec<TxRow>,
    pub frequencies: Vec<FreqRow>,
}

/// One radio's transmission within a call (an entry of its `srcList`).
pub struct TxRow {
    pub unit: i64,
    pub start_ms: i64,
    pub length_ms: i64,
    pub emergency: bool,
    pub signal_system: Option<String>,
    pub tag: Option<String>,
}

/// One stretch of a call on a frequency (an entry of its `freqList`).
pub struct FreqRow {
    pub freq_hz: i64,
    pub start_ms: i64,
    pub length_ms: i64,
    pub error_count: i64,
    pub spike_count: i64,
}

fn seconds_to_ms(s: f64) -> i64 {
    (s * 1000.0).round() as i64
}

fn some(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// The row for `record`, filed under `rel`. `record_json` is the record as
/// its `.json` file has it; `audio` and `json` say which files exist.
pub fn call_row(
    rel: &str,
    record: &CallRecord,
    record_json: &str,
    audio: bool,
    json: bool,
    recorder: Option<&str>,
    now_ms: i64,
) -> CallRow {
    let start = record.start_time_ms;
    let transmissions = record
        .src_list
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let begin = seconds_to_ms(s.pos);
            let end = record
                .src_list
                .get(i + 1)
                .map_or(record.call_length_ms, |n| seconds_to_ms(n.pos));
            TxRow {
                unit: s.src,
                start_ms: start + begin,
                length_ms: (end - begin).max(0),
                emergency: s.emergency,
                signal_system: some(&s.signal_system),
                tag: some(&s.tag),
            }
        })
        .collect();
    let frequencies = record
        .freq_list
        .iter()
        .map(|f| FreqRow {
            freq_hz: f.freq as i64,
            start_ms: start + seconds_to_ms(f.pos),
            length_ms: seconds_to_ms(f.len).max(0),
            error_count: f.error_count as i64,
            spike_count: f.spike_count as i64,
        })
        .collect();
    CallRow {
        path: rel.replace('\\', "/"),
        short_name: record.short_name.clone(),
        nac: some(&record.nac),
        site: some(&record.site),
        talkgroup: i64::from(record.talkgroup),
        start_ms: start,
        stop_ms: record.stop_time_ms,
        length_ms: record.call_length_ms.max(0),
        freq_hz: record.freq as i64,
        emergency: record.emergency,
        encrypted: record.encrypted,
        decrypted: record.decrypted,
        duplex: record.duplex,
        priority: (record.priority > 0).then_some(record.priority),
        audio_type: some(&record.audio_type),
        signal: record.signal,
        noise: record.noise,
        snr: record.snr,
        clean_voice_pct: record.clean_voice_pct,
        audio,
        json,
        recorder: recorder.map(str::to_string),
        record: record_json.to_string(),
        indexed_ms: now_ms,
        transmissions,
        frequencies,
    }
}

/// The call entry the protocol carries (`trunk_app::session::call_entry`)
/// for a stored call, with its two file flags.
pub fn entry(path: &str, record: &str, audio: bool, json: bool) -> Value {
    let record = serde_json::from_str(record).unwrap_or_else(|e| {
        log::warn!("db: stored record for {path} is not JSON: {e}");
        Value::Null
    });
    json!({ "path": path, "record": record, "audio": audio, "json": json })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use trunk_recorder_plugin::{FreqEntry, SrcEntry};

    pub(crate) fn record(tg: u32, start_ms: i64) -> CallRecord {
        CallRecord {
            short_name: "ohmarcs".into(),
            talkgroup: tg,
            start_time_ms: start_ms,
            start_time: start_ms / 1000,
            stop_time_ms: start_ms + 4200,
            call_length_ms: 4200,
            call_length: 4.2,
            freq: 851_350_000,
            nac: "35C".into(),
            site: "ohmarcs-1-13".into(),
            src_list: vec![
                SrcEntry {
                    src: 9001644,
                    time: start_ms / 1000,
                    pos: 0.0,
                    emergency: false,
                    ..Default::default()
                },
                SrcEntry {
                    src: 8344139,
                    time: start_ms / 1000 + 2,
                    pos: 2.5,
                    emergency: true,
                    ..Default::default()
                },
            ],
            freq_list: vec![FreqEntry {
                freq: 851_350_000,
                time: start_ms / 1000,
                pos: 0.0,
                len: 4.2,
                error_count: 3,
                spike_count: 1,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_row_carries_the_record_and_its_lists() {
        let r = record(9179, 1_700_000_000_000);
        let json = r.to_json();
        let row = call_row(
            "ohmarcs/2026/10/26/9179-1700000000_851350000",
            &r,
            &json,
            true,
            true,
            Some("abc"),
            5,
        );
        assert_eq!(row.talkgroup, 9179);
        assert_eq!(
            (row.start_ms, row.stop_ms, row.length_ms),
            (1_700_000_000_000, 1_700_000_004_200, 4200)
        );
        assert_eq!(row.nac.as_deref(), Some("35C"));
        assert_eq!(row.site.as_deref(), Some("ohmarcs-1-13"));
        assert_eq!(row.record, json);
        assert_eq!(row.recorder.as_deref(), Some("abc"));
        let tx: Vec<(i64, i64, i64, bool)> = row
            .transmissions
            .iter()
            .map(|t| (t.unit, t.start_ms, t.length_ms, t.emergency))
            .collect();
        assert_eq!(
            tx,
            [
                (9001644, 1_700_000_000_000, 2500, false),
                (8344139, 1_700_000_002_500, 1700, true)
            ]
        );
        assert_eq!(
            (
                row.frequencies[0].freq_hz,
                row.frequencies[0].length_ms,
                row.frequencies[0].error_count
            ),
            (851_350_000, 4200, 3)
        );
    }

    #[test]
    fn paths_are_stored_with_forward_slashes() {
        let r = record(1, 1000);
        let row = call_row(
            "ohmarcs\\2026\\10\\26\\1-1_851350000",
            &r,
            &r.to_json(),
            true,
            true,
            None,
            0,
        );
        assert_eq!(row.path, "ohmarcs/2026/10/26/1-1_851350000");
    }

    #[test]
    fn an_entry_is_the_call_entry_the_protocol_carries() {
        let r = record(9179, 1_700_000_000_000);
        let json = r.to_json();
        let e = entry("ohmarcs/x", &json, true, false);
        let mut want = trunk_app::session::call_entry("ohmarcs/x", &r);
        want["audio"] = json!(true);
        want["json"] = json!(false);
        assert_eq!(e, want);
    }
}
