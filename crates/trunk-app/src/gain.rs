//! A radio's gain as something to search: its stages, what each can be set
//! to, and the plan — which stage moves first, which way, in what steps —
//! that suits how the radio is built ([`GainPlan::of`]). The tuner
//! ([`crate::tune`]) follows the plan; config validation and Find my system
//! read the same ranges and tables.
//!
//! The rule for all of them: the front of the chain (the LNA) sets the noise
//! figure and is what strong signals overload, so it moves first; the stages
//! after it only bring the level into the ADC's range, so they move last. A
//! radio whose maker drew a path through its stages (libairspy's linearity
//! and sensitivity tables, an R820T's gain table, an AD9361's gain table
//! behind one overall gain) is searched along that path, one axis: each step
//! there is a setting its maker measured, and the search settles in a
//! handful of comparisons.
//!
//! - **RTL-SDR** (R820T/R828D): the tuner's 29 gains, librtlsdr's walk of the
//!   LNA and mixer with the VGA fixed. About 5 dB steps, then one entry. Its
//!   8-bit ADC overloads early near strong signals.
//!   Behind `rtl_tcp`, the table of the tuner its server reports (an R820T, R828D or E4000).
//! - **Airspy / HydraSDR**: the mode's table step, 0–21, 3 then 1.
//!   Linearity (the default) gives up LNA and mixer before the VGA, what
//!   wins against strong signals; sensitivity keeps the LNA up. In manual,
//!   the LNA, then the mixer, then the VGA, kept to 12 and under (13–15 with
//!   the LNA up saturate the ADC).
//! - **HackRF One**: the LNA (8 dB steps) first, then the VGA (6 dB, then
//!   2 dB, to 40 dB), the RF amp last and only tried on with the LNA at
//!   24 dB or under: it adds little and puts strong signals at the mixer
//!   (more than −5 dBm at the input damages it).
//! - **An Airspy through SoapySDR** (SoapyAirspy, here or behind SoapyRemote):
//!   libairspy's linearity (or sensitivity) table, as for an Airspy, each step
//!   set as SoapyAirspy's LNA, MIX and VGA elements; in manual, those three as
//!   an Airspy's stages. Never its overall gain: SoapySDR fills the stages
//!   front first with it (31 is LNA 15, MIX 15, VGA 1, and anything over 45
//!   the same as 45), which no Airspy table does.
//! - **Another SoapySDR radio that reported its gain elements**: one axis per
//!   element, by the name its driver gave it (MIXER, TIA, IFGR …), in the
//!   steps it reported: an LNA first, then a mixer, the others as listed, a VGA
//!   last; an AMP after them and never higher than it is set.
//! - Through SoapySDR the radio must go back exactly as it was after each
//!   trial: by its overall gain, or, with none set, by every element searched
//!   being set in the source; else the tuner refuses it.
//! - **bladeRF, USRP, other SoapySDR**: the overall gain, which the driver
//!   shares out over the stages front first (libbladeRF's and UHD's gain
//!   groups, the AD9361's table). 6 dB, then 1 dB.

use std::collections::BTreeMap;
use std::fmt;

use sdr_core::gain::Stage;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::config::{AirspyGain, AirspyGains, Source, SourceKind};

/// An R820T / R828D's gains, dB: what an RTL-SDR's gain is set to (the
/// nearest). The driver's table (`rtlsdr-nusb`'s `r82xx` GAINS, in tenths).
pub const R82XX_DB: [f64; 29] = [
    0.0, 0.9, 1.4, 2.7, 3.7, 7.7, 8.7, 12.5, 14.4, 15.7, 16.6, 19.7, 20.7, 22.9, 25.4, 28.0, 29.7, 32.8, 33.8, 36.4, 37.2, 38.6, 40.2, 42.1, 43.4, 43.9, 44.5, 48.0, 49.6,
];
/// An Airspy's (and a HydraSDR's) stage steps, manual: LNA 0..=14, mixer and VGA 0..=15.
pub const AIRSPY_LNA_MAX: u8 = 14;
pub const AIRSPY_STAGE_MAX: u8 = 15;
/// libairspy's linearity and sensitivity tables: steps 0..=21.
pub const AIRSPY_STEP_MAX: u8 = 21;
/// A HackRF's IF (LNA) gain, dB: 0–40 in steps of 8; its baseband (VGA) gain, 0–62 in 2.
pub const HACKRF_LNA: (u16, u16) = (40, 8);
pub const HACKRF_VGA: (u16, u16) = (62, 2);
/// The HackRF's RF amp, dB, when on.
pub const HACKRF_AMP_DB: u16 = 14;

/// libairspy's composite gain tables (airspy.c's `airspy_linearity_*_gains` and
/// `airspy_sensitivity_*_gains`, as crates/libairspy-rs has them): LNA, mixer, VGA,
/// indexed as libairspy indexes them, entry 21 − step.
#[rustfmt::skip]
const AIRSPY_LINEARITY: [[u8; 22]; 3] = [
    [14, 14, 14, 13, 12, 10, 9, 9, 8, 9, 8, 6, 5, 3, 1, 0, 0, 0, 0, 0, 0, 0],
    [12, 12, 11, 9, 8, 7, 6, 6, 5, 0, 0, 1, 0, 0, 2, 2, 1, 1, 1, 1, 0, 0],
    [13, 12, 11, 11, 11, 11, 11, 10, 10, 10, 10, 10, 10, 10, 10, 10, 9, 8, 7, 6, 5, 4],
];
#[rustfmt::skip]
const AIRSPY_SENSITIVITY: [[u8; 22]; 3] = [
    [14, 14, 14, 14, 14, 14, 14, 14, 14, 13, 12, 12, 9, 9, 8, 7, 6, 5, 3, 2, 1, 0],
    [12, 12, 12, 12, 11, 10, 10, 9, 9, 8, 7, 4, 4, 4, 3, 2, 2, 1, 0, 0, 0, 0],
    [13, 12, 11, 10, 9, 8, 7, 6, 5, 5, 5, 5, 5, 4, 4, 4, 4, 4, 4, 4, 4, 4],
];

/// The LNA, mixer and VGA steps libairspy sets for step `step` (0–21, higher is more
/// gain) of its sensitivity table, or else its linearity table.
pub fn airspy_preset(sensitivity: bool, step: u8) -> [u8; 3] {
    let t = if sensitivity { &AIRSPY_SENSITIVITY } else { &AIRSPY_LINEARITY };
    let i = usize::from(AIRSPY_STEP_MAX - step.min(AIRSPY_STEP_MAX));
    [t[0][i], t[1][i], t[2][i]]
}

/// SoapyAirspy's names for an Airspy's LNA, mixer and VGA.
const SOAPY_AIRSPY: [&str; 3] = ["LNA", "MIX", "VGA"];

/// An Airspy behind SoapySDR (SoapyAirspy, on this machine or through SoapyRemote): device
/// arguments with `driver=airspy` or `remote:driver=airspy`.
pub fn soapy_airspy(args: &str) -> bool {
    args.split(',').filter_map(|p| p.split_once('=')).any(|(k, v)| matches!(k.trim(), "driver" | "remote:driver") && v.trim().eq_ignore_ascii_case("airspy"))
}

