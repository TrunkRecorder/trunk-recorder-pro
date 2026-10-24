//! The per-call JSON, with Trunk Recorder's field names (call_concluder.cc
//! create_call_json) so existing tooling reads it, and TR's file base name.


pub use trunk_recorder_plugin::{CallRecord, ErrorInterval, FreqEntry, SrcEntry, ToneEntry, TonePattern};

use super::calls::{Call, Voice};
use crate::dsp::inband::signal::{matching, Signal};
use crate::dsp::inband::{self, ToneSeg};
use crate::dsp::tones::Tone;
use super::frames::FrameErrors;
use super::frames::{frames_jsonl, VoiceFrame};
use super::multisite::Held;
use super::sdr::{call_sdr, SdrInfo, SdrMode};
use super::units::{UnitAliases, UnitTags};
use crate::{loudness, mbe};

pub struct ConcludeInfo<'a> {
    pub short_name: &'a str,
    /// Its system's kind ([`super::Protocol::kind`], or "conventional").
    pub system_type: &'a str,
    /// Its site's name (empty: a conventional system's call, which has none).
    pub site: &'a str,
    /// Wall-clock epoch ms at sample-clock time 0.
    pub epoch_ms_at_zero: f64,
    pub audio_seconds: f64,
    pub errors: &'a FrameErrors,
    /// Its in-band tones, placed in the saved audio.
    pub tones: &'a [ToneSeg],
    /// The system's signals, which name its tones' patterns.
    pub signals: &'a [Signal],
    /// Written as `recorder_num`: the recorder pool slot of a trunked call;
    /// for a conventional call, the index of the channel (frequency) it came on.
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

/// What the air said of one transmission.
#[derive(Clone, Copy, Debug, Default)]
struct TxFlags {
    encrypted: bool,
    decrypted: bool,
}

impl TxFlags {
    /// Left out: encrypted, not decrypted.
    fn dropped(self) -> bool {
        self.encrypted && !self.decrypted
    }
}

/// Where in a call's audio each transmission starts (sample index), and
/// which were encrypted, and of those which decrypted.
#[derive(Clone, Debug, Default)]
pub struct Transmissions {
    pub starts: Vec<usize>,
    /// Per start.
    flags: Vec<TxFlags>,
    /// Announced before the next transmission's audio came.
    next: TxFlags,
    last_s: Option<f64>,
    /// (time, sample index) of each piece of audio appended.
    marks: Vec<(f64, usize)>,
}

impl Transmissions {
    /// Audio is about to be appended at `at` (the call's sample count), at time `t_s`.
    pub fn note(&mut self, at: usize, t_s: f64) {
        if self.last_s.is_none_or(|l| t_s - l > TRANSMISSION_GAP_S) && self.starts.last() != Some(&at) {
            self.starts.push(at);
            self.flags.push(std::mem::take(&mut self.next));
        }
        self.last_s = Some(t_s);
        if self.marks.last().is_none_or(|&(_, a)| a != at) {
            self.marks.push((t_s, at));
        }
    }

    /// Where in the call's `len` samples of audio each source not yet
    /// placed starts talking: the first audio appended at or after it was
    /// heard (the gaps between transmissions aren't in the audio, so its
    /// time since the call started is no guide).
    pub fn place(&self, sources: &mut [super::calls::CallSource], len: usize) {
        for s in sources.iter_mut().filter(|s| s.audio_pos.is_none()) {
            let i = self.marks.partition_point(|&(t, _)| t < s.time_s);
            s.audio_pos = Some(self.marks.get(i).map_or(len, |&(_, at)| at.min(len)));
        }
    }

    /// The flags of the transmission going on at `t_s`, or (none is) the next one's.
    fn at(&mut self, t_s: f64) -> &mut TxFlags {
        match self.flags.last_mut() {
            Some(f) if self.last_s.is_some_and(|l| t_s - l <= TRANSMISSION_GAP_S) => f,
            _ => &mut self.next,
        }
    }

