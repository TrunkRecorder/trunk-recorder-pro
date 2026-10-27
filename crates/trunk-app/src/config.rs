//! The app's configuration: which sources (RTL-SDRs, USRPs, Airspys, bladeRFs, SoapySDR devices or capture files), which
//! trunked systems (sites) and/or conventional channels, recording rules and the web server. Stored as JSON in the platform
//! config folder; the browser interface reads and edits it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trunk_core::dsp::inband::pattern::PatternKind;
use trunk_core::dsp::AnalogTuning;
use trunk_core::p25::diversity::BankConfig;
use trunk_core::trunk::{
    check_channels, Access, parse_csv, CallConfig, Protocol, ConvChannel, Talkgroups, ConvConfig, ConvMode, EngineConfig, IdField, Identity, SaveRules, SourceConfig,
    SiteConfig, SystemConfig, Talkgroup, UnitTags, UnitTagsMode,
};
pub use trunk_core::trunk::{usable_half_width, DEFAULT_GUARD_HZ};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Source {
    /// An RTL-SDR dongle. `serial` "" = the first free one; `center_hz` 0 =
    /// auto. `agc`: the tuner's AGC instead of `gain_db`.
    #[serde(rename_all = "camelCase")]
    Rtlsdr {
        serial: String,
        center_hz: f64,
        rate_hz: f64,
        #[serde(default = "rtl_gain")]
        gain_db: f64,
        #[serde(default)]
        agc: bool,
        ppm: i32,
        #[serde(default)]
        auto_tune: bool,
        /// Left unused at each edge of the band, Hz (the anti-alias roll-off).
        #[serde(default = "guard_default")]
        guard_hz: f64,
        /// Bytes per USB transfer (Trunk Recorder's `buflen`): a multiple of 512;
        /// 0 = the driver's 262144. Several dongles on one USB 2.0 bus want 65536.
        #[serde(default)]
        buf_len: u32,
        /// USB transfers kept in flight; 0 = enough for about 2 MiB (8 to 64).
        #[serde(default)]
        buffers: u32,
    },
    /// A USRP through UHD (installed separately; loaded at run time).
    /// `args`: UHD device arguments, "" = the first found ("serial=…",
    /// "type=b200", "addr=192.168.10.2"). `antenna` "" = the device's
    /// default. `agc`: the device's AGC (B200 / B210 / E3xx) instead of `gain_db`.
    #[serde(rename_all = "camelCase")]
    Usrp {
        #[serde(default)]
        args: String,
        center_hz: f64,
        rate_hz: f64,
        #[serde(default = "usrp_gain")]
        gain_db: f64,
        #[serde(default)]
        agc: bool,
        #[serde(default)]
        antenna: String,
        #[serde(default)]
        ppm: f64,
        #[serde(default)]
        auto_tune: bool,
        /// Left unused at each edge of the band, Hz (the anti-alias roll-off).
        #[serde(default = "guard_default")]
        guard_hz: f64,
    },
    /// An Airspy R2 / Mini through libairspy (installed separately; loaded
    /// at run time). `serial` hex, "" = the first. Gain: a 0..21 step of
    /// libairspy's linearity or sensitivity tables, or (`gain_mode` "manual")
    /// its three stages; `agc` hands the LNA and mixer stages to the Airspy's AGC.
    #[serde(rename_all = "camelCase")]
    Airspy {
        #[serde(default)]
        serial: String,
        center_hz: f64,
        rate_hz: f64,
        #[serde(default)]
        gain_mode: AirspyGain,
        #[serde(default = "airspy_gain")]
        gain_step: u8,
        /// Manual: LNA 0..14, mixer 0..15, VGA (IF) 0..15.
        #[serde(default = "airspy_stage")]
        lna_step: u8,
        #[serde(default = "airspy_stage")]
        mixer_step: u8,
        #[serde(default = "airspy_stage")]
        vga_step: u8,
        #[serde(default)]
        agc: bool,
        #[serde(default)]
        bias_tee: bool,
        /// 12-bit samples packed on the USB (a quarter less of the bus: an
        /// Airspy at 10 MS/s nearly fills a USB 2.0 bus by itself).
        #[serde(default = "yes")]
        packing: bool,
        #[serde(default)]
        ppm: f64,
        #[serde(default)]
        auto_tune: bool,
        /// Left unused at each edge of the band, Hz (the anti-alias roll-off).
        #[serde(default = "guard_default")]
        guard_hz: f64,
    },
    /// A bladeRF (2.0 micro xA4 / xA9, or the original x40 / x115) through
    /// libbladeRF 2.x (installed separately; loaded at run time). `serial`
    /// "" = the first. `bandwidth_hz`: the analog filter, 0 = the sample
    /// rate. `agc`: the board's AGC instead of `gain_db`. `bias_tee`: power
    /// an LNA over the RX coax (bladeRF 2.0 only).
    #[serde(rename_all = "camelCase")]
    Bladerf {
        #[serde(default)]
        serial: String,
        center_hz: f64,
        rate_hz: f64,
        #[serde(default)]
        bandwidth_hz: f64,
        #[serde(default = "bladerf_gain")]
        gain_db: f64,
        #[serde(default)]
        agc: bool,
        #[serde(default)]
        bias_tee: bool,
        #[serde(default)]
        ppm: f64,
        #[serde(default)]
        auto_tune: bool,
        /// Left unused at each edge of the band, Hz (the anti-alias roll-off).
        #[serde(default = "guard_default")]
        guard_hz: f64,
    },
    /// Any SDR with a SoapySDR module (SoapySDR installed separately; loaded
    /// at run time). `args`: device arguments, "" = the first found
    /// ("driver=hackrf", "driver=sdrplay,serial=…"). `agc`: the device's AGC;
    /// else `gain_db` overall (None: left as the device has it), then each
    /// stage in `gains` (HackRF LNA / VGA / AMP, SDRplay IFGR / RFGR, …);
    /// `settings` device settings ("biastee=true"); `antenna` "" = the default.
    #[serde(rename_all = "camelCase")]
    Soapy {
        #[serde(default)]
        args: String,
        center_hz: f64,
        rate_hz: f64,
        #[serde(default = "yes")]
        agc: bool,
        #[serde(default)]
        gain_db: Option<f64>,
        #[serde(default)]
        gains: BTreeMap<String, f64>,
        #[serde(default)]
        antenna: String,
        #[serde(default)]
        settings: String,
        #[serde(default)]
        ppm: f64,
        #[serde(default)]
        auto_tune: bool,
        /// Left unused at each edge of the band, Hz (the anti-alias roll-off).
        #[serde(default = "guard_default")]
        guard_hz: f64,
    },
    /// A capture on this machine: `format` "cu8" (rtl_sdr), "cs16" or "cf32"
    /// (GNU Radio / UHD complex float); left out, by the file's extension
    /// ([`SampleFormat::of`]).
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
        center_hz: f64,
        rate_hz: f64,
        realtime: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<SampleFormat>,
        #[serde(default)]
        auto_tune: bool,
        /// Left unused at each edge of the band, Hz (the anti-alias roll-off).
        #[serde(default = "guard_default")]
        guard_hz: f64,
    },
}

choice! {
    /// How an Airspy's gain is set: libairspy's linearity or sensitivity
    /// tables (one 0..21 step for all three stages), or each stage by hand.
    pub enum AirspyGain {
        #[default]
        Linearity = "linearity",
        Sensitivity = "sensitivity",
        Manual = "manual",
    }
}

fn guard_default() -> f64 {
    DEFAULT_GUARD_HZ
}
fn rtl_gain() -> f64 {
    RTL_DEFAULT_GAIN_DB
}
fn airspy_gain() -> u8 {
    14
}
fn airspy_stage() -> u8 {
    10
}
fn bladerf_gain() -> f64 {
    30.0
}

choice! {
    /// Sample format of a capture file.
    pub enum SampleFormat {
        /// Unsigned 8-bit I/Q (rtl_sdr).
        #[default]
        Cu8 = "cu8",
        /// Signed 16-bit I/Q, little-endian.
        Cs16 = "cs16",
        /// 32-bit float I/Q, little-endian (GNU Radio "complex", UHD fc32).
        Cf32 = "cf32",
    }
}

impl SampleFormat {
    /// From a file name's extension (.cf32 / .cfile / .fc32 / .complex → cf32, .cs16 / .sc16 → cs16, else
    /// cu8, .raw included).
    pub fn from_path(path: &str) -> Self {
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "cf32" | "cfile" | "fc32" | "complex" => SampleFormat::Cf32,
            "cs16" | "sc16" => SampleFormat::Cs16,
            _ => SampleFormat::Cu8,
        }
    }
    /// A capture's format: as given, else by its extension.
    pub fn of(format: Option<SampleFormat>, path: &str) -> Self {
        format.unwrap_or_else(|| SampleFormat::from_path(path))
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
            Source::Rtlsdr { center_hz, .. } | Source::Usrp { center_hz, .. } | Source::Airspy { center_hz, .. } | Source::Bladerf { center_hz, .. } | Source::Soapy { center_hz, .. } | Source::File { center_hz, .. } => *center_hz,
        }
    }
    pub fn rate_hz(&self) -> f64 {
        match self {
            Source::Rtlsdr { rate_hz, .. } | Source::Usrp { rate_hz, .. } | Source::Airspy { rate_hz, .. } | Source::Bladerf { rate_hz, .. } | Source::Soapy { rate_hz, .. } | Source::File { rate_hz, .. } => *rate_hz,
        }
    }
    /// The frequency correction set for it, ppm (a file: 0).
    pub fn ppm(&self) -> f64 {
        match self {
            Source::Rtlsdr { ppm, .. } => *ppm as f64,
            Source::Usrp { ppm, .. } | Source::Airspy { ppm, .. } | Source::Bladerf { ppm, .. } | Source::Soapy { ppm, .. } => *ppm,
            Source::File { .. } => 0.0,
        }
    }
    pub fn auto_tune(&self) -> bool {
        match self {
            Source::Rtlsdr { auto_tune, .. } | Source::Usrp { auto_tune, .. } | Source::Airspy { auto_tune, .. } | Source::Bladerf { auto_tune, .. } | Source::Soapy { auto_tune, .. } | Source::File { auto_tune, .. } => *auto_tune,
        }
    }
    pub fn guard_hz(&self) -> f64 {
        match self {
            Source::Rtlsdr { guard_hz, .. } | Source::Usrp { guard_hz, .. } | Source::Airspy { guard_hz, .. } | Source::Bladerf { guard_hz, .. } | Source::Soapy { guard_hz, .. } | Source::File { guard_hz, .. } => *guard_hz,
        }
    }
    /// How far from its centre a channel may be, Hz.
    pub fn usable_half_width(&self) -> f64 {
        usable_half_width(self.rate_hz(), self.guard_hz())
    }
    /// Whether the two may open the same radio (one program at a time can):
    /// of a kind and naming one device, or either "the first found". (A
    /// bladeRF may be named by the start of its serial.)
    pub fn may_be(&self, other: &Source) -> bool {
        let id = |s: &Source| -> Option<(u8, String)> {
            match s {
                Source::Rtlsdr { serial, .. } => Some((0, serial.trim().to_string())),
                Source::Usrp { args, .. } => Some((1, args.trim().to_string())),
                Source::Airspy { serial, .. } => Some((2, serial.trim().to_lowercase())),
                Source::Bladerf { serial, .. } => Some((3, serial.trim().to_lowercase())),
                Source::Soapy { args, .. } => Some((4, args.trim().to_string())),
                Source::File { .. } => None,
            }
        };
        match (id(self), id(other)) {
            (Some((k, a)), Some((j, b))) if k == j => a.is_empty() || b.is_empty() || a == b || (k == 3 && (a.starts_with(&b) || b.starts_with(&a))),
            _ => false,
        }
    }

    /// For the interface: "RTL-SDR SN 200", "USRP serial=…", "file x.cu8".
    pub fn label(&self) -> String {
        match self {
            Source::Rtlsdr { serial, .. } => format!("RTL-SDR {}", if serial.is_empty() { "(first)".into() } else { format!("SN {serial}") }),
            Source::Usrp { args, .. } => {
                // Which device, not how it's driven: buffer sizes and the like
                // would rename it (and start its stats history afresh).
                let which: Vec<&str> = args.split(',').map(str::trim).filter(|kv| ["serial=", "addr=", "type=", "product=", "name="].iter().any(|k| kv.starts_with(k))).collect();
                format!("USRP {}", if which.is_empty() { "(first)".to_string() } else { which.join(",") })
            }
            Source::Airspy { serial, .. } => format!("Airspy {}", if serial.is_empty() { "(first)".into() } else { format!("SN {serial}") }),
            Source::Bladerf { serial, .. } => format!("bladeRF {}", if serial.is_empty() { "(first)".into() } else { format!("SN {serial}") }),
            Source::Soapy { args, .. } => format!("SoapySDR {}", if args.is_empty() { "(first)" } else { args }),
            Source::File { path, .. } => format!("file {}", path.rsplit(['/', '\\']).next().unwrap_or(path)),
        }
    }
}

/// A trunked system: its talkgroups, radios, rules and plugin settings, and
/// the sites it is recorded from — each a control channel to follow. A call
/// heard on several of its sites is one call (saved once, from the site that
/// heard it best: see [`Recording::drop_duplicate_calls`]).
///
/// ```json
/// { "shortName": "dcfd", "type": "p25",
///   "sites": [ { "controlChannelsHz": [851012500] },
///              { "name": "dcfd-south", "controlChannelsHz": [852337500], "expect": { "rfss": 1, "site": 2 } } ] }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct System {
    pub short_name: String,
    /// What people call it ("County Public Safety"); the short name is its folder.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its FCC licenses by frequency: see [`FccLicenses`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fcc: FccLicenses,
    /// Share it with the community directory (trunkrecorder.pro/directory):
    /// what this recorder hears of it, now and then while it's recorded
    /// ([`crate::share`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub share: bool,
    /// The directory's record of it, once shared (or added from the directory).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<DirectoryLink>,
    #[serde(rename = "type")]
    pub kind: SystemKind,
    pub enabled: bool,
    /// Its own names for its radios (Trunk Recorder's unitTagsFile, kept here as text): see [`UnitNames`].
    #[serde(skip_serializing_if = "UnitNames::is_empty")]
    pub unit_names: UnitNames,
    /// Its own recording rules; what's left out is as in [`Config::recording`].
    #[serde(skip_serializing_if = "RecordingOverride::is_empty")]
    pub recording: RecordingOverride,
    /// SmartNet: voice mode of a talkgroup never heard granted.
    #[serde(skip_serializing_if = "is_default")]
    pub default_mode: SmartnetMode,
    /// NXDN (`type` "nxdn"): a control channel (the default) or IDAS distributed (list every repeater).
    #[serde(skip_serializing_if = "is_default")]
    pub nxdn_type: NxdnType,
    /// The encryption keys it decrypts with (P25): a key source's name, in
    /// [`Config::keys`]. Empty: none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub keys: String,
    /// Plugins' settings for this system, by plugin id (as each plugin's
    /// `system_config` schema describes them), with `override` for the plugin's
    /// own settings they change for it; a list for several copies of one.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, serde_json::Value>,
    /// How long plugins wait for its calls (all, some talkgroups', some
    /// plugins'): see [`DelayRule`]. These come before the whole recorder's.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugin_delays: Vec<DelayRule>,
    /// The tone signals it sends (pages, alerts, digit strings, Morse IDs): see [`SignalDef`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<SignalDef>,
    /// Where it is recorded from: one site, or several of one system.
    pub sites: Vec<Site>,
    /// Keys this version doesn't know (a newer version's, a hand edit's):
    /// kept, and saved back as they were.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

/// A tone signal a system sends: a page (two tones), an alert, a warble, a
/// digit string, a Morse ID. Its `id` is unique within its system (written
/// `CLMRN:CSP_FIRE` where the system isn't said): calls carrying it list it
/// (the call JSON's `signals`), and plugins hear it named (topic `tone_pattern`).
///
/// ```json
/// { "id": "NORWALK_EMS", "name": "Norwalk EMS page", "pattern": "two_tone", "tonesHz": [788.5, 879.0], "talkgroups": ["20341"] }
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SignalDef {
    pub id: String,
    /// What people call it (empty: its id).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The call JSON's `pattern`: `two_tone`, `long`, `hi_low`, `pulsed`,
    /// `digits`, `morse`, `sequence` or `beep`.
    pub pattern: String,
    /// Its tones, Hz, in order (as the call JSON's `tones`; digits: none).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tones_hz: Vec<f64>,
    /// Digits: the string; Morse: the text.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub digits: String,
    /// Digits: KNOX's tones, not DTMF's.
    #[serde(skip_serializing_if = "is_default")]
    pub knox: bool,
    /// How long each tone is, s, as the call JSON's `lengths` (for playing
    /// it back; not matched on).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lengths_s: Vec<f64>,
    /// How much further a heard tone may be than the recorder allows by
    /// itself, % of the tone (1.5 % in audio; 15.7 Hz + 0.5 % from P25 Phase
    /// 2's tone frames). 0: no further.
    #[serde(skip_serializing_if = "is_default")]
    pub extra_tolerance_pct: f64,
    /// The talkgroups it's sent on: numbers, ranges (`1000-1099`) and
    /// patterns (`507*`), as [`DelayRule::talkgroups`]. Empty: any.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "trunk_recorder_plugin::filter::patterns")]
    pub talkgroups: Vec<String>,
    /// Keys this version doesn't know (a newer version's, a hand edit's):
    /// kept, and saved back as they were.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

impl SignalDef {
    /// The engine's matcher; none when it isn't one ([`SignalDef::problem`]).
    pub fn engine(&self) -> Option<trunk_core::dsp::inband::signal::Signal> {
        if self.problem().is_some() {
            return None;
        }
        Some(trunk_core::dsp::inband::signal::Signal {
            id: self.id.trim().to_string(),
            pattern: PatternKind::from_name(&self.pattern)?,
            hz: self.tones_hz.iter().map(|&f| f as f32).collect(),
            digits: self.digits.trim().to_string(),
            knox: self.knox,
            extra_pct: self.extra_tolerance_pct as f32,
            talkgroups: self.talkgroups.clone(),
        })
    }

    /// An id made from what people call a signal: letters and digits, upper
    /// case, runs of anything else one `_` ("Norwalk EMS page" → NORWALK_EMS_PAGE).
    pub fn id_from(text: &str) -> String {
        let mut id = String::new();
        for c in text.chars() {
            if c.is_ascii_alphanumeric() {
                id.push(c.to_ascii_uppercase());
            } else if !id.is_empty() && !id.ends_with('_') {
                id.push('_');
            }
        }
        id.trim_end_matches('_').chars().take(40).collect()
    }

    /// What's wrong with it, as Setup and Start say it.
    pub fn problem(&self) -> Option<String> {
        let id = self.id.trim();
        let name = if id.is_empty() { "A signal" } else { id };
        if id.is_empty() {
            return Some("A signal needs an id (calls and plugins name it by that).".into());
        }
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Some(format!("Signal {id}: an id is letters, digits, _ and - (it is written SYSTEM:{id} where the system isn't said)."));
        }
        let Some(kind) = PatternKind::from_name(&self.pattern) else {
            return Some(format!("Signal {name}: no such pattern as \"{}\".", self.pattern));
        };
        match kind {
            PatternKind::Digits | PatternKind::Morse if self.digits.trim().is_empty() => Some(format!("Signal {name}: which {} does it send?", if kind == PatternKind::Morse { "text" } else { "digits" })),
            PatternKind::Digits | PatternKind::Morse => None,
            _ if self.tones_hz.is_empty() || self.tones_hz.iter().any(|&f| !(100.0..=4000.0).contains(&f)) => Some(format!("Signal {name}: its tones, Hz (100–4000).")),
            PatternKind::TwoTone | PatternKind::HiLow if self.tones_hz.len() != 2 => Some(format!("Signal {name}: a {} has two tones.", if kind == PatternKind::TwoTone { "two-tone page" } else { "hi-low warble" })),
            _ => None,
        }
    }
}