/// The stage a SoapySDR gain element is, by its name (any case): LNA, MIX or MIXER, PGA,
/// VGA, AMP. Others (IFGR, RFGR, TIA, …) none.
pub fn soapy_stage(element: &str) -> Option<Stage> {
    match element.trim().to_ascii_uppercase().as_str() {
        "LNA" => Some(Stage::Lna),
        "MIX" | "MIXER" => Some(Stage::Mixer),
        "PGA" => Some(Stage::Pga),
        "VGA" => Some(Stage::Vga),
        "AMP" => Some(Stage::Amp),
        _ => None,
    }
}

/// A SoapySDR source's gain for element `element` (its name in any case), if it sets one.
pub fn soapy_gain(gains: &BTreeMap<String, f64>, element: &str) -> Option<f64> {
    gains.iter().find(|(k, _)| k.eq_ignore_ascii_case(element)).map(|(_, v)| *v)
}

/// Set a SoapySDR source's gain for `element`: under the name it has it by (any case), else `element`.
pub fn set_soapy_gain(gains: &mut BTreeMap<String, f64>, element: &str, v: f64) {
    let k = gains.keys().find(|k| k.eq_ignore_ascii_case(element)).cloned().unwrap_or_else(|| element.to_string());
    gains.insert(k, v);
}

/// A gain element a SoapySDR radio reported when it was opened: its name and range.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportedStage {
    pub name: String,
    pub min: f64,
    pub max: f64,
    /// 0: not said (1 is used).
    pub step: f64,
}

impl ReportedStage {
    fn values(&self) -> Vec<f64> {
        let step = if self.step >= 0.1 { self.step } else { 1.0 };
        let n = ((self.max - self.min) / step + 1e-6).floor().max(0.0) as usize;
        (0..=n.min(200)).map(|k| self.min + k as f64 * step).collect()
    }
}

/// The most gain elements a SoapySDR source's gain carries (its whole state goes with every change).
pub const SOAPY_MAX_ELEMENTS: usize = 8;

/// The elements of `reported` searched, each under its own name, front first: an LNA, then a
/// mixer, then the others (TIA, PGA, IFGR …) as the radio listed them, a VGA, and an AMP last.
/// Only those with a range, and a name that fits ([`ElementName`]).
fn soapy_axes(reported: &[ReportedStage]) -> Vec<(ElementName, &ReportedStage)> {
    let mut v: Vec<(ElementName, &ReportedStage)> = reported.iter().filter(|r| r.max > r.min).filter_map(|r| Some((ElementName::new(&r.name)?, r))).collect();
    let order = |n: &ElementName| match soapy_stage(n.as_str()) {
        Some(Stage::Lna) => 0,
        Some(Stage::Mixer) => 1,
        Some(Stage::Vga) => 3,
        Some(Stage::Amp) => 4,
        _ => 2,
    };
    v.sort_by_key(|(n, _)| order(n));
    v
}

/// An Airspy's LNA, mixer and VGA through SoapySDR: its elements as set, else as SoapySDR
/// fills them from the overall gain (front first, LNA 0–15, then MIX, then VGA).
fn soapy_airspy_stages(gains: &BTreeMap<String, f64>, gain_db: Option<f64>) -> [f64; 3] {
    let mut left = gain_db.unwrap_or(0.0).max(0.0);
    SOAPY_AIRSPY.map(|e| {
        let filled = left.min(15.0);
        left -= filled;
        soapy_gain(gains, e).unwrap_or(filled)
    })
}

/// Something a search moves: a stage, a step of the radio's own table, one overall gain, or a
/// SoapySDR gain element by its own name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Knob {
    Stage(StageKey),
    /// A step of libairspy's linearity or sensitivity table.
    Step,
    /// The overall gain, dB (the driver shares it out).
    Db,
    /// A SoapySDR gain element, by the name its driver gave it (MIXER, TIA, IFGR …).
    Element(ElementName),
}

/// A SoapySDR gain element's name, as its driver spells it, kept inline (so a gain can be
/// copied like the others'): 1–15 printable ASCII characters, no `,` or `=`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementName {
    len: u8,
    bytes: [u8; 15],
}

impl ElementName {
    /// None: a name that doesn't fit (empty, longer than 15, or not plain ASCII).
    pub fn new(name: &str) -> Option<ElementName> {
        let ok = !name.is_empty() && name.len() <= 15 && name.bytes().all(|b| b.is_ascii_graphic() && b != b',' && b != b'=');
        ok.then(|| {
            let mut bytes = [0; 15];
            bytes[..name.len()].copy_from_slice(name.as_bytes());
            ElementName { len: name.len() as u8, bytes }
        })
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}

impl fmt::Debug for ElementName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// [`Stage`], ordered and hashable as a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StageKey(u8);

impl From<Stage> for Knob {
    fn from(s: Stage) -> Knob {
        Knob::Stage(StageKey(s.index() as u8))
    }
}

impl Knob {
    fn stage(self) -> Option<Stage> {
        match self {
            Knob::Stage(StageKey(i)) => Stage::ALL.get(i as usize).copied(),
            _ => None,
        }
    }
    /// Its key: a stage's (`lna`, `vga`, `amp`…), `step`, `gain`, or a SoapySDR element's name.
    pub fn key(&self) -> &str {
        match (self, self.stage()) {
            (Knob::Element(e), _) => e.as_str(),
            (_, Some(s)) => s.key(),
            (Knob::Step, _) => "step",
            _ => "gain",
        }
    }
    /// A stage's key, `step`, `gain`; any other name, a SoapySDR element's (as written).
    pub fn from_key(k: &str) -> Option<Knob> {
        match k.trim().to_ascii_lowercase().as_str() {
            "step" => Some(Knob::Step),
            "gain" | "db" => Some(Knob::Db),
            "mix" => Some(Stage::Mixer.into()),
            lower => Stage::from_key(lower).map(Knob::from).or_else(|| ElementName::new(k.trim()).map(Knob::Element)),
        }
    }
    /// Whether `k` (as written, [`Knob::from_key`]) names this knob: the same, or a SoapySDR element by its name in
    /// any case, or by the stage it is (`lna`, `mix` for an element named LNA, MIX or MIXER).
    fn named_by(&self, k: &Knob) -> bool {
        match (self, k) {
            (Knob::Element(e), Knob::Element(n)) => e.as_str().eq_ignore_ascii_case(n.as_str()),
            (Knob::Element(e), k) => k.stage().is_some_and(|s| soapy_stage(e.as_str()) == Some(s)),
            _ => self == k,
        }
    }
}

impl Serialize for Knob {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.key())
    }
}

impl<'de> Deserialize<'de> for Knob {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Knob, D::Error> {
        let k = String::deserialize(d)?;
        Knob::from_key(&k).ok_or_else(|| serde::de::Error::custom(format!("no gain stage {k:?}: lna, mixer, vga, amp, step or gain")))
    }
}