    /// The air said, at `t_s`, that the call is encrypted: the transmission
    /// going on then, or (none is) the next one.
    pub fn mark_encrypted(&mut self, t_s: f64) {
        self.at(t_s).encrypted = true;
    }

    /// The voice tracker decrypted, at `t_s`: the transmission going on
    /// then, or (none is) the next one.
    pub fn mark_decrypted(&mut self, t_s: f64) {
        self.at(t_s).decrypted = true;
    }

    /// The transmission going on now is left out: encrypted, not decrypted.
    pub fn current_dropped(&self) -> bool {
        self.flags.last().copied().unwrap_or(self.next).dropped()
    }

    /// The parts of a call's `len` samples of audio to keep: every
    /// transmission but those shorter than `min_s` (Trunk Recorder's
    /// minTransmissionDuration: key-ups, data bursts) and the encrypted ones
    /// that weren't decrypted (what a vocoder makes of them before the
    /// cipher is known is noise). None: all of it.
    pub fn kept(&self, len: usize, min_s: f64, rate: u32) -> Option<Vec<std::ops::Range<usize>>> {
        if (min_s <= 0.0 && !self.flags.iter().any(|f| f.dropped())) || self.starts.is_empty() {
            return None;
        }
        let min = (min_s.max(0.0) * rate as f64) as usize;
        // (Audio before the first mark is the first transmission's too.)
        let mut bounds: Vec<usize> = self.starts.iter().copied().filter(|&s| s > 0 && s < len).collect();
        bounds.insert(0, 0);
        bounds.push(len);
        let dropped = |at: usize| self.starts.iter().position(|&s| s == at).is_some_and(|i| self.flags[i].dropped());
        Some(bounds.windows(2).filter(|w| w[1] - w[0] >= min && !dropped(w[0])).map(|w| w[0]..w[1]).collect())
    }
}