/// A system's signals for the engine.
fn engine_signals(defs: &[SignalDef]) -> Vec<trunk_core::dsp::inband::signal::Signal> {
    defs.iter().filter_map(SignalDef::engine).collect()
}

/// What's wrong with a system's signals: one's own problem, or an id used twice.
fn signals_problem(system: &str, defs: &[SignalDef]) -> Option<String> {
    if let Some(p) = defs.iter().find_map(SignalDef::problem) {
        return Some(format!("{system}: {p}"));
    }
    let mut seen = BTreeSet::new();
    defs.iter().map(|d| d.id.trim()).find(|id| !seen.insert(*id)).map(|id| format!("{system}: two signals are called {id} (an id is the system's own)."))
}

/// One site of a [`System`]: a control channel to follow, and how.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Site {
    /// Its name, unique in its system (empty: the system's short name). The
    /// talkgroup file's Preferred Site names it; calls and plugins say it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The directory's ID for this site, once shared (or added from the directory).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub directory_id: String,
    pub enabled: bool,
    pub control_channels_hz: Vec<f64>,
    pub modulation: Modulation,
    /// Only follow a control channel with this identity (fields left out
    /// match anything) — this site, not its neighbour on a nearby frequency.
    pub expect: SiteIdentity,
    /// Voice channels the survey heard (for placing sources; informational).
    pub voice_channels_hz: Vec<f64>,
    /// SmartNet (`type` "smartnet"): the band plan, as Trunk Recorder names
    /// it — "800_standard", "800_reband", "800_splinter", "900", or
    /// "400_custom" with the four numbers below (Hz; offset is a channel number).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bandplan: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub bandplan_base_hz: f64,
    #[serde(skip_serializing_if = "is_zero")]
    pub bandplan_spacing_hz: f64,
    #[serde(skip_serializing_if = "is_zero_u16")]
    pub bandplan_offset: u16,
    #[serde(skip_serializing_if = "is_zero")]
    pub bandplan_high_hz: f64,
    /// DMR / NXDN / EDACS: logical channel number → frequency, Hz (Trunk
    /// Recorder's `lcnTable`). DMR and NXDN learn channels left out from the
    /// air; EDACS checks the list against the air but needs it.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub lcn_table_hz: BTreeMap<String, f64>,
    /// DMR: voice frequencies to watch besides the control channels (Trunk
    /// Recorder's `channels`; Capacity Plus: every repeater of the site).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dmr_channels_hz: Vec<f64>,
    /// DMR: only this colour code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_code: Option<u8>,
    /// NXDN: the control channel's (Type-D: the repeaters') rate.
    #[serde(skip_serializing_if = "is_default")]
    pub nxdn_rate: NxdnRate,
    /// NXDN: voice frequencies to watch besides the control channels (Type-D:
    /// the repeaters). A grant to a channel number not in `lcnTableHz` is
    /// learned when its call comes up on one of them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nxdn_channels_hz: Vec<f64>,
    /// NXDN: only this RAN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ran: Option<u8>,
    /// EDACS: the system scrambles its control channel with ESK 0xA0.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub esk: bool,
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}
fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}
fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// A system's record in the community directory: its ID there. (What
/// changes as it's shared, the version and when, is kept beside the config,
/// not in it: [`crate::share::Ledger::unsaved`].)
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DirectoryLink {
    pub id: String,
}

/// A system's FCC licenses, by frequency (Hz on the 6.25 kHz raster, as a
/// string: [`fcc_key`]): who is licensed on each of its control, voice and
/// conventional channels. Optional, for reference; plugins are told them.
/// A system's ID is its control channel's ([`fcc_primary`]).
///
/// ```json
/// "fcc": { "857987500": { "callSign": "WPXT459", "name": "DISTRICT OF COLUMBIA", "from": "lookup" },
///          "859087500": { "callSign": "KNJA391", "from": "air" } }
/// ```
pub type FccLicenses = BTreeMap<String, FccLicense>;

/// Who is licensed on a frequency.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct FccLicense {
    /// The license's call sign ("WPXT459").
    pub call_sign: String,
    /// Who holds it, as the FCC has it ("DISTRICT OF COLUMBIA"); empty: not looked up.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "is_default")]
    pub from: FccFrom,
}

/// A frequency's key in [`FccLicenses`]: Hz on the 6.25 kHz raster.
pub fn fcc_key(freq_hz: f64) -> String {
    (((freq_hz / 6250.0).round() * 6250.0) as u64).to_string()
}

/// The license that names a system: its control channels' (in order), else
/// the call sign on the most of its frequencies (a tie: the first in order).
pub fn fcc_primary<'a>(fcc: &'a FccLicenses, control_channels_hz: &[f64]) -> Option<&'a FccLicense> {
    let own = control_channels_hz.iter().find_map(|&f| fcc.get(&fcc_key(f)).filter(|l| !l.call_sign.is_empty()));
    own.or_else(|| {
        let mut n: BTreeMap<&str, (usize, &FccLicense)> = BTreeMap::new();
        for l in fcc.values().filter(|l| !l.call_sign.is_empty()) {
            n.entry(&l.call_sign).or_insert((0, l)).0 += 1;
        }
        n.values().rev().max_by_key(|(k, _)| *k).map(|(_, l)| *l)
    })
}

/// An FCC call sign as one can be: 3 to 8 letters and digits, with both.
pub fn plausible_call_sign(s: &str) -> bool {
    (3..=8).contains(&s.len())
        && s.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && s.bytes().any(|c| c.is_ascii_digit())
        && s.bytes().any(|c| c.is_ascii_uppercase())
}

// The config's closed choices ([`choice!`](crate::choice)): a value not
// listed is an error when the file is read, as a wrong type is.

choice! {
    /// Where an [`FccLicense`] came from: the user (never replaced), heard
    /// on the air (a Motorola base station ID), or a license lookup.
    pub enum FccFrom {
        #[default]
        User = "user",
        Air = "air",
        Lookup = "lookup",
    }
}


choice! {
    /// A trunked system's protocol (`type`).
    pub enum SystemKind {
        #[default]
        P25 = "p25",
        Smartnet = "smartnet",
        Dmr = "dmr",
        Nxdn = "nxdn",
        /// EDACS Standard or Networked (analog and ProVoice voice).
        Edacs = "edacs",
    }
}

choice! {
    /// SmartNet: the voice of a talkgroup never heard granted.
    pub enum SmartnetMode {
        /// P25.
        #[default]
        Digital = "digital",
        Analog = "analog",
    }
}

choice! {
    /// NXDN: a site with a control channel, or IDAS distributed (every repeater listed).
    pub enum NxdnType {
        #[default]
        TypeC = "typeC",
        TypeD = "typeD",
    }
}

choice! {
    /// A site's receivers: detect which decodes, or one of them only.
    pub enum Modulation {
        #[default]
        Auto = "auto",
        /// C4FM.
        Fsk4 = "fsk4",
        /// CQPSK (simulcast).
        Qpsk = "qpsk",
    }
}

choice! {
    /// NXDN: a control channel's (Type-D: the repeaters') rate.
    pub enum NxdnRate {
        #[default]
        Nxdn48 = "nxdn48" | "48" | "4800",
        Nxdn96 = "nxdn96" | "96" | "9600",
    }
}

impl NxdnRate {
    pub fn engine(self) -> trunk_core::nxdn::Rate {
        match self {
            NxdnRate::Nxdn48 => trunk_core::nxdn::Rate::N48,
            NxdnRate::Nxdn96 => trunk_core::nxdn::Rate::N96,
        }
    }
}

choice! {
    /// The IMBE vocoder for P25 Phase 1 voice.
    pub enum Vocoder {
        /// Pavel Yazev's fixed-point decoder (Trunk Recorder's softVocoder false).
        #[default]
        Fixed = "fixed" | "fixed-point",
        /// Trunk Recorder's float synthesis.
        Enhanced = "enhanced",
        Mbelib = "mbelib",
    }
}

impl Vocoder {
    pub fn engine(self) -> trunk_core::mbe::Profile {
        match self {
            Vocoder::Fixed => trunk_core::mbe::Profile::Fixed,
            Vocoder::Enhanced => trunk_core::mbe::Profile::Enhanced,
            Vocoder::Mbelib => trunk_core::mbe::Profile::Mbelib,
        }
    }
}

choice! {
    /// What encodes M4A: the first found, one of them, or none.
    pub enum M4aEncoder {
        #[default]
        Auto = "auto",
        Ffmpeg = "ffmpeg",
        Afconvert = "afconvert",
        Fdkaac = "fdkaac",
        None = "none",
    }
}

choice! {
    /// Which names a radio gets (Trunk Recorder's unitTagsMode).
    pub enum UnitNamesMode {
        /// These names first, then the talker aliases heard.
        #[default]
        User = "user",
        /// The aliases heard first.
        Ota = "ota",
        UserOnly = "user_only",
        None = "none",
    }
}

impl UnitNamesMode {
    fn engine(self) -> UnitTagsMode {
        match self {
            UnitNamesMode::User => UnitTagsMode::UserFirst,
            UnitNamesMode::Ota => UnitTagsMode::OtaFirst,
            UnitNamesMode::UserOnly => UnitTagsMode::UserOnly,
            UnitNamesMode::None => UnitTagsMode::None,
        }
    }
}

choice! {
    /// ANSI colour in the log (Trunk Recorder's logColor).
    pub enum LogColor {
        /// The console's when it is a terminal and NO_COLOR isn't set.
        #[default]
        Auto = "",
        Console = "console",
        Logfile = "logfile",
        All = "all",
        None = "none",
    }
}

impl System {
    pub fn is_smartnet(&self) -> bool {
        self.kind == SystemKind::Smartnet
    }
    pub fn is_dmr(&self) -> bool {
        self.kind == SystemKind::Dmr
    }
    pub fn is_nxdn(&self) -> bool {
        self.kind == SystemKind::Nxdn
    }
    pub fn is_edacs(&self) -> bool {
        self.kind == SystemKind::Edacs
    }

    /// Recording it: enabled, with a site that is.
    pub fn active(&self) -> bool {
        self.enabled && self.sites.iter().any(Site::active)
    }

    /// The sites being recorded, in order.
    pub fn active_sites(&self) -> impl Iterator<Item = &Site> {
        self.sites.iter().filter(|s| self.enabled && s.active())
    }

    /// Site `site`'s name: its own, else the system's short name.
    pub fn site_name<'a>(&'a self, site: &'a Site) -> &'a str {
        if site.name.trim().is_empty() { &self.short_name } else { site.name.trim() }
    }

    /// A system of one site, for code (and tests) that has only those fields.
    pub fn one_site(short_name: &str, site: Site) -> System {
        System { short_name: short_name.into(), sites: vec![site], ..Default::default() }
    }
}

impl Site {
    /// Recording it: enabled, with a control channel.
    pub fn active(&self) -> bool {
        self.enabled && !self.control_channels_hz.is_empty()
    }

    /// The NXDN settings (`lcnTableHz` is its channel table: channel number
    /// → frequency; Type-D: repeater number → frequency).
    pub fn nxdn(&self, sys: &System) -> trunk_core::nxdn::trunking::NxdnConfig {
        use trunk_core::nxdn::trunking::{Kind, NxdnConfig};
        NxdnConfig {
            kind: if sys.nxdn_type == NxdnType::TypeD { Kind::TypeD } else { Kind::TypeC },
            rate: self.nxdn_rate.engine(),
            channel_table: self.lcn_table(),
            channels: self.nxdn_channels_hz.clone(),
            ran: self.ran,
        }
    }

    /// Frequencies a watching protocol (DMR, NXDN, EDACS) needs besides the control channels.
    pub fn watched_channels(&self, sys: &System) -> Vec<f64> {
        if sys.is_dmr() {
            self.dmr().channels
        } else if sys.is_nxdn() {
            self.nxdn_channels_hz.clone()
        } else if sys.is_edacs() {
            self.edacs().lcns.values().map(|&f| f as f64).collect()
        } else {
            vec![]
        }
    }

    /// The DMR settings.
    pub fn dmr(&self) -> trunk_core::dmr::DmrConfig {
        trunk_core::dmr::DmrConfig {
            lcn_table: self.lcn_table(),
            channels: self.dmr_channels_hz.clone(),
            color_code: self.color_code,
        }
    }

    /// `lcnTableHz` with numbers for keys and frequencies in Hz (the keys that parse).
    fn lcn_table<K: std::str::FromStr + Ord>(&self) -> BTreeMap<K, u64> {
        self.lcn_table_hz.iter().filter_map(|(k, &v)| Some((k.trim().parse().ok()?, v.round() as u64))).collect()
    }

    /// The EDACS settings: `lcnTableHz` is the site's channel list (LCNs 1–25).
    pub fn edacs(&self) -> trunk_core::edacs::EdacsConfig {
        let mut lcns = self.lcn_table::<u8>();
        lcns.retain(|l, &mut f| (1..=25).contains(l) && f > 0);
        trunk_core::edacs::EdacsConfig { lcns, esk: self.esk }
    }

    /// The SmartNet settings, or why they don't work.
    pub fn smartnet(&self, sys: &System) -> Result<trunk_core::trunk::SmartnetConfig, String> {
        let bandplan = trunk_core::smartnet::Bandplan::from_config(&self.bandplan, self.bandplan_base_hz, self.bandplan_spacing_hz, self.bandplan_offset, self.bandplan_high_hz)?;
        Ok(trunk_core::trunk::SmartnetConfig { bandplan, analog_default: sys.default_mode == SmartnetMode::Analog })
    }

    fn bank(&self) -> BankConfig {
        let m = self.modulation;
        BankConfig { cqpsk: m != Modulation::Fsk4, cqpsk_eq: m != Modulation::Fsk4, c4fm: m != Modulation::Qpsk, ..Default::default() }
    }

    /// The engine's protocol settings for it, as a site of `sys`.
    fn protocol(&self, sys: &System) -> Protocol {
        if sys.is_dmr() {
            Protocol::Dmr(self.dmr())
        } else if sys.is_nxdn() {
            Protocol::Nxdn(self.nxdn(sys))
        } else if sys.is_edacs() {
            Protocol::Edacs(self.edacs())
        } else if let Some(sn) = sys.is_smartnet().then(|| self.smartnet(sys).ok()).flatten() {
            Protocol::SmartNet(sn)
        } else {
            Protocol::P25
        }
    }
}

impl Default for System {
    fn default() -> Self {
        System {
            other: BTreeMap::new(),
            keys: String::new(),
            short_name: "sys1".into(),
            name: String::new(),
            share: false,
            directory: None,
            fcc: BTreeMap::new(),
            kind: SystemKind::P25,
            enabled: true,
            unit_names: UnitNames::default(),
            recording: RecordingOverride::default(),
            default_mode: SmartnetMode::default(),
            nxdn_type: NxdnType::default(),
            plugins: BTreeMap::new(),
            plugin_delays: vec![],
            signals: vec![],
            sites: vec![],
        }
    }
}

impl Default for Site {
    fn default() -> Self {
        Site {
            name: String::new(),
            directory_id: String::new(),
            enabled: true,
            control_channels_hz: vec![],
            modulation: Modulation::Auto,
            expect: SiteIdentity::default(),
            voice_channels_hz: vec![],
            bandplan: String::new(),
            bandplan_base_hz: 0.0,
            bandplan_spacing_hz: 0.0,
            bandplan_offset: 0,
            bandplan_high_hz: 0.0,
            lcn_table_hz: Default::default(),
            dmr_channels_hz: vec![],
            color_code: None,
            nxdn_rate: NxdnRate::default(),
            nxdn_channels_hz: vec![],
            ran: None,
            esk: false,
            other: BTreeMap::new(),
        }
    }
}

/// A P25 site's identity. NAC, WACN and System ID are shown in hex.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct SiteIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nac: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wacn: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sys_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rfss: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<u32>,
}

/// What a control channel says of itself, as the config writes a site's
/// identity: the one shape status, the survey and the config share.
impl From<&Identity> for SiteIdentity {
    fn from(id: &Identity) -> Self {
        SiteIdentity {
            nac: id.get(IdField::Nac).map(|v| v as u16),
            wacn: id.get(IdField::Wacn),
            sys_id: id.get(IdField::SysId),
            rfss: id.get(IdField::Rfss),
            site: id.get(IdField::Site),
        }
    }
}