/// A gain as set: each knob's value (dB, or a step), in the plan's order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Setting(pub Vec<(Knob, f64)>);

impl Setting {
    /// `lna=11,vga=9`: knobs and values.
    pub fn parse(spec: &str) -> Result<Setting, String> {
        spec.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|kv| {
                let (k, v) = kv.split_once('=').ok_or_else(|| format!("{kv}: write stage=value"))?;
                let knob = Knob::from_key(k).ok_or_else(|| format!("{kv}: the stages are lna, mixer, vga, amp, step and gain"))?;
                let v = match v.trim().to_ascii_lowercase().as_str() {
                    "on" | "true" => 1.0,
                    "off" | "false" => 0.0,
                    n => n.parse::<f64>().map_err(|_| format!("{kv}: not a number"))?,
                };
                Ok((knob, v))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Setting)
    }
}

impl fmt::Display for Setting {
    /// "LNA 10 · MIX 7 · VGA 7", "linearity 14", "AMP off".
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|(k, v)| match (k, k.stage()) {
                (Knob::Element(e), _) => format!("{} {v}", e.as_str()),
                (_, Some(Stage::Amp)) => format!("AMP {}", if *v > 0.0 { "on" } else { "off" }),
                (_, Some(s)) => format!("{} {v}", s.label()),
                (Knob::Step, _) => format!("step {v}"),
                _ => format!("{v} dB"),
            })
            .collect();
        f.write_str(&parts.join(" · "))
    }
}

/// A place on each of a plan's axes (an index into its values).
pub type Point = Vec<usize>;

/// One axis: a knob and the values it takes, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub knob: Knob,
    pub values: Vec<f64>,
    /// The places the search may go (`lo..=hi`): a stage kept out of where
    /// the radio is known to do badly.
    pub lo: usize,
    pub hi: usize,
}

impl Axis {
    fn of(knob: impl Into<Knob>, values: Vec<f64>) -> Axis {
        let hi = values.len().saturating_sub(1);
        Axis { knob: knob.into(), values, lo: 0, hi }
    }
    /// Searched no higher than `v`.
    fn up_to(mut self, v: f64) -> Axis {
        self.hi = self.values.iter().rposition(|&x| x <= v).unwrap_or(0);
        self
    }
    fn nearest(&self, v: f64) -> usize {
        (0..self.values.len()).min_by(|&a, &b| (self.values[a] - v).abs().total_cmp(&(self.values[b] - v).abs())).unwrap_or(0)
    }
}

/// Which way a move goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
}

/// One move of a plan: axis `axis` by `step` places, first `first` and,
/// when that isn't better at once, the other way (unless `one_way`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Move {
    pub axis: usize,
    pub step: usize,
    pub first: Dir,
    pub one_way: bool,
    /// Tried only while axis `.0` is at or below place `.1` (a HackRF's amp: with the LNA at 24 dB or under).
    pub only_below: Option<(usize, usize)>,
}

/// How a radio's gain is searched: its axes, the moves in order, and what
/// a candidate fixes besides (its AGC off, an Airspy's mode).
#[derive(Clone, Debug, PartialEq)]
pub struct GainPlan {
    pub axes: Vec<Axis>,
    pub moves: Vec<Move>,
    /// What the plan is, a line a step (for the report).
    pub about: Vec<String>,
    kind: PlanKind,
    /// A SoapySDR radio's element each axis sets, by name (none for other radios).
    elements: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PlanKind {
    Rtl,
    AirspyTable(AirspyGain),
    AirspyManual,
    Hackrf,
    /// A SpyServer's gain step.
    Index,
    /// An Airspy through SoapySDR: a step of libairspy's table, set as its LNA, MIX and VGA elements.
    SoapyAirspyTable(AirspyGain),
    /// A SoapySDR radio's elements, one axis each ([`GainPlan::elements`]).
    SoapyStages,
    /// bladeRF, USRP, SoapySDR: the overall gain.
    Overall,
}

fn range(lo: u16, hi: u16, by: u16) -> Vec<f64> {
    (lo..=hi).step_by(by as usize).map(f64::from).collect()
}

fn mv(axis: usize, step: usize, first: Dir) -> Move {
    Move { axis, step, first, one_way: false, only_below: None }
}

/// An Airspy's or a HydraSDR's gains.
fn airspy_gains(k: &SourceKind) -> Option<&AirspyGains> {
    match k {
        SourceKind::Airspy { gains, .. } | SourceKind::Hydrasdr { gains, .. } => Some(gains),
        _ => None,
    }
}

impl GainPlan {
    /// The plan for source `src`. `mode`: an Airspy's other gain mode than the one it's in.
    pub fn of(src: &Source, mode: Option<AirspyGain>) -> Result<GainPlan, String> {
        Self::of_with(src, mode, None)
    }

    /// [`GainPlan::of`], given `steps`: the gains its radio reported when it was opened, where they aren't the
    /// kind's own (an RTL-SDR's tuner's table, dB, as `rtl_tcp` names the tuner; a SpyServer's steps, 0 up).
    pub fn of_with(src: &Source, mode: Option<AirspyGain>, steps: Option<&[f64]>) -> Result<GainPlan, String> {
        Self::of_reported(src, mode, steps, &[])
    }

