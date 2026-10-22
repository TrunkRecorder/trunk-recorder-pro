//! The wire protocol: JSON lines.
//!
//! The recorder starts a plugin's executable with no arguments and writes one
//! [`HostMessage`] per line to its stdin; the plugin writes one
//! [`PluginMessage`] per line to its stdout. stderr is the plugin's log.
//! Either side ignores message types and fields it doesn't know, so both can
//! grow within an [`API_VERSION`].
//!
//! `plugin --describe` prints the plugin's [`Manifest`] (one JSON object) and exits.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// The protocol version. A plugin states the version it was built for in its
/// manifest; the recorder runs plugins whose `api` it supports.
pub const API_VERSION: u32 = 1;

/// Exit status of a plugin that can't run with its configuration: the recorder
/// shows the error and doesn't restart it until recording next starts.
/// (Any other exit is a crash, and the plugin is restarted with backoff.)
pub const EXIT_CONFIG: i32 = 78;

/// What a plugin can subscribe to ([`Manifest::subscribe`]).
pub mod topic {
    /// A call started ([`super::CallInfo`]).
    pub const CALL_START: &str = "call.start";
    /// A call ended ([`super::CallInfo`]); its files follow in `call.concluded`.
    pub const CALL_END: &str = "call.end";
    /// A recorded call's files are on disk ([`super::ConcludedCall`]).
    pub const CALL_CONCLUDED: &str = "call.concluded";
    /// A radio registered, affiliated, … ([`super::UnitEvent`]).
    pub const UNIT: &str = "unit";
    /// Live audio of recording calls ([`super::AudioChunk`]).
    pub const AUDIO: &str = "audio";
    /// Every system's state, every few seconds ([`super::Status`]).
    pub const STATUS: &str = "status";

    pub const ALL: &[&str] = &[CALL_START, CALL_END, CALL_CONCLUDED, UNIT, AUDIO, STATUS];
}

/// Audio formats a plugin can ask for ([`Manifest::audio_formats`]). A plugin
/// that asks for none gets the WAV.
pub mod format {
    /// AAC in an MP4 container (OpenMHz, Broadcastify Calls).
    pub const M4A: &str = "m4a";
    /// The WAV file as well as the M4A: with an .m4a made, the WAV is
    /// otherwise written only when it's kept.
    pub const WAV: &str = "wav";
}

/// Who a plugin is and what it wants — printed by `plugin --describe`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Manifest {
    /// Unique, lowercase, `a-z0-9-` (the registry key and the install folder).
    pub id: String,
    /// Shown in the interface.
    pub name: String,
    /// Semver of the plugin.
    pub version: String,
    pub description: String,
    /// The protocol version the plugin speaks ([`API_VERSION`]).
    pub api: u32,
    /// Topics ([`topic`]) to receive. Nothing else is sent — or even built.
    pub subscribe: Vec<String>,
    /// Extra audio formats ([`format`](mod@format)) for `call.concluded`. The recorder
    /// encodes each call once for every plugin that asks, when it can.
    pub audio_formats: Vec<String>,
    /// JSON Schema of the plugin's settings (an object), for the settings form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// JSON Schema of the settings the plugin takes for each system (an
    /// object); the form repeats it under each system.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_config: Option<Value>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub repository: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub license: String,
}

impl Manifest {
    pub fn subscribes(&self, topic: &str) -> bool {
        self.subscribe.iter().any(|t| t == topic)
    }
    pub fn wants_format(&self, format: &str) -> bool {
        self.audio_formats.iter().any(|f| f == format)
    }
}

// ─── Recorder → plugin ────────────────────────────────────────────────────

