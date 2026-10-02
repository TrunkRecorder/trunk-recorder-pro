//! The per-call JSON, with Trunk Recorder's field names (call_concluder.cc
//! create_call_json) so existing tooling reads it, and TR's file base name.

use std::fmt::Write;

use super::calls::{Call, CONVENTIONAL};
use crate::dsp::tones::Tone;
use super::frames::FrameErrors;
use super::units::{UnitAliases, UnitTags};

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
    /// The system's own unit names, and which come first (each source's `tag`).
    pub unit_tags: Option<&'a UnitTags>,
    /// How far off its channel the voice was, Hz (0: not measured).
    pub freq_error_hz: i32,
    /// How strong it came in.
    pub reception: Reception,
}

/// How strong a call came in: its channel's power while it carried the call,
/// and the noise floor under the channel, both in dB full scale (a
/// full-scale carrier is 0 dBFS; the same units on every source).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reception {
    sum: f64,
    n: u32,
    noise: f64,
}

impl Reception {
    /// The channel's power (|x|², head output) at a moment the call was on the air.
    pub fn signal(&mut self, power: f64) {
        if power > 0.0 && power.is_finite() {
            self.sum += power;
            self.n += 1;
        }
    }
    /// The noise power under the channel (same units).
    pub fn noise(&mut self, power: f64) {
        if power > 0.0 && power.is_finite() {
            self.noise = power;
        }
    }
    pub fn signal_db(&self) -> Option<f64> {
        (self.n > 0).then(|| 10.0 * (self.sum / self.n as f64).log10())
    }
    pub fn noise_db(&self) -> Option<f64> {
        (self.noise > 0.0).then(|| 10.0 * self.noise.log10())
    }
    /// Signal over noise, dB.
    pub fn snr_db(&self) -> Option<f64> {
        Some(self.signal_db()? - self.noise_db()?)
    }
}

/// A transmission's audio ends when none comes for this long: a unit
/// unkeying and the next keying up (and its header) take longer than the
/// gaps between voice frames inside one (P25 delivers 180 ms at a time).
const TRANSMISSION_GAP_S: f64 = 0.5;

/// Where in a call's audio each transmission starts (sample index).
#[derive(Clone, Debug, Default)]
pub struct Transmissions {
    pub starts: Vec<usize>,
    last_s: Option<f64>,
}

impl Transmissions {
    /// Audio is about to be appended at `at` (the call's sample count), at time `t_s`.
    pub fn note(&mut self, at: usize, t_s: f64) {
        if self.last_s.is_none_or(|l| t_s - l > TRANSMISSION_GAP_S) && self.starts.last() != Some(&at) {
            self.starts.push(at);
        }
        self.last_s = Some(t_s);
    }

    /// `audio` without the transmissions shorter than `min_s` (Trunk
    /// Recorder's minTransmissionDuration: key-ups, data bursts).
    pub fn drop_short(&self, audio: &mut Vec<f32>, min_s: f64, rate: u32) {
        if min_s <= 0.0 || self.starts.is_empty() {
            return;
        }
        let min = (min_s * rate as f64) as usize;
        let mut kept = Vec::with_capacity(audio.len());
        // (Audio before the first mark is the first transmission's too.)
        let mut bounds: Vec<usize> = self.starts.iter().copied().filter(|&s| s > 0 && s < audio.len()).collect();
        bounds.insert(0, 0);
        bounds.push(audio.len());
        for w in bounds.windows(2) {
            if w[1] - w[0] >= min {
                kept.extend_from_slice(&audio[w[0]..w[1]]);
            }
        }
        *audio = kept;
    }
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

/// A level for the JSON, to a tenth of a dB; null when not measured.
fn db(v: Option<f64>) -> String {
    v.map_or("null".to_string(), |v| format!("{:.1}", v))
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
        "{{\"call_num\":{},\"freq\":{},\"freq_error\":{},\"signal\":{},\"noise\":{},\"snr\":{},\"clean_voice_pct\":{},\"source_num\":0,\"recorder_num\":{},\"tdma_slot\":{},\"phase2_tdma\":{},\
\"start_time\":{},\"stop_time\":{},\"start_time_ms\":{},\"stop_time_ms\":{},\"emergency\":{},\"priority\":{},\"mode\":{},\"duplex\":{},\"encrypted\":{},\
\"call_length\":{},\"call_length_ms\":{},\"talkgroup\":{},\"talkgroup_tag\":\"{}\",\"talkgroup_description\":\"{}\",\"talkgroup_group_tag\":\"{}\",\
\"talkgroup_group\":\"{}\",\"color_code\":{},\"tone_mode\":\"{}\",\"tone_detected\":\"{}\",\"tone_confidence\":{:.3},\
\"audio_type\":\"{}\",\"short_name\":\"{}\",",
        call.id,
        call.freq_hz,
        info.freq_error_hz,
        db(info.reception.signal_db()),
        db(info.reception.noise_db()),
        db(info.reception.snr_db()),
        info.errors.clean_share().map_or("null".to_string(), |c| format!("{:.1}", c * 100.0)),
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
            "{}{{\"src\":{},\"time\":{},\"pos\":{},\"emergency\":{},\"signal_system\":\"\",\"tag\":\"{}\",\"tag_ota\":\"{}\"}}",
            if i > 0 { "," } else { "" },
            s.src,
            ms(s.time_s).div_euclid(1000),
            ((s.time_s - call.start_s) * 100.0).round().max(0.0) / 100.0,
            s.emergency as u8,
            esc(&UnitTags::name(info.unit_tags, info.units, s.src).unwrap_or_default()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_transmissions_are_dropped() {
        let mut tx = Transmissions::default();
        // 1 s at t 0, a 0.2 s key-up at 3 s, 2 s at 5 s; frames 0.18 s apart inside each.
        let mut audio = Vec::new();
        for (t0, secs) in [(0.0, 1.0), (3.0, 0.2), (5.0, 2.0)] {
            let mut t = t0;
            let end = audio.len() + (secs * 8000.0) as usize;
            while audio.len() < end {
                tx.note(audio.len(), t);
                audio.extend(std::iter::repeat_n(t0 as f32, 1440.min(end - audio.len())));
                t += 0.18;
            }
        }
        assert_eq!(tx.starts, [0, 8000, 9600]);
        let mut a = audio.clone();
        tx.drop_short(&mut a, 0.5, 8000);
        assert_eq!(a.len(), 24000);
        assert!(!a.contains(&3.0));
        tx.drop_short(&mut audio, 0.0, 8000);
        assert_eq!(audio.len(), 25600);
    }
}
