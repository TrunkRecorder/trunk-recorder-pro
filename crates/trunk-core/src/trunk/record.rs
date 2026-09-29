//! The per-call JSON, with Trunk Recorder's field names (call_concluder.cc
//! create_call_json) so existing tooling reads it, and TR's file base name.

use std::fmt::Write;

use super::calls::Call;

pub struct ConcludeInfo<'a> {
    pub short_name: &'a str,
    /// Wall-clock epoch ms at sample-clock time 0.
    pub epoch_ms_at_zero: f64,
    pub audio_seconds: f64,
    pub error_count: u64,
    pub recorder_num: u32,
    pub end_s: f64,
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
\"talkgroup_group\":\"{}\",\"color_code\":-1,\"audio_type\":\"digital{}\",\"short_name\":\"{}\",",
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
        if call.phase2_tdma { " tdma" } else { "" },
        esc(info.short_name),
    );
    let _ = write!(
        j,
        "\"freqList\":[{{\"freq\":{},\"time\":{},\"pos\":0,\"len\":{},\"error_count\":{},\"spike_count\":0}}],\"srcList\":[",
        call.freq_hz,
        start_ms.div_euclid(1000),
        (secs * 100.0).round() / 100.0,
        info.error_count
    );
    for (i, s) in call.sources.iter().enumerate() {
        let _ = write!(
            j,
            "{}{{\"src\":{},\"time\":{},\"pos\":{},\"emergency\":{},\"signal_system\":\"\",\"tag\":\"\",\"tag_ota\":\"\"}}",
            if i > 0 { "," } else { "" },
            s.src,
            ms(s.time_s).div_euclid(1000),
            ((s.time_s - call.start_s) * 100.0).round().max(0.0) / 100.0,
            s.emergency as u8
        );
    }
    j.push_str("]}");
    let base = format!(
        "{}-{}_{}{}",
        call.talkgroup,
        start_ms.div_euclid(1000),
        call.freq_hz,
        if call.phase2_tdma { format!(".{}", call.tdma_slot) } else { String::new() }
    );
    (j, base)
}