// (Messages are read, handled and dropped one at a time: their size doesn't matter.)
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostMessage {
    /// Always first, once.
    #[serde(rename = "hello")]
    Hello(Hello),
    #[serde(rename = "call.start")]
    CallStart(CallInfo),
    #[serde(rename = "call.end")]
    CallEnd(CallInfo),
    #[serde(rename = "call.concluded")]
    CallConcluded(ConcludedCall),
    #[serde(rename = "unit")]
    Unit(UnitEvent),
    #[serde(rename = "audio")]
    Audio(AudioChunk),
    #[serde(rename = "status")]
    Status(Status),
    /// The recorder is stopping: finish what's queued, then exit. stdin closes
    /// after this, and the process is killed if it hasn't exited within
    /// [`Shutdown::grace_s`].
    #[serde(rename = "shutdown")]
    Shutdown(Shutdown),
    /// A message type from a newer recorder.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Hello {
    /// The recorder's protocol version.
    pub api: u32,
    pub host: HostInfo,
    /// The plugin's settings, as the user entered them (see [`Manifest::config`]);
    /// each system's, with its overrides, are in [`SystemInfo::settings`].
    pub config: Value,
    /// Every system calls can come from, including conventional channels.
    pub systems: Vec<SystemInfo>,
    /// Where calls are stored.
    pub capture_dir: PathBuf,
    /// A folder of the plugin's own that survives restarts and upgrades (queues, state).
    pub data_dir: PathBuf,
    /// The audio formats `call.concluded` will carry this run: always "wav",
    /// plus those the plugin asked for that the recorder can encode.
    pub audio_formats: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInfo {
    /// The number events carry for it (`system`) while the recorder runs:
    /// from 0, the trunked systems then the conventional ones (each a
    /// system of its own here). It can change between runs: know systems by
    /// `short_name`.
    pub index: u16,
    /// The system's short name: its identity (unique among all the
    /// recorder's systems), its folder, and what users know it by. Every
    /// event carries it.
    pub short_name: String,
    /// "p25" | "smartnet" | "dmr" | "nxdn" | "edacs" | "conventional"
    pub kind: String,
    /// What people call it ("County Public Safety"); empty when the user gave no name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its FCC licenses by frequency (Hz, as a string): who is licensed on
    /// each of its channels, from the user, the air (Motorola's base station
    /// ID) or a license lookup. Optional, so often empty.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fcc: std::collections::BTreeMap<String, FccLicense>,
    /// The call sign that names it, of those: its control channel's, else
    /// the one on the most frequencies ("WPXT459"); empty when none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fcc_call_sign: String,
    /// Who holds that license, as the FCC has it ("DISTRICT OF COLUMBIA"); empty when not known.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fcc_name: String,
    /// Its sites (trunked systems; none for conventional), by the name calls
    /// and unit events carry as `site`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<SiteInfo>,
    /// The plugin's settings for this system (see [`Manifest::system_config`]);
    /// null when the user left them empty.
    pub config: Value,
    /// The plugin's own settings ([`Hello::config`]) as they are for this
    /// system: a system can override some of them (another server, say).
    /// Null: as in `Hello::config`.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub settings: Value,
}

/// One site of a [`SystemInfo`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteInfo {
    /// What calls and unit events carry as `site`.
    pub name: String,
}

/// Who is licensed on one of a system's frequencies ([`SystemInfo::fcc`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FccLicense {
    pub call_sign: String,
    /// As the FCC has it; empty when not looked up.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// A call as it starts or ends.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CallInfo {
    /// Unique while the recorder runs (Trunk Recorder's call_num).
    pub id: u32,
    /// [`SystemInfo::index`]: for this run only; `short_name` is the system's identity.
    pub system: u16,
    pub short_name: String,
    /// The site it is on (its name; a system of one site's is its short
    /// name; empty for a conventional system's call). A call heard on
    /// several sites is told once, by the site that had it first.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub site: String,
    pub talkgroup: u32,
    /// Conventional P25 / DMR / NXDN: the talkgroup the air named (this
    /// recorder's addition, like `site`). `talkgroup` is what the call is
    /// filed under, which a channel row may set to another number; none
    /// for analog, trunked calls, or a call whose air named none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talkgroup_ota: Option<u32>,
    /// The talkgroup's alpha tag, when known.
    pub talkgroup_tag: String,
    pub freq_hz: u64,
    /// The TDMA slot (0 / 1) of a call on a two-slot channel: P25 Phase 2 or DMR.
    pub tdma_slot: Option<u8>,
    pub analog: bool,
    pub encrypted: bool,
    /// P25: the cipher an encrypted call's voice named, once heard: the
    /// algorithm ID (0x84 AES-256, 0xAA ADP, …) and key ID.
    pub algid: Option<u8>,
    pub key_id: Option<u16>,
    /// Some of its encrypted voice was decrypted (the system had the key).
    pub decrypted: bool,
    pub emergency: bool,
    /// Full duplex, packet mode and priority (P25's service options: 1
    /// lowest, 4 the default, 7 highest; 0 not given), as the grant said.
    pub duplex: bool,
    pub packet: bool,
    pub priority: u8,
    /// Recorded (false: only followed, or not recorded — see `reason`).
    pub recording: bool,
    /// Why it isn't recorded: "encrypted", "unknown_tg", "no_recorder", "no_source", …
    pub reason: Option<String>,
    /// Unix seconds.
    pub start_time: f64,
    /// Radios heard on the call so far.
    pub units: Vec<u32>,
    /// The same radios, in order, with what each one's transmission said of
    /// itself.
    pub sources: Vec<CallSource>,
    /// Talkgroups patched with this one so far, its own included, ascending;
    /// empty when it isn't patched.
    pub patched_talkgroups: Vec<u32>,
}

