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

use trunk_core::trunk::Origin;
use serde::{Deserialize, Serialize};
pub use crate::calls::{CallsPage, CallsQuery, StoredCall};
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

/// Where a recording run is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Running,
    /// Its sources are closed; the calls in flight are being saved.
    Stopping,
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
    History {
        entries: Vec<StoredCall>,
        /// Where older calls go on from (a `callsQuery` `cursor`), if there are any.
        #[serde(skip_serializing_if = "Option::is_none")]
        next: Option<String>,
    },
    /// A system's talker aliases as saved (Trunk Recorder's unitTagsOTA CSV):
    /// one for each system with some, on connecting (then `unitAlias` for each new one).
    Units { system: String, csv: String },
    /// Which key containers are unlocked.
    Keys { sources: Value },
    Radios { radios: Value },
    /// The recording's phase; `restart_needed`: the config was edited, since
    /// Start, in what a run reads only when it starts ([`crate::config::Config::needs_restart`]).
    State { phase: Phase, error: Option<String>, ended: bool, restart_needed: bool },
    /// The config, and its version: the number an edit of it is sent with.
    Config { config: Box<Config>, version: u64 },
    /// Answers setConfig: the config's version now; `error` when the edit
    /// wasn't taken (it was of an older version, or the config was bad) or
    /// wasn't saved.
    ConfigSaved { version: u64, #[serde(skip_serializing_if = "Option::is_none")] error: Option<String> },
    /// A system's talkgroups, in their order (none: it has none): one for
    /// each on connecting, and again whenever a system's are set.
    Talkgroups { system: String, talkgroups: Vec<trunk_core::trunk::TalkgroupRow> },
    /// One talkgroup of system `system` edited ([`Command::SetTalkgroup`]):
    /// each interface makes the same edit to its copy (`talkgroupsEdit`),
    /// with the origin it was given.
    TalkgroupEdited { system: String, talkgroup: u32, edit: TalkgroupEdit, origin: Origin },
    /// A system's own names for its radios, in their order (none: it has
    /// none): one for each on connecting, and again whenever a system's are set.
    UnitNames { system: String, unit_names: Vec<trunk_core::trunk::UnitNameRow> },
    /// Radio `unit` of system `system` named `name` ([`Command::SetUnitName`]):
    /// each interface makes the same edit to its copy (`unitNamesEdit`).
    UnitNameEdited { system: String, unit: u32, name: String, origin: Origin },
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
    /// How a plugin's button went: the answer to `pluginAction` `id`, to the connection that asked.
    PluginActionResult { id: u64, ok: bool, #[serde(skip_serializing_if = "String::is_empty")] message: String },
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
    /// Answers `callsQuery`: a page of calls ([`CallsPage`]: its `error` when
    /// the call store couldn't answer, the page then the recent calls alone).
    CallsResult {
        id: Value,
        #[serde(flatten)]
        page: CallsPage,
    },
    /// Answers `dataSummary`: what the store keeps of each system it has,
    /// the config's and those gone from it (or why it can't say).
    DataSummary { id: u64, systems: Vec<DataSystem>, #[serde(skip_serializing_if = "Option::is_none")] error: Option<String> },
    /// Answers `iqCaptures`, `iqCaptureStart` and `iqCaptureDelete`: the
    /// captures on disk, newest first, in `root`; the one being made;
    /// whether the radios are running; why the last request or capture failed.
    IqCaptures {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        active: Option<IqCaptureActive>,
        captures: Vec<IqCapture>,
        root: String,
        recording: bool,
        /// The rules shipped, each off until the config has it.
        presets: Vec<crate::config::CaptureRule>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Answers `iqRuleCheck`: why the rule can't run, if so.
    IqRuleChecked {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Answers `deleteData`: how much went (or why not all of it did).
    DataDeleted { id: u64, deleted: DataCounts, #[serde(skip_serializing_if = "Option::is_none")] error: Option<String> },
    /// Answers `compactData`: the store's size, bytes, before and after (or why it couldn't).
    DataCompacted { id: u64, before_bytes: i64, after_bytes: i64, #[serde(skip_serializing_if = "Option::is_none")] error: Option<String> },
    Quit,
}

/// A table of the store's, as Setup → Data exports it (a CSV file each:
/// `GET /api/v1/export/{table}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, strum::VariantArray, strum::IntoStaticStr, strum::EnumString)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum DataTable {
    /// The call index: a row a call.
    Calls,
    /// Each radio's transmissions in each call.
    Transmissions,
    /// The talkgroups each system was given (its talkgroup file).
    Talkgroups,
    /// The names each system was given for its radios (its unit names file).
    UnitNames,
    /// Talker aliases heard over the air.
    TalkerAliases,
    /// The radio registry's radios: every radio heard.
    Radios,
    /// The radio registry's talkgroups: every talkgroup heard.
    HeardTalkgroups,
    /// The dashboard's history, by the hour.
    DashboardHistory,
}

impl DataTable {
    pub const ALL: &'static [DataTable] = <Self as strum::VariantArray>::VARIANTS;

    /// Its name, as the route and the file name have it.
    pub fn name(self) -> &'static str {
        self.into()
    }
}

/// A kind of the store's data, as Setup → Data deletes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, strum::VariantArray)]
#[serde(rename_all = "camelCase")]
pub enum DataKind {
    /// The call index and each call's transmissions (the recordings stay).
    Calls,
    Talkgroups,
    /// Each system's own names for its radios.
    UnitNames,
    TalkerAliases,
    /// The radio registry: the radios, talkgroups, pairs and frequencies heard.
    Registry,
    DashboardHistory,
}

impl DataKind {
    pub const ALL: &'static [DataKind] = <Self as strum::VariantArray>::VARIANTS;
}

/// How many of each the store has (`dataSummary`) or deleted (`dataDeleted`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataCounts {
    pub calls: i64,
    pub talkgroups: i64,
    pub unit_names: i64,
    pub talker_aliases: i64,
    /// The radio registry's radios and talkgroups.
    pub radios: i64,
    pub heard_talkgroups: i64,
    /// The dashboard's series.
    pub history_series: i64,
}

/// A recording of what the radios received (`iqCaptures`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IqCapture {
    /// Its folder's name.
    pub name: String,
    pub path: String,
    /// `wideband`, `control` or `voice`.
    pub tier: String,
    /// `healthy` (made by hand) or `rule`.
    pub purpose: String,
    /// The rule that made it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub rule: String,
    pub seconds: u32,
    pub radios: u32,
    pub bytes: u64,
    pub note: String,
    /// Samples the radios lost during it.
    pub lost_samples: u64,
    /// Control channel messages decoded and failed, and calls ended, while it was made.
    pub messages_good: u64,
    pub messages_bad: u64,
    pub calls: u64,
}

/// A capture being made (`iqCaptures`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IqCaptureActive {
    pub name: String,
    pub seconds: u32,
    pub elapsed_s: f64,
}

/// A system the store keeps data of (`dataSummary`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataSystem {
    /// Its id (the config's `id`).
    pub id: u32,
    pub short_name: String,
    /// It's in the config (else it was removed and its data stayed).
    pub in_config: bool,
    #[serde(flatten)]
    pub counts: DataCounts,
}

