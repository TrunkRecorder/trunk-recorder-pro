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

/// Audio formats a plugin can ask for ([`Manifest::audio_formats`]). WAV is always there.
pub mod format {
    /// AAC in an MP4 container (OpenMHz, Broadcastify Calls).
    pub const M4A: &str = "m4a";
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
    /// The plugin's settings, as the user entered them (see [`Manifest::config`]).
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

/// The index calls and events use for the first conventional system; the
/// next ones count down from it (65534, 65533, …). Each is a system of its
/// own in [`Hello::systems`], with its own short name and settings.
pub const CONVENTIONAL: u16 = 65535;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInfo {
    /// The number events carry for it (`system`) while the recorder runs
    /// (conventional systems: [`CONVENTIONAL`] and down). It can change
    /// between runs: know systems by `short_name`.
    pub index: u16,
    /// The system's short name: its identity (unique among all the
    /// recorder's systems), its folder, and what users know it by. Every
    /// event carries it.
    pub short_name: String,
    /// "p25" | "smartnet" | "dmr" | "conventional"
    pub kind: String,
    /// The plugin's settings for this system (see [`Manifest::system_config`]);
    /// null when the user left them empty.
    pub config: Value,
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
    pub talkgroup: u32,
    /// The talkgroup's alpha tag, when known.
    pub talkgroup_tag: String,
    pub freq_hz: u64,
    /// Phase II TDMA slot.
    pub tdma_slot: Option<u8>,
    pub analog: bool,
    pub encrypted: bool,
    pub emergency: bool,
    /// Recorded (false: only followed, or not recorded — see `reason`).
    pub recording: bool,
    /// Why it isn't recorded: "encrypted", "unknown_tg", "no_recorder", "no_source", …
    pub reason: Option<String>,
    /// Unix seconds.
    pub start_time: f64,
    /// Radios heard on the call so far.
    pub units: Vec<u32>,
    /// Talkgroups patched with this one so far, its own included, ascending;
    /// empty when it isn't patched.
    pub patched_talkgroups: Vec<u32>,
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
    /// 16-bit mono WAV, 8 kHz.
    pub wav: PathBuf,
    /// When the plugin asked for M4A and the recorder could encode it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub m4a: Option<PathBuf>,
}

/// Trunk Recorder's call JSON. Fields this crate doesn't name are in `extra`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CallRecord {
    pub call_num: u64,
    pub short_name: String,
    pub talkgroup: u32,
    pub talkgroup_tag: String,
    pub talkgroup_description: String,
    pub talkgroup_group_tag: String,
    pub talkgroup_group: String,
    /// Hz.
    pub freq: u64,
    /// Unix seconds.
    pub start_time: i64,
    pub stop_time: i64,
    /// Seconds of audio.
    pub call_length: f64,
    #[serde(deserialize_with = "flag")]
    pub emergency: bool,
    #[serde(deserialize_with = "flag")]
    pub encrypted: bool,
    pub priority: i64,
    #[serde(deserialize_with = "flag")]
    pub phase2_tdma: bool,
    pub tdma_slot: i64,
    /// "digital" | "digital tdma" | "analog"
    pub audio_type: String,
    #[serde(rename = "freqList")]
    pub freq_list: Vec<FreqEntry>,
    #[serde(rename = "srcList")]
    pub src_list: Vec<SrcEntry>,
    /// Every talkgroup patched with this one during the call, its own
    /// included; absent (empty) when it wasn't patched with another.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub patched_talkgroups: Vec<u32>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CallRecord {
    /// Voice frames with bit errors, over the call.
    pub fn error_count(&self) -> u64 {
        self.freq_list.iter().map(|f| f.error_count).sum()
    }
    pub fn spike_count(&self) -> u64 {
        self.freq_list.iter().map(|f| f.spike_count).sum()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FreqEntry {
    pub freq: u64,
    /// Unix seconds.
    pub time: i64,
    /// Seconds into the audio.
    pub pos: f64,
    /// Seconds.
    pub len: f64,
    pub error_count: u64,
    pub spike_count: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A radio heard on the call.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SrcEntry {
    /// Radio ID.
    pub src: i64,
    /// Unix seconds.
    pub time: i64,
    /// Seconds into the audio.
    pub pos: f64,
    #[serde(deserialize_with = "flag")]
    pub emergency: bool,
    pub signal_system: String,
    /// The radio's name from the unit tags file.
    pub tag: String,
    /// Its talker alias, heard over the air.
    pub tag_ota: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
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
    /// "registration" | "deregistration" | "affiliation" | "acknowledge" |
    /// "location" | "data_grant" | "answer_request" | "call_alert"
    pub kind: String,
    /// Radio ID.
    pub unit: u32,
    /// The talkgroup involved (affiliation, location, answer request, call alert).
    pub talkgroup: Option<u32>,
    /// Unix seconds.
    pub time: f64,
}

/// A slice of a recording call's audio: 16-bit mono PCM, little-endian, base64.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioChunk {
    pub call_id: u32,
    pub system: u16,
    pub talkgroup: u32,
    pub sample_rate: u32,
    pub pcm: String,
}

impl AudioChunk {
    pub fn new(call_id: u32, system: u16, talkgroup: u32, sample_rate: u32, samples: &[i16]) -> Self {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        AudioChunk { call_id, system, talkgroup, sample_rate, pcm: base64::encode(&bytes) }
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
    /// The control channel being decoded (none: searching).
    pub control_channel_hz: Option<u64>,
    /// Control channel messages decoded per second.
    pub decode_rate: f64,
    pub active_calls: u32,
    pub recording: u32,
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
    #[serde(other)]
    Unknown,
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
        assert_eq!(c.extra["color_code"], -1);
        // And back, with the unknown field kept.
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["color_code"], -1);
        assert_eq!(v["srcList"][0]["src"], 1234);
        assert_eq!(v["patched_talkgroups"], serde_json::json!([101, 65001]));
        let unpatched = serde_json::to_value(CallRecord::default()).unwrap();
        assert!(unpatched.get("patched_talkgroups").is_none());
    }
}