/// A radio heard on a call, and what its transmission said of itself: P25's
/// service options (from the grant, link control and Phase 2's voice
/// channel user messages), or the flags other protocols have.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CallSource {
    pub unit: u32,
    /// Unix seconds it was first heard on the call.
    pub time: f64,
    pub emergency: bool,
    pub encrypted: bool,
    /// Full duplex (half otherwise).
    pub duplex: bool,
    /// Packet mode (circuit otherwise).
    pub packet: bool,
    /// 1 lowest, 4 the default, 7 highest; 0 not given.
    pub priority: u8,
}

/// A recorded call whose files are on disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConcludedCall {
    /// The call's key: its files' path relative to the capture folder, without
    /// an extension (`<shortName>/<Y>/<M>/<D>/<tg>-<start>_<freq>`).
    pub path: String,
    /// [`SystemInfo::index`]: for this run only; the system's identity is `call.short_name`.
    pub system: u16,
    /// Its call JSON, in Trunk Recorder's format (the `.json` file's contents).
    pub call: CallRecord,
    pub files: CallFiles,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CallFiles {
    pub json: PathBuf,
    /// 16-bit mono WAV, 8 kHz. There whenever `m4a` isn't; with an .m4a it
    /// may not be (the recorder writes the WAV only when it's kept, or a
    /// plugin asks for [`format::WAV`]).
    pub wav: PathBuf,
    /// When the plugin asked for M4A and the recorder could encode it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub m4a: Option<PathBuf>,
}