    /// [`GainPlan::of_with`], given also the gain elements a SoapySDR radio reported when it was opened.
    pub fn of_reported(src: &Source, mode: Option<AirspyGain>, steps: Option<&[f64]>, elements: &[ReportedStage]) -> Result<GainPlan, String> {
        let steps = steps.filter(|s| s.len() > 1);
        let mut names: Vec<String> = Vec::new();
        let (kind, axes, moves, about): (PlanKind, Vec<Axis>, Vec<Move>, Vec<&str>) = match &src.kind {
            // Its tuner's table: an R820T's, or another the radio reported (an E4000 behind `rtl_tcp`).
            SourceKind::Rtlsdr { .. } => {
                let table = steps.map_or(R82XX_DB.to_vec(), <[f64]>::to_vec);
                (
                    PlanKind::Rtl,
                    vec![Axis::of(Knob::Db, table.clone())],
                    vec![mv(0, (table.len() / 7).max(1), Dir::Up), mv(0, 1, Dir::Up)],
                    vec!["the tuner's gain table (its LNA and mixer together, the VGA fixed): a seventh of it at a time (about 5 dB on an R820T), then one entry; its AGC off"],
                )
            }
            // A SpyServer source: its server's gain steps, 0 to the highest it reported.
            SourceKind::Spyserver { .. } => {
                let Some(steps) = steps else {
                    return Err("its server hasn't said how many gain steps it has (it says when the recorder lists its radios, and when the source connects): check its address in Setup".into());
                };
                (
                    PlanKind::Index,
                    vec![Axis::of(Knob::Step, steps.to_vec())],
                    vec![mv(0, (steps.len() / 7).max(1), Dir::Up), mv(0, 1, Dir::Up)],
                    vec!["the server's gain steps, as it reported them (an Airspy's table, LNA, mixer and VGA together): a seventh of it at a time, then one step"],
                )
            }
            SourceKind::Airspy { gains, .. } | SourceKind::Hydrasdr { gains, .. } => match mode.unwrap_or(gains.gain_mode) {
                AirspyGain::Manual => (
                    PlanKind::AirspyManual,
                    vec![
                        Axis::of(Stage::Lna, range(0, AIRSPY_LNA_MAX.into(), 1)),
                        Axis::of(Stage::Mixer, range(0, AIRSPY_STAGE_MAX.into(), 1)),
                        Axis::of(Stage::Vga, range(0, AIRSPY_STAGE_MAX.into(), 1)).up_to(12.0),
                    ],
                    vec![mv(0, 3, Dir::Up), mv(1, 3, Dir::Up), mv(2, 3, Dir::Down), mv(0, 1, Dir::Up), mv(1, 1, Dir::Up), mv(2, 1, Dir::Down)],
                    vec![
                        "the LNA first (the noise figure, and what strong signals overload), then the mixer: 3 steps, then 1",
                        "the VGA last, only for the ADC's level, to 12 (higher with the LNA up saturates the ADC); its AGC off",
                    ],
                ),
                m => (
                    PlanKind::AirspyTable(m),
                    vec![Axis::of(Knob::Step, range(0, AIRSPY_STEP_MAX.into(), 1))],
                    vec![mv(0, 3, Dir::Up), mv(0, 1, Dir::Up)],
                    vec![if m == AirspyGain::Sensitivity {
                        "the sensitivity table's step (libairspy's path through LNA, mixer and VGA, the LNA kept up): 3, then 1"
                    } else {
                        "the linearity table's step (libairspy's path through LNA, mixer and VGA, giving up the LNA first): 3, then 1"
                    }],
                ),
            },
            SourceKind::Hackrf { .. } => (
                PlanKind::Hackrf,
                vec![Axis::of(Stage::Lna, range(0, HACKRF_LNA.0, HACKRF_LNA.1)), Axis::of(Stage::Vga, range(0, HACKRF_VGA.0, HACKRF_VGA.1)).up_to(40.0), Axis::of(Stage::Amp, vec![0.0, HACKRF_AMP_DB.into()])],
                vec![mv(0, 1, Dir::Up), mv(1, 3, Dir::Up), mv(1, 1, Dir::Up), Move { axis: 2, step: 1, first: Dir::Down, one_way: false, only_below: Some((0, 3)) }],
                vec![
                    "the LNA first, 8 dB steps (the noise figure, and what strong signals overload)",
                    "then the VGA, for the ADC's level: 6 dB, then 2 dB, to 40 dB",
                    "the RF amp last, tried on only with the LNA at 24 dB or under (more than -5 dBm at the input damages the HackRF)",
                ],
            ),
            // An Airspy through SoapySDR: as an Airspy, its stages set as SoapyAirspy's elements; never the overall gain.
            SourceKind::Soapy { args, agc: false, .. } if soapy_airspy(args) => {
                names = SOAPY_AIRSPY.map(String::from).to_vec();
                let el = |i: usize| Knob::Element(ElementName::new(SOAPY_AIRSPY[i]).expect("a short name"));
                match mode.unwrap_or(AirspyGain::Linearity) {
                    AirspyGain::Manual => (
                        PlanKind::SoapyStages,
                        vec![
                            Axis::of(el(0), range(0, AIRSPY_LNA_MAX.into(), 1)),
                            Axis::of(el(1), range(0, AIRSPY_STAGE_MAX.into(), 1)),
                            Axis::of(el(2), range(0, AIRSPY_STAGE_MAX.into(), 1)).up_to(12.0),
                        ],
                        vec![mv(0, 3, Dir::Up), mv(1, 3, Dir::Up), mv(2, 3, Dir::Down), mv(0, 1, Dir::Up), mv(1, 1, Dir::Up), mv(2, 1, Dir::Down)],
                        vec![
                            "the Airspy's LNA first, then its mixer (SoapySDR's LNA and MIX elements): 3 steps, then 1",
                            "the VGA last, only for the ADC's level, to 12; not the overall gain (SoapySDR fills the stages with it front first)",
                        ],
                    ),
                    m => (
                        PlanKind::SoapyAirspyTable(m),
                        vec![Axis::of(Knob::Step, range(0, AIRSPY_STEP_MAX.into(), 1))],
                        vec![mv(0, 3, Dir::Up), mv(0, 1, Dir::Up)],
                        vec![if m == AirspyGain::Sensitivity {
                            "the sensitivity table's step, as libairspy sets it, through SoapySDR's LNA, MIX and VGA elements: 3, then 1"
                        } else {
                            "the linearity table's step, as libairspy sets it, through SoapySDR's LNA, MIX and VGA elements: 3, then 1 (not the overall gain, which SoapySDR fills front first)"
                        }],
                    ),
                }
            }
            // Another SoapySDR radio that said which gain elements it has: one axis each, by its own name, the front first.
            SourceKind::Soapy { agc: false, gains, .. } if !soapy_axes(elements).is_empty() => {
                let found = soapy_axes(elements);
                names = found.iter().map(|(n, _)| n.as_str().to_string()).collect();
                // An AMP is never set higher than it is (on, it puts strong signals at the mixer): only down, or left.
                let amp = |n: &ElementName| soapy_stage(n.as_str()) == Some(Stage::Amp);
                let axes: Vec<Axis> = found
                    .iter()
                    .map(|(n, r)| {
                        let a = Axis::of(Knob::Element(*n), r.values());
                        if amp(n) { a.up_to(soapy_gain(gains, n.as_str()).unwrap_or(r.min)) } else { a }
                    })
                    .collect();
                let down = |n: &ElementName| amp(n) || soapy_stage(n.as_str()) == Some(Stage::Vga);
                let dir = |i: usize| if down(&found[i].0) { Dir::Down } else { Dir::Up };
                let coarse = axes.iter().enumerate().map(|(i, a)| mv(i, (a.values.len() / 7).max(1), dir(i)));
                let fine = axes.iter().enumerate().map(|(i, _)| mv(i, 1, dir(i)));
                let moves = coarse.chain(fine).collect();
                (
                    PlanKind::SoapyStages,
                    axes,
                    moves,
                    vec![
                        "each gain element the radio reported, by its own name, the front first (an LNA, then a mixer, then the others as it listed them, a VGA last): a seventh of each at a time, then one step",
                        "an AMP last, and never higher than it is set (on, it puts strong signals at the mixer)",
                    ],
                )
            }
            SourceKind::Bladerf { .. } | SourceKind::Usrp { .. } | SourceKind::Soapy { .. } => {
                let top = match &src.kind {
                    SourceKind::Usrp { .. } => 76,
                    _ => 60,
                };
                (
                    PlanKind::Overall,
                    vec![Axis::of(Knob::Db, range(0, top, 1))],
                    vec![mv(0, 6, Dir::Up), mv(0, 1, Dir::Up)],
                    vec!["the overall gain (the driver shares it over the stages, front first): 6 dB, then 1 dB; its AGC off"],
                )
            }
            SourceKind::File { .. } => return Err("a capture file has no gain".into()),
            SourceKind::Stream { .. } => return Err("a stream has no gain".into()),
        };
        if let (SourceKind::Soapy { gain_db: None, agc: false, .. }, PlanKind::Overall) = (&src.kind, kind) {
            return Err("this SoapySDR radio has no overall gain set (its gain is left as the device has it), and hasn't said which gain elements it has (it says when it's opened): set a gain in Setup first".into());
        }
        if matches!(&src.kind, SourceKind::Bladerf { agc: true, .. } | SourceKind::Usrp { agc: true, .. } | SourceKind::Soapy { agc: true, .. }) {
            return Err("its AGC is on, and switching that off takes a restart: turn AGC off in Setup, Stop and Start, then tune".into());
        }
        // The radio has to go back exactly as it was after each trial: by its overall gain (which sets every element
        // again), or, with none set, by every element the search moves being set in the source.
        if let SourceKind::Soapy { gain_db, gains, .. } = &src.kind
            && matches!(kind, PlanKind::SoapyStages | PlanKind::SoapyAirspyTable(_))
        {
            let unset: Vec<&str> = names.iter().map(String::as_str).filter(|n| soapy_gain(gains, n).is_none()).collect();
            if gain_db.is_none() && !unset.is_empty() {
                return Err(format!(
                    "it has no overall gain set and no gain set for {}, so it couldn't be put back as it is after a trial: set its overall gain, or those, in Setup first",
                    unset.join(", ")
                ));
            }
            let all = gains.len() + unset.len();
            if all > SOAPY_MAX_ELEMENTS || gains.keys().chain(&names).any(|n| ElementName::new(n).is_none()) {
                return Err(format!("it has {all} gain elements set or searched, or a name longer than 15 characters: the tuner takes {SOAPY_MAX_ELEMENTS} at most"));
            }
        }
        Ok(GainPlan { axes, moves, about: about.into_iter().map(String::from).collect(), kind, elements: names })
    }

