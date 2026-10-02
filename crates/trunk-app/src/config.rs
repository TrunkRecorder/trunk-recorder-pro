//! The app's configuration: which sources (RTL-SDRs, USRPs, Airspys, SoapySDR devices or capture files), which
//! trunked systems (sites) and/or conventional channels, recording rules and the web server. Stored as JSON in the platform
//! config folder; the browser interface reads and edits it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trunk_core::p25::diversity::BankConfig;
use trunk_core::trunk::{
    check_channels, conventional_index, Access, parse_csv, CallConfig, ConvChannel, ConvConfig, ConvMode, ConvSystem, EngineConfig, Identity, SaveRules, SourceConfig,
    SystemConfig, Talkgroup, UnitTags, UnitTagsMode, MAX_CONVENTIONAL,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    /// An RTL-SDR dongle. `serial` "" = the first free one; `center_hz` 0 =
    /// auto. `agc`: the tuner's AGC instead of `gain_db`.
    #[serde(rename_all = "camelCase")]
    Rtlsdr {
        serial: String,
        center_hz: f64,
        rate_hz: f64,
        #[serde(default = "rtl_gain")]
        gain_db: f32,
        #[serde(default)]
        agc: bool,
        ppm: i32,
        #[serde(default)]
        auto_tune: bool,
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
        #[serde(default)]
        gain_db: f64,
        #[serde(default)]
        agc: bool,
        #[serde(default)]
        antenna: String,
        #[serde(default)]
        ppm: f64,
        #[serde(default)]
        auto_tune: bool,
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
        gain: u8,
        /// Manual: LNA 0..14, mixer 0..15, VGA (IF) 0..15.
        #[serde(default = "airspy_stage")]
        lna_gain: u8,
        #[serde(default = "airspy_stage")]
        mixer_gain: u8,
        #[serde(default = "airspy_stage")]
        vga_gain: u8,
        #[serde(default)]
        agc: bool,
        #[serde(default)]
        bias_tee: bool,
        #[serde(default)]
        ppm: f64,
        #[serde(default)]
        auto_tune: bool,
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
        #[serde(default)]
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
        #[serde(default)]
        auto_tune: bool,
    },
}

/// How an Airspy's gain is set: libairspy's linearity or sensitivity
/// tables (one 0..21 step for all three stages), or each stage by hand.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AirspyGain {
    #[default]
    Linearity,
    Sensitivity,
    Manual,
}

fn rtl_gain() -> f32 {
    RTL_DEFAULT_GAIN_DB
}
fn airspy_gain() -> u8 {
    14
}
fn airspy_stage() -> u8 {
    10
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
            Source::Rtlsdr { center_hz, .. } | Source::Usrp { center_hz, .. } | Source::Airspy { center_hz, .. } | Source::Soapy { center_hz, .. } | Source::File { center_hz, .. } => *center_hz,
        }
    }
    pub fn rate_hz(&self) -> f64 {
        match self {
            Source::Rtlsdr { rate_hz, .. } | Source::Usrp { rate_hz, .. } | Source::Airspy { rate_hz, .. } | Source::Soapy { rate_hz, .. } | Source::File { rate_hz, .. } => *rate_hz,
        }
    }
    pub fn auto_tune(&self) -> bool {
        match self {
            Source::Rtlsdr { auto_tune, .. } | Source::Usrp { auto_tune, .. } | Source::Airspy { auto_tune, .. } | Source::Soapy { auto_tune, .. } | Source::File { auto_tune, .. } => *auto_tune,
        }
    }
    /// For the interface: "RTL-SDR SN 200", "USRP serial=…", "file x.cu8".
    pub fn label(&self) -> String {
        match self {
            Source::Rtlsdr { serial, .. } => format!("RTL-SDR {}", if serial.is_empty() { "(first)".into() } else { format!("SN {serial}") }),
            Source::Usrp { args, .. } => format!("USRP {}", if args.is_empty() { "(first)" } else { args }),
            Source::Airspy { serial, .. } => format!("Airspy {}", if serial.is_empty() { "(first)".into() } else { format!("SN {serial}") }),
            Source::Soapy { args, .. } => format!("SoapySDR {}", if args.is_empty() { "(first)" } else { args }),
            Source::File { path, .. } => format!("file {}", path.rsplit(['/', '\\']).next().unwrap_or(path)),
        }
    }
}