/// A call's JSON, in Trunk Recorder's format: the `.json` file beside its
/// audio. The recorder makes one of these for every call and writes it as
/// it serialises (field order, flags as 0/1, levels to a tenth of a dB);
/// read back, it accepts Trunk Recorder's own files too (flags as 0/1 or
/// booleans). Fields neither writes are kept in `extra`. Its default is the
/// recorder's for what wasn't heard (`-1`, `"off"`, `"digital"`).
///
/// Serialise it as JSON with serde_json ([`CallRecord::to_json`]): its
/// numbers are written through serde_json's raw values, which other serde
/// formats don't take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CallRecord {
    pub call_num: u64,
    /// Hz.
    pub freq: u64,
    /// How far off its channel the voice carrier came in, Hz (the mean over the call).
    pub freq_error: i64,
    /// Levels while it was heard, dB (null: not measured).
    #[serde(serialize_with = "tenth_or_null")]
    pub signal: Option<f64>,
    #[serde(serialize_with = "tenth_or_null")]
    pub noise: Option<f64>,
    #[serde(serialize_with = "tenth_or_null")]
    pub snr: Option<f64>,
    /// Voice frames decoded without errors, % (null: no digital voice).
    #[serde(serialize_with = "tenth_or_null")]
    pub clean_voice_pct: Option<f64>,
    pub source_num: i64,
    pub recorder_num: i64,
    pub tdma_slot: i64,
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub phase2_tdma: bool,
    /// Unix seconds.
    pub start_time: i64,
    pub stop_time: i64,
    /// Unix ms.
    pub start_time_ms: i64,
    pub stop_time_ms: i64,
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub emergency: bool,
    pub priority: i64,
    /// Packet mode.
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub mode: bool,
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub duplex: bool,
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub encrypted: bool,
    /// The cipher its encryption sync named (-1: none heard).
    pub algid: i64,
    pub key_id: i64,
    /// Its encrypted voice was decrypted, at least in part (this recorder's addition).
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub decrypted: bool,
    /// Seconds of audio.
    #[serde(serialize_with = "plain")]
    pub call_length: f64,
    pub call_length_ms: i64,
    pub talkgroup: u32,
    /// Conventional P25 / DMR / NXDN: the talkgroup the air named (this
    /// recorder's addition, like `site`). `talkgroup` is what the call is
    /// filed under, which a channel row may set to another number; none
    /// for analog, trunked calls, or a call whose air named none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talkgroup_ota: Option<u32>,
    pub talkgroup_tag: String,
    pub talkgroup_description: String,
    pub talkgroup_group_tag: String,
    pub talkgroup_group: String,
    /// P25: the NAC its voice carried on a conventional channel, its site's
    /// on a trunked system, in hex as radios and channel files write it
    /// (`"293"`; empty: none; this recorder's addition).
    pub nac: String,
    /// DMR: its colour code (-1: none).
    pub color_code: i64,
    /// NXDN: its RAN (-1: none).
    pub ran: i64,
    /// An analog call's tone (Trunk Recorder's): "ctcss", "dcs", "search" (identified, not matched) or "off".
    pub tone_mode: String,
    pub tone_detected: String,
    #[serde(serialize_with = "thousandth")]
    pub tone_confidence: f64,
    /// "digital" | "digital tdma" | "analog"
    pub audio_type: String,
    /// The kind of system it came on, as [`SystemInfo::kind`] has it: "p25" |
    /// "smartnet" | "dmr" | "nxdn" | "edacs" | "conventional" (this recorder's addition).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub system_type: String,
    /// What its voice was: "p25" | "p25_phase2" | "dmr" | "nxdn48" | "nxdn96"
    /// | "provoice" | "fm" | "am" — a conventional channel's `mode`, or a trunked grant's
    /// (this recorder's addition).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub voice_mode: String,
    pub short_name: String,
    /// The site its copy was kept from (this recorder's addition; a system
    /// of one site: its short name; none for a conventional system's).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub site: String,
    /// Every talkgroup patched with this one during the call, its own
    /// included; absent (empty) when it wasn't patched with another.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub patched_talkgroups: Vec<u32>,
    #[serde(rename = "freqList")]
    pub freq_list: Vec<FreqEntry>,
    /// Voice frame errors over the call, in stretches of its audio.
    #[serde(rename = "errorList")]
    pub error_list: Vec<ErrorInterval>,
    #[serde(rename = "srcList")]
    pub src_list: Vec<SrcEntry>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for CallRecord {
    fn default() -> Self {
        CallRecord {
            call_num: 0,
            freq: 0,
            freq_error: 0,
            signal: None,
            noise: None,
            snr: None,
            clean_voice_pct: None,
            source_num: 0,
            recorder_num: 0,
            tdma_slot: 0,
            phase2_tdma: false,
            start_time: 0,
            stop_time: 0,
            start_time_ms: 0,
            stop_time_ms: 0,
            emergency: false,
            priority: 0,
            mode: false,
            duplex: false,
            encrypted: false,
            algid: -1,
            key_id: -1,
            decrypted: false,
            call_length: 0.0,
            call_length_ms: 0,
            talkgroup: 0,
            talkgroup_ota: None,
            talkgroup_tag: String::new(),
            talkgroup_description: String::new(),
            talkgroup_group_tag: String::new(),
            talkgroup_group: String::new(),
            nac: String::new(),
            color_code: -1,
            ran: -1,
            tone_mode: "off".into(),
            tone_detected: String::new(),
            tone_confidence: 0.0,
            audio_type: "digital".into(),
            system_type: String::new(),
            voice_mode: String::new(),
            short_name: String::new(),
            site: String::new(),
            patched_talkgroups: Vec::new(),
            freq_list: Vec::new(),
            error_list: Vec::new(),
            src_list: Vec::new(),
            extra: Map::new(),
        }
    }
}