    /// Where `src`'s gain is, as a place on each axis (the nearest value).
    pub fn point(&self, src: &Source) -> Point {
        self.axes.iter().enumerate().map(|(i, a)| a.nearest(self.read(src, i))).collect()
    }

    fn read(&self, src: &Source, axis: usize) -> f64 {
        let knob = self.axes[axis].knob;
        if let SourceKind::Soapy { args, gains, gain_db, .. } = &src.kind {
            return match self.kind {
                // The table's step nearest the stages it has.
                PlanKind::SoapyAirspyTable(m) => {
                    let now = soapy_airspy_stages(gains, *gain_db);
                    let off = |step: u8| airspy_preset(m == AirspyGain::Sensitivity, step).iter().zip(now).map(|(t, n)| (f64::from(*t) - n).abs()).sum::<f64>();
                    (0..=AIRSPY_STEP_MAX).min_by(|a, b| off(*a).total_cmp(&off(*b))).map_or(0.0, f64::from)
                }
                // (An Airspy's elements as SoapySDR filled them, where it sets none.)
                PlanKind::SoapyStages if soapy_airspy(args) => soapy_airspy_stages(gains, *gain_db)[axis.min(2)],
                PlanKind::SoapyStages => soapy_gain(gains, &self.elements[axis]).unwrap_or(self.axes[axis].values[0]),
                _ => gain_db.unwrap_or(0.0),
            };
        }
        let g = airspy_gains(&src.kind);
        match (&src.kind, knob, knob.stage()) {
            (SourceKind::Rtlsdr { gain_db, .. }, ..) => *gain_db,
            (SourceKind::Spyserver { gain, .. }, ..) => f64::from(*gain),
            (_, Knob::Step, _) => g.map_or(0.0, |g| g.gain_step.into()),
            (SourceKind::Hackrf { lna_db, .. }, _, Some(Stage::Lna)) => (*lna_db).into(),
            (SourceKind::Hackrf { vga_db, .. }, _, Some(Stage::Vga)) => (*vga_db).into(),
            (SourceKind::Hackrf { amp, .. }, _, Some(Stage::Amp)) => if *amp { HACKRF_AMP_DB.into() } else { 0.0 },
            (_, _, Some(Stage::Lna)) => g.map_or(0.0, |g| g.lna_step.into()),
            (_, _, Some(Stage::Mixer)) => g.map_or(0.0, |g| g.mixer_step.into()),
            (_, _, Some(Stage::Vga)) => g.map_or(0.0, |g| g.vga_step.into()),
            (SourceKind::Bladerf { gain_db, .. } | SourceKind::Usrp { gain_db, .. }, ..) => *gain_db,
            (SourceKind::Soapy { gain_db, .. }, ..) => gain_db.unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// `src` with its gain set to `p` (and what the plan fixes: its AGC off, an Airspy's mode).
    pub fn apply(&self, src: &Source, p: &Point) -> Source {
        let mut s = src.clone();
        let v = |i: usize| self.axes[i].values[p[i]];
        match (&mut s.kind, self.kind) {
            (SourceKind::Rtlsdr { gain_db, agc, .. }, PlanKind::Rtl) => (*gain_db, *agc) = (v(0), false),
            (SourceKind::Airspy { gains, .. } | SourceKind::Hydrasdr { gains, .. }, PlanKind::AirspyTable(m)) => {
                (gains.gain_mode, gains.gain_step) = (m, v(0) as u8);
            }
            (SourceKind::Airspy { gains, .. } | SourceKind::Hydrasdr { gains, .. }, PlanKind::AirspyManual) => {
                (gains.gain_mode, gains.agc) = (AirspyGain::Manual, false);
                (gains.lna_step, gains.mixer_step, gains.vga_step) = (v(0) as u8, v(1) as u8, v(2) as u8);
            }
            (SourceKind::Spyserver { gain, .. }, PlanKind::Index) => *gain = v(0) as u32,
            (SourceKind::Hackrf { lna_db, vga_db, amp, .. }, PlanKind::Hackrf) => (*lna_db, *vga_db, *amp) = (v(0) as u16, v(1) as u16, v(2) > 0.0),
            (SourceKind::Bladerf { gain_db, .. } | SourceKind::Usrp { gain_db, .. }, PlanKind::Overall) => *gain_db = v(0),
            (SourceKind::Soapy { gain_db, .. }, PlanKind::Overall) => *gain_db = Some(v(0)),
            // The step's stages as SoapyAirspy's elements (they replace what the overall gain filled).
            (SourceKind::Soapy { gains, .. }, PlanKind::SoapyAirspyTable(m)) => {
                for (e, level) in SOAPY_AIRSPY.iter().zip(airspy_preset(m == AirspyGain::Sensitivity, v(0) as u8)) {
                    set_soapy_gain(gains, e, level.into());
                }
            }
            (SourceKind::Soapy { gains, .. }, PlanKind::SoapyStages) => {
                for (i, e) in self.elements.iter().enumerate() {
                    set_soapy_gain(gains, e, v(i));
                }
            }
            _ => {}
        }
        s
    }

    /// `p` as a setting.
    pub fn setting(&self, p: &Point) -> Setting {
        Setting(self.axes.iter().zip(p).map(|(a, &i)| (a.knob, a.values[i])).collect())
    }

    /// `start` with `s`'s knobs set (each to the nearest value its axis takes).
    pub fn point_of(&self, start: &Point, s: &Setting) -> Result<Point, String> {
        let mut p = start.clone();
        for (k, v) in &s.0 {
            let i = self.axes.iter().position(|a| a.knob.named_by(k)).ok_or_else(|| format!("this radio's stages are {}", self.axes.iter().map(|a| a.knob.key()).collect::<Vec<_>>().join(", ")))?;
            p[i] = self.axes[i].nearest(*v);
        }
        Ok(p)
    }

    /// A walk through the plan's moves ([`Walk`]).
    pub fn walk(&self) -> Walk<'_> {
        Walk { plan: self, at: 0, dir: None, moved: false, turned: false, pass: 0, pass_moved: false }
    }
}

/// The plan's moves, one candidate at a time: [`Walk::next`] proposes one
/// from the best so far; [`Walk::kept`] or [`Walk::refused`] says how it went.
/// A move goes on its way while each step is kept; refused at its first
/// step, it tries the other way. After the last move, the moves are walked
/// once more if any was kept (a stage moved can change what suits another).
pub struct Walk<'a> {
    plan: &'a GainPlan,
    at: usize,
    /// The way the current move is going (none: not started).
    dir: Option<Dir>,
    /// The current move has kept a step.
    moved: bool,
    /// It turned to the other way.
    turned: bool,
    pass: usize,
    pass_moved: bool,
}

/// Passes over the moves at most.
const PASSES: usize = 2;

impl Walk<'_> {
    /// The next candidate from `best`; none: the plan is done.
    pub fn next(&mut self, best: &Point) -> Option<Point> {
        loop {
            let m = match self.plan.moves.get(self.at) {
                Some(m) => *m,
                None if self.pass_moved && self.pass + 1 < PASSES => {
                    (self.at, self.pass, self.pass_moved) = (0, self.pass + 1, false);
                    continue;
                }
                None => return None,
            };
            if m.only_below.is_some_and(|(axis, at)| best[axis] > at) {
                self.advance();
                continue;
            }
            let dir = *self.dir.get_or_insert(m.first);
            let a = &self.plan.axes[m.axis];
            let to = match dir {
                Dir::Up => best[m.axis].checked_add(m.step).filter(|&t| t <= a.hi),
                Dir::Down => best[m.axis].checked_sub(m.step).filter(|&t| t >= a.lo),
            };
            match to {
                Some(t) => {
                    let mut p = best.clone();
                    p[m.axis] = t;
                    return Some(p);
                }
                // At the end that way: as if refused there.
                None => {
                    if !self.turn() {
                        self.advance();
                    }
                }
            }
        }
    }