/// A trunked system — or one site of a multi-site system: each site you
/// record from its own control channel is a system here (as in Trunk
/// Recorder), with its own short name and folder.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct System {
    pub short_name: String,
    /// What people call it ("County Public Safety"); the short name is its folder.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub enabled: bool,
    pub control_channels: Vec<f64>,
    /// "auto" | "fsk4" | "qpsk"
    pub modulation: String,
    pub talkgroups_csv: String,
    pub talkgroups_name: String,
    /// Its own names for its radios (Trunk Recorder's unitTagsFile, kept here as text): see [`UnitNames`].
    #[serde(skip_serializing_if = "UnitNames::is_empty")]
    pub unit_names: UnitNames,
    /// Only follow a control channel with this identity (fields left out
    /// match anything) — e.g. this site of a multi-site system, not its
    /// neighbour on a nearby frequency.
    pub expect: SiteIdentity,
    /// Voice channels the survey heard (for placing sources; informational).
    pub voice_channels: Vec<f64>,
    /// Its own recording rules; what's left out is as in [`Config::recording`].
    #[serde(skip_serializing_if = "RecordingOverride::is_empty")]
    pub recording: RecordingOverride,
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
    /// DMR (`type` "dmr"): logical channel number → frequency, Hz (Trunk
    /// Recorder's `lcnTable`); channels left out are learned from the air.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub lcn_table: BTreeMap<String, f64>,
    /// DMR: voice frequencies to watch besides the control channels (Trunk
    /// Recorder's `channels`; Capacity Plus: every repeater of the site).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<f64>,
    /// DMR: only this colour code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_code: Option<u8>,
    /// Multi-site: sites with the same group are one system, and a call
    /// heard on several of them is saved once (see `Recording::drop_duplicate_calls`).
    /// Empty: grouped by what the control channels say — P25 WACN and System
    /// ID, SmartNet System ID. Name a group for DMR sites, systems linked by
    /// ISSI, or to keep a site out of its system's group.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub site_group: String,
    /// Plugins' settings for this system, by plugin id (as each plugin's
    /// `system_config` schema describes them).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, serde_json::Value>,
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
    pub fn is_dmr(&self) -> bool {
        self.kind.eq_ignore_ascii_case("dmr")
    }

    /// The DMR settings; Trunk Recorder configs give frequencies in Hz or MHz.
    pub fn dmr(&self) -> trunk_core::dmr::DmrConfig {
        let hz = |v: f64| if v > 0.0 && v < 1e5 { v * 1e6 } else { v };
        trunk_core::dmr::DmrConfig {
            lcn_table: self.lcn_table.iter().filter_map(|(k, &v)| Some((k.trim().parse().ok()?, hz(v).round() as u64))).collect(),
            channels: self.channels.iter().map(|&v| hz(v)).collect(),
            color_code: self.color_code,
        }
    }

    /// The SmartNet settings, or why they don't work.
    pub fn smartnet(&self) -> Result<trunk_core::trunk::SmartnetConfig, String> {
        // Trunk Recorder configs give these in Hz or in MHz.
        let hz = |v: f64, mhz_below: f64| if v > 0.0 && v < mhz_below { v * 1e6 } else { v };
        let bandplan = trunk_core::smartnet::Bandplan::from_config(
            &self.bandplan,
            hz(self.bandplan_base, 1e5),
            hz(self.bandplan_spacing, 1.0),
            self.bandplan_offset,
            hz(self.bandplan_high, 1e5),
        )?;
        Ok(trunk_core::trunk::SmartnetConfig { bandplan, analog_default: self.default_mode.eq_ignore_ascii_case("analog") })
    }
}

impl Default for System {
    fn default() -> Self {
        System {
            short_name: "sys1".into(),
            name: String::new(),
            kind: "p25".into(),
            enabled: true,
            control_channels: vec![],
            modulation: "auto".into(),
            talkgroups_csv: String::new(),
            talkgroups_name: String::new(),
            unit_names: UnitNames::default(),
            expect: SiteIdentity::default(),
            voice_channels: vec![],
            recording: RecordingOverride::default(),
            bandplan: String::new(),
            bandplan_base: 0.0,
            bandplan_spacing: 0.0,
            bandplan_offset: 0,
            bandplan_high: 0.0,
            default_mode: String::new(),
            lcn_table: Default::default(),
            channels: vec![],
            color_code: None,
            site_group: String::new(),
            plugins: BTreeMap::new(),
        }
    }
}