impl CallRecord {
    /// Voice frames with bit errors, over the call.
    pub fn error_count(&self) -> u64 {
        self.freq_list.iter().map(|f| f.error_count).sum()
    }
    pub fn spike_count(&self) -> u64 {
        self.freq_list.iter().map(|f| f.spike_count).sum()
    }
    /// As the `.json` file has it.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FreqEntry {
    pub freq: u64,
    /// Unix seconds.
    pub time: i64,
    /// Seconds into the audio.
    #[serde(serialize_with = "plain")]
    pub pos: f64,
    /// Seconds.
    #[serde(serialize_with = "plain")]
    pub len: f64,
    pub error_count: u64,
    pub spike_count: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A stretch of the call's audio, and its voice frames' errors.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ErrorInterval {
    /// Seconds into the audio.
    #[serde(serialize_with = "plain")]
    pub pos: f64,
    /// Seconds.
    #[serde(serialize_with = "plain")]
    pub len: f64,
    pub frames: u64,
    /// Bit errors corrected.
    pub error_count: u64,
    /// Frames too damaged to use.
    pub bad_frames: u64,
    pub max_frame_errors: u64,
}

/// A radio heard on the call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SrcEntry {
    /// Radio ID.
    pub src: i64,
    /// Unix seconds.
    pub time: i64,
    /// Seconds into the audio.
    #[serde(serialize_with = "plain")]
    pub pos: f64,
    /// What its transmission said of itself (this recorder's: Trunk
    /// Recorder writes only `emergency` here).
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub emergency: bool,
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub encrypted: bool,
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub duplex: bool,
    /// Packet mode.
    #[serde(deserialize_with = "flag", serialize_with = "flag_out")]
    pub mode: bool,
    /// 1 lowest, 4 the default, 7 highest; 0 not given.
    pub priority: i64,
    pub signal_system: String,
    /// The radio's name from the unit tags file.
    pub tag: String,
    /// Its talker alias, heard over the air.
    pub tag_ota: String,
    /// Where it said it was (GPS, decimal degrees), when it did.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "millionth")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "millionth")]
    pub lon: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// How the call JSON writes its numbers (as Trunk Recorder's files have
// them): a flag as 0/1, a level to a tenth, a confidence to a thousandth, a
// position to a millionth, seconds as Rust prints them ("4", not "4.0").

fn number<S: serde::Serializer>(text: String, s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::Error;
    serde_json::value::RawValue::from_string(text).map_err(S::Error::custom)?.serialize(s)
}

fn finite<S: serde::Serializer>(v: f64, text: impl FnOnce(f64) -> String, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() { number(text(v), s) } else { s.serialize_none() }
}

fn flag_out<S: serde::Serializer>(v: &bool, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u8(*v as u8)
}

fn plain<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    finite(*v, |v| format!("{v}"), s)
}

fn thousandth<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    finite(*v, |v| format!("{v:.3}"), s)
}

fn tenth_or_null<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => finite(*v, |v| format!("{v:.1}"), s),
        None => s.serialize_none(),
    }
}

fn millionth<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => finite(*v, |v| format!("{v:.6}"), s),
        None => s.serialize_none(),
    }
}

/// Trunk Recorder writes flags as 0/1 and as booleans; take either.
fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_f64().is_some_and(|v| v != 0.0),
        Value::String(s) => s == "true" || s == "1",
        _ => false,
    })
}

/// Something a radio did on the control channel.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UnitEvent {
    /// [`SystemInfo::index`]: for this run only; `short_name` is the system's identity.
    pub system: u16,
    pub short_name: String,
    /// The site whose control channel told it (its name).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub site: String,
    /// "registration" | "deregistration" | "affiliation" | "acknowledge" |
    /// "location" | "data_grant" | "answer_request" | "call_alert" |
    /// "emergency_alarm"
    pub kind: String,
    /// Radio ID.
    pub unit: u32,
    /// The talkgroup involved (affiliation, location, answer request, call
    /// alert, emergency alarm).
    pub talkgroup: Option<u32>,
    /// The message said it is an emergency (an emergency alarm, a data
    /// grant's options).
    pub emergency: bool,
    /// Unix seconds.
    pub time: f64,
}

/// A slice of a recording call's audio: 16-bit mono PCM, little-endian, base64.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioChunk {
    pub call_id: u32,
    /// [`SystemInfo::index`]: for this run only; `short_name` is the system's identity.
    pub system: u16,
    pub short_name: String,
    pub talkgroup: u32,
    pub sample_rate: u32,
    pub pcm: String,
}

