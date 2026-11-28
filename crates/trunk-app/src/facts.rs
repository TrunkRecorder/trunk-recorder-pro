//! What capture rules' conditions know of an event (`call.…`), and what a
//! capture a rule made keeps of the event that set it off (`trp:trigger`).

use serde::{Deserialize, Serialize};
use trunk_recorder_plugin::CallRecord;

/// A call that ended (`call.end`: saved; `call.not_saved`: not).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CallFacts {
    pub system: String,
    pub site: String,
    pub talkgroup: u32,
    pub freq: u64,
    /// Unix s.
    pub start_time: f64,
    pub stop_time: f64,
    /// Seconds it lasted (`call.end`: of audio saved; `call.not_saved`: on the air).
    pub call_length: f64,
    /// (An analog call has no vocoder frames to be clean or not: 100.)
    pub clean_voice_pct: f64,
    pub snr: f64,
    pub signal: f64,
    pub noise: f64,
    pub freq_error: i64,
    pub encrypted: bool,
    pub decrypted: bool,
    pub emergency: bool,
    pub analog: bool,
    /// What its voice is: the call JSON's `voice_mode` ("p25", "p25_phase2", "dmr", "fm", …).
    pub voice: String,
    /// Its TDMA slot (0 also when it has none, as the call JSON's).
    pub tdma_slot: i64,
    /// How many radios talked on it, and which.
    pub units: usize,
    pub unit_ids: Vec<u32>,
    /// Why it wasn't saved ([`trunk_core::trunk::NotSaved::key`]: "silent", "short", "encrypted"); "" when it was.
    pub not_saved: String,
    /// Seconds of audio it had, and its voice frames and how many were bad (`call.not_saved`; a saved call's are in its file).
    pub audio_s: f64,
    pub frames: u64,
    pub bad_frames: u64,
    /// Where it was recorded: its recorder and the radio it was heard on (−1: none).
    pub recorder: i64,
    pub source: i64,
}

impl CallFacts {
    /// A saved call's, from its record.
    pub fn of_record(r: &CallRecord) -> CallFacts {
        CallFacts {
            system: r.short_name.clone(),
            site: r.site.clone(),
            talkgroup: r.talkgroup,
            freq: r.freq,
            start_time: r.start_time_ms as f64 / 1000.0,
            stop_time: r.stop_time_ms as f64 / 1000.0,
            call_length: r.call_length,
            clean_voice_pct: r.clean_voice_pct.unwrap_or(100.0),
            snr: r.snr.unwrap_or(0.0),
            signal: r.signal.unwrap_or(0.0),
            noise: r.noise.unwrap_or(0.0),
            freq_error: r.freq_error,
            encrypted: r.encrypted,
            decrypted: r.decrypted,
            emergency: r.emergency,
            analog: r.audio_type == "analog",
            voice: r.voice_mode.clone(),
            tdma_slot: r.tdma_slot,
            units: r.src_list.len(),
            unit_ids: r.src_list.iter().map(|s| s.src as u32).collect(),
            audio_s: r.call_length,
            recorder: r.recorder_num,
            source: r.source_num,
            ..Default::default()
        }
    }

    /// As a condition sees it: `call`.
    pub fn facts(&self) -> serde_json::Value {
        serde_json::json!({ "call": self })
    }
}