impl SiteIdentity {
    pub(crate) fn engine(&self) -> Identity {
        let mut id = Identity::default();
        id.set_opt(IdField::Nac, self.nac.map(u32::from));
        id.set_opt(IdField::Wacn, self.wacn);
        id.set_opt(IdField::SysId, self.sys_id);
        id.set_opt(IdField::Rfss, self.rfss);
        id.set_opt(IdField::Site, self.site);
        id
    }
}

/// A conventional system (Trunk Recorder's `conventional`, `conventionalP25`
/// and `conventionalDMR` systems): channels of one frequency each, found by
/// energy detection, with their own short name (folder), call rules, unit
/// names and upload settings. Either listed here, or kept in a CSV file of
/// its own (`channelFile`, desktop) to edit in a spreadsheet — see
/// [`crate::channels`] for its columns. A frequency belongs to one system.
///
/// ```json
/// "conventional": [
///   { "shortName": "fire", "squelchDb": 8, "channels": [
///       { "freqHz": 154430000, "mode": "fm", "name": "County Fire Dispatch", "talkgroup": 1001 } ] },
///   { "shortName": "police", "channelFile": "police.csv" }
/// ]
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Conventional {
    /// Folder and record name of its calls (Trunk Recorder's shortName).
    pub short_name: String,
    /// What people call it ("County Fire"); the short name is its folder.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its channels' FCC licenses by frequency (they can be different
    /// licensees'): see [`FccLicenses`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fcc: FccLicenses,
    /// Record it.
    pub enabled: bool,
    /// Open threshold for every channel, dB above the measured noise floor.
    pub squelch_db: f64,
    /// How its analog channels are demodulated; what's left out is the default.
    #[serde(skip_serializing_if = "Demod::is_empty")]
    pub demod: Demod,
    /// A CSV the channels are read from, absolute or relative to the config
    /// file's folder; read when the app starts, when recording starts and
    /// on Reload. Empty: the channels are the list below.
    pub channel_file: String,
    /// The channels (while a channel file is linked: its contents, not saved here).
    pub channels: Vec<Channel>,
    /// How the channel file last read, for the interface (not saved: every
    /// read of the file sets it, and saving clears it).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub channel_file_status: String,
    /// Plugins' settings for the conventional channels, by plugin id (to
    /// plugins they're one more system); a list for several copies of one.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, serde_json::Value>,
    /// How long plugins wait for their calls, as a trunked system's ([`System::plugin_delays`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugin_delays: Vec<DelayRule>,
    /// The tone signals they send, as a trunked system's ([`System::signals`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<SignalDef>,
    /// Their own recording rules; what's left out is as in [`Config::recording`].
    #[serde(skip_serializing_if = "RecordingOverride::is_empty")]
    pub recording: RecordingOverride,
    /// Names for the radios heard on them.
    #[serde(skip_serializing_if = "UnitNames::is_empty")]
    pub unit_names: UnitNames,
    /// The encryption keys its P25 channels decrypt with (as [`System::keys`]).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub keys: String,
    /// Keys this version doesn't know (a newer version's, a hand edit's):
    /// kept, and saved back as they were.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

/// Names for a system's radios: Trunk Recorder's unitTagsFile (headerless
/// `unit,name` lines; a unit between slashes is a regular expression) and
/// unitTagsMode: "user" (these first, then the talker aliases heard),
/// "ota" (the aliases first), "user_only" or "none".
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct UnitNames {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub csv: String,
    /// The file it came from.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "is_default")]
    pub mode: UnitNamesMode,
}

impl UnitNames {
    pub fn is_empty(&self) -> bool {
        *self == UnitNames::default()
    }
    fn engine(&self) -> UnitTags {
        UnitTags::parse_csv(&self.csv, self.mode.engine()).0
    }
}

impl Default for Conventional {
    fn default() -> Self {
        Conventional {
            other: BTreeMap::new(),
            keys: String::new(),
            short_name: "conv".into(),
            name: String::new(),
            fcc: BTreeMap::new(),
            enabled: true,
            squelch_db: ConvConfig::default().squelch_db,
            demod: Demod::default(),
            channel_file: String::new(),
            channels: vec![],
            channel_file_status: String::new(),
            plugins: BTreeMap::new(),
            plugin_delays: vec![],
            signals: vec![],
            recording: RecordingOverride::default(),
            unit_names: UnitNames::default(),
        }
    }
}

/// How a conventional system's analog channels are demodulated: FM's
/// de-emphasis, AM's level and low cut, and the squelch gate's timing (each
/// channel picks its own bandwidth: [`Channel::bandwidth_khz`]). None: the default.
///
/// ```json
/// "demod": { "fmDeemphasis": false, "amLevel": "slow", "amLowCutHz": 300, "squelchOpenMs": 30, "squelchHangMs": 150 }
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Demod {
    /// FM: undo the transmitter's 750 µs pre-emphasis (default true). Off
    /// for channels sent without it, which sound muffled with it on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fm_deemphasis: Option<bool>,
    /// AM: how fast the audio level follows the carrier: "fast" (50 ms,
    /// default) or "slow" (0.5 s, for steady signals: less pumping).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub am_level: Option<AmLevel>,
    /// AM: the audio high-pass's corner, Hz (default 200; 0 = none): carrier hum and flutter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub am_low_cut_hz: Option<f64>,
    /// How long the carrier must hold before the audio opens, ms (default 30).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub squelch_open_ms: Option<f64>,
    /// How long the audio stays open after the carrier goes, ms (default 2):
    /// longer bridges fades on weak signals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub squelch_hang_ms: Option<f64>,
}

choice! {
    pub enum AmLevel {
        #[default]
        Fast = "fast",
        Slow = "slow",
    }
}

/// The ranges [`Config::problem`] accepts (the interface's too).
pub const AM_LOW_CUT_MAX_HZ: f64 = 600.0;
pub const SQUELCH_OPEN_MAX_MS: f64 = 500.0;
pub const SQUELCH_HANG_MAX_MS: f64 = 1000.0;
pub const BANDWIDTH_KHZ: (f64, f64) = (4.0, 20.0);

impl Demod {
    pub fn is_empty(&self) -> bool {
        *self == Demod::default()
    }

    /// What's wrong with it, if anything.
    fn problem(&self) -> Option<String> {
        let bad = |v: Option<f64>, max: f64| v.is_some_and(|v| !(0.0..=max).contains(&v));
        if bad(self.am_low_cut_hz, AM_LOW_CUT_MAX_HZ) {
            return Some(format!("AM low cut is 0–{AM_LOW_CUT_MAX_HZ} Hz."));
        }
        if bad(self.squelch_open_ms, SQUELCH_OPEN_MAX_MS) {
            return Some(format!("Squelch open delay is 0–{SQUELCH_OPEN_MAX_MS} ms."));
        }
        if bad(self.squelch_hang_ms, SQUELCH_HANG_MAX_MS) {
            return Some(format!("Squelch hang is 0–{SQUELCH_HANG_MAX_MS} ms."));
        }
        None
    }

    /// Channel `c`'s tuning: the mode's (and frequency's) defaults, with these and its bandwidth.
    fn tuning(&self, c: &Channel) -> AnalogTuning {
        let mut t = if c.mode == ChannelMode::Am { AnalogTuning::am(c.freq_hz) } else { AnalogTuning::default() };
        if let Some(khz) = c.bandwidth_khz {
            t.half_bw_hz = khz * 500.0;
        }
        if self.fm_deemphasis == Some(false) {
            t.deemph_tau_s = 0.0;
        }
        if self.am_level == Some(AmLevel::Slow) {
            t.level_tau_s = trunk_core::dsp::am::LEVEL_SLOW_S;
        }
        if let Some(hz) = self.am_low_cut_hz {
            t.low_cut_hz = hz;
        }
        if let Some(ms) = self.squelch_open_ms {
            t.squelch.open_s = ms / 1000.0;
        }
        if let Some(ms) = self.squelch_hang_ms {
            t.squelch.hang_s = ms / 1000.0;
        }
        t
    }
}

choice! {
    pub enum ChannelMode {
        /// Analog FM (narrowband unless its bandwidth says wide).
        #[default]
        Fm = "fm",
        /// Analog AM (airband).
        Am = "am",
        /// P25 Phase 1.
        P25 = "p25",
        /// DMR (both slots).
        Dmr = "dmr",
        /// NXDN at 4800 bps (6.25 kHz).
        Nxdn48 = "nxdn48",
        /// NXDN at 9600 bps (12.5 kHz).
        Nxdn96 = "nxdn96",
    }
}

impl ChannelMode {
    pub fn conv(self) -> ConvMode {
        match self {
            ChannelMode::Fm => ConvMode::Fm,
            ChannelMode::Am => ConvMode::Am,
            ChannelMode::P25 => ConvMode::P25,
            ChannelMode::Dmr => ConvMode::Dmr,
            ChannelMode::Nxdn48 => ConvMode::Nxdn(trunk_core::nxdn::Rate::N48),
            ChannelMode::Nxdn96 => ConvMode::Nxdn(trunk_core::nxdn::Rate::N96),
        }
    }
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
    /// Talkgroup number the calls are filed under (TG Number). None: the
    /// talkgroup the air names (P25, DMR, NXDN), else the frequency in kHz
    /// (further rows on the frequency: that and a digit, see
    /// [`ConvChannel::default_talkgroup_at`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talkgroup: Option<u32>,
    /// The code it records: FM's CTCSS tone or DCS code in Trunk Recorder's
    /// form (`151.4`, `D023N`), P25's NAC (`293`), DMR's colour code (`1`),
    /// NXDN's RAN (`5`); empty: any. With `slot` and `matchTalkgroup`, the
    /// row's [`Access`] (none of them: any — beside rows with codes, the
    /// rest). (Was `tone`, all three in one: see [`crate::migrate`].)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// DMR: record only this slot (1, 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u8>,
    /// P25, DMR, NXDN: record only transmissions naming this talkgroup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_talkgroup: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// This channel's open threshold, dB above the noise floor (else the section's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub squelch_db: Option<f64>,
    /// Analog: the channel filter's width, kHz (else the mode's: FM 11 —
    /// 12.5 kHz channels, 16 for 25 kHz wide FM; AM 11, or 16 from 225 MHz;
    /// 6 for 8.33 kHz airband channels).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_khz: Option<f64>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Its calls' carrier offsets score its source's AutoTune (digital rows;
    /// a repeater output to trust, not radios on simplex). Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_tune: bool,
}

/// A USRP's gain when none is given, dB (as the interface starts one).
fn usrp_gain() -> f64 {
    40.0
}

fn yes() -> bool {
    true
}

/// What each channel files a call under when neither it nor the air names
/// a talkgroup: by its place among the rows on its frequency (enabled or
/// not, so switching one off renumbers nothing).
pub fn default_talkgroups(channels: &[Channel]) -> Vec<u32> {
    channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let k = channels[..i].iter().filter(|o| (o.freq_hz - c.freq_hz).abs() < 1.0).count();
            ConvChannel::default_talkgroup_at(c.freq_hz, k)
        })
        .collect()
}

/// Before v0.2.6 a P25 row on a frequency with others filed its calls
/// under its own number (the default, if it gave none), the air's thrown
/// away; now a row without one takes the air's. Writes that number in, so
/// an older config or channel file keeps filing the same. Returns how many.
pub fn keep_p25_numbers(channels: &mut [Channel]) -> usize {
    let defaults = default_talkgroups(channels);
    let mut n = 0;
    for i in 0..channels.len() {
        let c = &channels[i];
        let shared = channels.iter().filter(|o| (o.freq_hz - c.freq_hz).abs() < 1.0).count() > 1;
        if c.mode == ChannelMode::P25 && shared && c.talkgroup.is_none() {
            channels[i].talkgroup = Some(defaults[i]);
            n += 1;
        }
    }
    n
}

impl Channel {
    pub fn conv_mode(&self) -> ConvMode {
        self.mode.conv()
    }

    /// FM or AM.
    pub fn analog(&self) -> bool {
        matches!(self.mode, ChannelMode::Fm | ChannelMode::Am)
    }

    /// What it records (code, slot and talkgroup), read for its mode.
    pub fn parsed_access(&self) -> Result<Option<Access>, String> {
        Access::join(self.conv_mode(), &self.code, self.slot, self.match_talkgroup)
    }

    /// `code`, `slot` and `matchTalkgroup` from one string as people write
    /// it (`CC1 TS2 TG201`, `NAC 293 TG 1`, `151.4`): the old `tone`, a
    /// RadioReference paste. Left as it is if it doesn't read.
    pub fn set_access(&mut self, text: &str) -> Result<(), String> {
        let (code, slot, tg) = Access::parse(self.conv_mode(), text)?.map_or((String::new(), None, None), Access::parts);
        (self.code, self.slot, self.match_talkgroup) = (code, slot, tg);
        Ok(())
    }

    fn engine_channel(&self, default_talkgroup: u32) -> ConvChannel {
        let tg = self.talkgroup.unwrap_or(default_talkgroup);
        let named = !(self.name.is_empty() && self.description.is_empty() && self.tag.is_empty() && self.group.is_empty());
        ConvChannel {
            freq_hz: self.freq_hz,
            mode: self.conv_mode(),
            talkgroup: self.talkgroup,
            default_talkgroup,
            info: named.then(|| Talkgroup {
                number: tg,
                mode: if self.analog() { "A".into() } else { "D".into() },
                alpha_tag: self.name.clone(),
                description: self.description.clone(),
                tag: self.tag.clone(),
                group: self.group.clone(),
                priority: 1,
                preferred_nac: 0,
                preferred_site: String::new(),
                ignore: false,
            }),
            squelch_db: self.squelch_db,
            tune_ref: self.auto_tune,
            access: self.parsed_access().ok().flatten(),
            system: 0,
            analog: Demod::default().tuning(self),
        }
    }
}

