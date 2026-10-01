//! The per-call JSON, with Trunk Recorder's field names (call_concluder.cc
//! create_call_json) so existing tooling reads it, and TR's file base name.

use std::fmt::Write;

use super::calls::{Call, CONVENTIONAL};
use crate::dsp::tones::Tone;
use super::frames::FrameErrors;
use super::units::UnitAliases;

pub struct ConcludeInfo<'a> {
    pub short_name: &'a str,
    /// Wall-clock epoch ms at sample-clock time 0.
    pub epoch_ms_at_zero: f64,
    pub audio_seconds: f64,
    pub errors: &'a FrameErrors,
    pub recorder_num: u32,
    pub end_s: f64,
    /// The system's talker aliases (each source's `tag_ota`).
    pub units: Option<&'a UnitAliases>,
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

/// (json, base name `<talkgroup>-<start epoch>_<freq>[.slot]`).
pub fn call_record(call: &Call, info: &ConcludeInfo) -> (String, String) {
    let ms = |s: f64| (info.epoch_ms_at_zero + s * 1000.0).round() as i64;
    let (start_ms, stop_ms) = (ms(call.start_s), ms(info.end_s));
    let tg = call.talkgroup_info.as_ref();
    let t = |f: fn(&super::talkgroups::Talkgroup) -> &str| tg.map_or(String::new(), |t| esc(f(t)));
    let secs = info.audio_seconds;
    let mut j = String::new();
    let _ = write!(
        j,
        "{{\"call_num\":{},\"freq\":{},\"freq_error\":0,\"signal\":0,\"noise\":0,\"source_num\":0,\"recorder_num\":{},\"tdma_slot\":{},\"phase2_tdma\":{},\
\"start_time\":{},\"stop_time\":{},\"start_time_ms\":{},\"stop_time_ms\":{},\"emergency\":{},\"priority\":{},\"mode\":{},\"duplex\":{},\"encrypted\":{},\
\"call_length\":{},\"call_length_ms\":{},\"talkgroup\":{},\"talkgroup_tag\":\"{}\",\"talkgroup_description\":\"{}\",\"talkgroup_group_tag\":\"{}\",\
\"talkgroup_group\":\"{}\",\"color_code\":{},\"tone_mode\":\"{}\",\"tone_detected\":\"{}\",\"tone_confidence\":{:.3},\
\"audio_type\":\"{}\",\"short_name\":\"{}\",",
        call.id,
        call.freq_hz,
        info.recorder_num,
        call.tdma_slot,
        call.phase2_tdma as u8,
        start_ms.div_euclid(1000),
        stop_ms.div_euclid(1000),
        start_ms,
        stop_ms,
        call.emergency as u8,
        call.priority,
        call.mode as u8,
        call.duplex as u8,
        call.encrypted as u8,
        secs.round() as i64,
        (secs * 1000.0).round() as i64,
        call.talkgroup,
        t(|t| &t.alpha_tag),
        t(|t| &t.description),
        t(|t| &t.tag),
        t(|t| &t.group),
        call.color_code.map_or(-1, i32::from),
        // Trunk Recorder's (PR #1137) tone fields; "search": identified, not matched.
        match call.tone_set {
            Some(Tone::Ctcss(_)) => "ctcss",
            Some(Tone::Dcs(..)) => "dcs",
            None if call.analog && call.system == CONVENTIONAL => "search",
            None => "off",
        },
        call.tone.map_or(String::new(), |t| t.tone.to_string()),
        call.tone.map_or(0.0, |t| t.confidence),
        if call.analog { "analog" } else if call.phase2_tdma { "digital tdma" } else { "digital" },
        esc(info.short_name),
    );
    // Only when patched with another, as Trunk Recorder writes it.
    if call.patched_talkgroups.len() > 1 {
        let tgs: Vec<String> = call.patched_talkgroups.iter().map(|t| t.to_string()).collect();
        let _ = write!(j, "\"patched_talkgroups\":[{}],", tgs.join(","));
    }
    let _ = write!(
        j,
        "\"freqList\":[{{\"freq\":{},\"time\":{},\"pos\":0,\"len\":{},\"error_count\":{},\"spike_count\":0}}],{},\"srcList\":[",
        call.freq_hz,
        start_ms.div_euclid(1000),
        (secs * 100.0).round() / 100.0,
        info.errors.total_errors(),
        info.errors.json()
    );
    for (i, s) in call.sources.iter().enumerate() {
        let _ = write!(
            j,
            "{}{{\"src\":{},\"time\":{},\"pos\":{},\"emergency\":{},\"signal_system\":\"\",\"tag\":\"\",\"tag_ota\":\"{}\"}}",
            if i > 0 { "," } else { "" },
            s.src,
            ms(s.time_s).div_euclid(1000),
            ((s.time_s - call.start_s) * 100.0).round().max(0.0) / 100.0,
            s.emergency as u8,
            esc(info.units.and_then(|u| u.get(s.src)).unwrap_or(""))
        );
    }
    j.push_str("]}");
    let base = format!(
        "{}-{}_{}{}",
        call.talkgroup,
        start_ms.div_euclid(1000),
        call.freq_hz,
        if call.phase2_tdma || call.color_code.is_some() { format!(".{}", call.tdma_slot) } else { String::new() }
    );
    (j, base)
}
