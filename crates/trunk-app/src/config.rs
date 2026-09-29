//! The app's configuration: which sources (dongles or capture files), which
//! system, recording rules and the web server. Stored as JSON in the platform
//! config folder; the browser interface reads and edits it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trunk_core::p25::diversity::BankConfig;
use trunk_core::trunk::{CallConfig, EngineConfig, SourceConfig};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    /// An RTL-SDR dongle. `serial` "" = the first free one; `center_hz` 0 = auto.
    #[serde(rename_all = "camelCase")]
    Rtlsdr { serial: String, center_hz: f64, rate_hz: f64, gain_db: Option<f32>, ppm: i32 },
    /// An rtl_sdr capture (unsigned 8-bit IQ) on this machine.
    #[serde(rename_all = "camelCase")]
    File { path: String, center_hz: f64, rate_hz: f64, realtime: bool },
}

impl Source {
    pub fn center_hz(&self) -> f64 {
        match self {
            Source::Rtlsdr { center_hz, .. } | Source::File { center_hz, .. } => *center_hz,
        }
    }
    pub fn rate_hz(&self) -> f64 {
        match self {
            Source::Rtlsdr { rate_hz, .. } | Source::File { rate_hz, .. } => *rate_hz,
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
    pub system: System,
    pub recording: Recording,
    pub server: Server,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            sources: vec![Source::Rtlsdr { serial: String::new(), center_hz: 0.0, rate_hz: 2_400_000.0, gain_db: Some(38.6), ppm: 0 }],
            system: System::default(),
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
    pub fn load(path: &Path) -> Config {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// Each source's centre: as set, or (0 = auto) placed over the control
    /// channels for the first source.
    pub fn resolved_centers(&self) -> Vec<f64> {
        self.sources
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if s.center_hz() > 0.0 || i > 0 {
                    s.center_hz()
                } else {
                    auto_center(&self.system.control_channels, s.rate_hz()).unwrap_or(0.0)
                }
            })
            .collect()
    }

    /// Why this config can't start, or None.
    pub fn problem(&self) -> Option<String> {
        if self.sources.is_empty() {
            return Some("Add a source (a dongle or a capture file).".into());
        }
        if self.system.control_channels.is_empty() {
            return Some("Add at least one control channel.".into());
        }
        let centers = self.resolved_centers();
        if centers.iter().any(|&c| c <= 0.0) {
            return Some("Set a center frequency for every source (the first can be automatic when the control channels fit one dongle).".into());
        }
        let covered = self.system.control_channels.iter().any(|&f| {
            self.sources.iter().zip(&centers).any(|(s, &c)| (f - c).abs() <= usable_half_width(s.rate_hz()))
        });
        if !covered {
            return Some("No control channel falls inside any source's bandwidth — move a center frequency.".into());
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