/// A call's record (Trunk Recorder's call JSON, [`CallRecord`]), and its
/// base name `<talkgroup>-<start epoch>_<freq>[.slot]`.
pub fn call_record(call: &Call, info: &ConcludeInfo) -> (CallRecord, String) {
    let ms = |s: f64| (info.epoch_ms_at_zero + s * 1000.0).round() as i64;
    let (start_ms, stop_ms) = (ms(call.start_s), ms(info.end_s));
    let tg = call.talkgroup_info.as_ref();
    let t = |f: fn(&super::talkgroups::Talkgroup) -> &str| tg.map_or(String::new(), |t| f(t).to_string());
    let secs = info.audio_seconds;
    let patterns = inband::pattern::patterns(info.tones);
    let record = CallRecord {
        call_num: call.id as u64,
        freq: call.freq_hz,
        freq_error: info.freq_error_hz as i64,
        signal: info.reception.signal_db(),
        noise: info.reception.noise_db(),
        snr: info.reception.snr_db(),
        clean_voice_pct: info.errors.clean_share().map(|c| c * 100.0),
        source_num: 0,
        recorder_num: info.recorder_num as i64,
        tdma_slot: call.voice.slot().unwrap_or(0) as i64,
        phase2_tdma: call.voice.tdma(),
        start_time: start_ms.div_euclid(1000),
        stop_time: stop_ms.div_euclid(1000),
        start_time_ms: start_ms,
        stop_time_ms: stop_ms,
        emergency: call.emergency,
        priority: call.priority as i64,
        mode: call.mode,
        duplex: call.duplex,
        encrypted: call.encrypted,
        // The cipher its encryption sync named (-1: none heard).
        algid: call.cipher.map_or(-1, |c| c.algid as i64),
        key_id: call.cipher.map_or(-1, |c| c.keyid as i64),
        decrypted: call.decrypted,
        clock_off: false,
        call_length: secs.round(),
        call_length_ms: (secs * 1000.0).round() as i64,
        talkgroup: call.talkgroup,
        talkgroup_ota: call.talkgroup_ota,
        talkgroup_tag: t(|t| &t.alpha_tag),
        talkgroup_description: t(|t| &t.description),
        talkgroup_group_tag: t(|t| &t.tag),
        talkgroup_group: t(|t| &t.group),
        nac: call.nac.filter(|_| call.voice.decryptable()).map_or(String::new(), |n| format!("{n:03X}")),
        color_code: call.voice.color_code().map_or(-1, i64::from),
        ran: call.ran.map_or(-1, i64::from),
        // Trunk Recorder's (PR #1137) tone fields; "search": identified, not matched.
        tone_mode: match call.tone_set {
            Some(Tone::Ctcss(_)) => "ctcss",
            Some(Tone::Dcs(..)) => "dcs",
            None if call.voice.analog() && call.site.is_none() => "search",
            None => "off",
        }
        .into(),
        tone_detected: call.tone.map_or(String::new(), |t| t.tone.to_string()),
        tone_confidence: call.tone.map_or(0.0, |t| t.confidence as f64),
        audio_type: if call.voice.analog() { "analog" } else if call.voice.tdma() { "digital tdma" } else { "digital" }.into(),
        system_type: info.system_type.to_string(),
        voice_mode: call.voice.mode_name().to_string(),
        short_name: info.short_name.to_string(),
        site: info.site.to_string(),
        // Only when patched with another, as Trunk Recorder writes it.
        patched_talkgroups: if call.patched_talkgroups.len() > 1 { call.patched_talkgroups.clone() } else { Vec::new() },
        freq_list: vec![FreqEntry {
            freq: call.freq_hz,
            time: start_ms.div_euclid(1000),
            pos: 0.0,
            len: (secs * 100.0).round() / 100.0,
            error_count: info.errors.total_errors(),
            spike_count: 0,
            extra: Default::default(),
        }],
        error_list: info.errors.intervals(),
        src_list: call
            .sources
            .iter()
            .map(|s| SrcEntry {
                src: s.src as i64,
                time: ms(s.time_s).div_euclid(1000),
                // Seconds into the saved audio, as Trunk Recorder's (the WAV's length when it was heard).
                pos: (s.audio_pos.unwrap_or(0) as f64 / mbe::SAMPLE_RATE as f64 * 100.0).round() / 100.0,
                // What its transmission said of itself (the call's own flags
                // are above; these are this radio's): Trunk Recorder's names.
                emergency: s.flags.emergency,
                encrypted: s.flags.encrypted,
                duplex: s.flags.duplex,
                mode: s.flags.packet,
                priority: s.flags.priority as i64,
                signal_system: String::new(),
                tag: UnitTags::name(info.unit_tags, info.units, s.src).unwrap_or_default(),
                tag_ota: info.units.and_then(|u| u.get(s.src)).unwrap_or("").to_string(),
                lat: s.position.map(|p| p.0),
                lon: s.position.map(|p| p.1),
                extra: Default::default(),
            })
            .collect(),
        tones: info.tones.iter().map(|t| tone_entry(t, &call.sources)).collect(),
        tone_patterns: patterns.iter().map(|p| tone_pattern(p, &call.sources, matching(info.signals, p, call.talkgroup).map(String::from).collect())).collect(),
        signals: {
            let mut ids: Vec<String> = Vec::new();
            for p in &patterns {
                for id in matching(info.signals, p, call.talkgroup) {
                    if !ids.iter().any(|i| i == id) {
                        ids.push(id.into());
                    }
                }
            }
            ids
        },
        extra: Default::default(),
    };
    let base = format!(
        "{}-{}_{}{}",
        call.talkgroup,
        start_ms.div_euclid(1000),
        call.freq_hz,
        call.voice.slot().map_or(String::new(), |s| format!(".{s}"))
    );
    (record, base)
}