impl AudioChunk {
    pub fn new(call_id: u32, system: u16, talkgroup: u32, sample_rate: u32, samples: &[i16]) -> Self {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        AudioChunk { call_id, system, talkgroup, sample_rate, pcm: base64::encode(&bytes), ..Default::default() }
    }
    /// The samples.
    pub fn samples(&self) -> Vec<i16> {
        let b = base64::decode(&self.pcm);
        b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    /// Unix seconds.
    pub time: f64,
    pub systems: Vec<SystemStatus>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemStatus {
    pub index: u16,
    pub short_name: String,
    /// One entry per site: this one's name (a system of one site: its short
    /// name). `site` below is its number, as the control channel says it.
    pub site_name: String,
    /// The control channel being decoded (none: searching).
    pub control_channel_hz: Option<u64>,
    /// Control channel messages decoded per second.
    pub decode_rate: f64,
    pub active_calls: u32,
    pub recording: u32,
    /// The system's identity, as far as its control channel has stated it
    /// (none until it has): P25 NAC, WACN, system ID, RFSS and site; site
    /// also for SmartNet when its OBT sends one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nac: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wacn: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sys_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rfss: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Shutdown {
    /// Seconds the plugin has to exit.
    pub grace_s: f64,
}

// ─── Plugin → recorder ────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PluginMessage {
    /// The plugin read its hello and is running.
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "log")]
    Log { level: Level, message: String },
    /// The plugin's health, shown next to it in the interface.
    #[serde(rename = "status")]
    Status {
        state: State,
        #[serde(default)]
        message: String,
    },
    /// What became of a concluded call (`path`: [`ConcludedCall::path`]).
    #[serde(rename = "call.result")]
    CallResult {
        path: String,
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
        /// Where the call can be found now (a link to it on the service).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        url: String,
    },
    /// How the plugin's work is going, for the dashboard: queue, results,
    /// timing and the services it talks to. Send it when it changes, at most
    /// every few seconds ([`crate::CallQueue`] does it for you).
    #[serde(rename = "metrics")]
    Metrics(Metrics),
    #[serde(other)]
    Unknown,
}

/// A plugin's figures ([`PluginMessage::Metrics`]). Every field is optional:
/// send what you know. Totals are since the plugin started.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Metrics {
    /// Calls waiting to be worked on (ready, or waiting to retry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<u64>,
    /// Of those, waiting to retry after a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrying: Option<u64>,
    /// Being worked on now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<u64>,
    /// Tries that failed and will be retried, in total.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<u64>,
    /// Bytes sent, in total.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// How long one piece of work takes (an upload), ms, smoothed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    /// Unix seconds of the last success and the last failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<f64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub last_error_text: String,
    /// The services it talks to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<Endpoint>,
    /// Anything else worth showing, by name (shown as it is).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// A service a plugin talks to, and how it's answering.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Endpoint {
    /// What it is ("Broadcastify Calls", a host name).
    pub name: String,
    pub state: EndpointState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<f64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub last_error: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EndpointState {
    #[default]
    Unknown,
    /// Answering.
    Up,
    /// Answering, with failures (retries).
    Degraded,
    /// Not answering.
    Down,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Working.
    Ok,
    /// Working, but something needs attention (a service down, calls queued).
    Warning,
    /// Not working (bad settings, nothing it can do).
    Error,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Done (uploaded, sent, stored).
    Ok,
    /// Deliberately not handled (a system it isn't set up for, a filtered talkgroup).
    Skipped,
    /// Gave up on it.
    Failed,
}