/// Something a recorder can do beyond recording (`hello.capabilities`). The
/// desktop app can do all of them (`calls` while its call store is open);
/// the browser build, none.
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
    /// The call store: `callsQuery`.
    Calls,
    /// The dashboard's history further back than a week (`statsQuery`'s
    /// `30d`, `90d`, `1y`), as far as `storage.statsDays` keeps: the store.
    History,
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

/// What an edit of one talkgroup sets (what's left out stays as it is).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TalkgroupEdit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Never record it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<bool>,
}

impl TalkgroupEdit {
    /// The cells it sets, by the talkgroup file's column names ([`trunk_core::trunk::edit_rows`]).
    pub fn cells(&self) -> Vec<(&'static str, &str)> {
        let text = [("Alpha Tag", &self.alpha_tag), ("Description", &self.description), ("Tag", &self.tag), ("Category", &self.category)];
        let mut cells: Vec<(&'static str, &str)> = text.into_iter().filter_map(|(c, v)| v.as_deref().map(|v| (c, v.trim()))).collect();
        cells.extend(self.ignore.map(|i| ("Ignore", if i { "x" } else { "" })));
        cells
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
    /// System `system`'s talkgroups, in their order (none: it has none). A
    /// talkgroup file is read into them, and written from them, by the
    /// recorder's own code (the page's `talkgroupsFromCsv` / `talkgroupsToCsv`).
    /// `from`: where they came from, the origin of the rows that change
    /// (`at` is the recorder's); none: the rows keep their own (moved).
    SetTalkgroups { system: String, talkgroups: Vec<trunk_core::trunk::TalkgroupRow>, #[serde(default)] from: Option<Origin> },
    /// System `system`'s own names for its radios, as [`Command::SetTalkgroups`].
    SetUnitNames { system: String, unit_names: Vec<trunk_core::trunk::UnitNameRow>, #[serde(default)] from: Option<Origin> },
    /// Radio `unit` of system `system` named `name` (blank: its own name
    /// taken out). Answered to every interface with `unitNameEdited`.
    SetUnitName { system: String, unit: u32, name: String },
    /// One talkgroup of system `system`: only the cells `edit` sets change
    /// (a row added when there's none). Answered to every interface with
    /// `talkgroupEdited`.
    SetTalkgroup { system: String, talkgroup: u32, edit: TalkgroupEdit },
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
    RadioQuery(Body),
    /// Recorded calls ([`CallsQuery`]), answered with `callsResult` `id`.
    CallsQuery {
        #[serde(default)]
        id: Value,
        #[serde(flatten)]
        query: CallsQuery,
    },
    /// Press plugin `plugin`'s button `action` on `value` (the settings
    /// object it's on, as edited): answered with `pluginActionResult` `id`
    /// once the plugin says how it went.
    PluginAction { id: u64, plugin: String, action: String, #[serde(default)] value: Value },
    /// The radios' captures on disk: answered with `iqCaptures` `id`.
    IqCaptures { id: u64 },
    /// Record every radio for `seconds` of a recording that's working
    /// well, to files (nothing is sent anywhere): answered with `iqCaptures` `id`.
    IqCaptureStart { id: u64, seconds: u32, #[serde(default)] note: String },
    /// Whether capture rule `rule` could run: answered with `iqRuleChecked` `id`.
    IqRuleCheck { id: u64, rule: Value },
    /// Delete capture `name`: answered with `iqCaptures` `id`.
    IqCaptureDelete { id: u64, name: String },
    /// What the store keeps of each system: answered with `dataSummary` `id`.
    DataSummary { id: u64 },
    /// Delete `kinds` of system `system`'s data (its id; none: every
    /// system's) from the store, and from what the recorder holds of it:
    /// answered with `dataDeleted` `id`.
    DeleteData { id: u64, kinds: Vec<DataKind>, #[serde(default)] system: Option<u32> },
    /// Give the space deleted data left in the store back to the disk:
    /// answered with `dataCompacted` `id`.
    CompactData { id: u64 },
    Quit,
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
        matches!(self, Command::Subscribe { .. } | Command::StatsQuery(_) | Command::RadioQuery(_) | Command::CallsQuery { .. } | Command::Listen { .. })
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
            "30d" => 30 * 86400,
            "90d" => 90 * 86400,
            "1y" => 365 * 86400,
            _ => 3600,
        };
        let to = self.to.unwrap_or(now);
        (self.from.unwrap_or(to - back), to)
    }

    /// The series patterns it asks for (at most 200).
    pub fn patterns(&self) -> Vec<String> {
        self.series.iter().take(200).cloned().collect()
    }

    /// Its answer from `history` (`loading`: its files are still being read).
    pub fn answer(&self, history: &crate::stats::History, now: i64, loading: bool) -> ToClient {
        let (from, to) = self.span(now);
        self.result(from, to, loading, history.query(&self.patterns(), from, to, self.points))
    }

    /// Its answer: `series` ([`crate::stats::Span::answer`]) from `from` to `to`.
    pub fn result(&self, from: i64, to: i64, loading: bool, series: serde_json::Map<String, Value>) -> ToClient {
        ToClient::StatsResult(body(serde_json::json!({ "id": self.id, "from": from, "to": to, "loading": loading, "series": series })))
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
