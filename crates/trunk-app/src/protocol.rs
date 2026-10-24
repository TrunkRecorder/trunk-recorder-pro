//! The interface protocol: every message the recorder sends an interface
//! ([`ToClient`]) and every command it takes from one ([`Command`]).
//!
//! `web/src/protocol.ts` (`FromRecorder`, `ToRecorder`) is the source of
//! truth for their shapes and documents them; the desktop app's tests check
//! these enums' variants against it, both ways, and every message sent
//! against its schema. A message's `type` is its variant's name, in
//! camelCase. Bodies that are still put together as JSON ride as a
//! [`Body`] (an object, without its `type`).
//!
//! What a connection may hear and send by its access (listen or control) is
//! decided here too: [`ToClient::audience`], [`Command::listen_may_send`].

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Config;

/// A message's body, put together as JSON: an object, without its `type`.
pub type Body = Map<String, Value>;

/// `v` (an object) as a [`Body`]: its fields, any `type` left out (the
/// message's variant says it).
pub fn body(v: Value) -> Body {
    match v {
        Value::Object(mut m) => {
            m.remove("type");
            m
        }
        _ => Body::new(),
    }
}

/// A message to an interface (protocol.ts's `FromRecorder`).
#[derive(Clone, Debug, Serialize, strum::IntoStaticStr, strum::VariantNames)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum ToClient {
    /// The last message of a connection's greeting: every message before it
    /// said how the recorder is now (its config, talkgroups, phase, recent
    /// calls and so on, as ordinary messages), so the interface is caught up.
    /// The browser build's worker sends the same greeting.
    Hello {
        version: String,
        platform: String,
        /// The interface API version (trunk-pro's `interfaces::API_VERSION`).
        api: u32,
        /// The config file (control access only).
        #[serde(skip_serializing_if = "Option::is_none")]
        config_path: Option<String>,
        survey_bands: Value,
        /// What this recorder can do beyond recording: an interface shows only what works.
        capabilities: Vec<Capability>,
    },
    /// The latest calls, newest first: on connecting (then `concluded` for each new one).
    History { entries: Vec<Value> },
    /// A system's talker aliases as saved (Trunk Recorder's unitTagsOTA CSV):
    /// one for each system with some, on connecting (then `unitAlias` for each new one).
    Units { system: String, csv: String },
    /// Which key containers are unlocked.
    Keys { sources: Value },
    Radios { radios: Value },
    /// The recording's phase.
    State { phase: &'static str, error: Option<String>, ended: bool },
    /// The config, and its version: the number an edit of it is sent with.
    Config { config: Box<Config>, version: u64 },
    /// Answers setConfig: the config's version now; `error` when the edit
    /// wasn't taken (it was of an older version, or the config was bad) or
    /// wasn't saved.
    ConfigSaved { version: u64, #[serde(skip_serializing_if = "Option::is_none")] error: Option<String> },
    /// A system's talkgroup file (Trunk Recorder's CSV; "" for none): one for
    /// each on connecting, and again whenever one changes.
    Talkgroups { system: String, csv: String },
    Status { status: Value, sources: Vec<Value>, load: f64, calls: Vec<Value> },
    Spectrum { source: usize, center_hz: f64, rate_hz: f64, bins: Vec<f32> },
    Log { lines: Vec<Value> },
    /// A call stored: its history entry.
    Concluded { entry: Value },
    /// A call's upload plugins are done and it kept other files than its entry said.
    CallFiles { path: String, audio: bool, json: bool },
    UnitAlias { system: String, unit: u32, alias: String },
    Heard { heard: Value },
    Survey(Body),
    SurveySpectrum { source: usize, center_hz: f64, rate_hz: f64, bins: Vec<f32> },
    /// Answers `licenseQuery`, to its sender: the API's `results`, or why there are none.
    LicenseResult { id: u64, results: Vec<Value>, error: Option<String> },
    /// Answers `callSignQuery`, to its sender: each active call sign's holder, or why there are none.
    CallSignResult { id: u64, results: Vec<Value>, error: Option<String> },
    SourceProfile(Body),
    Devices { devices: Value },
    Dir(Body),
    TrConfig(Body),
    Error { message: String },
    Notice { message: String },
    Plugins(Body),
    PluginRuntime { id: String, runtime: Value },
    Interfaces(Body),
    /// A store's list (plugins' or interfaces'): `{ source, fetched, problem, listings, … }`.
    Store { kind: StoreKind, #[serde(flatten)] list: Body },
    /// An install from a store (or a GitHub release) as it goes, to everyone.
    InstallProgress { kind: StoreKind, key: String, id: String, stage: &'static str, message: Option<String> },
    /// Every second while recording: each series' value now.
    Stats { t: f64, values: Body },
    /// The computer's and the plugins' series, and the computer's own figures (desktop app).
    Host(Body),
    RfDetail { source: usize, profile: Vec<f64>, channels: Vec<Value> },
    /// A system's decoding (the `decode:<short name>` topic): its channels,
    /// each running site's neighbours and DMR / NXDN detail, its patches.
    DecodeDetail { system: String, channels: Vec<Value>, sites: Vec<Value>, patches: Vec<Value> },
    MonitorEvent(Body),
    Subscribed { topics: Vec<String> },
    StatsResult(Body),
    RadioResult(Body),
    OlderCalls { before: i64, entries: Vec<Value>, more: bool },
    Quit,
}

/// Something a recorder can do beyond recording (`hello.capabilities`). The
/// desktop app can do all of them; the browser build, none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, strum::VariantArray)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    /// A program of its own on a computer: quit, start recording when it
    /// starts, log files, M4A encoding, files by path (capture files, channel
    /// files, browsing folders, a Trunk Recorder config, the recordings
    /// folder, the RAM spool, keeping audio and JSON once plugins are done).
    Service,
    /// Radio drivers beyond RTL-SDR (USRP, Airspy, SoapySDR), finding
    /// radios, an RTL-SDR's USB transfer sizes.
    Drivers,
    /// Plugins: listing, adding, the store, their settings.
    Plugins,
    /// Other interfaces, served on ports of their own.
    Interfaces,
    /// The computer's and plugins' figures (`host`).
    Host,
    /// Key containers opened with a password.
    Keys,
}

impl Capability {
    /// All of them (the desktop app).
    pub const ALL: &'static [Capability] = <Self as strum::VariantArray>::VARIANTS;
}

/// Who may hear a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
    Everyone,
    /// Control access only: plugin and interface management, key containers.
    Control,
}

impl ToClient {
    /// Every message's `type`.
    pub const TYPES: &'static [&'static str] = <Self as strum::VariantNames>::VARIANTS;

    /// Its `type`.
    pub fn kind(&self) -> &'static str {
        self.into()
    }

    pub fn audience(&self) -> Audience {
        use ToClient::*;
        match self {
            Plugins(_) | PluginRuntime { .. } | Interfaces(_) | Store { .. } | InstallProgress { .. } | Keys { .. } => Audience::Control,
            _ => Audience::Everyone,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        ToClient::Error { message: message.into() }
    }

    pub fn notice(message: impl Into<String>) -> Self {
        ToClient::Notice { message: message.into() }
    }

    /// As JSON text, for the wire.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// As a JSON value (tests, and bodies built from it).
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// Which store: the plugins' or the interfaces'.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StoreKind {
    Plugin,
    Interface,
}