/// Standard base64 (for [`AudioChunk::pcm`]).
pub mod base64 {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(b: &[u8]) -> String {
        let mut o = String::with_capacity(b.len().div_ceil(3) * 4);
        for c in b.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    o.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    o.push('=');
                }
            }
        }
        o
    }

    /// Invalid characters are skipped.
    pub fn decode(s: &str) -> Vec<u8> {
        let mut o = Vec::with_capacity(s.len() / 4 * 3);
        let (mut acc, mut bits) = (0u32, 0);
        for ch in s.bytes() {
            let v = match ch {
                b'A'..=b'Z' => ch - b'A',
                b'a'..=b'z' => ch - b'a' + 26,
                b'0'..=b'9' => ch - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            };
            acc = acc << 6 | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                o.push((acc >> bits) as u8);
            }
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Trunk Recorder file (flags as booleans, a field this crate doesn't
    /// name) reads; written back, flags are 0/1 as the recorder's own files
    /// have them, levels to a tenth, seconds as written, and the unknown field kept.
    #[test]
    fn a_call_record_round_trips_as_the_recorders_files_are() {
        let tr = r#"{"freq":851012500,"signal":-31.24,"snr":null,"emergency":true,"encrypted":false,"call_length":4,"talkgroup":101,"short_name":"dcfd",
            "srcList":[{"src":7,"pos":1.5,"emergency":false,"lat":30.0529}],"freqList":[{"freq":851012500,"len":4.0}],"rec_state_type":"STOPPED"}"#;
        let r: CallRecord = serde_json::from_str(tr).unwrap();
        assert!(r.emergency && !r.encrypted && r.signal == Some(-31.24) && r.snr.is_none());
        let back = r.to_json();
        assert!(back.contains("\"signal\":-31.2,\"noise\":null,\"snr\":null,"), "{back}");
        assert!(back.contains("\"emergency\":1,") && back.contains("\"encrypted\":0,"), "{back}");
        assert!(back.contains("\"call_length\":4,") && back.contains("\"len\":4,"), "{back}");
        assert!(back.contains("\"pos\":1.5,\"emergency\":0,") && back.contains("\"lat\":30.052900"), "{back}");
        assert!(back.ends_with("\"rec_state_type\":\"STOPPED\"}"), "{back}");
        // Read and written again, it's the same.
        assert_eq!(serde_json::from_str::<CallRecord>(&back).unwrap().to_json(), back);
    }

    #[test]
    fn base64_roundtrip() {
        for n in 0..10 {
            let b: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(base64::decode(&base64::encode(&b)), b);
        }
        assert_eq!(base64::encode(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn unknown_types_and_fields_are_ignored() {
        let m: HostMessage = serde_json::from_str(r#"{"type":"from.the.future","x":1}"#).unwrap();
        assert!(matches!(m, HostMessage::Unknown));
        let m: HostMessage = serde_json::from_str(r#"{"type":"shutdown","grace_s":5,"new_field":true}"#).unwrap();
        assert!(matches!(m, HostMessage::Shutdown(Shutdown { grace_s }) if grace_s == 5.0));
    }

    #[test]
    fn call_record_reads_trunk_recorder_json() {
        let j = r#"{"call_num":7,"freq":851012500,"start_time":1700000000,"stop_time":1700000005,"emergency":0,"encrypted":1,
            "call_length":5,"talkgroup":101,"talkgroup_tag":"Fire Disp","audio_type":"digital","short_name":"dcfd","phase2_tdma":0,
            "freqList":[{"freq":851012500,"time":1700000000,"pos":0,"len":5,"error_count":3,"spike_count":1}],
            "srcList":[{"src":1234,"time":1700000000,"pos":0.5,"emergency":0,"signal_system":"","tag":"","tag_ota":"E1"}],
            "color_code":-1,"patched_talkgroups":[101,65001]}"#;
        let c: CallRecord = serde_json::from_str(j).unwrap();
        assert!(c.encrypted && !c.emergency);
        assert_eq!(c.patched_talkgroups, [101, 65001]);
        assert_eq!((c.error_count(), c.spike_count()), (3, 1));
        assert_eq!(c.src_list[0].tag_ota, "E1");
        assert_eq!(c.color_code, -1);
        // And back.
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["color_code"], -1);
        assert_eq!(v["srcList"][0]["src"], 1234);
        assert_eq!(v["patched_talkgroups"], serde_json::json!([101, 65001]));
        let unpatched = serde_json::to_value(CallRecord::default()).unwrap();
        assert!(unpatched.get("patched_talkgroups").is_none());
    }

    #[test]
    fn system_identity_is_there_once_stated() {
        // From a recorder that doesn't send it: none of it.
        let s: SystemStatus = serde_json::from_str(r#"{"index":0,"short_name":"county","decode_rate":39.8}"#).unwrap();
        assert_eq!((s.nac, s.wacn, s.sys_id, s.rfss, s.site), (None, None, None, None, None));
        // Nothing stated yet: nothing written.
        let v = serde_json::to_value(&s).unwrap();
        assert!(["nac", "wacn", "sys_id", "rfss", "site"].iter().all(|k| v.get(k).is_none()), "{v}");
        let s = SystemStatus { nac: Some(0x3A0), wacn: Some(0xBEE00), sys_id: Some(0x3AB), rfss: Some(1), site: Some(5), ..s };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!((v["nac"].as_u64(), v["wacn"].as_u64(), v["site"].as_u64()), (Some(0x3A0), Some(0xBEE00), Some(5)));
    }
}