impl System {
    fn bank(&self) -> BankConfig {
        let m = self.modulation.as_str();
        BankConfig { cqpsk: m != "fsk4", cqpsk_eq: m != "fsk4", c4fm: m != "qpsk", ..Default::default() }
    }
    /// Recording it: enabled, with a control channel.
    pub fn active(&self) -> bool {
        self.enabled && !self.control_channels.is_empty()
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

impl SiteIdentity {
    fn engine(&self) -> Identity {
        Identity { nac: self.nac, wacn: self.wacn, sys_id: self.sys_id, rfss: self.rfss, site: self.site }
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
    /// Record it.
    pub enabled: bool,
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
    /// Plugins' settings for the conventional channels, by plugin id (to
    /// plugins they're one more system).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, serde_json::Value>,
    /// Their own recording rules; what's left out is as in [`Config::recording`].
    #[serde(skip_serializing_if = "RecordingOverride::is_empty")]
    pub recording: RecordingOverride,
    /// Names for the radios heard on them.
    #[serde(skip_serializing_if = "UnitNames::is_empty")]
    pub unit_names: UnitNames,
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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mode: String,
}

impl UnitNames {
    pub fn is_empty(&self) -> bool {
        *self == UnitNames::default()
    }
    fn engine(&self) -> UnitTags {
        UnitTags::parse_csv(&self.csv, UnitTagsMode::from_name(&self.mode)).0
    }
}

impl Default for Conventional {
    fn default() -> Self {
        Conventional {
            short_name: "conv".into(),
            name: String::new(),
            enabled: true,
            squelch_db: ConvConfig::default().squelch_db,
            channel_file: String::new(),
            channels: vec![],
            channel_file_status: String::new(),
            plugins: BTreeMap::new(),
            recording: RecordingOverride::default(),
            unit_names: UnitNames::default(),
        }
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
    /// DMR (both slots).
    Dmr,
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
    /// kHz (further rows on the frequency: that and a digit, see
    /// [`ConvChannel::default_talkgroup_at`]). P25 files calls under the
    /// talkgroup the air names, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talkgroup: Option<u32>,
    /// The code it records ([`Access`]): FM's CTCSS tone or DCS code in Trunk
    /// Recorder's form (`151.4`, `D023N`), P25's NAC (`NAC 293`), DMR's colour
    /// code / slot / talkgroup (`CC 1 TS 2 TG 201`); empty: any (beside rows
    /// with codes: the rest).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tone: String,
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

/// Each channel's talkgroup: its own; else a DMR row's talkgroup in its
/// Tone; else by its place among the rows on its frequency (enabled or not,
/// so switching one off renumbers nothing).
pub fn channel_talkgroups(channels: &[Channel]) -> Vec<u32> {
    channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let k = channels[..i].iter().filter(|o| (o.freq_hz - c.freq_hz).abs() < 1.0).count();
            let air = match c.parsed_access() {
                Ok(Some(Access::Dmr { tg, .. })) => tg,
                _ => None,
            };
            c.talkgroup.or(air).unwrap_or_else(|| ConvChannel::default_talkgroup_at(c.freq_hz, k))
        })
        .collect()
}

impl Channel {
    pub fn conv_mode(&self) -> ConvMode {
        match self.mode {
            ChannelMode::Fm => ConvMode::Fm,
            ChannelMode::P25 => ConvMode::P25,
            ChannelMode::Dmr => ConvMode::Dmr,
        }
    }

    /// The code it records, read for its mode.
    pub fn parsed_access(&self) -> Result<Option<Access>, String> {
        Access::parse(self.conv_mode(), &self.tone)
    }

    fn engine_channel(&self, tg: u32) -> ConvChannel {
        let named = !(self.name.is_empty() && self.description.is_empty() && self.tag.is_empty() && self.group.is_empty());
        ConvChannel {
            freq_hz: self.freq_hz,
            mode: self.conv_mode(),
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
                preferred_site: String::new(),
                ignore: false,
            }),
            squelch_db: self.squelch_db,
            access: self.parsed_access().ok().flatten(),
            system: 0,
        }
    }
}

/// How calls are recorded and saved. The settings marked "per system" can
/// be set again on each system (and the conventional channels): see
/// [`RecordingOverride`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Recording {
    pub capture_dir: String,
    pub preroll_s: f64,
    pub max_recorders: usize,
    /// Per system: a call ends this long after its last grant or audio.
    pub call_timeout_s: f64,
    /// Per system: talkgroups not in the talkgroup file.
    pub record_unknown: bool,
    /// Per system.
    pub record_encrypted: bool,
    /// Per system.
    pub record_unit_to_unit: bool,
    /// Per system: keep calls with no audio (encrypted, nothing decoded).
    pub keep_silent_calls: bool,
    /// Per system: drop calls with less audio than this, s; 0 = keep all
    /// (Trunk Recorder's minDuration).
    pub min_call_s: f64,
    /// Per system: save a call this long and carry on in a new one, s; 0 =
    /// no limit (maxDuration).
    pub max_call_s: f64,
    /// Per system: leave out transmissions shorter than this, s; 0 = keep
    /// all (minTransmissionDuration).
    pub min_transmission_s: f64,
    /// Save each call's vocoder frames next to its audio, for diagnosis.
    pub capture_frames: bool,
    /// Per system: bring every call's speech to the same loudness (as
    /// Trunk Recorder's uploads were).
    pub normalize_audio: bool,
    /// Per system: then raise or lower digital / analog calls by this much,
    /// dB (Trunk Recorder's digitalLevels / analogLevels).
    pub digital_level_db: f64,
    pub analog_level_db: f64,
    /// Per system: also keep an .m4a of every call (Trunk Recorder's compressWav).
    pub compress_wav: bool,
    /// Per system: keep the audio once every upload plugin has handled the
    /// call (audioArchive); off, it's deleted then.
    pub audio_archive: bool,
    /// Per system: keep the call's JSON then (callLog).
    pub call_log: bool,
    /// Per system: when an upload failed, keep the files anyway (archiveFilesOnFailure).
    pub archive_files_on_failure: bool,
    /// Per system: where calls go under the recordings folder, and their
    /// names ([`crate::filename`]); empty = `<short name>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>`.
    pub filename_format: String,
    /// A call heard on several sites of one system: save only the best copy
    /// (each is recorded; the one decoded most cleanly, or the talkgroup's
    /// preferred site, is kept). Trunk Recorder's multiSite.
    pub drop_duplicate_calls: bool,
    /// IMBE vocoder for P25 Phase 1 voice: "fixed" (Pavel Yazev's fixed-point
    /// decoder, Trunk Recorder's softVocoder false), "enhanced" (TR's float
    /// synthesis) or "mbelib".
    pub vocoder: String,
    /// M4A for the plugins that upload it, encoded once per call.
    pub m4a: M4a,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct M4a {
    /// "auto" | "ffmpeg" | "afconvert" | "fdkaac" | "none"
    pub encoder: String,
    pub bitrate_kbps: u32,
}

impl Default for M4a {
    fn default() -> Self {
        M4a { encoder: "auto".into(), bitrate_kbps: 32 }
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
            min_call_s: 0.0,
            max_call_s: 0.0,
            min_transmission_s: 0.0,
            capture_frames: false,
            normalize_audio: true,
            digital_level_db: 0.0,
            analog_level_db: 0.0,
            compress_wav: false,
            audio_archive: true,
            call_log: true,
            archive_files_on_failure: true,
            filename_format: String::new(),
            drop_duplicate_calls: true,
            vocoder: "fixed".into(),
            m4a: M4a::default(),
        }
    }
}