/// Which plugin or interface to install: one listed in the store, or a
/// GitHub release (its latest, or `tag`).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Install {
    Listed { id: String },
    Release { repository: String, #[serde(default)] tag: Option<String> },
}

impl Install {
    /// What it is known by while it installs: the id, or the repository.
    pub fn key(&self) -> &str {
        match self {
            Install::Listed { id } => id.trim(),
            Install::Release { repository, .. } => repository.trim(),
        }
    }
}

/// A command from an interface (protocol.ts's `ToRecorder`). (No `Debug`:
/// `UnlockKeys` carries a password.)
#[derive(Deserialize, strum::IntoStaticStr, strum::VariantNames)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum Command {
    /// The whole config (read as [`Config::from_value`] reads a file), edited
    /// from version `version`: refused when the config has changed since.
    SetConfig { config: Value, version: u64 },
    /// System `system`'s talkgroup file (Trunk Recorder's CSV; "" for none).
    SetTalkgroups { system: String, csv: String },
    Start,
    Stop,
    Devices,
    FindRadios,
    ListDir { #[serde(default)] path: String },
    ReadTrConfig { path: String },
    Listen { on: bool, #[serde(default)] system: Option<String>, #[serde(default)] talkgroup: Option<u32> },
    UnlockKeys { source: String, password: String },
    LockKeys { source: String },
    ChannelFile { index: usize, path: String },
    SurveyStart(Body),
    SurveyListen { freq_hz: f64 },
    SurveyRescan,
    SurveyStop,
    /// Who is licensed on `signals` within `radius_m` of the location the
    /// user gave (sent to trunkrecorder.pro): answered with `licenseResult`.
    LicenseQuery { id: u64, lat: f64, lon: f64, radius_m: f64, signals: Vec<crate::licenses::Signal> },
    /// Who holds these call signs (heard on the air; sent to
    /// trunkrecorder.pro, no location): answered with `callSignResult`.
    CallSignQuery { id: u64, call_signs: Vec<String> },
    ProfileSource(Body),
    Plugins,
    AddPlugin { path: String },
    RemovePlugin { id: String },
    Interfaces,
    /// A store's list; `refresh` fetches it again now.
    Store { kind: StoreKind, #[serde(default)] refresh: bool },
    /// Install (or update) from a store, or from a GitHub release.
    Install { kind: StoreKind, #[serde(flatten)] what: Install },
    AddInterface { path: String },
    RemoveInterface { id: String },
    Subscribe { topics: Vec<String> },
    StatsQuery(StatsQuery),
    OlderCalls { #[serde(default = "i64_max")] before: i64, #[serde(default = "hundred")] limit: usize },
    RadioQuery(Body),
    Quit,
}

fn i64_max() -> i64 {
    i64::MAX
}

fn hundred() -> usize {
    100
}

impl Command {
    /// Every command's `type`.
    pub const TYPES: &'static [&'static str] = <Self as strum::VariantNames>::VARIANTS;

    /// Its `type`.
    pub fn kind(&self) -> &'static str {
        self.into()
    }

    /// Read a command (JSON text). `Err`: what to say back.
    pub fn parse(text: &str) -> Result<Command, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("Not JSON: {e}"))?;
        let ty = v["type"].as_str().unwrap_or("").to_string();
        serde_json::from_value(v).map_err(|e| format!("Bad \"{ty}\" command: {e}"))
    }

    /// What a listen-only connection may send: looking, never changing.
    pub fn listen_may_send(&self) -> bool {
        matches!(self, Command::Subscribe { .. } | Command::StatsQuery(_) | Command::RadioQuery(_) | Command::OlderCalls { .. } | Command::Listen { .. })
    }
}

