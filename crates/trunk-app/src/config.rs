//! The app's configuration: which sources (RTL-SDRs, USRPs, Airspys or capture files), which
//! trunked system and/or conventional channels, recording rules and the web server. Stored as JSON in the platform
//! config folder; the browser interface reads and edits it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trunk_core::p25::diversity::BankConfig;
use trunk_core::trunk::{CallConfig, ConvChannel, ConvConfig, ConvMode, EngineConfig, SourceConfig, Talkgroup};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    /// An RTL-SDR dongle. `serial` "" = the first free one; `center_hz` 0 = auto.
    #[serde(rename_all = "camelCase")]
    Rtlsdr { serial: String, center_hz: f64, rate_hz: f64, gain_db: Option<f32>, ppm: i32 },
    /// A USRP through UHD (installed separately; loaded at run time).
    /// `args`: UHD device arguments, "" = the first found ("serial=…",
    /// "type=b200", "addr=192.168.10.2"). `antenna` "" = the device's default.
    #[serde(rename_all = "camelCase")]
    Usrp {
        #[serde(default)]
        args: String,
        center_hz: f64,
        rate_hz: f64,
        #[serde(default)]
        gain_db: f64,
        #[serde(default)]
        antenna: String,
        #[serde(default)]
        ppm: f64,
    },
    /// An Airspy R2 / Mini through libairspy (installed separately; loaded
    /// at run time). `serial` hex, "" = the first; `gain` the linearity gain
    /// step 0..21.
    #[serde(rename_all = "camelCase")]
    Airspy {
        #[serde(default)]
        serial: String,
        center_hz: f64,
        rate_hz: f64,
        #[serde(default = "airspy_gain")]
        gain: u8,
        #[serde(default)]
        bias_tee: bool,
        #[serde(default)]
        ppm: f64,
    },
    /// A capture on this machine: `format` "cu8" (rtl_sdr), "cs16" or "cf32"
    /// (GNU Radio / UHD complex float).
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
        center_hz: f64,
        rate_hz: f64,
        realtime: bool,
        #[serde(default)]
        format: SampleFormat,
    },
}

fn airspy_gain() -> u8 {
    14
}

/// Sample format of a capture file.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SampleFormat {
    /// Unsigned 8-bit I/Q (rtl_sdr).
    #[default]
    Cu8,
    /// Signed 16-bit I/Q, little-endian.
    Cs16,
    /// 32-bit float I/Q, little-endian (GNU Radio "complex", UHD fc32).
    Cf32,
}

impl SampleFormat {
    /// From a file name's extension (.cf32 / .cfile / .fc32 / .raw → cf32, .cs16 / .sc16 → cs16, else cu8).
    pub fn from_path(path: &str) -> Self {
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "cf32" | "cfile" | "fc32" | "complex" => SampleFormat::Cf32,
            "cs16" | "sc16" => SampleFormat::Cs16,
            _ => SampleFormat::Cu8,
        }
    }
    pub fn bytes_per_sample(self) -> usize {
        match self {
            SampleFormat::Cu8 => 2,
            SampleFormat::Cs16 => 4,
            SampleFormat::Cf32 => 8,
        }
    }
}