/// One system's own recording rules (or the conventional channels'): each
/// left out is as in [`Recording`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_timeout_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_unknown: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_encrypted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_unit_to_unit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_silent_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_call_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_call_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_transmission_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalize_audio: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digital_level_db: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analog_level_db: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compress_wav: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_archive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_log: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_files_on_failure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename_format: Option<String>,
}

impl RecordingOverride {
    pub fn is_empty(&self) -> bool {
        *self == RecordingOverride::default()
    }
}

impl Recording {
    /// These settings with a system's own on top.
    pub fn with(&self, o: &RecordingOverride) -> Recording {
        let mut r = self.clone();
        macro_rules! take {
            ($($f:ident),*) => { $( if let Some(v) = &o.$f { r.$f = v.clone(); } )* };
        }
        take!(call_timeout_s, record_unknown, record_encrypted, record_unit_to_unit, keep_silent_calls, min_call_s, max_call_s, min_transmission_s);
        take!(normalize_audio, digital_level_db, analog_level_db, compress_wav, audio_archive, call_log, archive_files_on_failure, filename_format);
        r
    }

    /// What the engine does with a finished call.
    fn save_rules(&self) -> SaveRules {
        SaveRules {
            keep_silent: self.keep_silent_calls,
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

/// A new RTL-SDR's gain, dB. Higher gains overload the front end near
/// strong transmitters without clipping the ADC: on an 858 MHz simulcast
/// system 38.6 dB gave ~3x the FEC errors (and 5 % repeated voice frames)
/// that 23–28 dB did.
pub const RTL_DEFAULT_GAIN_DB: f32 = 25.4;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub sources: Vec<Source>,
    /// The trunked systems (sites).
    pub systems: Vec<System>,
    /// The conventional systems; the `k`th's calls are numbered
    /// [`trunk_core::trunk::conventional_system`]`(k)`.
    pub conventional: Vec<Conventional>,
    pub recording: Recording,
    pub server: Server,
    /// The plugins, by id: on or off, and their settings for the whole recorder.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, PluginSetup>,
    /// The log: how much, where to, and how lines read.
    pub log: LogSettings,
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
    /// ANSI colour: "console", "logfile", "all" or "none"; empty = the
    /// console's when it is a terminal and NO_COLOR isn't set.
    pub color: String,
    pub frequency_format: crate::log::FrequencyFormat,
    pub talkgroup_display_format: crate::log::TalkgroupFormat,
    pub status_as_string: bool,
    /// A control channel decoding fewer messages a second than this is
    /// logged as an error; −1 logs the rate always.
    pub control_warn_rate: f64,
}

impl Default for LogSettings {
    fn default() -> Self {
        LogSettings {
            level: crate::log::Level::Info,
            console: true,
            file: false,
            dir: String::new(),
            syslog_friendly: false,
            syslog: false,
            color: String::new(),
            frequency_format: Default::default(),
            talkgroup_display_format: Default::default(),
            status_as_string: true,
            control_warn_rate: 10.0,
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
            sources: vec![Source::Rtlsdr { serial: String::new(), center_hz: 0.0, rate_hz: 2_400_000.0, gain_db: RTL_DEFAULT_GAIN_DB, agc: false, ppm: 0, auto_tune: false }],
            systems: vec![],
            conventional: vec![],
            recording: Recording::default(),
            server: Server::default(),
            plugins: BTreeMap::new(),
            log: LogSettings::default(),
        }
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

pub fn default_capture_dir() -> PathBuf {
    home().join("TrunkRecorderPro")
}

impl Config {
    /// The config at `path`, with a linked channel file read in; the
    /// defaults when there's no file yet. A file that isn't a config is an
    /// error, not the defaults (which would be saved over it).
    pub fn load(path: &Path) -> Result<Config, String> {
        let mut c: Config = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: not a config this version reads ({e})", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        c.load_channel_files(path);
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
        std::fs::write(path, serde_json::to_string_pretty(&c).unwrap_or_default())
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
    fn engine_channels(&self) -> Vec<ConvChannel> {
        let mut out = Vec::new();
        for (k, v) in self.conventional.iter().enumerate().filter(|(_, v)| v.enabled) {
            let tgs = channel_talkgroups(&v.channels);
            for (c, tg) in v.channels.iter().zip(tgs).filter(|(c, _)| c.enabled) {
                let mut e = c.engine_channel(tg);
                e.system = k;
                e.squelch_db = Some(e.squelch_db.unwrap_or(v.squelch_db));
                out.push(e);
            }
        }
        out
    }

    /// The systems being recorded (enabled, with a control channel), in the
    /// engine's order — a call's `system` indexes this.
    pub fn active_systems(&self) -> impl Iterator<Item = &System> {
        self.systems.iter().filter(|s| s.active())
    }

    /// Each source's centre: as set, or (0 = auto) placed over what the
    /// sources before it don't cover yet — every system (control and known
    /// voice channels) and the conventional channels if they fit together,
    /// else the control and conventional channels, else the first uncovered
    /// system, else its control channels alone.
    pub fn resolved_centers(&self) -> Vec<f64> {
        // Groups to cover: (all its channels, the ones it can't do without).
        let mut groups: Vec<(Vec<f64>, Vec<f64>)> = self
            .active_systems()
            .map(|s| {
                // A DMR site's watched frequencies are all needed.
                let dmr: Vec<f64> = if s.is_dmr() { s.dmr().channels } else { vec![] };
                let need: Vec<f64> = s.control_channels.iter().chain(&dmr).copied().collect();
                (need.iter().chain(&s.voice_channels).copied().collect(), need)
            })
            .collect();
        for v in self.conventional.iter().filter(|v| v.enabled) {
            let conv: Vec<f64> = v.channels.iter().filter(|c| c.enabled).map(|c| c.freq_hz).filter(|&f| f > 0.0).collect();
            if !conv.is_empty() {
                groups.push((conv.clone(), conv));
            }
        }
        let mut centers: Vec<f64> = self.sources.iter().map(|s| s.center_hz()).collect();
        let covered = |centers: &[f64], f: f64| self.sources.iter().zip(centers).any(|(s, &c)| c > 0.0 && (f - c).abs() <= usable_half_width(s.rate_hz()));
        for i in 0..self.sources.len() {
            if centers[i] > 0.0 {
                continue;
            }
            let open: Vec<&(Vec<f64>, Vec<f64>)> = groups.iter().filter(|(_, need)| !need.iter().any(|&f| covered(&centers, f))).collect();
            let Some(first) = open.first() else { break };
            let rate = self.sources[i].rate_hz();
            let all: Vec<f64> = open.iter().flat_map(|g| g.0.iter().copied()).collect();
            let needed: Vec<f64> = open.iter().flat_map(|g| g.1.iter().copied()).collect();
            centers[i] = auto_center(&all, rate)
                .or_else(|| auto_center(&needed, rate))
                .or_else(|| auto_center(&first.0, rate))
                .or_else(|| auto_center(&first.1, rate))
                .unwrap_or(0.0);
        }
        centers
    }

    /// Why this config can't start, or None.
    pub fn problem(&self) -> Option<String> {
        if self.sources.is_empty() {
            return Some("Add a source (a dongle or a capture file).".into());
        }
        let trunked = self.active_systems().next().is_some();
        if !trunked && self.enabled_channels().next().is_none() {
            return Some("Add a system with a control channel, or a conventional channel.".into());
        }
        let mut names = std::collections::HashSet::new();
        for s in self.active_systems() {
            if s.short_name.is_empty() {
                return Some("Every system needs a short name.".into());
            }
            if !names.insert(s.short_name.as_str()) {
                return Some(format!("Two systems are named \"{}\" — each needs its own short name (its folder).", s.short_name));
            }
        }
        let centers = self.resolved_centers();
        if let Some(i) = centers.iter().position(|&c| c <= 0.0) {
            return Some(format!(
                "Set a center frequency for source {} — it couldn't be placed automatically (nothing left for it to cover, or the channels don't fit one source).",
                i + 1
            ));
        }
        let inside = |f: f64| self.sources.iter().zip(&centers).any(|(s, &c)| (f - c).abs() <= usable_half_width(s.rate_hz()));
        for s in self.active_systems() {
            if s.is_smartnet() {
                if let Err(e) = s.smartnet() {
                    return Some(format!("{}: {e}", s.short_name));
                }
            }
            if s.is_dmr() {
                let out: Vec<String> = s.control_channels.iter().chain(&s.dmr().channels).filter(|&&f| !inside(f)).map(|f| format!("{:.5}", f / 1e6)).collect();
                if !out.is_empty() {
                    return Some(format!("{}: DMR frequencies outside every source's bandwidth: {} MHz — move a center frequency or add a source.", s.short_name, out.join(", ")));
                }
            }
            if !s.control_channels.iter().any(|&f| inside(f)) {
                return Some(format!("No control channel of {} falls inside any source's bandwidth — move a center frequency or add a source.", s.short_name));
            }
        }
        let mut conv_names = std::collections::HashSet::new();
        for v in self.conventional.iter().filter(|v| v.enabled) {
            if v.short_name.is_empty() {
                return Some("Every conventional system needs a short name.".into());
            }
            if !conv_names.insert(v.short_name.as_str()) {
                return Some(format!("Two conventional systems are named \"{}\" — each needs its own short name (its folder).", v.short_name));
            }
        }
        if self.conventional.len() > MAX_CONVENTIONAL {
            return Some(format!("At most {MAX_CONVENTIONAL} conventional systems."));
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
        check_channels(&self.engine_channels()).err()
    }

    /// System `s`'s recording rules (None: the conventional channels').
    /// The recording rules of the system a call's `system` names: a trunked
    /// system's (by the engine's order) or a conventional system's.
    pub fn recording_of(&self, system: u16) -> Recording {
        let own = match conventional_index(system) {
            Some(k) => self.conventional.get(k).map(|v| &v.recording),
            None => self.active_systems().nth(system as usize).map(|s| &s.recording),
        };
        own.map_or_else(|| self.recording.clone(), |o| self.recording.with(o))
    }

    /// The short name of the system a call's `system` names.
    pub fn short_name_of(&self, system: u16) -> Option<&str> {
        match conventional_index(system) {
            Some(k) => self.conventional.get(k).map(|v| v.short_name.as_str()),
            None => self.active_systems().nth(system as usize).map(|s| s.short_name.as_str()),
        }
    }

    pub fn engine_config(&self, epoch_ms: f64) -> EngineConfig {
        let centers = self.resolved_centers();
        let systems: Vec<SystemConfig> = self
            .active_systems()
            .map(|s| SystemConfig {
                short_name: s.short_name.clone(),
                control_channels: s.control_channels.clone(),
                calls: self.recording.with(&s.recording).call_config(),
                save: self.recording.with(&s.recording).save_rules(),
                unit_tags: s.unit_names.engine(),
                bank: s.bank(),
                talkgroups: parse_csv(&s.talkgroups_csv),
                expect: s.expect.engine(),
                smartnet: if s.is_smartnet() { s.smartnet().ok() } else { None },
                dmr: s.is_dmr().then(|| s.dmr()),
                site_group: s.site_group.trim().to_string(),
            })
            .collect();
        // A conventional system's P25 / DMR channels look up the talkgroup names
        // of the trunked system with its short name — or of the only one.
        // With one trunked system, they use its receivers too.
        let conv_bank = match systems.as_slice() {
            [one] => one.bank,
            _ => BankConfig::default(),
        };
        let conv_systems: Vec<ConvSystem> = self
            .conventional
            .iter()
            .map(|v| {
                let r = self.recording.with(&v.recording);
                let named = systems.iter().find(|s| s.short_name == v.short_name).or(if systems.len() == 1 { systems.first() } else { None });
                ConvSystem {
                    short_name: v.short_name.clone(),
                    calls: r.call_config(),
                    save: r.save_rules(),
                    talkgroups: named.map(|s| s.talkgroups.clone()).unwrap_or_default(),
                    unit_tags: v.unit_names.engine(),
                }
            })
            .collect();
        EngineConfig {
            systems,
            sources: self.sources.iter().zip(centers).map(|(s, c)| SourceConfig { center_hz: c, rate_hz: s.rate_hz(), auto_tune: s.auto_tune() }).collect(),
            preroll_s: self.recording.preroll_s,
            max_recorders: self.recording.max_recorders,
            conv_systems,
            epoch_ms_at_zero: epoch_ms,
            bank: conv_bank,
            conventional: self.engine_channels(),
            conv: ConvConfig::default(),
            capture_frames: self.recording.capture_frames,
            drop_duplicates: self.recording.drop_duplicate_calls,
            vocoder: trunk_core::mbe::Profile::from_name(&self.recording.vocoder).unwrap_or(trunk_core::mbe::Profile::Fixed),
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
                "sources": [{ "kind": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "agc": true, "ppm": 0 }],
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
        let e = c.engine_config(0.0);
        assert!(e.systems.is_empty());
        assert_eq!(e.conventional.len(), 2);
        assert_eq!(e.conventional[0].talkgroup, 1001);
        assert_eq!(e.conventional[0].info.as_ref().unwrap().alpha_tag, "County Fire Dispatch");
        assert_eq!(e.conventional[1].talkgroup, 154100);
        assert_eq!(e.conventional[1].squelch_db, Some(12.0));
        assert!(e.conventional[1].info.is_none());
        // Enabling the far channel no longer fits one dongle.
        c.conventional[0].channels[2].enabled = true;
        assert!(c.problem().unwrap().contains("center frequency"));
        c.conventional[0].channels.clear();
        assert!(c.problem().unwrap().contains("conventional channel"));
    }

    #[test]
    fn several_conventional_systems() {
        let mut c: Config = serde_json::from_str(
            r#"{
                "sources": [{ "kind": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "agc": true, "ppm": 0 }],
                "conventional": [
                    { "shortName": "fire", "squelchDb": 10, "channels": [{ "freqHz": 154430000, "mode": "fm" }] },
                    { "shortName": "police", "recording": { "minCallS": 2 }, "channels": [{ "freqHz": 154100000, "mode": "fm", "squelchDb": 14 }] },
                    { "shortName": "off", "enabled": false, "channels": [{ "freqHz": 154200000 }] }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(c.problem(), None);
        let e = c.engine_config(0.0);
        assert_eq!(e.conventional.iter().map(|x| (x.system, x.squelch_db)).collect::<Vec<_>>(), [(0, Some(10.0)), (1, Some(14.0))]);
        assert_eq!(e.conv_systems.iter().map(|x| (x.short_name.as_str(), x.save.min_call_s)).collect::<Vec<_>>(), [("fire", 0.0), ("police", 2.0), ("off", 0.0)]);
        assert_eq!(c.short_name_of(trunk_core::trunk::conventional_system(1)), Some("police"));
        assert_eq!(c.recording_of(trunk_core::trunk::conventional_system(1)).min_call_s, 2.0);
        // One frequency, two systems: an error naming both.
        c.conventional[1].channels[0].freq_hz = 154_430_000.0;
        assert!(c.problem().unwrap().contains("both fire and police"), "{:?}", c.problem());
        c.conventional[1].channels[0].freq_hz = 154_100_000.0;
        c.conventional[1].short_name = "fire".into();
        assert!(c.problem().unwrap().contains("Two conventional systems"));
    }

    #[test]
    fn site_groups_and_duplicates() {
        // Older configs: duplicates dropped, groups from the air.
        let c: Config = serde_json::from_str(r#"{ "systems": [{ "shortName": "a", "controlChannels": [851012500] }], "recording": { "preroll_s": 1 } }"#).unwrap();
        assert!(c.recording.drop_duplicate_calls);
        assert!(!serde_json::to_string(&c.systems[0]).unwrap().contains("siteGroup"));
        let c: Config = serde_json::from_str(
            r#"{ "systems": [{ "shortName": "a", "controlChannels": [851012500], "siteGroup": " capmax " }], "recording": { "dropDuplicateCalls": false } }"#,
        )
        .unwrap();
        let e = c.engine_config(0.0);
        assert!(!e.drop_duplicates);
        assert_eq!(e.systems[0].site_group, "capmax");
    }

    #[test]
    fn dmr_system_takes_trunk_recorders_lcn_table() {
        // Trunk Recorder's keys; frequencies in MHz or Hz.
        let s: System = serde_json::from_str(
            r#"{ "shortName": "capmax", "type": "dmr", "controlChannels": [452175000],
                 "lcnTable": { "101": 452.275, "102": 452300000 }, "channels": [452.275], "colorCode": 0 }"#,
        )
        .unwrap();
        assert!(s.is_dmr());
        let d = s.dmr();
        assert_eq!(d.lcn_table.into_iter().collect::<Vec<_>>(), [(101, 452_275_000), (102, 452_300_000)]);
        assert_eq!((d.channels, d.color_code), (vec![452_275_000.0], Some(0)));
        let back: System = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    fn system(name: &str, ccs: &[f64]) -> System {
        System { short_name: name.into(), control_channels: ccs.to_vec(), ..Default::default() }
    }

    #[test]
    fn several_systems_centers_and_engine_config() {
        let rtl = || Source::Rtlsdr { serial: String::new(), center_hz: 0.0, rate_hz: 2_400_000.0, gain_db: 30.0, agc: false, ppm: 0, auto_tune: false };
        let mut c = Config { sources: vec![rtl(), rtl()], ..Default::default() };
        let mut a = system("east", &[851_012_500.0]);
        a.voice_channels = vec![851_500_000.0, 852_000_000.0];
        a.expect = SiteIdentity { nac: Some(0x443), site: Some(3), ..Default::default() };
        let mut b = system("west", &[771_106_250.0]);
        b.recording.record_unknown = Some(false);
        b.recording.min_call_s = Some(2.0);
        c.systems = vec![a, b, System { enabled: false, ..system("off", &[460_000_000.0]) }];
        assert_eq!(c.problem(), None);
        // Each auto source takes a system the ones before it don't cover.
        let centers = c.resolved_centers();
        let hw = usable_half_width(2_400_000.0);
        for f in [851_012_500.0, 851_500_000.0, 852_000_000.0] {
            assert!((f - centers[0]).abs() <= hw, "{f} not in source 1 at {}", centers[0]);
        }
        assert!((771_106_250.0 - centers[1]).abs() <= hw, "west's CC not in source 2 at {}", centers[1]);
        let e = c.engine_config(0.0);
        assert_eq!(e.systems.iter().map(|s| s.short_name.as_str()).collect::<Vec<_>>(), ["east", "west"]);
        assert_eq!(e.systems[0].expect.nac, Some(0x443));
        assert_eq!(e.systems[0].expect.site, Some(3));
        assert!(e.systems[0].calls.record_unknown && !e.systems[1].calls.record_unknown);
        assert_eq!((e.systems[0].save.min_call_s, e.systems[1].save.min_call_s), (0.0, 2.0));
        let saved = serde_json::to_string(&c.systems[1]).unwrap();
        assert!(saved.contains(r#""recording":{"recordUnknown":false,"minCallS":2.0}"#), "{saved}");
        assert!(!serde_json::to_string(&c.systems[0]).unwrap().contains("recording"));
        assert!(e.drop_duplicates && e.systems[0].site_group.is_empty());
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
            tone: tone.into(),
            enabled: true,
        };
        let mut c = Config { conventional: vec![Conventional::default()], ..Default::default() };
        c.conventional[0].channels = vec![row("", None), row("D023N", None), row("151.4", Some(500)), row("94.8", None)];
        assert_eq!(c.problem(), None);
        let e = c.engine_channels();
        assert_eq!(e.iter().map(|x| x.talkgroup).collect::<Vec<_>>(), vec![154325, 1543251, 500, 1543253]);
        assert_eq!(e[1].access, Some(Access::Tone(trunk_core::dsp::tones::Tone::Dcs(23, false))));
        c.conventional[0].channels.push(row("D047I", None));
        assert_eq!(c.problem().as_deref(), Some("Conventional channel 154.32500 MHz has two rows for D023N and D047I (the same signal)."));
        c.conventional[0].channels.pop();
        c.conventional[0].channels.push(row("", None));
        assert!(c.problem().unwrap().contains("listed twice without a tone"));
        c.conventional[0].channels.pop();
        c.conventional[0].channels.push(row("151.5", None));
        assert_eq!(c.problem().as_deref(), Some("Conventional channel 154.32500 MHz: 151.5 Hz isn't a standard CTCSS tone — 151.4?."));
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
            tone: String::new(),
            enabled: true,
        }];
        // Linking a new path writes the current list there.
        c.link_channel_file(0, &cfg_path, "channels.csv").unwrap();
        let file = dir.join("channels.csv");
        assert!(std::fs::read_to_string(&file).unwrap().contains("154.4300,,fm,Fire"));
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
}