    /// The last candidate was kept: the move goes on its way.
    pub fn kept(&mut self) {
        (self.moved, self.pass_moved) = (true, true);
    }

    /// The last candidate wasn't better (or was tried before): the move turns
    /// back once if it hadn't moved yet, else the next move.
    pub fn refused(&mut self) {
        if !self.turn() {
            self.advance();
        }
    }

    /// The other way, when this move hasn't moved or turned yet.
    fn turn(&mut self) -> bool {
        let m = self.plan.moves[self.at];
        if self.moved || self.turned || m.one_way {
            return false;
        }
        self.turned = true;
        self.dir = Some(match self.dir.unwrap_or(m.first) {
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
        });
        true
    }

    fn advance(&mut self) {
        (self.at, self.dir, self.moved, self.turned) = (self.at + 1, None, false, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(mut kind: serde_json::Value) -> Source {
        (kind["centerHz"], kind["rateHz"]) = (851e6.into(), 2.4e6.into());
        serde_json::from_value(kind).unwrap()
    }

    /// The candidates the walk proposes from `start`, keeping those `keep` says.
    fn walked(plan: &GainPlan, start: Point, keep: impl Fn(&Point, &Point) -> bool) -> Vec<Point> {
        let mut w = plan.walk();
        let mut best = start;
        let mut seen = std::collections::BTreeSet::from([best.clone()]);
        let mut out = vec![];
        while let Some(p) = w.next(&best) {
            if !seen.insert(p.clone()) {
                w.refused();
                continue;
            }
            out.push(p.clone());
            if keep(&best, &p) {
                best = p;
                w.kept();
            } else {
                w.refused();
            }
        }
        out
    }

    #[test]
    fn an_airspy_is_searched_along_its_mode_table() {
        let s = src(serde_json::json!({ "type": "airspy", "gainMode": "linearity", "gainStep": 14 }));
        let plan = GainPlan::of(&s, None).unwrap();
        assert_eq!(plan.point(&s), vec![14]);
        assert_eq!(plan.setting(&[17].to_vec()).to_string(), "step 17");
        let SourceKind::Airspy { gains, .. } = plan.apply(&s, &vec![17]).kind else { panic!() };
        assert_eq!((gains.gain_mode, gains.gain_step), (AirspyGain::Linearity, 17));
        // Toward 19: up by 3 while better (17, 20), then by 1: up (21) refused, so down (19, then 18 refused);
        // a second pass, since it moved, tries only what it hasn't.
        let tried = walked(&plan, vec![14], |b, p| p[0].abs_diff(19) < b[0].abs_diff(19));
        assert_eq!(tried, [[17], [20], [21], [19], [18], [16]].map(|p| p.to_vec()));
    }

    #[test]
    fn an_airspy_in_manual_moves_its_lna_first_and_its_vga_last_and_low() {
        let s = src(serde_json::json!({ "type": "airspy", "gainMode": "manual", "lnaStep": 10, "mixerStep": 10, "vgaStep": 10 }));
        let plan = GainPlan::of(&s, None).unwrap();
        let tried = walked(&plan, plan.point(&s), |_, _| false);
        // Coarse: LNA up then down, the mixer, then the VGA down first (up would pass 12); then fine, in that order.
        let want = [[13, 10, 10], [7, 10, 10], [10, 13, 10], [10, 7, 10], [10, 10, 7], [11, 10, 10], [9, 10, 10], [10, 11, 10], [10, 9, 10], [10, 10, 9], [10, 10, 11]];
        assert_eq!(tried, want.map(|p| p.to_vec()));
        assert!(tried.iter().all(|p| p[2] <= 12), "the VGA stays at 12 or under: {tried:?}");
        let SourceKind::Airspy { gains, .. } = plan.apply(&s, &vec![9, 8, 7]).kind else { panic!() };
        assert_eq!((gains.lna_step, gains.mixer_step, gains.vga_step, gains.agc), (9, 8, 7, false));
    }

    #[test]
    fn a_hackrfs_amp_is_tried_last_and_only_with_the_lna_low() {
        let s = src(serde_json::json!({ "type": "hackrf", "lnaDb": 32, "vgaDb": 20, "amp": false }));
        let plan = GainPlan::of(&s, None).unwrap();
        let start = plan.point(&s);
        assert_eq!(plan.setting(&start).to_string(), "LNA 32 · VGA 20 · AMP off");
        // Nothing better: the amp isn't tried with the LNA at 32.
        assert!(walked(&plan, start.clone(), |_, _| false).iter().all(|p| p[2] == 0));
        // Lower LNA kept: the amp is tried, last.
        let tried = walked(&plan, start, |b, p| p[0] < b[0]);
        let amp_on = tried.iter().position(|p| p[2] == 1).expect("the amp tried");
        let vga_fine = tried.iter().position(|p| p == &vec![0, 11, 0]).expect("the VGA's fine steps");
        assert!(amp_on > vga_fine, "after the LNA and the VGA: {tried:?}");
        assert!(tried.iter().filter(|p| p[2] == 1).all(|p| p[0] <= 3), "with the LNA at 24 dB or under");
        assert!(tried.iter().all(|p| p[1] <= 20), "VGA to 40 dB at most");
        let SourceKind::Hackrf { lna_db, vga_db, amp, .. } = plan.apply(&src(serde_json::json!({ "type": "hackrf" })), &vec![3, 12, 1]).kind else { panic!() };
        assert_eq!((lna_db, vga_db, amp), (24, 24, true));
    }

    #[test]
    fn an_rtl_sdr_walks_its_tuners_table_with_its_agc_off() {
        let s = src(serde_json::json!({ "type": "rtlsdr", "serial": "", "gainDb": 30, "agc": true }));
        let plan = GainPlan::of(&s, None).unwrap();
        let p = plan.point(&s);
        assert_eq!(R82XX_DB[p[0]], 29.7);
        let SourceKind::Rtlsdr { gain_db, agc, .. } = plan.apply(&s, &p).kind else { panic!() };
        assert_eq!((gain_db, agc), (29.7, false));
    }

    #[test]
    fn an_rtl_sdr_walks_its_tuners_table_on_usb_or_the_network() {
        let s = src(serde_json::json!({ "type": "rtlsdr", "address": "pi", "gainDb": 30, "agc": true }));
        // An R820T's, unless it reported another (an empty list is none).
        for reported in [None, Some(&[][..])] {
            assert_eq!(GainPlan::of_with(&s, None, reported).unwrap().axes[0].values, R82XX_DB.to_vec());
        }
        let plan = GainPlan::of(&s, None).unwrap();
        let p = plan.point(&s);
        assert_eq!(R82XX_DB[p[0]], 29.7);
        let SourceKind::Rtlsdr { gain_db, agc, .. } = plan.apply(&s, &p).kind else { panic!() };
        assert_eq!((gain_db, agc), (29.7, false));
        // An E4000's own 14 steps (dB) behind `rtl_tcp`, and coarser moves for a shorter table.
        let e4000 = [-1.0, 1.5, 4.0, 6.5, 9.0, 11.5, 14.0, 16.5, 19.0, 21.5, 24.0, 29.0, 34.0, 42.0];
        let plan = GainPlan::of_with(&s, None, Some(&e4000)).unwrap();
        assert_eq!(plan.axes[0].values, e4000);
        assert_eq!((plan.moves[0].step, plan.moves[1].step), (2, 1));
        assert_eq!(GainPlan::of(&s, None).unwrap().moves[0].step, 4, "about 5 dB on an R820T, as before");
    }

    #[test]
    fn a_spyserver_walks_the_gain_steps_its_server_reported() {
        let s = src(serde_json::json!({ "type": "spyserver", "address": "pi", "gain": 14 }));
        assert!(GainPlan::of(&s, None).unwrap_err().contains("hasn't said"), "not asked yet");
        let steps: Vec<f64> = (0..=21).map(f64::from).collect();
        let plan = GainPlan::of_with(&s, None, Some(&steps)).unwrap();
        assert_eq!(plan.point(&s), vec![14]);
        assert_eq!((plan.moves[0].step, plan.moves[1].step), (3, 1));
        let SourceKind::Spyserver { gain, .. } = plan.apply(&s, &vec![9]).kind else { panic!() };
        assert_eq!(gain, 9);
        assert_eq!(plan.setting(&vec![9]).to_string(), "step 9");
    }

    #[test]
    fn an_overall_gain_radio_needs_its_agc_off_and_a_gain() {
        assert!(GainPlan::of(&src(serde_json::json!({ "type": "usrp", "gainDb": 40, "agc": false })), None).is_ok());
        assert!(GainPlan::of(&src(serde_json::json!({ "type": "bladerf", "agc": true })), None).unwrap_err().contains("AGC"));
        assert!(GainPlan::of(&src(serde_json::json!({ "type": "soapy", "agc": false })), None).unwrap_err().contains("overall gain"));
    }

    /// An Airspy through SoapySDR (here behind SoapyRemote) is searched along libairspy's table, each step set as
    /// SoapyAirspy's LNA, MIX and VGA elements: never along the overall gain, which SoapySDR fills front first.
    #[test]
    fn an_airspy_through_soapysdr_is_searched_along_libairspys_table() {
        let args = "driver=remote,remote=tcp://server:55132,remote:driver=airspy,remote:prot=tcp";
        let s = src(serde_json::json!({ "type": "soapy", "args": args, "agc": false, "gainDb": 31 }));
        let plan = GainPlan::of(&s, None).unwrap();
        assert_eq!(plan.axes.iter().map(|a| a.knob).collect::<Vec<_>>(), [Knob::Step], "no overall gain axis");
        // libairspy's linearity step 14 is entry 7: LNA 9, mixer 6, VGA 10.
        let SourceKind::Soapy { gains, gain_db, .. } = plan.apply(&s, &vec![14]).kind else { panic!() };
        assert_eq!(gains, BTreeMap::from([("LNA".into(), 9.0), ("MIX".into(), 6.0), ("VGA".into(), 10.0)]));
        assert_eq!(gain_db, Some(31.0), "left as it was: the elements replace what it filled");
        let applied = plan.apply(&s, &vec![14]);
        assert_eq!(plan.point(&applied), vec![14], "read back from the elements");
        // An overall 31 is LNA 15, MIX 15, VGA 1 to SoapySDR: no table step, so the start isn't one of its settings.
        assert_ne!(plan.apply(&s, &plan.point(&s)), s);
        // Sensitivity: its own table (step 21 is entry 0: LNA 14, mixer 12, VGA 13); no gainDb needed when its
        // three elements are set (a trial is undone by setting them back).
        let staged = src(serde_json::json!({ "type": "soapy", "args": "driver=airspy", "agc": false, "gains": { "LNA": 5, "MIX": 5, "VGA": 5 } }));
        let plan = GainPlan::of(&staged, Some(AirspyGain::Sensitivity)).unwrap();
        let SourceKind::Soapy { gains, .. } = plan.apply(&staged, &vec![21]).kind else { panic!() };
        assert_eq!(gains, BTreeMap::from([("LNA".into(), 14.0), ("MIX".into(), 12.0), ("VGA".into(), 13.0)]));
        // Neither an overall gain nor its elements: it couldn't be put back after a trial, so it isn't tuned.
        let bare = src(serde_json::json!({ "type": "soapy", "args": "driver=airspy", "agc": false }));
        assert!(GainPlan::of(&bare, None).unwrap_err().contains("couldn't be put back"));
        // Manual: the three elements as an Airspy's stages, the VGA kept to 12; names as the source has them.
        let named = src(serde_json::json!({ "type": "soapy", "args": args, "agc": false, "gains": { "lna": 10, "MIX": 10, "VGA": 10 } }));
        let plan = GainPlan::of(&named, Some(AirspyGain::Manual)).unwrap();
        assert_eq!(plan.point(&named), vec![10, 10, 10]);
        assert_eq!(plan.axes[2].hi, 12);
        let SourceKind::Soapy { gains, .. } = plan.apply(&named, &vec![11, 9, 7]).kind else { panic!() };
        assert_eq!(gains, BTreeMap::from([("lna".into(), 11.0), ("MIX".into(), 9.0), ("VGA".into(), 7.0)]));
        assert_eq!(plan.setting(&vec![11, 9, 7]).to_string(), "LNA 11 · MIX 9 · VGA 7");
    }

    /// libairspy's tables as airspy.c has them, entry 21 − step (trunk-pro checks them against libairspy-rs's own).
    #[test]
    fn libairspys_tables_are_read_from_the_end() {
        assert_eq!(airspy_preset(false, 21), [14, 12, 13]);
        assert_eq!(airspy_preset(false, 0), [0, 0, 4]);
        assert_eq!(airspy_preset(true, 0), [0, 0, 4]);
        assert_eq!(airspy_preset(false, 30), airspy_preset(false, 21), "clamped as libairspy clamps");
    }

    /// Another SoapySDR radio that reported its gain elements is searched stage by stage: the elements named as a
    /// stage, the front first, in the steps it reported; the others, and an AMP, stay as set.
    #[test]
    fn a_soapysdr_radio_is_searched_by_the_elements_it_reported() {
        let el = |name: &str, min: f64, max: f64, step: f64| ReportedStage { name: name.into(), min, max, step };
        // As the radio listed them: TIA, a mixer called MIXER, LNA, AMP, IFGR.
        let reported = [el("TIA", 0.0, 12.0, 3.0), el("MIXER", 0.0, 15.0, 1.0), el("LNA", 0.0, 30.0, 1.0), el("AMP", 0.0, 14.0, 14.0), el("IFGR", 20.0, 59.0, 1.0)];
        let s = src(serde_json::json!({ "type": "soapy", "args": "driver=lime", "agc": false, "gainDb": 20, "gains": { "AMP": 14 } }));
        let plan = GainPlan::of_reported(&s, None, None, &reported).unwrap();
        // One axis each, by its own name: an LNA, a mixer, the others as listed, an AMP last.
        assert_eq!(plan.axes.iter().map(|a| a.knob.key()).collect::<Vec<_>>(), ["LNA", "MIXER", "TIA", "IFGR", "AMP"]);
        assert_eq!((plan.axes[3].values[0], plan.axes[3].values.len()), (20.0, 40));
        assert_eq!(plan.axes[4].hi, 1, "the AMP at most as it is (on)");
        assert_eq!(plan.point(&s), vec![0, 0, 0, 0, 1]);
        let SourceKind::Soapy { gains, gain_db, .. } = plan.apply(&s, &vec![10, 6, 2, 5, 0]).kind else { panic!() };
        let want = [("AMP", 0.0), ("IFGR", 25.0), ("LNA", 10.0), ("MIXER", 6.0), ("TIA", 6.0)];
        assert_eq!(gains, BTreeMap::from(want.map(|(k, v)| (k.to_string(), v))), "MIXER stays MIXER");
        assert_eq!(gain_db, Some(20.0));
        assert_eq!(plan.setting(&vec![10, 6, 2, 5, 0]).to_string(), "LNA 10 · MIXER 6 · TIA 6 · IFGR 25 · AMP 0");
        assert_eq!(plan.moves[0], mv(0, 4, Dir::Up), "a seventh of the LNA's 31 steps");
        assert_eq!(plan.point_of(&plan.point(&s), &Setting::parse("tia=9, mix=3").unwrap()).unwrap(), vec![0, 3, 3, 0, 1], "by name in any case, or by stage");
        // An AMP not set: never switched on.
        let off = src(serde_json::json!({ "type": "soapy", "args": "driver=lime", "agc": false, "gainDb": 20 }));
        assert_eq!(GainPlan::of_reported(&off, None, None, &reported).unwrap().axes[4].hi, 0);
        // No overall gain: every element searched must be set, or a trial couldn't be undone.
        let two = [el("LNA", 0.0, 30.0, 1.0), el("TIA", 0.0, 12.0, 3.0)];
        let partial = src(serde_json::json!({ "type": "soapy", "args": "driver=lime", "agc": false, "gains": { "LNA": 20 } }));
        let e = GainPlan::of_reported(&partial, None, None, &two).unwrap_err();
        assert!(e.contains("couldn't be put back") && e.contains("TIA"), "{e}");
        let full = src(serde_json::json!({ "type": "soapy", "args": "driver=lime", "agc": false, "gains": { "LNA": 20, "TIA": 9 } }));
        assert!(GainPlan::of_reported(&full, None, None, &two).is_ok());
        // More elements than a gain carries.
        let many: Vec<ReportedStage> = (1..=9).map(|i| el(&format!("G{i}"), 0.0, 10.0, 1.0)).collect();
        assert!(GainPlan::of_reported(&s, None, None, &many).unwrap_err().contains("at most"));
        // Nothing reported (not opened yet), and no overall gain: nothing to search.
        assert!(GainPlan::of(&partial, None).unwrap_err().contains("which gain elements"));
        // Its AGC on: as before.
        let agc = src(serde_json::json!({ "type": "soapy", "args": "driver=lime", "agc": true }));
        assert!(GainPlan::of_reported(&agc, None, None, &reported).unwrap_err().contains("AGC"));
    }

    #[test]
    fn a_setting_is_written_by_stage() {
        let s = src(serde_json::json!({ "type": "airspy", "gainMode": "manual", "lnaStep": 14, "mixerStep": 7, "vgaStep": 7 }));
        let plan = GainPlan::of(&s, None).unwrap();
        let start = plan.point(&s);
        assert_eq!(plan.point_of(&start, &Setting::parse("lna=11, vga=9").unwrap()).unwrap(), vec![11, 7, 9]);
        assert_eq!(plan.point_of(&start, &Setting::parse("mix=20").unwrap()).unwrap(), vec![14, 15, 7], "snapped to what the stage takes");
        assert!(plan.point_of(&start, &Setting::parse("step=3").unwrap()).unwrap_err().contains("lna, mixer, vga"));
        assert!(Setting::parse("lna").is_err());
    }
}