impl Source {
    pub fn center_hz(&self) -> f64 {
        match self {
            Source::Rtlsdr { center_hz, .. } | Source::Usrp { center_hz, .. } | Source::Airspy { center_hz, .. } | Source::File { center_hz, .. } => *center_hz,
        }
    }
    pub fn rate_hz(&self) -> f64 {
        match self {
            Source::Rtlsdr { rate_hz, .. } | Source::Usrp { rate_hz, .. } | Source::Airspy { rate_hz, .. } | Source::File { rate_hz, .. } => *rate_hz,
        }
    }
    /// For the interface: "RTL-SDR SN 200", "USRP serial=…", "file x.cu8".
    pub fn label(&self) -> String {
        match self {
            Source::Rtlsdr { serial, .. } => format!("RTL-SDR {}", if serial.is_empty() { "(first)".into() } else { format!("SN {serial}") }),
            Source::Usrp { args, .. } => format!("USRP {}", if args.is_empty() { "(first)" } else { args }),
            Source::Airspy { serial, .. } => format!("Airspy {}", if serial.is_empty() { "(first)".into() } else { format!("SN {serial}") }),
            Source::File { path, .. } => format!("file {}", path.rsplit(['/', '\\']).next().unwrap_or(path)),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct System {
    pub short_name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub control_channels: Vec<f64>,
    /// "auto" | "fsk4" | "qpsk"
    pub modulation: String,
    pub talkgroups_csv: String,
    pub talkgroups_name: String,
    /// SmartNet (`type` "smartnet"): the band plan, as Trunk Recorder names
    /// it — "800_standard", "800_reband", "800_splinter", "900", or
    /// "400_custom" with the four numbers below (Hz; offset is a channel number).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bandplan: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub bandplan_base: f64,
    #[serde(skip_serializing_if = "is_zero")]
    pub bandplan_spacing: f64,
    #[serde(skip_serializing_if = "is_zero_u16")]
    pub bandplan_offset: u16,
    #[serde(skip_serializing_if = "is_zero")]
    pub bandplan_high: f64,
    /// SmartNet: voice mode of a talkgroup never heard granted — "digital" (P25) or "analog".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub default_mode: String,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}
fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

impl System {
    pub fn is_smartnet(&self) -> bool {
        self.kind.eq_ignore_ascii_case("smartnet")
    }

    /// The SmartNet settings, or why they don't work.
    pub fn smartnet(&self) -> Result<trunk_core::trunk::SmartnetConfig, String> {
        let bandplan = trunk_core::smartnet::Bandplan::from_config(&self.bandplan, self.bandplan_base, self.bandplan_spacing, self.bandplan_offset, self.bandplan_high)?;
        Ok(trunk_core::trunk::SmartnetConfig { bandplan, analog_default: self.default_mode.eq_ignore_ascii_case("analog") })
    }
}

impl Default for System {
    fn default() -> Self {
        System {
            short_name: "sys1".into(),
            kind: "p25".into(),
            control_channels: vec![],
            modulation: "auto".into(),
            talkgroups_csv: String::new(),
            talkgroups_name: String::new(),
            bandplan: String::new(),
            bandplan_base: 0.0,
            bandplan_spacing: 0.0,
            bandplan_offset: 0,
            bandplan_high: 0.0,
            default_mode: String::new(),
        }
    }
}

/// Conventional channels: one frequency each, found by energy detection.
/// Either listed here, or kept in a CSV file (`channelFile`, desktop) to edit
/// in a spreadsheet — see [`crate::channels`] for its columns.
///
/// ```json
/// "conventional": {
///   "squelchDb": 8,
///   "channels": [
///     { "freqHz": 154430000, "mode": "fm", "name": "County Fire Dispatch", "talkgroup": 1001 },
///     { "freqHz": 460125000, "mode": "p25", "name": "PD Tac 2", "squelchDb": 12 }
///   ]
/// }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Conventional {
    /// Open threshold for every channel, dB above the measured noise floor.
    pub squelch_db: f64,
    /// A CSV the channels are read from, absolute or relative to the config
    /// file's folder; read when the app starts, when recording starts and
    /// on Reload. Empty: the channels are the list below.
    pub channel_file: String,
    /// The channels (while a channel file is linked: its contents, not saved here).
    pub channels: Vec<Channel>,
    /// How the channel file last read, for the interface (not saved).
    #[serde(skip_deserializing)]
    pub channel_file_status: String,
}

impl Default for Conventional {
    fn default() -> Self {
        Conventional { squelch_db: ConvConfig::default().squelch_db, channel_file: String::new(), channels: vec![], channel_file_status: String::new() }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChannelMode {
    /// Analog narrowband FM.
    #[default]
    Fm,
    /// P25 Phase 1.
    P25,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub freq_hz: f64,
    #[serde(default)]
    pub mode: ChannelMode,
    /// Short name (Trunk Recorder's alpha tag).
    #[serde(default)]
    pub name: String,
    /// Talkgroup number the calls are filed under; default the frequency in
    /// kHz. P25 files calls under the talkgroup the air names, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talkgroup: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// This channel's open threshold, dB above the noise floor (else the section's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub squelch_db: Option<f64>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl Channel {
    pub fn talkgroup(&self) -> u32 {
        self.talkgroup.unwrap_or_else(|| ConvChannel::default_talkgroup(self.freq_hz))
    }

    fn engine_channel(&self) -> ConvChannel {
        let tg = self.talkgroup();
        let named = !(self.name.is_empty() && self.description.is_empty() && self.tag.is_empty() && self.group.is_empty());
        ConvChannel {
            freq_hz: self.freq_hz,
            mode: match self.mode {
                ChannelMode::Fm => ConvMode::Fm,
                ChannelMode::P25 => ConvMode::P25,
            },
            talkgroup: tg,
            info: named.then(|| Talkgroup {
                number: tg,
                mode: if self.mode == ChannelMode::Fm { "A".into() } else { "D".into() },
                alpha_tag: self.name.clone(),
                description: self.description.clone(),
                tag: self.tag.clone(),
                group: self.group.clone(),
                priority: 1,
                preferred_nac: 0,
            }),
            squelch_db: self.squelch_db,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Recording {
    pub capture_dir: String,
    pub preroll_s: f64,
    pub max_recorders: usize,
    pub call_timeout_s: f64,
    pub record_unknown: bool,
    pub record_encrypted: bool,
    pub record_unit_to_unit: bool,
    pub keep_silent_calls: bool,
    /// Save each call's vocoder frames next to its audio, for diagnosis.
    pub capture_frames: bool,
}

impl Default for Recording {
    fn default() -> Self {
        Recording {
            capture_dir: default_capture_dir().display().to_string(),
            preroll_s: 1.0,
            max_recorders: 32,
            call_timeout_s: 3.0,
            record_unknown: true,
            record_encrypted: false,
            record_unit_to_unit: true,
            keep_silent_calls: false,
            capture_frames: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Server {
    pub bind: String,
    pub port: u16,
    /// Start recording when the app starts (headless machines, after a reboot).
    pub auto_start: bool,
}

impl Default for Server {
    fn default() -> Self {
        Server { bind: "127.0.0.1".into(), port: 8080, auto_start: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub sources: Vec<Source>,
    /// The trunked system (none when it has no control channels).
    pub system: System,
    pub conventional: Conventional,
    pub recording: Recording,
    pub server: Server,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            sources: vec![Source::Rtlsdr { serial: String::new(), center_hz: 0.0, rate_hz: 2_400_000.0, gain_db: Some(38.6), ppm: 0 }],
            system: System::default(),
            conventional: Conventional::default(),
            recording: Recording::default(),
            server: Server::default(),
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// `~/Library/Application Support/trunk-lite`, `%APPDATA%\trunk-lite`, or
/// `$XDG_CONFIG_HOME/trunk-lite` (`~/.config/trunk-lite`).
pub fn config_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/trunk-lite")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join("trunk-lite")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("trunk-lite")
    }
}

pub fn default_capture_dir() -> PathBuf {
    home().join("TrunkRecorderLite")
}

impl Config {
    /// The config at `path` (defaults if missing or unreadable), with a
    /// linked channel file read in.
    pub fn load(path: &Path) -> Config {
        let mut c: Config = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let _ = c.load_channel_file(path);
        c
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut c = self.clone();
        c.conventional.channel_file_status.clear();
        if !c.conventional.channel_file.is_empty() {
            // The file holds them.
            c.conventional.channels.clear();
        }
        std::fs::write(path, serde_json::to_string_pretty(&c).unwrap_or_default())
    }

    /// The linked channel file's location (relative paths from the config
    /// file's folder), or None.
    pub fn channel_file_path(&self, config_path: &Path) -> Option<PathBuf> {
        let f = self.conventional.channel_file.trim();
        if f.is_empty() {
            return None;
        }
        let p = PathBuf::from(f);
        Some(if p.is_absolute() { p } else { config_path.parent().unwrap_or(Path::new(".")).join(p) })
    }

    /// Read the linked channel file into `conventional.channels` (a no-op
    /// when none is linked). On an error the channels are left as they were;
    /// either way `channel_file_status` says what happened.
    pub fn load_channel_file(&mut self, config_path: &Path) -> Result<(), String> {
        let Some(p) = self.channel_file_path(config_path) else {
            self.conventional.channel_file_status.clear();
            return Ok(());
        };
        let r = std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| crate::channels::parse(&t));
        match r {
            Ok(parsed) => {
                let n = parsed.channels.len();
                self.conventional.channels = parsed.channels;
                self.conventional.channel_file_status =
                    format!("{n} channel{} read.{}", if n == 1 { "" } else { "s" }, parsed.notes.iter().map(|x| format!(" {x}")).collect::<String>());
                Ok(())
            }
            Err(e) => {
                let msg = format!("Channel file {}: {e}", p.display());
                self.conventional.channel_file_status = msg.clone();
                Err(msg)
            }
        }
    }

    /// Link the channels to a CSV at `path` (created from the current list
    /// if it doesn't exist yet), or unlink with "" — the channels then stay
    /// in the config, as last read.
    pub fn link_channel_file(&mut self, config_path: &Path, path: &str) -> Result<(), String> {
        let old = std::mem::replace(&mut self.conventional.channel_file, path.trim().to_string());
        let Some(p) = self.channel_file_path(config_path) else {
            self.conventional.channel_file_status.clear();
            return Ok(());
        };
        if !p.exists() {
            let made = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&p, crate::channels::write(&self.conventional.channels)));
            if let Err(e) = made {
                self.conventional.channel_file = old;
                return Err(format!("Couldn't create {}: {e}", p.display()));
            }
        }
        if let Err(e) = self.load_channel_file(config_path) {
            self.conventional.channel_file = old;
            return Err(e);
        }
        Ok(())
    }

    /// The conventional channels that are switched on.
    pub fn enabled_channels(&self) -> impl Iterator<Item = &Channel> {
        self.conventional.channels.iter().filter(|c| c.enabled)
    }

    /// Each source's centre: as set, or (0 = auto) placed for the first
    /// source over the control channels and conventional channels — or the
    /// control channels alone, if everything doesn't fit.
    pub fn resolved_centers(&self) -> Vec<f64> {
        let ccs = &self.system.control_channels;
        let all: Vec<f64> = ccs.iter().copied().chain(self.enabled_channels().map(|c| c.freq_hz)).collect();
        self.sources
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if s.center_hz() > 0.0 || i > 0 {
                    s.center_hz()
                } else {
                    auto_center(&all, s.rate_hz()).or_else(|| auto_center(ccs, s.rate_hz())).unwrap_or(0.0)
                }
            })
            .collect()
    }

    /// Why this config can't start, or None.
    pub fn problem(&self) -> Option<String> {
        if self.sources.is_empty() {
            return Some("Add a source (a dongle or a capture file).".into());
        }
        let trunked = !self.system.control_channels.is_empty();
        if !trunked && self.enabled_channels().next().is_none() {
            return Some("Add a control channel (trunked system) or a conventional channel.".into());
        }
        let centers = self.resolved_centers();
        if centers.iter().any(|&c| c <= 0.0) {
            return Some("Set a center frequency for every source (the first can be automatic when the channels fit one source).".into());
        }
        let inside = |f: f64| self.sources.iter().zip(&centers).any(|(s, &c)| (f - c).abs() <= usable_half_width(s.rate_hz()));
        if trunked && self.system.is_smartnet() {
            if let Err(e) = self.system.smartnet() {
                return Some(e);
            }
        }
        if trunked && !self.system.control_channels.iter().any(|&f| inside(f)) {
            return Some("No control channel falls inside any source's bandwidth — move a center frequency.".into());
        }
        if self.enabled_channels().any(|c| c.freq_hz <= 0.0) {
            return Some("A conventional channel has no frequency yet.".into());
        }
        let outside: Vec<String> = self.enabled_channels().filter(|c| !inside(c.freq_hz)).map(|c| format!("{:.5}", c.freq_hz / 1e6)).collect();
        if !outside.is_empty() {
            return Some(format!("Conventional channel(s) outside every source's bandwidth: {} MHz — move a center frequency or disable them.", outside.join(", ")));
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(d) = self.enabled_channels().find(|c| !seen.insert(c.freq_hz.round() as u64)) {
            return Some(format!("Conventional channel {:.5} MHz is listed twice.", d.freq_hz / 1e6));
        }
        None
    }

    pub fn engine_config(&self, epoch_ms: f64) -> EngineConfig {
        let centers = self.resolved_centers();
        let m = self.system.modulation.as_str();
        EngineConfig {
            short_name: self.system.short_name.clone(),
            control_channels: self.system.control_channels.clone(),
            sources: self.sources.iter().zip(centers).map(|(s, c)| SourceConfig { center_hz: c, rate_hz: s.rate_hz() }).collect(),
            preroll_s: self.recording.preroll_s,
            max_recorders: self.recording.max_recorders,
            keep_silent_calls: self.recording.keep_silent_calls,
            calls: CallConfig {
                call_timeout_s: self.recording.call_timeout_s,
                record_unknown: self.recording.record_unknown,
                record_encrypted: self.recording.record_encrypted,
                record_unit_to_unit: self.recording.record_unit_to_unit,
                new_call_from_update: true,
            },
            epoch_ms_at_zero: epoch_ms,
            bank: BankConfig { cqpsk: m != "fsk4", cqpsk_eq: m != "fsk4", c4fm: m != "qpsk", ..Default::default() },
            conventional: self.enabled_channels().map(Channel::engine_channel).collect(),
            conv: ConvConfig { squelch_db: self.conventional.squelch_db, ..Default::default() },
            capture_frames: self.recording.capture_frames,
            smartnet: if self.system.is_smartnet() { self.system.smartnet().ok() } else { None },
        }
    }
}

/// Usable half-width of a source (the edges are filter roll-off).
pub fn usable_half_width(rate_hz: f64) -> f64 {
    rate_hz / 2.0 * 0.9
}

/// A centre that puts every control channel inside one source (and off the
/// DC spike), or None if they span too much.
pub fn auto_center(ccs: &[f64], rate_hz: f64) -> Option<f64> {
    if ccs.is_empty() {
        return None;
    }
    let lo = ccs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = ccs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let half = usable_half_width(rate_hz);
    if hi - lo > 2.0 * half - 50_000.0 {
        return None;
    }
    let mut c = ((lo + hi) / 2.0 / 1000.0).round() * 1000.0;
    let mut step = 0;
    while step < 40 && ccs.iter().any(|f| (f - c).abs() < 25_000.0) {
        c += if step % 2 == 1 { -1.0 } else { 1.0 } * 12_500.0 * (step + 1) as f64;
        step += 1;
    }
    ccs.iter().all(|f| (f - c).abs() <= half - 10_000.0).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventional_only_config() {
        let mut c: Config = serde_json::from_str(
            r#"{
                "sources": [{ "kind": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "gainDb": null, "ppm": 0 }],
                "conventional": { "channels": [
                    { "freqHz": 154430000, "mode": "fm", "name": "County Fire Dispatch", "talkgroup": 1001 },
                    { "freqHz": 154100000, "mode": "p25", "squelchDb": 12 },
                    { "freqHz": 453000000, "enabled": false }
                ] }
            }"#,
        )
        .unwrap();
        assert_eq!(c.conventional.squelch_db, 8.0);
        assert_eq!(c.conventional.channels[2].mode, ChannelMode::Fm);
        // No control channels: the centre is placed over the enabled channels.
        assert_eq!(c.problem(), None);
        let center = c.resolved_centers()[0];
        assert!((center - 154_265_000.0).abs() < 100_000.0, "center {center}");
        let e = c.engine_config(0.0);
        assert!(e.control_channels.is_empty());
        assert_eq!(e.conventional.len(), 2);
        assert_eq!(e.conventional[0].talkgroup, 1001);
        assert_eq!(e.conventional[0].info.as_ref().unwrap().alpha_tag, "County Fire Dispatch");
        assert_eq!(e.conventional[1].talkgroup, 154100);
        assert_eq!(e.conventional[1].squelch_db, Some(12.0));
        assert!(e.conventional[1].info.is_none());
        // Enabling the far channel no longer fits one dongle.
        c.conventional.channels[2].enabled = true;
        assert!(c.problem().unwrap().contains("center frequency"));
        c.conventional.channels.clear();
        assert!(c.problem().unwrap().contains("conventional channel"));
    }

    #[test]
    fn channel_file_link_edit_save_load_unlink() {
        let dir = std::env::temp_dir().join(format!("trunk-lite-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg_path = dir.join("config.json");
        let mut c = Config::default();
        c.conventional.channels = vec![Channel {
            freq_hz: 154_430_000.0,
            mode: ChannelMode::Fm,
            name: "Fire".into(),
            talkgroup: None,
            description: String::new(),
            tag: String::new(),
            group: String::new(),
            squelch_db: None,
            enabled: true,
        }];
        // Linking a new path writes the current list there.
        c.link_channel_file(&cfg_path, "channels.csv").unwrap();
        let file = dir.join("channels.csv");
        assert!(std::fs::read_to_string(&file).unwrap().contains("154.4300,fm,Fire"));
        // Edited in a spreadsheet: re-read.
        std::fs::write(&file, "Frequency,Mode,Alpha Tag
154.4300,fm,Fire
460.125,p25,PD
").unwrap();
        c.load_channel_file(&cfg_path).unwrap();
        assert_eq!(c.conventional.channels.len(), 2);
        assert_eq!(c.conventional.channel_file_status, "2 channels read.");
        // Saved without the list; loading reads the file again.
        c.save(&cfg_path).unwrap();
        let saved = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(saved.contains("\"channelFile\": \"channels.csv\"") && saved.contains("\"channels\": []"), "{saved}");
        assert_eq!(Config::load(&cfg_path).conventional.channels.len(), 2);
        // A broken file keeps the last good list and says why.
        std::fs::write(&file, "Name\nx\n").unwrap();
        assert!(c.load_channel_file(&cfg_path).unwrap_err().contains("No Frequency column"));
        assert_eq!(c.conventional.channels.len(), 2);
        // Unlinking keeps the channels in the config.
        c.link_channel_file(&cfg_path, "").unwrap();
        c.save(&cfg_path).unwrap();
        assert_eq!(Config::load(&cfg_path).conventional.channels.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