/// Seconds, to a hundredth.
fn hundredths(s: f64) -> f64 {
    (s * 100.0).round() / 100.0
}

fn secs(n: usize) -> f64 {
    hundredths(n as f64 / mbe::SAMPLE_RATE as f64)
}

/// The source talking at sample `at` of the saved audio (0: none known).
fn talking_at(sources: &[super::calls::CallSource], at: usize) -> i64 {
    sources.iter().filter(|s| s.audio_pos.is_some_and(|p| p <= at)).max_by_key(|s| s.audio_pos).map_or(0, |s| s.src as i64)
}

/// A tone as the call JSON has it: its sender is the source talking where it starts.
fn tone_entry(t: &ToneSeg, sources: &[super::calls::CallSource]) -> ToneEntry {
    ToneEntry {
        kind: t.kind.name().into(),
        value: t.kind.to_string(),
        pos: secs(t.start),
        len: secs(t.len),
        src: talking_at(sources, t.start),
        origin: t.origin.name().into(),
        level: t.level.map(inband::level_dbm0),
        extra: Default::default(),
    }
}

fn tone_pattern(p: &inband::pattern::Pattern, sources: &[super::calls::CallSource], signals: Vec<String>) -> TonePattern {
    TonePattern {
        signals,
        pattern: p.kind.name().into(),
        tones: p.hz.iter().map(|&f| hundredths(f as f64)).collect(),
        lengths: p.lengths.iter().map(|&l| hundredths(l as f64)).collect(),
        digits: p.digits.clone(),
        knox: p.knox,
        cycles: p.cycles,
        format: p.format.unwrap_or("").into(),
        pos: secs(p.start),
        len: secs(p.len),
        src: talking_at(sources, p.start),
        origin: p.origin.name().into(),
        extra: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_are_written_with_their_sender() {
        use super::super::calls::CallSource;
        use crate::dsp::inband::{Origin, ToneKind};
        let src = |src, audio_pos| CallSource { src, time_s: 0.0, audio_pos, flags: Default::default(), position: None };
        let sources = [src(7, Some(0)), src(9, Some(16000)), src(11, None)];
        let t = ToneSeg { kind: ToneKind::Dtmf('#'), start: 20000, len: 800, level: Some(127), origin: Origin::Frame };
        let e = tone_entry(&t, &sources);
        assert_eq!((e.kind.as_str(), e.value.as_str(), e.pos, e.len, e.src, e.origin.as_str()), ("dtmf", "#", 2.5, 0.1, 9, "frame"));
        let r = CallRecord { tones: vec![e], ..Default::default() };
        let json = r.to_json();
        assert!(json.contains(r##""tones":[{"type":"dtmf","value":"#","pos":2.5,"len":0.1,"src":9,"origin":"frame","level":3.2}]"##), "{json}");
        assert!(!CallRecord::default().to_json().contains("tones"), "none: no field");
        // Its pattern: a page, written with its format.
        let page = [ToneSeg { kind: ToneKind::Single(781.25), start: 16000, len: 7840, level: None, origin: Origin::Frame }, ToneSeg { kind: ToneKind::Single(875.0), start: 24000, len: 23840, level: None, origin: Origin::Frame }];
        let p: Vec<TonePattern> = inband::pattern::patterns(&page).iter().map(|p| tone_pattern(p, &sources, vec!["NORWALK_EMS".into()])).collect();
        let r = CallRecord { tone_patterns: p, ..Default::default() };
        let json = r.to_json();
        assert!(json.contains(r#""tone_patterns":[{"pattern":"two_tone","tones":[781.25,875.0],"lengths":[0.98,2.98],"format":"Quick Call II","pos":2,"len":3.98,"src":9,"origin":"frame","signals":["NORWALK_EMS"]}]"#), "{json}");
    }

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
        assert_eq!(tx.kept(audio.len(), 0.5, 8000), Some(vec![0..8000, 9600..25600]));
        assert_eq!(tx.kept(audio.len(), 0.0, 8000), None);
        // The last one turns out encrypted: only it goes.
        tx.mark_encrypted(5.3);
        assert_eq!(tx.kept(audio.len(), 0.0, 8000), Some(vec![0..8000, 8000..9600]));
        assert!(tx.current_dropped());
        // Unless it was decrypted.
        tx.mark_decrypted(5.4);
        assert!(!tx.current_dropped());
        assert_eq!(tx.kept(audio.len(), 0.0, 8000), None);
        // Announced before its audio: the next transmission's.
        let mut tx = Transmissions::default();
        tx.mark_encrypted(0.0);
        tx.mark_decrypted(0.0);
        tx.note(0, 0.1);
        assert!(!tx.current_dropped());
    }

    #[test]
    fn a_sources_position_is_in_the_call_json() {
        use crate::position::Position;
        use crate::trunk::calls::{CallManager, Flags};
        let mut call = Call { talkgroup: 101, ..Default::default() };
        for src in [7, 8] {
            CallManager::note_source(&mut call, Some(src), 0.0, Default::default());
        }
        assert!(CallManager::note_position(&mut call, &Position { unit: 8, lat: 30.0529, lon: -83.17565, source: "HarrisP25_TDMA" }));
        assert!(!CallManager::note_position(&mut call, &Position { unit: 8, lat: 30.0529, lon: -83.17565, source: "HarrisP25_TDMA" }), "the same again");
        assert!(!CallManager::note_position(&mut call, &Position { unit: 9, lat: 1.0, lon: 1.0, source: "HarrisP25_TDMA" }), "not on the call");
        let errors = FrameErrors::default();
        let info = ConcludeInfo {
            short_name: "s",
            system_type: "p25",
            site: "",
            epoch_ms_at_zero: 0.0,
            audio_seconds: 1.0,
            errors: &errors,
            tones: &[],
            signals: &[],
            recorder_num: 0,
            end_s: 1.0,
            units: None,
            unit_tags: None,
            freq_error_hz: 0,
            reception: Reception::default(),
        };
        let json = call_record(&call, &info).0.to_json();
        assert!(json.contains("\"src\":7,") && json.matches("\"lat\"").count() == 1, "{json}");
        // What it was and what the air carried: its system's kind, its voice, its NAC (P25 only).
        call.nac = Some(0x293);
        let r = call_record(&call, &info).0;
        assert_eq!((r.system_type.as_str(), r.voice_mode.as_str(), r.nac.as_str()), ("p25", "p25", "293"));
        call.voice = Voice::Fm;
        let r = call_record(&call, &info).0;
        assert_eq!((r.voice_mode.as_str(), r.nac.as_str()), ("fm", ""), "an analog call has no NAC");
        call.voice = Voice::P25;
        call.nac = Some(0x2a);
        assert_eq!(call_record(&call, &info).0.nac, "02A", "three digits, as the NAC is written");
        call.voice = Voice::P25;
        // Each source's own flags.
        CallManager::note_source(&mut call, Some(8), 0.0, Flags { emergency: true, priority: 7, ..Default::default() });
        let json = call_record(&call, &info).0.to_json();
        assert!(json.contains("\"src\":7,\"time\":0,\"pos\":0,\"emergency\":0,\"encrypted\":0,\"duplex\":0,\"mode\":0,\"priority\":0,"), "{json}");
        assert!(json.contains("\"src\":8,\"time\":0,\"pos\":0,\"emergency\":1,\"encrypted\":0,\"duplex\":0,\"mode\":0,\"priority\":7,"), "{json}");
        assert!(json.contains("\"tag_ota\":\"\",\"lat\":30.052900,\"lon\":-83.175650}]"), "{json}");
    }
}

/// What becomes of a finished call's audio: which calls are kept, and how
/// they sound.
#[derive(Clone, Copy, Debug)]
pub struct SaveRules {
    /// Keep calls with no decoded audio (encrypted, lost).
    pub keep_silent: bool,
    /// Keep encrypted calls (Trunk Recorder's recordEncrypted): with no
    /// audio, for what they say — who, when — even when silent calls aren't kept.
    pub keep_encrypted: bool,
    /// Drop calls with less audio than this, s (Trunk Recorder's minDuration).
    pub min_call_s: f64,
    /// Leave out transmissions shorter than this, s (minTransmissionDuration).
    pub min_transmission_s: f64,
    /// Bring each call's speech to one level ([`crate::loudness`]).
    pub normalize: bool,
    /// Then raise (or lower) digital and analog audio by this much, dB,
    /// under the limiter (Trunk Recorder's digitalLevels / analogLevels).
    pub digital_gain_db: f32,
    pub analog_gain_db: f32,
}

impl Default for SaveRules {
    fn default() -> Self {
        SaveRules { keep_silent: false, keep_encrypted: false, min_call_s: 0.0, min_transmission_s: 0.0, normalize: true, digital_gain_db: 0.0, analog_gain_db: 0.0 }
    }
}

#[derive(Clone, Debug)]
pub struct Concluded {
    pub call: Call,
    /// Its record: Trunk Recorder's call JSON.
    pub record: CallRecord,
    /// The call's system's short name (its folder).
    pub short_name: String,
    /// The site its copy was kept from ([`super::engine::SiteConfig::name`]; empty: conventional).
    pub site: String,
    /// `<talkgroup>-<start epoch>_<freq>[.slot]`
    pub base_name: String,
    /// 8 kHz mono in [−1, 1].
    pub audio: Vec<f32>,
    /// With [`EngineConfig::capture_frames`]: the vocoder frames behind
    /// `audio` (None for analog: there are none).
    pub frames: Option<Capture>,
}

/// A call's frame capture: an `.sdr` file ([`super::sdr`]), or JSON lines
/// ([`super::frames::frames_jsonl`]) for NXDN, which `.sdr` has no layout for.
#[derive(Clone, Debug, PartialEq)]
pub struct Capture {
    /// The file's extension: `sdr` or `frames.jsonl`.
    pub ext: &'static str,
    pub bytes: Vec<u8>,
}

/// What saving a call needs from its system.
pub struct SaveContext<'a> {
    pub rules: SaveRules,
    pub short_name: &'a str,
    /// Its system's kind ([`super::Protocol::kind`], or "conventional").
    pub system_type: &'a str,
    /// The site's name (empty: conventional).
    pub site: &'a str,
    /// Wall clock (Unix ms) at the engine's time 0.
    pub epoch_ms_at_zero: f64,
    /// Its radios' talker aliases, and its unit names file.
    pub units: Option<&'a UnitAliases>,
    pub unit_tags: Option<&'a UnitTags>,
    /// Its P25 identity, as far as the control channel told it (for the `.sdr` header).
    pub wacn: Option<u32>,
    pub sys_id: Option<u32>,
    pub nac: Option<u16>,
    /// The system's signals: its calls' tone patterns are named by them.
    pub signals: &'a [Signal],
}

/// The frames as an `.sdr` file when the call's mode has a layout there, else JSON lines.
fn capture(call: &Call, frames: &[VoiceFrame], audio_len: usize, freq_error_hz: Option<f64>, reception: &Reception, cx: &SaveContext) -> Capture {
    let mode = match call.voice {
        Voice::Nxdn(_) | Voice::ProVoice => None,
        Voice::Dmr { .. } => Some(SdrMode::Dmr),
        Voice::P25Tdma { .. } => Some(SdrMode::P25Tdma),
        Voice::P25 | Voice::Fm | Voice::Am => Some(SdrMode::P25Fdma),
    };
    let (first, last) = (call.sources.first().map_or(0, |s| s.src), call.sources.last().map_or(0, |s| s.src));
    let info = SdrInfo {
        start_us: ((cx.epoch_ms_at_zero + call.start_s * 1000.0) * 1000.0).round().max(0.0) as u64,
        duration_ms: (audio_len as f64 * 1000.0 / mbe::SAMPLE_RATE as f64).round() as u32,
        hz: call.freq_hz as u32,
        nac: call.voice.color_code().map(u16::from).or(call.nac).or(cx.nac).unwrap_or(0),
        wacn: cx.wacn.unwrap_or(0),
        sysid: cx.sys_id.unwrap_or(0) as u16,
        system: match (cx.wacn, cx.sys_id) {
            (Some(w), Some(s)) => format!("{w:05X}{s:03X}"),
            _ => cx.short_name.to_string(),
        },
        conventional: call.site.is_none(),
        slot: call.voice.slot().unwrap_or(0),
        tg: call.talkgroup,
        first_src: first,
        src: last,
        encrypted: call.encrypted,
        emergency: call.emergency,
        signal_db: reception.signal_db().map(|v| v as f32),
        floor_db: reception.noise_db().map(|v| v as f32),
        offset_hz: freq_error_hz.map(|e| e.round() as i32),
        alias: cx.units.and_then(|u| u.get(last)).unwrap_or("").to_string(),
        alphatag: call.talkgroup_info.as_ref().map_or(String::new(), |t| t.alpha_tag.clone()),
    };
    match mode.and_then(|m| call_sdr(m, frames, &info)) {
        Some(bytes) => Capture { ext: "sdr", bytes },
        None => Capture { ext: "frames.jsonl", bytes: frames_jsonl(frames).into_bytes() },
    }
}

/// A finished call made ready to save: transmissions shorter than its
/// rules allow left out, its loudness and level set, Trunk Recorder's call
/// JSON written. `Err(call)` when it isn't kept (silent, or shorter than
/// its system's minimum).
pub fn save_call(mut h: Held, cx: &SaveContext) -> Result<Concluded, Call> {
    let rules = &cx.rules;
    // Short transmissions and encrypted ones left out (an encrypted one's
    // "audio" is a few frames vocoded before the cipher was known: noise).
    h.trim(rules.min_transmission_s);
    let Held { call, mut audio, mut frames, recorder_num, reception, freq_error_hz, .. } = h;
    frames.tones.finish();
    let short = (audio.len() as f64) < rules.min_call_s * mbe::SAMPLE_RATE as f64 && !audio.is_empty();
    if (audio.is_empty() && !(rules.keep_silent || call.encrypted && rules.keep_encrypted)) || short {
        return Err(call);
    }
    let gain_db = if call.voice.analog() { rules.analog_gain_db } else { rules.digital_gain_db } as f64;
    if rules.normalize {
        loudness::normalize(&mut audio, mbe::SAMPLE_RATE, gain_db);
    } else {
        loudness::gain(&mut audio, mbe::SAMPLE_RATE, gain_db);
    }
    let (record, base_name) = call_record(
        &call,
        &ConcludeInfo {
            short_name: cx.short_name,
            system_type: cx.system_type,
            site: cx.site,
            epoch_ms_at_zero: cx.epoch_ms_at_zero,
            audio_seconds: audio.len() as f64 / mbe::SAMPLE_RATE as f64,
            errors: &frames.errors,
            tones: &frames.tones.segs,
            signals: cx.signals,
            recorder_num,
            end_s: call.last_audio_s,
            units: cx.units,
            unit_tags: cx.unit_tags,
            reception,
            freq_error_hz: freq_error_hz.map_or(0, |e| e.round() as i32),
        },
    );
    let frames = frames.captured.filter(|f| !audio.is_empty() && !f.is_empty()).map(|f| capture(&call, &f, audio.len(), freq_error_hz, &reception, cx));
    Ok(Concluded { call, record, short_name: cx.short_name.to_string(), site: cx.site.to_string(), base_name, audio, frames })
}