/// `statsQuery`: the history of `series` (names, or prefixes ending in `*`)
/// over `range` back from now, or `from`–`to` (Unix s), in about `points` points.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct StatsQuery {
    pub id: Value,
    pub series: Vec<String>,
    pub range: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub points: usize,
}

impl StatsQuery {
    /// Its span, (from, to) Unix s, at `now`.
    pub fn span(&self, now: i64) -> (i64, i64) {
        let back = match self.range.as_deref().unwrap_or("1h") {
            "10m" => 600,
            "6h" => 6 * 3600,
            "24h" => 86400,
            "7d" => 7 * 86400,
            _ => 3600,
        };
        let to = self.to.unwrap_or(now);
        (self.from.unwrap_or(to - back), to)
    }

    /// Its answer from `history` (`loading`: its files are still being read).
    pub fn answer(&self, history: &crate::stats::History, now: i64, loading: bool) -> ToClient {
        let (from, to) = self.span(now);
        let series: Vec<String> = self.series.iter().take(200).cloned().collect();
        let found = history.query(&series, from, to, self.points);
        ToClient::StatsResult(body(serde_json::json!({ "id": self.id, "from": from, "to": to, "loading": loading, "series": found })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_its_type_and_its_fields() {
        let m = ToClient::UnitAlias { system: "dcfd".into(), unit: 7, alias: "E1".into() };
        assert_eq!(m.to_json(), r#"{"type":"unitAlias","system":"dcfd","unit":7,"alias":"E1"}"#);
        assert_eq!(ToClient::Quit.to_json(), r#"{"type":"quit"}"#);
        let s = ToClient::Spectrum { source: 1, center_hz: 2.0, rate_hz: 3.0, bins: vec![] };
        assert_eq!(s.to_json(), r#"{"type":"spectrum","source":1,"centerHz":2.0,"rateHz":3.0,"bins":[]}"#);
        // A body's own "type" never doubles the message's.
        let d = ToClient::Dir(body(serde_json::json!({ "type": "dir", "path": "/" })));
        assert_eq!(d.to_json(), r#"{"type":"dir","path":"/"}"#);
        assert_eq!((d.kind(), d.audience()), ("dir", Audience::Everyone));
        assert_eq!(ToClient::Keys { sources: Value::Null }.audience(), Audience::Control);
    }

    #[test]
    fn commands_are_read_strictly_and_listen_may_only_look() {
        let c = Command::parse(r#"{"type":"listen","on":true,"system":"dcfd"}"#).unwrap();
        assert!(matches!(&c, Command::Listen { on: true, system: Some(s), talkgroup: None } if s == "dcfd") && c.listen_may_send());
        assert!(matches!(Command::parse(r#"{"type":"install","kind":"plugin","repository":"a/b"}"#), Ok(Command::Install { kind: StoreKind::Plugin, what: Install::Release { tag: None, .. } })));
        assert!(matches!(Command::parse(r#"{"type":"install","kind":"interface","id":"trunkamp"}"#), Ok(Command::Install { kind: StoreKind::Interface, what: Install::Listed { .. } })));
        let e = Command::parse(r#"{"type":"channelFile","index":0}"#).err().unwrap();
        assert!(e.contains("channelFile") && e.contains("path"), "{e}");
        assert!(Command::parse(r#"{"type":"nope"}"#).is_err());
        let c = Command::parse(r#"{"type":"setConfig","config":{},"version":1}"#).unwrap();
        assert!(!c.listen_may_send() && c.kind() == "setConfig");
        let q = match Command::parse(r#"{"type":"statsQuery","id":3,"series":["a"],"range":"10m"}"#).unwrap() {
            Command::StatsQuery(q) => q,
            _ => panic!(),
        };
        assert_eq!(q.span(1000), (400, 1000));
    }
}