/// Recording's settings, declared once: those each system can set again for
/// itself (`per_system`) make [`RecordingOverride`] and [`Recording::with`]
/// too (protocol.ts: `RecordingRules`, of which `RecordingOverride` is a
/// `Partial`).
macro_rules! recording {
    (
        $(#[$sm:meta])*
        pub struct Recording {
            per_system { $( $(#[$pm:meta])* pub $pf:ident: $pt:ty, )* }
            $( $(#[$om:meta])* pub $of:ident: $ot:ty, )*
        }
    ) => {
        $(#[$sm])*
        pub struct Recording {
            $( $(#[$pm])* pub $pf: $pt, )*
            $( $(#[$om])* pub $of: $ot, )*
        }

        /// One system's own recording rules (or the conventional channels'):
        /// each left out is as in [`Recording`].
        #[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
        #[serde(rename_all = "camelCase", default)]
        pub struct RecordingOverride {
            $( #[serde(skip_serializing_if = "Option::is_none")] pub $pf: Option<$pt>, )*
        }

        impl Recording {
            /// These settings with a system's own on top.
            pub fn with(&self, o: &RecordingOverride) -> Recording {
                let mut r = self.clone();
                $( if let Some(v) = &o.$pf { r.$pf = v.clone(); } )*
                r
            }
        }
    };
}

recording! {
    /// How calls are recorded and saved. Those `per_system` can be set again
    /// on each system (and the conventional channels): [`RecordingOverride`].
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase", default)]
    pub struct Recording {
        per_system {
            /// A call ends this long after its last grant or audio.
            pub call_timeout_s: f64,
            /// A call is the talkgroup's conversation: its transmissions until
            /// it's quiet for `call_timeout_s`. Off, each transmission is a call
            /// of its own: on a system with several sites, the best copy of each
            /// transmission is kept, and none is lost with another's duplicates.
            pub conversation_mode: bool,
            /// Talkgroups not in the talkgroup file.
            pub record_unknown: bool,
            pub record_encrypted: bool,
            pub record_unit_to_unit: bool,
            /// Keep calls with no audio (encrypted, nothing decoded).
            pub keep_silent_calls: bool,
            /// Drop calls with less audio than this, s; 0 = keep all (Trunk
            /// Recorder's minDuration).
            pub min_call_s: f64,
            /// Save a call this long and carry on in a new one, s; 0 = parts of
            /// `STUCK_CALL_S` (600 s), so a stuck carrier never records without
            /// bound (maxDuration).
            pub max_call_s: f64,
            /// Leave out transmissions shorter than this, s; 0 = keep all
            /// (minTransmissionDuration).
            pub min_transmission_s: f64,
            /// Bring every call's speech to the same loudness (as Trunk
            /// Recorder's uploads were).
            pub normalize_audio: bool,
            /// Then raise or lower digital / analog calls by this much, dB
            /// (Trunk Recorder's digitalLevels / analogLevels).
            pub digital_level_db: f64,
            pub analog_level_db: f64,
            /// Keep each call's .wav once every upload plugin has handled
            /// it; off, it's deleted then (with `archive_m4a`, Trunk
            /// Recorder's audioArchive and compressWav: [`crate::migrate`]).
            pub archive_wav: bool,
            /// Keep an .m4a of each call (made for it when no plugin wants one).
            pub archive_m4a: bool,
            /// Keep the call's JSON then (callLog).
            pub call_log: bool,
            /// When an upload failed, keep the files anyway (archiveFilesOnFailure).
            pub archive_files_on_failure: bool,
            /// Where calls go under the recordings folder, and their names
            /// ([`crate::filename`]); empty = `<short name>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>`.
            pub filename_format: String,
        }
        pub capture_dir: String,
        pub preroll_s: f64,
        pub max_recorders: usize,
        /// Save each call's vocoder frames next to its audio, for diagnosis.
        pub capture_frames: bool,
        /// A call heard on several sites of one system: save only the best copy
        /// (each is recorded; the one decoded most cleanly, or the talkgroup's
        /// preferred site, is kept). Trunk Recorder's multiSite.
        pub drop_duplicate_calls: bool,
        /// IMBE vocoder for P25 Phase 1 voice.
        pub vocoder: Vocoder,
        /// M4A for the plugins that upload it, encoded once per call.
        pub m4a: M4a,
        /// Calls waiting for the upload plugins kept in memory, not on the disk.
        pub ram_spool: RamSpool,
        /// Keys this version doesn't know (a newer version's, a hand edit's):
        /// kept, and saved back as they were.
        #[serde(flatten)]
        pub other: BTreeMap<String, serde_json::Value>,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct M4a {
    pub encoder: M4aEncoder,
    pub bitrate_kbps: u32,
}

impl Default for M4a {
    fn default() -> Self {
        M4a { encoder: M4aEncoder::Auto, bitrate_kbps: 32 }
    }
}

/// A RAM disk for the files only the upload plugins need: they go there
/// instead of the recordings folder, and to the recordings folder only when
/// they're kept after all (an upload failed). macOS: a RAM disk the app makes;
/// Linux: a folder in /dev/shm.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RamSpool {
    pub enabled: bool,
    /// Its size (Linux: the most it may hold of /dev/shm), MB.
    pub size_mb: u32,
    /// A RAM-backed folder of your own instead (a tmpfs mount); empty = the usual.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub dir: String,
}

impl Default for RamSpool {
    fn default() -> Self {
        RamSpool { enabled: false, size_mb: 256, dir: String::new() }
    }
}

/// A plugin, as the config has it: on or off, and its settings for the whole
/// recorder. Its settings for each system are in that system's `plugins`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PluginSetup {
    pub enabled: bool,
    /// Its settings, as its `config` schema describes them.
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub settings: serde_json::Value,
    /// Run this executable instead of the installed plugin (a build of your own).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Give it calls decrypted with the recorder's keys (their audio and files). Off: none are.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub share_decrypted: bool,
}

/// Plugins wait this long for some calls: those its fields pick, each left
/// out picking all. In the config's `pluginDelays` (every system) and in a
/// system's (that system).
///
/// ```json
/// "pluginDelays": [{ "plugins": ["openmhz"], "delayS": 120 }],
/// "systems": [{ "shortName": "dcfd", …, "pluginDelays": [
///     { "delayS": 600 },
///     { "talkgroups": ["1000-1099", "2*"], "delayS": 1800 },
///     { "plugins": ["openmhz"], "delayS": 0 } ] }]
/// ```
///
/// When several apply, one that narrows another (picks only what it picks:
/// a system's over every system's, one talkgroup's over its system's)
/// replaces it, and of the rest the longest wins (trunk-pro's
/// `plugins::delay` has the rules and examples).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DelayRule {
    /// The plugins it's for, by id (one copy of one: `openmhz#2`). Empty: every plugin.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
    /// The talkgroups it's for: numbers, ranges (`1000-1099`) and patterns
    /// (`507*`, `12?45`). Empty: every talkgroup (and a system's events that have none).
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "trunk_recorder_plugin::filter::patterns")]
    pub talkgroups: Vec<String>,
    /// Talkgroups it isn't for, of those `talkgroups` picks (all of them, when
    /// it's empty): the same kinds of entry. "All but 1001" is `talkgroups`
    /// empty and this `["1001"]`.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "trunk_recorder_plugin::filter::patterns")]
    pub except_talkgroups: Vec<String>,
    /// How long, s (0: none, as for something it narrows).
    pub delay_s: f64,
    /// Keys this version doesn't know (a newer version's, a hand edit's):
    /// kept, and saved back as they were.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

impl Default for Recording {
    fn default() -> Self {
        Recording {
            other: BTreeMap::new(),
            capture_dir: default_capture_dir().display().to_string(),
            preroll_s: 1.0,
            max_recorders: 32,
            call_timeout_s: 3.0,
            conversation_mode: true,
            record_unknown: true,
            record_encrypted: false,
            record_unit_to_unit: true,
            keep_silent_calls: false,
            min_call_s: 0.0,
            max_call_s: 0.0,
            min_transmission_s: 0.0,
            capture_frames: false,
            normalize_audio: true,
            digital_level_db: 0.0,
            analog_level_db: 0.0,
            archive_wav: true,
            archive_m4a: false,
            call_log: true,
            archive_files_on_failure: true,
            filename_format: String::new(),
            drop_duplicate_calls: true,
            vocoder: Vocoder::Fixed,
            m4a: M4a::default(),
            ram_spool: RamSpool::default(),
        }
    }
}

impl RecordingOverride {
    pub fn is_empty(&self) -> bool {
        *self == RecordingOverride::default()
    }
}

impl Recording {
    /// What the engine does with a finished call.
    fn save_rules(&self) -> SaveRules {
        SaveRules {
            keep_silent: self.keep_silent_calls,
            keep_encrypted: self.record_encrypted,
            min_call_s: self.min_call_s.max(0.0),
            min_transmission_s: self.min_transmission_s.max(0.0),
            normalize: self.normalize_audio,
            digital_gain_db: self.digital_level_db.clamp(-40.0, 40.0) as f32,
            analog_gain_db: self.analog_level_db.clamp(-40.0, 40.0) as f32,
        }
    }

    fn call_config(&self) -> CallConfig {
        CallConfig {
            call_timeout_s: self.call_timeout_s,
            record_unknown: self.record_unknown,
            record_encrypted: self.record_encrypted,
            record_unit_to_unit: self.record_unit_to_unit,
            new_call_from_update: true,
            max_call_s: self.max_call_s.max(0.0),
            conversation: self.conversation_mode,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Server {
    /// The address the recorder's interfaces listen on (all of them).
    pub bind: String,
    /// The built-in interface's port: the recorder's own, for whoever runs it.
    pub port: u16,
    /// Start recording when the app starts (headless machines, after a reboot).
    pub auto_start: bool,
    /// Web pages from other origins that may use the built-in interface's
    /// API (a page served elsewhere): "http://host:port" as the browser sends
    /// it, "null" a page opened from a file, "*" any. Its own pages and
    /// programs that aren't browsers always may.
    pub allowed_origins: Vec<String>,
    /// The other interfaces, by id: web pages installed from the interface
    /// store or folders of the user's own, each served on a port of its own
    /// with the recorder's API beside it (docs/interfaces).
    pub interfaces: BTreeMap<String, Interface>,
    /// Where the trunkrecorder.pro API is (the survey's license lookup):
    /// empty for https://trunkrecorder.pro; another for testing one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub trp_api: String,
    /// Keys this version doesn't know (a newer version's, a hand edit's):
    /// kept, and saved back as they were.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

impl Default for Server {
    fn default() -> Self {
        Server { bind: "127.0.0.1".into(), port: 8080, auto_start: false, allowed_origins: vec![], interfaces: BTreeMap::new(), trp_api: String::new(), other: BTreeMap::new() }
    }
}

impl Server {
    /// The trunkrecorder.pro API's base URL (no trailing slash).
    pub fn trp_api_base(&self) -> &str {
        let t = self.trp_api.trim().trim_end_matches('/');
        if t.is_empty() { "https://trunkrecorder.pro" } else { t }
    }

    /// The first port above the built-in interface's that nothing in the
    /// config uses (nor `taken`).
    pub fn free_port(&self, taken: &[u16]) -> u16 {
        let used = |p: u16| p == self.port || taken.contains(&p) || self.interfaces.values().any(|i| i.port == p);
        (self.port.saturating_add(1)..=u16::MAX).chain(1024..self.port).find(|p| !used(*p)).unwrap_or(0)
    }

    /// Give each interface without a port (0: one written by hand, or from
    /// before ports) the next free one.
    pub fn assign_ports(&mut self) {
        let ids: Vec<String> = self.interfaces.iter().filter(|(_, i)| i.port == 0).map(|(id, _)| id.clone()).collect();
        for id in ids {
            let p = self.free_port(&[]);
            self.interfaces.get_mut(&id).unwrap().port = p;
        }
    }

    /// Why interface `id` can't be served as the config has it, if it can't
    /// (its id, or its port: the built-in one's, or another's before it).
    pub fn interface_problem(&self, id: &str) -> Option<String> {
        let i = self.interfaces.get(id)?;
        if !Interface::valid_id(id) {
            return Some(format!("\"{id}\" can't be an interface's id: use lowercase letters, digits and -."));
        }
        if i.port == 0 {
            return Some("It needs a port.".into());
        }
        if i.port == self.port {
            return Some(format!("Port {} is the built-in interface's.", i.port));
        }
        if let Some((other, _)) = self.interfaces.iter().find(|(o, x)| x.port == i.port && x.enabled && o.as_str() < id) {
            return Some(format!("Port {} is {other}'s too.", i.port));
        }
        None
    }
}

/// An interface served besides the built-in one: its own web page (static
/// files, run in the browser), on a port of its own. The recorder's API is
/// served beside it, so the page reaches it at `/api/` as it would on the
/// built-in interface's port, with what `access` allows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Interface {
    /// Its port (0: the next free one is given it on load).
    pub port: u16,
    /// Served (false: kept, but not served).
    pub enabled: bool,
    /// What the page may do through the API.
    pub access: ApiAccess,
    /// A folder of the user's own, absolute, `~/…` or relative to the config
    /// file's folder; empty: the copy installed from the store.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub path: String,
}

impl Default for Interface {
    fn default() -> Self {
        Interface { port: 0, enabled: true, access: ApiAccess::Listen, path: String::new() }
    }
}

impl Interface {
    /// An id is its folder's name too: lowercase letters, digits and -.
    pub fn valid_id(id: &str) -> bool {
        !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !id.starts_with('-')
    }
}

choice! {
    /// What a page may do with the recorder's API.
    #[derive(PartialOrd, Ord)]
    pub enum ApiAccess {
        /// See what the recorder does and play its calls, live and recorded;
        /// change nothing. Its settings for plugins and keys aren't shown.
        #[default]
        Listen = "listen",
        /// Everything the built-in interface can do: change the config, start
        /// and stop, install plugins, quit.
        Control = "control",
    }
}

/// A new RTL-SDR's gain, dB. Higher gains overload the front end near
/// strong transmitters without clipping the ADC: on an 858 MHz simulcast
/// system 38.6 dB gave ~3x the FEC errors (and 5 % repeated voice frames)
/// that 23–28 dB did.
pub const RTL_DEFAULT_GAIN_DB: f64 = 25.4;

/// A system calls can come from ([`Config::call_systems`]).
#[derive(Clone, Copy, Debug)]
pub struct CallSystem<'a> {
    /// The number its calls carry (`Call::system`).
    pub index: u16,
    pub short_name: &'a str,
    /// "p25", "smartnet", "dmr", "nxdn", "edacs" or "conventional".
    pub kind: &'static str,
    /// Its own recording rules (over the config's).
    pub recording: &'a RecordingOverride,
    /// Its plugin settings, by plugin id.
    pub plugins: &'a BTreeMap<String, serde_json::Value>,
    /// How long plugins wait for its calls.
    pub plugin_delays: &'a [DelayRule],
}

/// Where a set of encryption keys comes from (see [`crate::keys`]). Only file
/// names: the keys themselves are never in the config.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct KeySource {
    /// A keys file (JSON) or a KFDtool key container (`.ekc`), absolute or
    /// relative to the config file's folder; which, is read from the file.
    pub file: String,
    /// A container's password, in a file of its own. Empty: none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub password_file: String,
    /// Keys this version doesn't know (a newer version's, a hand edit's):
    /// kept, and saved back as they were.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

/// A key source as [`Config::load_keys`] read it, and the systems that use it.
pub struct LoadedKeys {
    pub name: String,
    pub systems: Vec<String>,
    /// None: a container with no `passwordFile`, not unlocked yet (see [`Config::lockable`]).
    pub ring: Option<std::sync::Arc<trunk_core::p25::crypt::Keyring>>,
    /// Container keys left out, as "name (reason)" (never a key).
    pub skipped: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub sources: Vec<Source>,
    /// The trunked systems, each with its sites.
    pub systems: Vec<System>,
    /// The conventional systems (numbered after the trunked ones: [`Config::call_systems`]).
    pub conventional: Vec<Conventional>,
    pub recording: Recording,
    pub server: Server,
    /// The plugins, by id: on or off, and their settings for the whole recorder.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, PluginSetup>,
    /// How long plugins wait for calls, for every system ([`DelayRule`]);
    /// a system's own come first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugin_delays: Vec<DelayRule>,
    /// Encryption key sources, by name; a system names one in its `keys`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, KeySource>,
    /// The log: how much, where to, and how lines read.
    pub log: LogSettings,
    /// The call store (desktop app).
    pub store: StoreSettings,
    /// The dashboard's watch on the computer (desktop app).
    pub monitor: MonitorSettings,
    /// Keys this version doesn't know (a newer version's, a hand edit's):
    /// kept, and saved back as they were.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
    /// What reading it changed (an older config upgraded: [`crate::migrate`]), to log.
    #[serde(skip)]
    pub notes: Vec<String>,
}

/// The log (desktop app), with Trunk Recorder's options: `logLevel`,
/// `consoleLog`, `logFile`, `logDir`, `syslogFriendly`, `logColor`,
/// `frequencyFormat`, `talkgroupDisplayFormat`, `statusAsString`,
/// `controlWarnRate` — and `syslog`, the system log too.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LogSettings {
    pub level: crate::log::Level,
    /// To the console (stderr).
    pub console: bool,
    /// To files in `dir`: a new one each day and at 100 MB, as Trunk Recorder names them.
    pub file: bool,
    /// Absolute, or relative to the config file's folder; empty = `logs` there.
    pub dir: String,
    /// One file, `trunk-pro.log`, appended to and never rotated here (for
    /// logrotate: SIGHUP reopens it).
    pub syslog_friendly: bool,
    /// To the system log too (syslog; Linux and macOS).
    pub syslog: bool,
    /// ANSI colour.
    pub color: LogColor,
    pub frequency_format: crate::log::FrequencyFormat,
    pub talkgroup_display_format: crate::log::TalkgroupFormat,
    pub status_as_string: bool,
    /// A control channel decoding fewer messages a second than this is
    /// logged as an error; −1 logs the rate always.
    pub control_warn_rate_per_s: f64,
    /// Keys this version doesn't know (a newer version's, a hand edit's):
    /// kept, and saved back as they were.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

impl Default for LogSettings {
    fn default() -> Self {
        LogSettings {
            other: BTreeMap::new(),
            level: crate::log::Level::Info,
            console: true,
            file: false,
            dir: String::new(),
            syslog_friendly: false,
            syslog: false,
            color: LogColor::Auto,
            frequency_format: Default::default(),
            talkgroup_display_format: Default::default(),
            status_as_string: true,
            control_warn_rate_per_s: 10.0,
        }
    }
}

impl LogSettings {
    pub fn format(&self) -> crate::log::Format {
        crate::log::Format { frequency: self.frequency_format, talkgroup: self.talkgroup_display_format, status_as_string: self.status_as_string }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            other: BTreeMap::new(),
            sources: vec![Source::Rtlsdr { serial: String::new(), center_hz: 0.0, rate_hz: 2_400_000.0, gain_db: RTL_DEFAULT_GAIN_DB, agc: false, ppm: 0, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ, buf_len: 0, buffers: 0 }],
            systems: vec![],
            conventional: vec![],
            recording: Recording::default(),
            server: Server::default(),
            plugins: BTreeMap::new(),
            plugin_delays: vec![],
            keys: BTreeMap::new(),
            log: LogSettings::default(),
            store: StoreSettings::default(),
            monitor: MonitorSettings::default(),
            notes: vec![],
        }
    }
}

/// The call store: the index of recorded calls in `trunk-pro.sqlite`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct StoreSettings {
    /// Days of call records kept in the call store; 0 keeps everything.
    pub retention_days: u32,
}

impl Default for StoreSettings {
    fn default() -> Self {
        StoreSettings { retention_days: 120 }
    }
}

/// What the dashboard watches on the computer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MonitorSettings {
    /// Where to check the internet answers (`host:port`, a TCP connect every
    /// 15 s): the uplink's history. Empty: no checks.
    pub probe_hosts: Vec<String>,
    /// Off: neither the internet checks nor the time server are asked,
    /// whatever's set (the dashboard's Network switch). Applies at once.
    pub network_checks: bool,
    /// The time server asked how far off the clock is (SNTP, every 15
    /// minutes; a name or address, port 123 unless given). It only measures:
    /// the operating system's time service keeps the clock. Empty: not asked.
    pub time_server: String,
    /// The clock off by this much or more, ms: a warning, and calls marked
    /// `clock_off` (a problem at 5 s, or 4 times this). Applies at once.
    pub clock_warn_ms: f64,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        MonitorSettings { probe_hosts: vec!["1.1.1.1:443".into(), "dns.google:443".into()], network_checks: true, time_server: "pool.ntp.org".into(), clock_warn_ms: 250.0 }
    }
}

/// The user's home folder.
pub fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// `~/Library/Application Support/trunk-pro`, `%APPDATA%\trunk-pro`, or
/// `$XDG_CONFIG_HOME/trunk-pro` (`~/.config/trunk-pro`).
pub fn config_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/trunk-pro")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join("trunk-pro")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("trunk-pro")
    }
}

/// Write `data` to `path` so that a crash or power cut leaves the old file
/// or the new one, never part of either: into a temporary file beside it,
/// flushed to disk, then renamed over it.
pub fn write_atomic(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    let name = path.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(data.as_ref())?;
        f.sync_all()
    });
    match written.and_then(|_| std::fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub fn default_capture_dir() -> PathBuf {
    home().join("TrunkRecorderPro")
}

/// A source's `type` names its kind (the tag of [`Source`]), read in any case
/// as the other closed choices are (`"RTLSDR"` is `"rtlsdr"`).
const SOURCE_TYPES: [&str; 6] = ["rtlsdr", "usrp", "airspy", "bladerf", "soapy", "file"];

fn source_types(v: &mut serde_json::Value) {
    for s in v.get_mut("sources").and_then(serde_json::Value::as_array_mut).into_iter().flatten() {
        let Some(t) = s.get_mut("type") else { continue };
        if let Some(name) = t.as_str().and_then(|t| SOURCE_TYPES.into_iter().find(|n| n.eq_ignore_ascii_case(t.trim()))) {
            *t = name.into();
        }
    }
}

impl Config {
    /// A call sign heard on the air on `freq_hz` (Motorola's base station
    /// ID: what a control channel says of a channel, or a voice channel of
    /// itself), into system `short_name`'s (trunked or conventional)
    /// [`FccLicenses`]: over a lookup's or another heard there, never over
    /// the user's. Whether it changed.
    /// The directory's answer to a share of system `short_name`: its ID and
    /// version there, when, and its sites' IDs (`site_ids`, in the order of
    /// the share's sites, named `site_names`). Whether the config changed.
    /// Link system `short_name` (and its sites, by name) to the directory's
    /// records, as a share's answer named them. True when that changed the
    /// config: only a first share, or a site new to the directory, does.
    pub fn link_shared(&mut self, short_name: &str, id: &str, site_names: &[String], site_ids: &[String]) -> bool {
        let Some(sys) = self.systems.iter_mut().find(|s| s.short_name == short_name) else { return false };
        let before = sys.clone();
        sys.directory = Some(DirectoryLink { id: id.to_string() });
        for (name, sid) in site_names.iter().zip(site_ids) {
            let names: Vec<String> = sys.sites.iter().map(|t| sys.site_name(t).to_string()).collect();
            if let Some(k) = names.iter().position(|n| n == name) {
                sys.sites[k].directory_id = sid.clone();
            }
        }
        *sys != before
    }

    pub fn learn_call_sign(&mut self, short_name: &str, freq_hz: f64, call_sign: &str) -> bool {
        if !plausible_call_sign(call_sign) || freq_hz <= 0.0 {
            return false;
        }
        let fcc = match self.systems.iter_mut().find(|s| s.short_name == short_name) {
            Some(s) => &mut s.fcc,
            None => match self.conventional.iter_mut().find(|c| c.short_name == short_name) {
                Some(c) => &mut c.fcc,
                None => return false,
            },
        };
        let e = fcc.entry(fcc_key(freq_hz)).or_default();
        if (e.from == FccFrom::User && !e.call_sign.is_empty()) || (e.from == FccFrom::Air && e.call_sign == call_sign) {
            return false;
        }
        // A lookup's name is the heard call sign's only when it named the same one.
        let name = if e.call_sign == call_sign { std::mem::take(&mut e.name) } else { String::new() };
        *e = FccLicense { call_sign: call_sign.to_string(), name, from: FccFrom::Air };
        true
    }

    /// The config at `path`, with a linked channel file read in; the
    /// defaults when there's no file yet. A file that isn't a config is an
    /// error, not the defaults (which would be saved over it).
    pub fn load(path: &Path) -> Result<Config, String> {
        // An older config's talkgroup files are written beside it first: whoever
        // reads it (the desktop app, its command lines) then saves it without them.
        crate::migrate::move_talkgroups(path).map_err(|e| format!("{}: couldn't move its talkgroup files out of it ({e})", path.display()))?;
        let mut c: Config = match std::fs::read_to_string(path) {
            Ok(s) => Config::from_json(&s).map_err(|e| format!("{}: not a config this version reads ({e})", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        c.load_channel_files(path);
        c.server.assign_ports();
        Ok(c)
    }

    /// A config from its JSON, an older one upgraded ([`crate::migrate`];
    /// what that changed is in [`Config::notes`]).
    pub fn from_json(s: &str) -> Result<Config, serde_json::Error> {
        Config::from_value(serde_json::from_str(s)?)
    }

    /// [`Config::from_json`], from its JSON value.
    pub fn from_value(mut v: serde_json::Value) -> Result<Config, serde_json::Error> {
        let notes = crate::migrate::config(&mut v);
        source_types(&mut v);
        let mut c: Config = serde_json::from_value(v)?;
        c.notes = notes;
        Ok(c)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut c = self.clone();
        for v in &mut c.conventional {
            v.channel_file_status.clear();
            if !v.channel_file.is_empty() {
                // The file holds them.
                v.channels.clear();
            }
        }
        write_atomic(path, serde_json::to_string_pretty(&c).unwrap_or_default())
    }

    /// Conventional system `k`'s linked channel file's location (relative
    /// paths from the config file's folder), or None.
    pub fn channel_file_path(&self, k: usize, config_path: &Path) -> Option<PathBuf> {
        let f = self.conventional.get(k)?.channel_file.trim();
        if f.is_empty() {
            return None;
        }
        let p = PathBuf::from(f);
        Some(if p.is_absolute() { p } else { config_path.parent().unwrap_or(Path::new(".")).join(p) })
    }

    /// Some system that records names encryption keys.
    pub fn has_keys(&self) -> bool {
        self.active_systems().any(|s| !s.keys.trim().is_empty()) || self.conventional.iter().any(|c| c.enabled && !c.keys.trim().is_empty())
    }

    /// Read the key sources the systems name, each once: its keys and the
    /// systems that use them, for [`crate::session::Session::set_keys`]. A
    /// source no system names isn't read. An error names the source, the
    /// file and what is wrong in it (never a key). A container with no
    /// `passwordFile` is locked: its keys are `unlocked`'s, if it has them.
    pub fn load_keys(&self, config_path: &Path, unlocked: &dyn Fn(&str) -> Option<std::sync::Arc<trunk_core::p25::crypt::Keyring>>) -> Result<Vec<LoadedKeys>, String> {
        let mut out: Vec<LoadedKeys> = Vec::new();
        for (system, name) in self.key_users() {
            if let Some(l) = out.iter_mut().find(|l| l.name == name) {
                l.systems.push(system.clone());
                continue;
            }
            let src = self.keys.get(name).ok_or_else(|| format!("{system}: no key source \"{name}\" (keys)"))?;
            let file = crate::keys::path(&src.file, config_path).ok_or_else(|| format!("Key source {name}: no file"))?;
            let (ring, skipped) = if src.password_file.trim().is_empty() && crate::keys::is_container(&file) {
                (unlocked(name), vec![])
            } else {
                let l = crate::keys::load(&file, crate::keys::path(&src.password_file, config_path).as_deref()).map_err(|e| format!("Key source {name}: {e}"))?;
                (Some(std::sync::Arc::new(l.ring)), l.skipped)
            };
            out.push(LoadedKeys { name: name.to_string(), systems: vec![system.clone()], ring, skipped });
        }
        Ok(out)
    }

    /// (A system that records, the key source it names), for each that names one.
    fn key_users(&self) -> impl Iterator<Item = (&String, &str)> {
        let trunked = self.active_systems().map(|s| (&s.short_name, s.keys.trim()));
        trunked.chain(self.conventional.iter().filter(|c| c.enabled).map(|c| (&c.short_name, c.keys.trim()))).filter(|(_, k)| !k.is_empty())
    }

    /// The key sources the systems name that are unlocked from the interface (a
    /// container with no `passwordFile`): name, file (resolved), the systems.
    pub fn lockable(&self, config_path: &Path) -> Vec<(String, PathBuf, Vec<String>)> {
        let mut out: Vec<(String, PathBuf, Vec<String>)> = Vec::new();
        for (system, name) in self.key_users() {
            if let Some(l) = out.iter_mut().find(|l| l.0 == name) {
                l.2.push(system.clone());
                continue;
            }
            let Some(src) = self.keys.get(name).filter(|s| s.password_file.trim().is_empty()) else { continue };
            if let Some(file) = crate::keys::path(&src.file, config_path).filter(|f| crate::keys::is_container(f)) {
                out.push((name.to_string(), file, vec![system.clone()]));
            }
        }
        out
    }

    /// The keys files of the key sources the systems name (absolute).
    pub fn key_files(&self, config_path: &Path) -> Vec<PathBuf> {
        let named: Vec<&str> = self.key_users().map(|(_, k)| k).collect();
        self.keys.iter().filter(|(n, _)| named.contains(&n.as_str())).filter_map(|(_, s)| crate::keys::path(&s.file, config_path)).collect()
    }

    /// Read every conventional system's linked channel file (errors are in
    /// each one's `channel_file_status`).
    pub fn load_channel_files(&mut self, config_path: &Path) {
        for k in 0..self.conventional.len() {
            let _ = self.load_channel_file(k, config_path);
        }
    }

    /// Read conventional system `k`'s linked channel file into its
    /// `channels` (a no-op when none is linked). On an error the channels
    /// are left as they were; either way `channel_file_status` says what happened.
    pub fn load_channel_file(&mut self, k: usize, config_path: &Path) -> Result<(), String> {
        let path = self.channel_file_path(k, config_path);
        let Some(v) = self.conventional.get_mut(k) else { return Err(format!("No conventional system {}", k + 1)) };
        let Some(p) = path else {
            v.channel_file_status.clear();
            return Ok(());
        };
        let r = std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| crate::channels::parse(&t));
        match r {
            Ok(parsed) => {
                let n = parsed.channels.len();
                v.channels = parsed.channels;
                v.channel_file_status = format!("{n} channel{} read.{}", if n == 1 { "" } else { "s" }, parsed.notes.iter().map(|x| format!(" {x}")).collect::<String>());
                Ok(())
            }
            Err(e) => {
                let msg = format!("Channel file {}: {e}", p.display());
                v.channel_file_status = msg.clone();
                Err(msg)
            }
        }
    }

    /// Link conventional system `k`'s channels to a CSV at `path` (created
    /// from its current list if it doesn't exist yet), or unlink with "" —
    /// the channels then stay in the config, as last read.
    pub fn link_channel_file(&mut self, k: usize, config_path: &Path, path: &str) -> Result<(), String> {
        let Some(v) = self.conventional.get_mut(k) else { return Err(format!("No conventional system {}", k + 1)) };
        let old = std::mem::replace(&mut v.channel_file, path.trim().to_string());
        let Some(p) = self.channel_file_path(k, config_path) else {
            self.conventional[k].channel_file_status.clear();
            return Ok(());
        };
        if !p.exists() {
            let made = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&p, crate::channels::write(&self.conventional[k].channels)));
            if let Err(e) = made {
                self.conventional[k].channel_file = old;
                return Err(format!("Couldn't create {}: {e}", p.display()));
            }
        }
        if let Err(e) = self.load_channel_file(k, config_path) {
            self.conventional[k].channel_file = old;
            return Err(e);
        }
        Ok(())
    }

    /// The conventional channels being recorded: switched on, in a system that is.
    pub fn enabled_channels(&self) -> impl Iterator<Item = &Channel> {
        self.conventional.iter().filter(|v| v.enabled).flat_map(|v| v.channels.iter().filter(|c| c.enabled))
    }

    /// The engine's conventional channels: each with its system's number,
    /// and its system's squelch unless it has its own.
    /// The conventional channels being recorded, each with its system's
    /// number ([`Config::call_systems`]).
    fn engine_channels(&self) -> Vec<ConvChannel> {
        let mut out = Vec::new();
        let first = self.active_systems().count();
        for (k, v) in self.active_conventional().enumerate() {
            let tgs = default_talkgroups(&v.channels);
            for (c, tg) in v.channels.iter().zip(tgs).filter(|(c, _)| c.enabled) {
                let mut e = c.engine_channel(tg);
                e.system = first + k;
                e.squelch_db = Some(e.squelch_db.unwrap_or(v.squelch_db));
                e.analog = v.demod.tuning(c);
                out.push(e);
            }
        }
        out
    }

    /// What conventional rows name talkgroups known before a call is heard
    /// ([`ConvChannel::names_ahead`]), by system short name: the dashboard's
    /// names for them, ahead of the talkgroup file's, as a call's are. (A
    /// number that comes from the air is named by the calls that carry it.)
    pub fn row_talkgroups(&self) -> HashMap<String, Talkgroups> {
        let names: Vec<&str> = self.active_systems().map(|s| s.short_name.as_str()).chain(self.active_conventional().map(|v| v.short_name.as_str())).collect();
        let mut out: HashMap<String, Talkgroups> = HashMap::new();
        for c in self.engine_channels() {
            if let (Some(t), Some(name)) = (c.names_ahead(), names.get(c.system)) {
                out.entry(name.to_string()).or_default().entry(t.number).or_insert_with(|| t.clone());
            }
        }
        out
    }

    /// The systems being recorded (enabled, with a site that is), in the
    /// engine's order — a call's `system` indexes this.
    pub fn active_systems(&self) -> impl Iterator<Item = &System> {
        self.systems.iter().filter(|s| s.active())
    }

    /// The conventional systems being recorded (enabled, with a channel
    /// that is), in order: numbered after the trunked ones.
    pub fn active_conventional(&self) -> impl Iterator<Item = &Conventional> {
        self.conventional.iter().filter(|v| v.enabled && v.channels.iter().any(|c| c.enabled))
    }

    /// Advice for 4 or more RTL-SDRs at 2 MS/s or more left on the driver's
    /// 256 KiB USB transfers: on one USB 2.0 bus each dongle waits too long
    /// for its turn and its small FIFO overflows (losses the driver can't
    /// see; the control channel suffers first, IMBE hides most of it in voice).
    /// `most_on_a_bus`: the most RTL-SDRs plugged into one USB bus, if known
    /// (spread over buses, there's nothing to say).
    pub fn rtl_buflen_hint(&self, most_on_a_bus: Option<usize>) -> Option<String> {
        let busy = self.sources.iter().filter(|s| matches!(s, Source::Rtlsdr { rate_hz, buf_len: 0, .. } if *rate_hz >= RTL_BUSY_RATE)).count();
        if busy < RTL_BUSY_COUNT || most_on_a_bus.is_some_and(|n| n < RTL_BUSY_COUNT) {
            return None;
        }
        let bus = match most_on_a_bus {
            Some(n) => format!(", {n} of them on one USB bus"),
            None => String::new(),
        };
        Some(format!(
            "{busy} RTL-SDRs at 2 MS/s or more with the default 256 KiB USB transfers{bus}: dongles sharing a USB 2.0 bus can lose samples (an unreliable control channel is the usual sign). Set USB transfer size to 64 KiB on each in Setup → Sources (Trunk Recorder's buflen=65536)."
        ))
    }

    /// The sites being recorded, each with its system, in the engine's order
    /// — a call's `site` indexes this.
    pub fn active_sites(&self) -> impl Iterator<Item = (&System, &Site)> {
        self.active_systems().flat_map(|s| s.active_sites().map(move |x| (s, x)))
    }

    /// The site a call's `site` names, with its system (None: conventional).
    pub fn site_of(&self, site: u16) -> Option<(&System, &Site)> {
        self.active_sites().nth(site as usize)
    }

    /// Each source's centre: as set, or (0 = auto) placed over what the
    /// sources before it don't cover yet — every system (control and known
    /// voice channels) and the conventional channels if they fit together,
    /// else the control and conventional channels, else the first uncovered
    /// system, else its control channels alone.
    pub fn resolved_centers(&self) -> Vec<f64> {
        // Groups to cover: (all its channels, the ones it can't do without).
        let mut groups: Vec<(Vec<f64>, Vec<f64>)> = self
            .active_sites()
            .map(|(sys, s)| {
                // A DMR / NXDN site's watched frequencies are all needed.
                let watched = s.watched_channels(sys);
                let need: Vec<f64> = s.control_channels_hz.iter().chain(&watched).copied().collect();
                (need.iter().chain(&s.voice_channels_hz).copied().collect(), need)
            })
            .collect();
        for v in self.conventional.iter().filter(|v| v.enabled) {
            let conv: Vec<f64> = v.channels.iter().filter(|c| c.enabled).map(|c| c.freq_hz).filter(|&f| f > 0.0).collect();
            if !conv.is_empty() {
                groups.push((conv.clone(), conv));
            }
        }
        let mut centers: Vec<f64> = self.sources.iter().map(|s| s.center_hz()).collect();
        let covered = |centers: &[f64], f: f64| self.sources.iter().zip(centers).any(|(s, &c)| c > 0.0 && (f - c).abs() <= s.usable_half_width());
        for i in 0..self.sources.len() {
            if centers[i] > 0.0 {
                continue;
            }
            let open: Vec<&(Vec<f64>, Vec<f64>)> = groups.iter().filter(|(_, need)| !need.iter().any(|&f| covered(&centers, f))).collect();
            let Some(first) = open.first() else { break };
            let (rate, guard) = (self.sources[i].rate_hz(), self.sources[i].guard_hz());
            let all: Vec<f64> = open.iter().flat_map(|g| g.0.iter().copied()).collect();
            let needed: Vec<f64> = open.iter().flat_map(|g| g.1.iter().copied()).collect();
            centers[i] = auto_center(&all, rate, guard)
                .or_else(|| auto_center(&needed, rate, guard))
                .or_else(|| auto_center(&first.0, rate, guard))
                .or_else(|| auto_center(&first.1, rate, guard))
                .unwrap_or(0.0);
        }
        centers
    }

    /// A short name is a system's identity — its folder, its band plan and
    /// talker aliases, its plugin settings — so every system, trunked or
    /// conventional, on or off, has one of its own.
    pub fn name_problem(&self) -> Option<String> {
        let mut names = std::collections::HashSet::new();
        let all = self.systems.iter().map(|s| s.short_name.as_str()).chain(self.conventional.iter().map(|v| v.short_name.as_str()));
        for n in all {
            if n.trim().is_empty() {
                return Some("Every system needs a short name.".into());
            }
            if !names.insert(n) {
                return Some(format!("Two systems are named \"{n}\" — each needs a short name of its own (it is its folder, and how plugins know it)."));
            }
        }
        // A site's name is its own in its system (its band plan's file, the talkgroup file's Preferred Site).
        for sys in &self.systems {
            let mut seen = std::collections::HashSet::new();
            if let Some(site) = sys.sites.iter().find(|x| !seen.insert(sys.site_name(x))) {
                return Some(format!("Two sites of {} are named \"{}\" — each needs a name of its own.", sys.short_name, sys.site_name(site)));
            }
        }
        None
    }

    /// Why this config can't start, or None.
    pub fn problem(&self) -> Option<String> {
        if self.sources.is_empty() {
            return Some("Add a source (a dongle or a capture file).".into());
        }
        if self.sources.iter().any(|s| matches!(s, Source::File { path, .. } if path.trim().is_empty())) {
            return Some("Choose the capture file to replay.".into());
        }
        for (i, s) in self.sources.iter().enumerate() {
            if let Source::Rtlsdr { buf_len, buffers, .. } = *s {
                if buf_len % 512 != 0 || buf_len > 16 << 20 {
                    return Some(format!("Source {}: the USB transfer size must be a multiple of 512 bytes, at most 16 MiB (65536 for several dongles on one bus).", i + 1));
                }
                if buffers > 256 {
                    return Some(format!("Source {}: at most 256 USB transfers in flight.", i + 1));
                }
            }
        }
        let trunked = self.active_systems().next().is_some();
        if !trunked && self.enabled_channels().next().is_none() {
            return Some("Add a system with a control channel, or a conventional channel.".into());
        }
        if let Some(p) = self.name_problem() {
            return Some(p);
        }
        let signals = self.systems.iter().map(|s| (&s.short_name, &s.signals)).chain(self.conventional.iter().map(|c| (&c.short_name, &c.signals)));
        if let Some(p) = signals.filter_map(|(n, d)| signals_problem(n, d)).next() {
            return Some(p);
        }
        let centers = self.resolved_centers();
        if let Some(i) = centers.iter().position(|&c| c <= 0.0) {
            return Some(format!(
                "Set a center frequency for source {} — it couldn't be placed automatically (nothing left for it to cover, or the channels don't fit one source).",
                i + 1
            ));
        }
        let inside = |f: f64| self.sources.iter().zip(&centers).any(|(s, &c)| (f - c).abs() <= s.usable_half_width());
        for (sys, s) in self.active_sites() {
            // "dcfd", or "dcfd (site south)" for a site with its own name.
            let key = trunk_core::trunk::SiteKey { system: &sys.short_name, site: sys.site_name(s) };
            let name = key.own_name().map_or_else(|| sys.short_name.clone(), |n| format!("{} (site {n})", sys.short_name));
            if sys.is_smartnet() {
                if let Err(e) = s.smartnet(sys) {
                    return Some(format!("{name}: {e}"));
                }
            }
            if sys.is_dmr() || sys.is_nxdn() || sys.is_edacs() {
                let out: Vec<String> = s.control_channels_hz.iter().chain(&s.watched_channels(sys)).filter(|&&f| !inside(f)).map(|f| format!("{:.5}", f / 1e6)).collect();
                if !out.is_empty() {
                    let what = if sys.is_dmr() { "DMR" } else if sys.is_nxdn() { "NXDN" } else { "EDACS" };
                    return Some(format!("{name}: {what} frequencies outside every source's bandwidth: {} MHz — move a center frequency or add a source.", out.join(", ")));
                }
            }
            if !s.control_channels_hz.iter().any(|&f| inside(f)) {
                return Some(format!("No control channel of {name} falls inside any source's bandwidth — move a center frequency or add a source."));
            }
            if sys.is_edacs() && s.edacs().lcns.is_empty() {
                return Some(format!("{name}: an EDACS site needs its channel list — each LCN (1–25) and its frequency."));
            }
        }
        // A frequency belongs to one conventional system.
        let mut owner: Vec<(f64, &str)> = Vec::new();
        for v in self.conventional.iter().filter(|v| v.enabled) {
            for c in v.channels.iter().filter(|c| c.enabled) {
                if let Some((_, other)) = owner.iter().find(|(f, o)| (f - c.freq_hz).abs() < 1.0 && *o != v.short_name) {
                    return Some(format!("Conventional channel {:.5} MHz is in both {other} and {} — a frequency belongs to one conventional system.", c.freq_hz / 1e6, v.short_name));
                }
                owner.push((c.freq_hz, &v.short_name));
            }
        }
        if self.enabled_channels().any(|c| c.freq_hz <= 0.0) {
            return Some("A conventional channel has no frequency yet.".into());
        }
        let outside: Vec<String> = self.enabled_channels().filter(|c| !inside(c.freq_hz)).map(|c| format!("{:.5}", c.freq_hz / 1e6)).collect();
        if !outside.is_empty() {
            return Some(format!("Conventional channel(s) outside every source's bandwidth: {} MHz — move a center frequency or disable them.", outside.join(", ")));
        }
        if let Some((c, e)) = self.enabled_channels().find_map(|c| c.parsed_access().err().map(|e| (c, e))) {
            return Some(format!("Conventional channel {:.5} MHz: {e}.", c.freq_hz / 1e6));
        }
        let (lo, hi) = BANDWIDTH_KHZ;
        if let Some(c) = self.enabled_channels().find(|c| c.bandwidth_khz.is_some_and(|b| !(lo..=hi).contains(&b))) {
            return Some(format!("Conventional channel {:.5} MHz: a bandwidth is {lo}–{hi} kHz.", c.freq_hz / 1e6));
        }
        if let Some((v, e)) = self.conventional.iter().filter(|v| v.enabled).find_map(|v| v.demod.problem().map(|e| (v, e))) {
            return Some(format!("{}: {e}", v.short_name));
        }
        // File name formats: the config's, and each system's own.
        if let Some(e) = crate::filename::problem(&self.recording.filename_format) {
            return Some(format!("File name format: {e}"));
        }
        for s in self.call_systems() {
            if let Some(e) = s.recording.filename_format.as_deref().and_then(crate::filename::problem) {
                return Some(format!("{}'s file name format: {e}", s.short_name));
            }
        }
        check_channels(&self.engine_channels()).err()
    }

    /// Each system's recording rules (its own on the config's), by the
    /// number calls give it ([`Config::call_systems`]): worked out once when
    /// recording starts.
    pub fn recordings(&self) -> Vec<Recording> {
        self.call_systems().iter().map(|s| self.recording.with(s.recording)).collect()
    }

    /// Every system a call can come from, numbered as calls number them
    /// (`Call::system`): the active trunked systems, then the active
    /// conventional ones. The one place that number is worked out for a
    /// config — the engine, plugins, status and the interface all go by it.
    pub fn call_systems(&self) -> Vec<CallSystem<'_>> {
        let trunked = self.active_systems().map(|s| (&s.short_name, s.kind.name(), &s.recording, &s.plugins, &s.plugin_delays[..]));
        let conventional = self.active_conventional().map(|c| (&c.short_name, "conventional", &c.recording, &c.plugins, &c.plugin_delays[..]));
        trunked
            .chain(conventional)
            .enumerate()
            .map(|(i, (short_name, kind, recording, plugins, plugin_delays))| CallSystem { index: i as u16, short_name, kind, recording, plugins, plugin_delays })
            .collect()
    }

    /// The short name of the system a call's `system` names.
    pub fn short_name_of(&self, system: u16) -> Option<&str> {
        self.active_systems().map(|s| s.short_name.as_str()).chain(self.active_conventional().map(|c| c.short_name.as_str())).nth(system as usize)
    }

    /// The engine's config, with each system's talkgroup file (`talkgroups`:
    /// its CSV by short name; kept beside the config, not in it).
    pub fn engine_config(&self, epoch_ms: f64, talkgroups: &TalkgroupFiles) -> EngineConfig {
        let tg = |short: &str| talkgroups.get(short).map_or_else(Default::default, |csv| parse_csv(csv));
        let centers = self.resolved_centers();
        // (Keys are read by the app: [`Config::load_keys`].)
        let mut systems: Vec<SystemConfig> = self
            .active_systems()
            .map(|sys| {
                let rules = self.recording.with(&sys.recording);
                let sites = sys
                    .active_sites()
                    .map(|s| SiteConfig { name: sys.site_name(s).to_string(), control_channels: s.control_channels_hz.clone(), bank: s.bank(), expect: s.expect.engine(), protocol: s.protocol(sys) })
                    .collect();
                SystemConfig {
                    calls: rules.call_config(),
                    save: rules.save_rules(),
                    talkgroups: tg(&sys.short_name),
                    unit_tags: sys.unit_names.engine(),
                    signals: engine_signals(&sys.signals),
                    ..SystemConfig::trunked(&sys.short_name, sites)
                }
            })
            .collect();
        // A conventional system's P25 / DMR / NXDN channels name the talkgroups
        // they hear by its own talkgroup file (a row's own name comes from the
        // channel list).
        for v in self.active_conventional() {
            let r = self.recording.with(&v.recording);
            systems.push(SystemConfig {
                calls: r.call_config(),
                save: r.save_rules(),
                talkgroups: tg(&v.short_name),
                unit_tags: v.unit_names.engine(),
                signals: engine_signals(&v.signals),
                ..SystemConfig::conventional(&v.short_name)
            });
        }
        EngineConfig {
            systems,
            sources: self.sources.iter().zip(centers).map(|(s, c)| SourceConfig { center_hz: c, rate_hz: s.rate_hz(), auto_tune: s.auto_tune(), guard_hz: s.guard_hz() }).collect(),
            preroll_s: self.recording.preroll_s,
            max_recorders: self.recording.max_recorders,
            epoch_ms_at_zero: epoch_ms,
            bank: BankConfig::default(),
            conventional: self.engine_channels(),
            conv: ConvConfig::default(),
            capture_frames: self.recording.capture_frames,
            drop_duplicates: self.recording.drop_duplicate_calls,
            vocoder: self.recording.vocoder.engine(),
        }
    }
}


/// Each system's talkgroup file (Trunk Recorder's talkgroup CSV), by its
/// short name. Kept apart from the config: the desktop app beside it
/// (`<short name>.talkgroups.csv`), the browser in its storage; the
/// interface edits one with `setTalkgroups`.
pub type TalkgroupFiles = BTreeMap<String, String>;

/// Where the desktop app keeps system `short_name`'s talkgroup file: beside
/// the config (in `dir`).
pub fn talkgroups_path(dir: &Path, short_name: &str) -> PathBuf {
    dir.join(format!("{short_name}.talkgroups.csv"))
}

/// A centre that puts every control channel inside one source (and off the
/// DC spike), or None if they span too much.
pub fn auto_center(ccs: &[f64], rate_hz: f64, guard_hz: f64) -> Option<f64> {
    if ccs.is_empty() {
        return None;
    }
    let lo = ccs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = ccs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let half = usable_half_width(rate_hz, guard_hz);
    if hi - lo > 2.0 * half - 50_000.0 {
        return None;
    }
    let mid = ((lo + hi) / 2.0 / 1000.0).round() * 1000.0;
    off_dc(mid, ccs, |c| ccs.iter().all(|f| (f - c).abs() <= half - 10_000.0))
}

/// How far a channel is kept from a source's centre (its DC spike).
const DC_CLEAR_HZ: f64 = 25_000.0;

/// `c` off the DC spike: moved to either side in widening 12.5 kHz steps
/// until no channel of `hz` is within [`DC_CLEAR_HZ`] of it and `keep(c)`
/// (what it must still reach). None if 40 steps don't find one. Every
/// centre the recorder or Setup picks goes through this.
fn off_dc(mut c: f64, hz: &[f64], keep: impl Fn(f64) -> bool) -> Option<f64> {
    for step in 0..=40 {
        if hz.iter().all(|f| (f - c).abs() >= DC_CLEAR_HZ) && keep(c) {
            return Some(c);
        }
        c += if step % 2 == 1 { -1.0 } else { 1.0 } * 12_500.0 * (step + 1) as f64;
    }
    None
}

/// A source to place ([`plan_centers`]): its rate and guard, and its centre
/// if it can't be moved (a capture file).
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placing {
    pub rate_hz: f64,
    #[serde(default = "guard_default")]
    pub guard_hz: f64,
    #[serde(default)]
    pub fixed_hz: Option<f64>,
}

/// Where to tune each source for a system found on the air (Setup's
/// first-run guide): source `first` at `first_center` (it covers the control
/// channels `ccs`), each other over the busiest voice channels (`voice`: Hz
/// and calls seen) still out of reach, or, when every one is reached,
/// alongside `first` on the side with more of the system. A fixed source
/// stays; every centre is off the DC spike as [`auto_center`]'s are.
pub fn plan_centers(sources: &[Placing], first: usize, first_center: f64, ccs: &[f64], voice: &[(f64, f64)]) -> Vec<Option<f64>> {
    let half = |s: &Placing| usable_half_width(s.rate_hz, s.guard_hz);
    let mut out: Vec<Option<f64>> = sources.iter().map(|s| s.fixed_hz).collect();
    if let Some(o) = out.get_mut(first) {
        o.get_or_insert(first_center);
    }
    let all: Vec<f64> = ccs.iter().copied().chain(voice.iter().map(|v| v.0)).collect();
    for k in 0..sources.len() {
        if out[k].is_some() {
            continue;
        }
        let h = half(&sources[k]);
        let reached = |hz: f64| out.iter().zip(sources).any(|(c, s)| c.is_some_and(|c| (hz - c).abs() <= half(s)));
        let left: Vec<(f64, f64)> = voice.iter().copied().filter(|v| !reached(v.0)).collect();
        let best = if let Some(&(lowest, _)) = left.first() {
            // Each channel still out of reach as the window's low edge: the window that takes in the most calls.
            let score = |c: f64| left.iter().filter(|o| (o.0 - c).abs() <= h).map(|o| o.1).sum::<f64>();
            let mut best = (lowest + h - 1000.0, -1.0);
            for v in &left {
                let c = v.0 + h - 1000.0;
                let sc = score(c);
                if sc > best.1 {
                    best = (c, sc);
                }
            }
            best.0
        } else {
            let mid = all.iter().sum::<f64>() / all.len().max(1) as f64;
            let beside = sources.get(first).map_or(h, half) + h;
            first_center + if mid >= first_center { beside } else { -beside }
        };
        let best = (best / 1000.0).round() * 1000.0;
        let n = |c: f64| all.iter().filter(|f| (*f - c).abs() <= h).count();
        let want = n(best);
        out[k] = Some(off_dc(best, &all, |c| n(c) >= want).unwrap_or(best));
    }
    out
}

/// Several RTL-SDRs at this rate or more, on one USB 2.0 bus, want smaller transfers…
const RTL_BUSY_RATE: f64 = 2_000_000.0;
/// …once there are this many (Trunk Recorder users find trouble from 4 or 5).
const RTL_BUSY_COUNT: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_section_defaults_to_120_days() {
        let c = Config::from_value(serde_json::json!({})).unwrap();
        assert_eq!(c.store.retention_days, 120);
        let c = Config::from_value(serde_json::json!({ "store": { "retentionDays": 0 } })).unwrap();
        assert_eq!(c.store.retention_days, 0);
    }

    /// A call sign heard on a frequency goes into its system's licenses there (trunked or
    /// conventional, on the raster): over a lookup's (keeping its name only for the same
    /// call sign) and another heard, never over the user's.
    #[test]
    fn a_call_sign_heard_goes_in_by_frequency() {
        let mut c: Config = serde_json::from_value(serde_json::json!({
            "systems": [{ "shortName": "dcfd", "sites": [{ "controlChannelsHz": [857987500] }],
                          "fcc": { "859087500": { "callSign": "KNJA391", "name": "DISTRICT OF COLUMBIA", "from": "lookup" },
                                   "860000000": { "callSign": "WMINE1" } } }],
            "conventional": [{ "shortName": "county", "channels": [] }]
        }))
        .unwrap();
        assert!(!c.learn_call_sign("dcfd", 859_987_500.0, "knje616"), "not a call sign as sent");
        assert!(!c.learn_call_sign("dcfd", 0.0, "KNJE616"));
        assert!(c.learn_call_sign("dcfd", 859_987_600.0, "KNJE616"));
        let f = &c.systems[0].fcc;
        assert_eq!(f["859987500"], FccLicense { call_sign: "KNJE616".into(), name: String::new(), from: FccFrom::Air });
        // The same again: nothing. The lookup's, confirmed: its name kept.
        assert!(!c.learn_call_sign("dcfd", 859_987_500.0, "KNJE616"));
        assert!(c.learn_call_sign("dcfd", 859_087_500.0, "KNJA391"));
        assert_eq!(c.systems[0].fcc["859087500"], FccLicense { call_sign: "KNJA391".into(), name: "DISTRICT OF COLUMBIA".into(), from: FccFrom::Air });
        // The user's stays; a conventional system's go in too.
        assert!(!c.learn_call_sign("dcfd", 860_000_000.0, "KNJA391"));
        assert!(c.learn_call_sign("county", 155_475_000.0, "KQX123"));
        assert_eq!(c.conventional[0].fcc["155475000"].call_sign, "KQX123");
        assert!(!c.learn_call_sign("nope", 155_475_000.0, "KQX123"));
    }

    /// The license that names a system: its control channel's, else the call sign on the most frequencies.
    #[test]
    fn the_license_that_names_a_system() {
        let lic = |c: &str| FccLicense { call_sign: c.into(), ..Default::default() };
        let fcc: FccLicenses = [("859087500", lic("KNJA391")), ("859987500", lic("KNJE616")), ("860012500", lic("KNJE616"))].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        assert_eq!(fcc_primary(&fcc, &[857_987_500.0]).map(|l| l.call_sign.as_str()), Some("KNJE616"));
        assert_eq!(fcc_primary(&fcc, &[857_987_500.0, 859_087_500.0]).map(|l| l.call_sign.as_str()), Some("KNJA391"));
        assert_eq!(fcc_primary(&FccLicenses::new(), &[857_987_500.0]), None);
        // Optional: none written when there are none; `from` the user's when left out.
        assert!(serde_json::to_value(System::default()).unwrap().get("fcc").is_none());
        let sys: System = serde_json::from_value(serde_json::json!({ "shortName": "x", "fcc": { "857987500": { "callSign": "WPXT459" } } })).unwrap();
        assert_eq!(sys.fcc["857987500"].from, FccFrom::User);
        assert_eq!(serde_json::to_value(&sys).unwrap()["fcc"], serde_json::json!({ "857987500": { "callSign": "WPXT459" } }));
    }

    /// A system's signals are saved as written, a newer version's keys kept; none, no field.
    #[test]
    fn signals_round_trip() {
        assert!(serde_json::to_value(System::default()).unwrap().get("signals").is_none());
        let page = serde_json::json!({ "id": "NORWALK_EMS", "name": "Norwalk EMS page", "pattern": "two_tone", "tonesHz": [781.25, 875.0], "lengthsS": [0.98, 2.98], "talkgroups": ["20341"], "color": "red" });
        let sys: System = serde_json::from_value(serde_json::json!({ "shortName": "x", "signals": [page.clone(), { "id": "SIREN_SIGNAL_1", "pattern": "digits", "digits": "091363" }, { "id": "NEWER", "pattern": "chirp" }] })).unwrap();
        assert_eq!((sys.signals[0].tones_hz.clone(), sys.signals[1].digits.as_str()), (vec![781.25, 875.0], "091363"));
        let back = serde_json::to_value(&sys).unwrap();
        assert_eq!(back["signals"][0], page);
        assert_eq!(back["signals"][1], serde_json::json!({ "id": "SIREN_SIGNAL_1", "pattern": "digits", "digits": "091363" }), "defaults left out");
        // A pattern this version doesn't know is a problem, which Start says; the engine leaves it out.
        assert_eq!(engine_signals(&sys.signals).iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["NORWALK_EMS", "SIREN_SIGNAL_1"]);
        assert_eq!(signals_problem("x", &sys.signals).unwrap(), "x: Signal NEWER: no such pattern as \"chirp\".");
    }

    #[test]
    fn signals_say_whats_wrong() {
        assert_eq!(SignalDef::id_from(" Norwalk EMS 1 page! "), "NORWALK_EMS_1_PAGE");
        let def = |v: serde_json::Value| -> SignalDef { serde_json::from_value(v).unwrap() };
        assert_eq!(def(serde_json::json!({ "id": "A", "pattern": "two_tone", "tonesHz": [400.0, 500.0] })).problem(), None);
        assert!(def(serde_json::json!({ "pattern": "two_tone", "tonesHz": [400.0, 500.0] })).problem().unwrap().contains("needs an id"));
        assert!(def(serde_json::json!({ "id": "A B", "pattern": "long", "tonesHz": [400.0] })).problem().unwrap().contains("letters, digits"));
        assert!(def(serde_json::json!({ "id": "A", "pattern": "two_tone", "tonesHz": [400.0] })).problem().unwrap().contains("two tones"));
        assert!(def(serde_json::json!({ "id": "A", "pattern": "digits" })).problem().unwrap().contains("which digits"));
        let two = [def(serde_json::json!({ "id": "A", "pattern": "morse", "digits": "W1AW" })), def(serde_json::json!({ "id": "A", "pattern": "long", "tonesHz": [1000.0] }))];
        assert!(signals_problem("x", &two).unwrap().contains("two signals are called A"));
    }

    /// A system's `keys` that named a file (before key sources) becomes a source named after the file;
    /// systems that named the same file share it, and one already a source's name is left as it is.
    #[test]
    fn a_keys_file_named_by_a_system_becomes_a_key_source() {
        let dir = std::env::temp_dir().join(format!("trunk-config-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        std::fs::write(
            &cfg,
            serde_json::json!({
                "keys": { "keys": { "file": "other.json" } },
                "systems": [{ "shortName": "a", "keys": "keys.json" }, { "shortName": "b", "keys": "keys.json" }, { "shortName": "c", "keys": "keys" }],
                "conventional": [{ "shortName": "v", "keys": "/etc/radio/county.ekc" }]
            })
            .to_string(),
        )
        .unwrap();
        let c = Config::load(&cfg).unwrap();
        let named: Vec<&str> = c.systems.iter().map(|s| s.keys.as_str()).collect();
        assert_eq!(named, ["keys-2", "keys-2", "keys"]);
        assert_eq!(c.keys["keys-2"].file, "keys.json");
        assert_eq!(c.conventional[0].keys, "county");
        assert_eq!(c.keys["county"].file, "/etc/radio/county.ekc");
        assert_eq!(c.keys.len(), 3);
    }

    /// A source is read once however many systems name it; one no system names isn't read at all.
    #[test]
    fn a_key_source_is_read_once_for_the_systems_that_name_it() {
        let dir = std::env::temp_dir().join(format!("trunk-config-keysrc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("k.json"), r#"{ "keys": [{ "algId": "0xAA", "keyId": 1, "key": "1234567890" }] }"#).unwrap();
        let cc = vec![851_012_500.0];
        let sys = |n: &str, k: &str| System { keys: k.into(), ..System::one_site(n, Site { control_channels_hz: cc.clone(), ..Default::default() }) };
        let mut c = Config { systems: vec![sys("a", "county"), sys("b", "county"), sys("c", "")], ..Default::default() };
        c.keys.insert("county".into(), KeySource { file: "k.json".into(), ..Default::default() });
        c.keys.insert("unused".into(), KeySource { file: "missing.json".into(), ..Default::default() });
        let l = c.load_keys(&dir.join("config.json"), &|_| None).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].name.as_str(), l[0].systems.clone(), l[0].ring.as_ref().unwrap().keys().len()), ("county", vec!["a".to_string(), "b".to_string()], 1));
        assert!(c.lockable(&dir.join("config.json")).is_empty(), "a JSON file has no password");
        c.systems[2].keys = "nope".into();
        assert!(c.load_keys(&dir.join("config.json"), &|_| None).err().unwrap().contains("no key source \"nope\""));
    }

    #[test]
    fn usrp_label_names_the_device() {
        let label = |args: &str| serde_json::from_str::<Source>(&format!(r#"{{"type":"usrp","args":"{args}","centerHz":0,"rateHz":8e6}}"#)).unwrap().label();
        assert_eq!(label(""), "USRP (first)");
        assert_eq!(label("num_recv_frames=256"), "USRP (first)");
        assert_eq!(label("serial=31A, num_recv_frames=256"), "USRP serial=31A");
    }

    #[test]
    fn am_channels_and_demod_settings() {
        let c: Config = serde_json::from_str(
            r#"{
                "conventional": [{ "demod": { "fmDeemphasis": false, "amLevel": "slow", "amLowCutHz": 0, "squelchHangMs": 150 }, "channels": [
                    { "freqHz": 119100000, "mode": "am", "name": "Tower" },
                    { "freqHz": 128100000, "mode": "am", "bandwidthKhz": 6 },
                    { "freqHz": 243000000, "mode": "am", "name": "Guard" },
                    { "freqHz": 154430000, "mode": "fm", "bandwidthKhz": 16 } ] }],
                "sources": [{ "type": "file", "path": "x.cu8", "centerHz": 150000000, "rateHz": 240000000, "realtime": false }]
            }"#,
        )
        .unwrap();
        let e = c.engine_config(0.0, &Default::default());
        let t: Vec<_> = e.conventional.iter().map(|x| (x.mode, x.analog.half_bw_hz)).collect();
        assert_eq!(t, [(ConvMode::Am, 5500.0), (ConvMode::Am, 3000.0), (ConvMode::Am, 8000.0), (ConvMode::Fm, 8000.0)]);
        let a = &e.conventional[0].analog;
        assert_eq!((a.deemph_tau_s, a.level_tau_s, a.low_cut_hz, a.squelch.hang_s), (0.0, trunk_core::dsp::am::LEVEL_SLOW_S, 0.0, 0.15));
        assert_eq!(e.conventional[0].info.as_ref().unwrap().mode, "A");
        // Defaults aren't written back.
        let d = Conventional::default();
        assert!(!serde_json::to_string(&d).unwrap().contains("demod"));
        let mut bad = c.clone();
        bad.conventional[0].demod.squelch_hang_ms = Some(5000.0);
        assert!(bad.problem().unwrap().contains("hang"), "{:?}", bad.problem());
        let mut bad = c.clone();
        bad.conventional[0].channels[1].bandwidth_khz = Some(40.0);
        assert!(bad.problem().unwrap().contains("bandwidth"), "{:?}", bad.problem());
        let mut bad = c;
        bad.conventional[0].channels[0].code = "100.0".into();
        assert!(bad.problem().unwrap().contains("AM channels carry no tone"), "{:?}", bad.problem());
    }

    #[test]
    fn conventional_only_config() {
        let mut c: Config = serde_json::from_str(
            r#"{
                "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "agc": true, "ppm": 0 }],
                "conventional": [{ "channels": [
                    { "freqHz": 154430000, "mode": "fm", "name": "County Fire Dispatch", "talkgroup": 1001 },
                    { "freqHz": 154100000, "mode": "p25", "squelchDb": 12 },
                    { "freqHz": 453000000, "enabled": false }
                ] }]
            }"#,
        )
        .unwrap();
        assert_eq!(c.conventional[0].squelch_db, 8.0);
        assert_eq!(c.conventional[0].channels[2].mode, ChannelMode::Fm);
        // No control channels: the centre is placed over the enabled channels.
        assert_eq!(c.problem(), None);
        let center = c.resolved_centers()[0];
        assert!((center - 154_265_000.0).abs() < 100_000.0, "center {center}");
        let e = c.engine_config(0.0, &Default::default());
        assert!(e.systems.iter().all(SystemConfig::is_conventional));
        assert_eq!(e.conventional.len(), 2);
        assert_eq!(e.conventional[0].talkgroup, Some(1001));
        assert_eq!(e.conventional[0].info.as_ref().unwrap().alpha_tag, "County Fire Dispatch");
        assert_eq!((e.conventional[1].talkgroup, e.conventional[1].default_talkgroup), (None, 154100));
        assert_eq!(e.conventional[1].squelch_db, Some(12.0));
        assert!(e.conventional[1].info.is_none());
        // Enabling the far channel no longer fits one dongle.
        c.conventional[0].channels[2].enabled = true;
        assert!(c.problem().unwrap().contains("center frequency"));
        c.conventional[0].channels.clear();
        assert!(c.problem().unwrap().contains("conventional channel"));
    }

    /// A choice the config doesn't list is an error when it's read (as a
    /// wrong type is); an empty one is the default, as it always meant.
    #[test]
    fn a_choice_not_listed_is_refused() {
        let sys = |kind: &str, modulation: &str| Config::from_json(&format!(r#"{{ "systems": [{{ "shortName": "a", "type": "{kind}", "sites": [{{ "modulation": "{modulation}" }}] }}] }}"#));
        let c = sys("smartnet", "qpsk").unwrap();
        assert_eq!((c.systems[0].kind, c.systems[0].sites[0].modulation), (SystemKind::Smartnet, Modulation::Qpsk));
        let e = sys("P25x", "auto").err().unwrap().to_string();
        assert!(e.contains("P25x") && e.contains("smartnet"), "{e}");
        assert!(sys("p25", "both").is_err());
        let c = sys("", "").unwrap();
        assert_eq!((c.systems[0].kind, c.systems[0].sites[0].modulation), (SystemKind::P25, Modulation::Auto));
        let c = Config::from_json(r#"{ "log": { "color": "" }, "recording": { "vocoder": "fixed-point", "m4a": { "encoder": "none" } } }"#).unwrap();
        assert_eq!((c.log.color, c.recording.vocoder, c.recording.m4a.encoder), (LogColor::Auto, Vocoder::Fixed, M4aEncoder::None));
        let back = serde_json::to_value(&c).unwrap();
        assert_eq!((back["log"]["color"].as_str(), back["recording"]["vocoder"].as_str()), (Some(""), Some("fixed")), "saved as the interface writes them");
    }

    /// A conventional system names what its digital channels hear by its own
    /// talkgroup file, not by a trunked system's.
    #[test]
    fn a_conventional_system_has_its_own_talkgroup_file() {
        let c: Config = serde_json::from_str(
            r#"{
                "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "agc": true, "ppm": 0 }],
                "systems": [{ "shortName": "county", "sites": [{ "controlChannelsHz": [155500000] }] }],
                "conventional": [{ "shortName": "fire", "channels": [{ "freqHz": 154430000, "mode": "p25" }] }]
            }"#,
        )
        .unwrap();
        let files: TalkgroupFiles = [("county".to_string(), "1,County TG\n".to_string()), ("fire".to_string(), "7,Fire Ops\n".to_string())].into();
        let e = c.engine_config(0.0, &files);
        let fire = e.systems.iter().find(|s| s.short_name == "fire").unwrap();
        assert!(fire.talkgroups.contains_key(&7) && !fire.talkgroups.contains_key(&1), "its own, not county's");
    }

    /// The dashboard names a conventional system's talkgroups as its calls
    /// are named: by a row, for the numbers known ahead, before the file.
    #[test]
    fn rows_name_the_numbers_they_file_under() {
        let c: Config = serde_json::from_str(
            r#"{
                "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "agc": true, "ppm": 0 }],
                "conventional": [{ "shortName": "town", "channels": [
                    { "freqHz": 154430000, "mode": "fm", "name": "Fire Dispatch" },
                    { "freqHz": 460125000, "mode": "p25", "talkgroup": 46012, "name": "Townville PD", "tag": "Law Dispatch" },
                    { "freqHz": 453500000, "mode": "p25", "name": "Townville (other)" },
                    { "freqHz": 155100000, "mode": "fm" }
                ] }]
            }"#,
        )
        .unwrap();
        let rows = c.row_talkgroups();
        let town = &rows["town"];
        let mut named: Vec<(u32, &str)> = town.values().map(|t| (t.number, t.alpha_tag.as_str())).collect();
        named.sort();
        // The analog row's default and the P25 row's TG Number; not the catch-all (its numbers come from the air), nor a row without names.
        assert_eq!(named, [(46012, "Townville PD"), (154430, "Fire Dispatch")]);
        assert_eq!(town[&46012].tag, "Law Dispatch");
        // Ahead of the talkgroup file, whose Ignore flag stays.
        let mut book = crate::stats::NameBook::default();
        let files: TalkgroupFiles = [("town".to_string(), "46012,File name\n9,Nine,,,,,,,true\n".to_string())].into();
        book.set_config(&c, &files);
        assert_eq!(book.tg("town", 46012).map(|t| t.alpha_tag), Some("Townville PD".into()));
        assert_eq!(book.tg("town", 154430).map(|t| t.alpha_tag), Some("Fire Dispatch".into()));
        assert!(book.tg("town", 7).is_none());
    }

    #[test]
    fn several_conventional_systems() {
        let mut c: Config = serde_json::from_str(
            r#"{
                "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "agc": true, "ppm": 0 }],
                "conventional": [
                    { "shortName": "fire", "squelchDb": 10, "channels": [{ "freqHz": 154430000, "mode": "fm" }] },
                    { "shortName": "police", "recording": { "minCallS": 2 }, "channels": [{ "freqHz": 154100000, "mode": "fm", "squelchDb": 14 }] },
                    { "shortName": "off", "enabled": false, "channels": [{ "freqHz": 154200000 }] }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(c.problem(), None);
        let e = c.engine_config(0.0, &Default::default());
        assert_eq!(e.conventional.iter().map(|x| (x.system, x.squelch_db)).collect::<Vec<_>>(), [(0, Some(10.0)), (1, Some(14.0))]);
        assert_eq!(e.systems.iter().map(|x| (x.short_name.as_str(), x.save.min_call_s)).collect::<Vec<_>>(), [("fire", 0.0), ("police", 2.0)], "the one switched off isn't numbered");
        assert_eq!(c.short_name_of(1), Some("police"));
        assert_eq!(c.recordings()[1].min_call_s, 2.0);
        assert_eq!(c.call_systems().iter().map(|s| (s.index, s.short_name)).collect::<Vec<_>>(), [(0, "fire"), (1, "police")]);
        // A file name format with a token no one knows: refused, the config's or a system's own.
        c.recording.filename_format = "{short_name}/{nope}".into();
        assert!(c.problem().unwrap().contains("File name format: Unknown token: {nope}"), "{:?}", c.problem());
        c.recording.filename_format.clear();
        c.conventional[1].recording.filename_format = Some("{tg".into());
        assert!(c.problem().unwrap().starts_with("police's file name format"), "{:?}", c.problem());
        c.conventional[1].recording.filename_format = None;
        assert_eq!(c.problem(), None);
        // One frequency, two systems: an error naming both.
        c.conventional[1].channels[0].freq_hz = 154_430_000.0;
        assert!(c.problem().unwrap().contains("both fire and police"), "{:?}", c.problem());
        c.conventional[1].channels[0].freq_hz = 154_100_000.0;
        c.conventional[1].short_name = "fire".into();
        assert!(c.problem().unwrap().contains("Two systems are named \"fire\""));
        // A short name is a system's own: not a trunked system's either, nor a switched-off one's.
        c.conventional[1].short_name = "off".into();
        assert!(c.problem().unwrap().contains("Two systems are named \"off\""));
        c.conventional[1].short_name = "police".into();
        c.systems.push(System { short_name: "police".into(), ..Default::default() });
        assert!(c.problem().unwrap().contains("Two systems are named \"police\""));
        c.systems[0].short_name = "county".into();
        assert_eq!(c.problem(), None);
    }

    #[test]
    fn dmr_system_takes_a_channel_table() {
        let s: System = serde_json::from_str(
            r#"{ "shortName": "capmax", "type": "dmr", "sites": [{ "controlChannelsHz": [452175000],
                 "lcnTableHz": { "101": 452275000, "102": 452300000 }, "dmrChannelsHz": [452275000], "colorCode": 0 }] }"#,
        )
        .unwrap();
        assert!(s.is_dmr());
        let d = s.sites[0].dmr();
        assert_eq!(d.lcn_table.into_iter().collect::<Vec<_>>(), [(101, 452_275_000), (102, 452_300_000)]);
        assert_eq!((d.channels, d.color_code), (vec![452_275_000.0], Some(0)));
        let back: System = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn an_edacs_site() {
        let mut c = Config::from_json(
            r#"{ "sources": [{ "type": "airspy", "serial": "", "centerHz": 857500000, "rateHz": 10000000 }],
                 "systems": [{ "shortName": "nycomco", "type": "edacs", "sites": [{ "name": "beacon", "controlChannelsHz": [856187500],
                     "lcnTableHz": { "1": 856187500, "3": 857712500, "4": 854137500, "30": 851000000 } }] }] }"#,
        )
        .unwrap();
        let s = &c.systems[0];
        assert!(s.is_edacs());
        let e = s.sites[0].edacs();
        assert_eq!(e.lcns.into_iter().collect::<Vec<_>>(), [(1, 856_187_500), (3, 857_712_500), (4, 854_137_500)], "LCN 30 isn't a channel");
        assert!(!e.esk);
        assert!(s.sites[0].watched_channels(s).contains(&857_712_500.0));
        let back: System = serde_json::from_str(&serde_json::to_string(s).unwrap()).unwrap();
        assert_eq!(&back, s);
        assert_eq!(c.problem(), None);
        let ec = c.engine_config(0.0, &Default::default());
        assert!(matches!(ec.systems[0].sites[0].protocol, trunk_core::trunk::Protocol::Edacs(_)));
        // No channel list: nothing to follow calls with.
        c.systems[0].sites[0].lcn_table_hz.clear();
        assert!(c.problem().unwrap().contains("channel list"), "{:?}", c.problem());
    }

    #[test]
    fn rtl_usb_transfers_checked_and_hinted() {
        let rtl = |rate_hz: f64, buf_len: u32| Source::Rtlsdr { serial: String::new(), center_hz: 851e6, rate_hz, gain_db: 30.0, agc: false, ppm: 0, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ, buf_len, buffers: 0 };
        let mut c = Config { sources: vec![rtl(2.4e6, 0); 3], systems: vec![system("s", &[851_012_500.0])], ..Default::default() };
        assert_eq!(c.rtl_buflen_hint(None), None);
        c.sources.push(rtl(1.2e6, 0));
        assert_eq!(c.rtl_buflen_hint(None), None);
        c.sources.push(rtl(2.4e6, 0));
        assert!(c.rtl_buflen_hint(None).unwrap().starts_with("4 RTL-SDRs"));
        assert!(c.rtl_buflen_hint(Some(5)).unwrap().contains("5 of them on one USB bus"));
        assert_eq!(c.rtl_buflen_hint(Some(2)), None);
        c.sources[0] = rtl(2.4e6, 65_536);
        assert_eq!(c.rtl_buflen_hint(None), None);
        assert_eq!(c.problem(), None);
        c.sources[0] = rtl(2.4e6, 65_000);
        assert!(c.problem().unwrap().contains("multiple of 512"));
        let back: Config = serde_json::from_str(r#"{"sources":[{"type":"rtlsdr","serial":"","centerHz":0,"rateHz":2400000,"ppm":0}]}"#).unwrap();
        assert!(matches!(back.sources[0], Source::Rtlsdr { buf_len: 0, buffers: 0, .. }));
        let json = serde_json::to_value(&rtl(2.4e6, 65_536)).unwrap();
        assert_eq!((json["bufLen"].as_u64(), json["buffers"].as_u64()), (Some(65_536), Some(0)));
    }

    fn system(name: &str, ccs: &[f64]) -> System {
        System::one_site(name, Site { control_channels_hz: ccs.to_vec(), ..Default::default() })
    }

    #[test]
    fn a_first_run_plan_covers_the_busiest_channels() {
        let rtl = |fixed_hz| Placing { rate_hz: 2_400_000.0, guard_hz: DEFAULT_GUARD_HZ, fixed_hz };
        let ccs = [851_000_000.0];
        // Busy voice channels 3–4 MHz up, a quiet one further: the second radio takes the busy ones.
        let voice = [(854_000_000.0, 50.0), (854_500_000.0, 40.0), (856_900_000.0, 1.0), (851_200_000.0, 9.0)];
        let plan = plan_centers(&[rtl(None), rtl(None)], 0, 851_400_000.0, &ccs, &voice);
        assert_eq!(plan[0], Some(851_400_000.0));
        let c = plan[1].unwrap();
        let half = usable_half_width(2_400_000.0, DEFAULT_GUARD_HZ);
        assert!((854_000_000.0 - c).abs() <= half && (854_500_000.0 - c).abs() <= half, "{c}");
        assert!(voice.iter().chain([(851_000_000.0, 0.0)].iter()).all(|v| (v.0 - c).abs() >= DC_CLEAR_HZ), "off the DC spike: {c}");
        // A capture file stays where it is; with everything reached, a spare radio goes alongside.
        let plan = plan_centers(&[rtl(None), rtl(Some(853_000_000.0)), rtl(None)], 0, 851_400_000.0, &ccs, &voice[..2]);
        assert_eq!(plan[1], Some(853_000_000.0));
        assert!(plan[2].unwrap() > 851_400_000.0 + 2.0 * half - 100_000.0, "{plan:?}");
    }

    #[test]
    fn several_systems_centers_and_engine_config() {
        let rtl = || Source::Rtlsdr { serial: String::new(), center_hz: 0.0, rate_hz: 2_400_000.0, gain_db: 30.0, agc: false, ppm: 0, auto_tune: false, guard_hz: DEFAULT_GUARD_HZ, buf_len: 0, buffers: 0 };
        let mut c = Config { sources: vec![rtl(), rtl()], ..Default::default() };
        let mut a = system("east", &[851_012_500.0]);
        a.sites[0].voice_channels_hz = vec![851_500_000.0, 852_000_000.0];
        a.sites[0].expect = SiteIdentity { nac: Some(0x443), site: Some(3), ..Default::default() };
        let mut b = system("west", &[771_106_250.0]);
        b.recording.record_unknown = Some(false);
        b.recording.min_call_s = Some(2.0);
        c.systems = vec![a, b, System { enabled: false, ..system("off", &[460_000_000.0]) }];
        assert_eq!(c.problem(), None);
        // Each auto source takes a system the ones before it don't cover.
        let centers = c.resolved_centers();
        let hw = usable_half_width(2_400_000.0, DEFAULT_GUARD_HZ);
        for f in [851_012_500.0, 851_500_000.0, 852_000_000.0] {
            assert!((f - centers[0]).abs() <= hw, "{f} not in source 1 at {}", centers[0]);
        }
        assert!((771_106_250.0 - centers[1]).abs() <= hw, "west's CC not in source 2 at {}", centers[1]);
        let e = c.engine_config(0.0, &Default::default());
        assert_eq!(e.systems.iter().map(|s| s.short_name.as_str()).collect::<Vec<_>>(), ["east", "west"]);
        assert_eq!(e.systems[0].sites[0].expect.nac(), Some(0x443));
        assert_eq!(e.systems[0].sites[0].expect.site(), Some(3));
        assert!(e.systems[0].calls.record_unknown && !e.systems[1].calls.record_unknown);
        assert_eq!((e.systems[0].save.min_call_s, e.systems[1].save.min_call_s), (0.0, 2.0));
        let saved = serde_json::to_string(&c.systems[1]).unwrap();
        assert!(saved.contains(r#""recording":{"recordUnknown":false,"minCallS":2.0}"#), "{saved}");
        assert!(!serde_json::to_string(&c.systems[0]).unwrap().contains("recording"));
        assert!(e.drop_duplicates);
        // A second site of east: its own control channel, east's rules; site names its own.
        c.systems[0].sites.push(Site { name: "east-2".into(), control_channels_hz: vec![851_212_500.0], ..Default::default() });
        let e = c.engine_config(0.0, &Default::default());
        let sites: Vec<_> = trunk_core::trunk::sites(&e.systems).map(|(n, s)| (e.systems[n as usize].short_name.as_str(), s.name.as_str(), e.systems[n as usize].calls.record_unknown)).collect();
        assert_eq!(sites, [("east", "east", true), ("east", "east-2", true), ("west", "west", false)]);
        c.systems[0].sites[1].name = "east".into();
        assert!(c.problem().unwrap().contains("Two sites of east are named \"east\""), "{:?}", c.problem());
        c.systems[0].sites.pop();
        // One source can't hold both.
        c.sources.pop();
        assert!(c.problem().unwrap().contains("west"), "{:?}", c.problem());
        // Short names are folders: unique.
        c.sources.push(rtl());
        c.systems[1].short_name = "east".into();
        assert!(c.problem().unwrap().contains("east"));
    }

    #[test]
    fn rows_sharing_a_frequency() {
        let row = |tone: &str, tg: Option<u32>| Channel {
            freq_hz: 154_325_000.0,
            mode: ChannelMode::Fm,
            name: String::new(),
            talkgroup: tg,
            description: String::new(),
            tag: String::new(),
            group: String::new(),
            squelch_db: None,
            bandwidth_khz: None,
            code: tone.into(),
            slot: None,
            match_talkgroup: None,
            enabled: true, auto_tune: false,
        };
        let mut c = Config { conventional: vec![Conventional::default()], ..Default::default() };
        c.conventional[0].channels = vec![row("", None), row("D023N", None), row("151.4", Some(500)), row("94.8", None)];
        assert_eq!(c.problem(), None);
        let e = c.engine_channels();
        assert_eq!(e.iter().map(|x| x.talkgroup.unwrap_or(x.default_talkgroup)).collect::<Vec<_>>(), vec![154325, 1543251, 500, 1543253]);
        assert_eq!(e[1].access, Some(Access::Tone(trunk_core::dsp::inband::subaudible::Tone::Dcs(23, false))));
        c.conventional[0].channels.push(row("D047I", None));
        assert_eq!(c.problem().as_deref(), Some("Conventional channel 154.32500 MHz has two rows for D023N and D047I (the same signal)."));
        c.conventional[0].channels.pop();
        c.conventional[0].channels.push(row("", None));
        assert!(c.problem().unwrap().contains("listed twice without a tone"));
        c.conventional[0].channels.pop();
        c.conventional[0].channels.push(row("151.5", None));
        assert_eq!(c.problem().as_deref(), Some("Conventional channel 154.32500 MHz: 151.5 Hz isn't a standard CTCSS tone — 151.4?."));
        c.conventional[0].channels.pop();
        // FM names no talkgroup, and has no slots.
        c.conventional[0].channels[3].match_talkgroup = Some(1);
        assert_eq!(c.problem().as_deref(), Some("Conventional channel 154.32500 MHz: Match TG 1: FM names no talkgroup — leave it empty."));
        c.conventional[0].channels[3].match_talkgroup = None;
        c.conventional[0].channels[3].slot = Some(1);
        assert!(c.problem().unwrap().contains("only DMR has slots"), "{:?}", c.problem());
        // P25 rows split by talkgroup, a typed number filed under, the others the air's.
        let p25 = |code: &str, tg: Option<u32>, m: Option<u32>| Channel { freq_hz: 154_325_000.0, mode: ChannelMode::P25, code: code.into(), match_talkgroup: m, ..row("", tg) };
        c.conventional[0].channels = vec![p25("293", Some(45351), Some(1)), p25("293", None, Some(2)), p25("293", None, None)];
        assert_eq!(c.problem(), None);
        let e = c.engine_channels();
        assert_eq!(e.iter().map(|x| (x.talkgroup, x.default_talkgroup)).collect::<Vec<_>>(), [(Some(45351), 154325), (None, 1543251), (None, 1543252)]);
        assert_eq!(e[0].access, Some(Access::P25 { nac: Some(0x293), tg: Some(1) }));
        c.conventional[0].channels.push(p25("NAC 293 TG 2", None, None));
        assert_eq!(c.problem().as_deref(), Some("Conventional channel 154.32500 MHz has two rows for NAC 293 TG 2."));
    }

    #[test]
    fn channel_file_link_edit_save_load_unlink() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg_path = dir.join("config.json");
        let mut c = Config { conventional: vec![Conventional::default()], ..Default::default() };
        c.conventional[0].channels = vec![Channel {
            freq_hz: 154_430_000.0,
            mode: ChannelMode::Fm,
            name: "Fire".into(),
            talkgroup: None,
            description: String::new(),
            tag: String::new(),
            group: String::new(),
            squelch_db: None,
            bandwidth_khz: None,
            code: String::new(),
            slot: None,
            match_talkgroup: None,
            enabled: true, auto_tune: false,
        }];
        // Linking a new path writes the current list there.
        c.link_channel_file(0, &cfg_path, "channels.csv").unwrap();
        let file = dir.join("channels.csv");
        assert!(std::fs::read_to_string(&file).unwrap().contains("154.4300,,,,fm,Fire"));
        // Edited in a spreadsheet: re-read.
        std::fs::write(&file, "Frequency,Mode,Alpha Tag
154.4300,fm,Fire
460.125,p25,PD
").unwrap();
        c.load_channel_file(0, &cfg_path).unwrap();
        assert_eq!(c.conventional[0].channels.len(), 2);
        assert_eq!(c.conventional[0].channel_file_status, "2 channels read.");
        // Saved without the list; loading reads the file again.
        c.save(&cfg_path).unwrap();
        let saved = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(saved.contains("\"channelFile\": \"channels.csv\"") && saved.contains("\"channels\": []"), "{saved}");
        assert_eq!(Config::load(&cfg_path).unwrap().conventional[0].channels.len(), 2);
        // A broken file keeps the last good list and says why.
        std::fs::write(&file, "Name\nx\n").unwrap();
        assert!(c.load_channel_file(0, &cfg_path).unwrap_err().contains("No Frequency column"));
        assert_eq!(c.conventional[0].channels.len(), 2);
        // Unlinking keeps the channels in the config.
        c.link_channel_file(0, &cfg_path, "").unwrap();
        c.save(&cfg_path).unwrap();
        assert_eq!(Config::load(&cfg_path).unwrap().conventional[0].channels.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Keys this version doesn't know are saved back as they were (the
    /// interface saves the whole config on every change).
    #[test]
    fn unknown_keys_survive_a_save() {
        let text = r#"{ "systems": [{ "shortName": "a", "controlChannelsHz": [851012500], "futureKey": [1, 2] }],
                        "conventional": [{ "shortName": "c", "note": "mine" }],
                        "recording": { "newRule": true }, "server": { "tls": { "cert": "x" } }, "log": { "rotate": 7 },
                        "multisite": { "window": 3 } }"#;
        let c: Config = serde_json::from_str(text).unwrap();
        let back = serde_json::to_value(&c).unwrap();
        assert_eq!(back["systems"][0]["futureKey"], serde_json::json!([1, 2]));
        assert_eq!(back["conventional"][0]["note"], "mine");
        assert_eq!(back["recording"]["newRule"], true);
        assert_eq!(back["server"]["tls"]["cert"], "x");
        assert_eq!(back["log"]["rotate"], 7);
        assert_eq!(back["multisite"]["window"], 3);
        assert_eq!(serde_json::from_value::<Config>(back.clone()).unwrap(), c);
        assert!(back["conventional"][0].get("channelFileStatus").is_none(), "an empty status isn't written");
    }

    /// Every kind of source is named in [`SOURCE_TYPES`] (or its `type` wouldn't read in any case).
    #[test]
    fn every_source_type_is_known() {
        for t in SOURCE_TYPES {
            let e = serde_json::from_value::<Source>(serde_json::json!({ "type": t })).unwrap_err().to_string();
            assert!(!e.contains("unknown variant"), "{t}: {e}");
        }
        let e = serde_json::from_value::<Source>(serde_json::json!({ "type": "nope" })).unwrap_err().to_string();
        for t in SOURCE_TYPES {
            assert!(e.contains(t), "the source kinds are {SOURCE_TYPES:?}, serde lists: {e}");
        }
    }

    #[test]
    fn a_closed_choice_reads_in_any_case_and_is_saved_in_its_own() {
        let c = Config::from_json(r#"{ "systems": [{ "shortName": "a", "type": "P25", "sites": [{ "controlChannelsHz": [1], "modulation": "QPSK" }], "unitNames": { "mode": " OTA " } }],
            "conventional": [{ "shortName": "c", "channels": [{ "freqHz": 1, "mode": "NXDN48" }] }], "recording": { "vocoder": "Fixed-Point", "m4a": { "encoder": "FFmpeg" } } }"#).unwrap();
        assert_eq!((c.systems[0].kind, c.systems[0].sites[0].modulation, c.systems[0].unit_names.mode), (SystemKind::P25, Modulation::Qpsk, UnitNamesMode::Ota));
        assert_eq!((c.conventional[0].channels[0].mode, c.recording.vocoder, c.recording.m4a.encoder), (ChannelMode::Nxdn48, Vocoder::Fixed, M4aEncoder::Ffmpeg));
        let saved = serde_json::to_string(&c).unwrap();
        assert!(saved.contains(r#""type":"p25""#) && saved.contains(r#""modulation":"qpsk""#) && saved.contains(r#""mode":"ota""#) && saved.contains(r#""vocoder":"fixed""#), "{saved}");
        let e = Config::from_json(r#"{ "systems": [{ "shortName": "a", "type": "p26" }] }"#).unwrap_err().to_string();
        assert!(e.contains("p26") && e.contains("smartnet"), "names the value and the choices: {e}");
        let c = Config::from_json(r#"{ "sources": [{ "type": "RTLSDR", "serial": "", "centerHz": 0, "rateHz": 2.4e6, "ppm": 0 }], "log": { "level": "Warn", "frequencyFormat": "MHz", "talkgroupDisplayFormat": "ID_TAG" },
            "server": { "interfaces": { "city": { "port": 8090, "access": "Control" } } } }"#).unwrap();
        assert!(matches!(c.sources[0], Source::Rtlsdr { .. }));
        assert_eq!((c.log.level, c.log.frequency_format, c.log.talkgroup_display_format), (crate::log::Level::Warning, crate::log::FrequencyFormat::Mhz, crate::log::TalkgroupFormat::IdTag));
        assert_eq!(c.server.interfaces["city"].access, ApiAccess::Control);
        let saved = serde_json::to_string(&c).unwrap();
        assert!(saved.contains(r#""type":"rtlsdr""#) && saved.contains(r#""level":"warning""#) && saved.contains(r#""talkgroupDisplayFormat":"id_tag""#) && saved.contains(r#""access":"control""#), "{saved}");
    }

    #[test]
    fn loading_an_older_config_writes_its_talkgroup_files_beside_it() {
        let dir = std::env::temp_dir().join(format!("trunk-load-tg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{ "systems": [{ "shortName": "a", "sites": [{ "controlChannelsHz": [1] }], "talkgroupsCsv": "1,a" }] }"#).unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(std::fs::read_to_string(talkgroups_path(&dir, "a")).unwrap(), "1,a", "the file is there before the config is saved back without it");
        assert!(!serde_json::to_string(&c).unwrap().contains("talkgroupsCsv"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
