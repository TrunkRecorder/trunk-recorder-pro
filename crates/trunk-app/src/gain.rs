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
//! - **RTL-SDR through `rtl_tcp`**: the same walk of the table of the tuner its server reports
//!   (an R820T, R828D or E4000), known once the source has connected.
//! - **Airspy / HydraSDR**: the mode's table step, 0–21, 3 then 1.
//!   Linearity (the default) gives up LNA and mixer before the VGA, what
//!   wins against strong signals; sensitivity keeps the LNA up. In manual,
//!   the LNA, then the mixer, then the VGA, kept to 12 and under (13–15 with
//!   the LNA up saturate the ADC).
//! - **HackRF One**: the LNA (8 dB steps) first, then the VGA (6 dB, then
//!   2 dB, to 40 dB), the RF amp last and only tried on with the LNA at
//!   24 dB or under: it adds little and puts strong signals at the mixer
//!   (more than −5 dBm at the input damages it).
//! - **bladeRF, USRP, SoapySDR**: the overall gain, which the driver
//!   shares out over the stages front first (libbladeRF's and UHD's gain
//!   groups, the AD9361's table). 6 dB, then 1 dB.

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

/// Something a search moves: a stage, a step of the radio's own table, or one overall gain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Knob {
    Stage(StageKey),
    /// A step of libairspy's linearity or sensitivity table.
    Step,
    /// The overall gain, dB (the driver shares it out).
    Db,
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
    /// Its key: a stage's (`lna`, `vga`, `amp`…), `step`, `gain`.
    pub fn key(self) -> &'static str {
        match (self, self.stage()) {
            (_, Some(s)) => s.key(),
            (Knob::Step, _) => "step",
            _ => "gain",
        }
    }
    pub fn from_key(k: &str) -> Option<Knob> {
        match k.trim().to_ascii_lowercase().as_str() {
            "step" => Some(Knob::Step),
            "gain" | "db" => Some(Knob::Db),
            "mix" => Some(Stage::Mixer.into()),
            k => Stage::from_key(k).map(Knob::from),
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PlanKind {
    Rtl,
    AirspyTable(AirspyGain),
    AirspyManual,
    Hackrf,
    /// A SpyServer's gain step.
    Index,
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

    /// [`GainPlan::of`], given `steps`: what an `rtl_tcp` or SpyServer source's server reported of its gain, the one thing
    /// about such a source its config doesn't say (the tuner's gain table, dB; a SpyServer's steps, 0 up). Another kind
    /// has no use for it.
    pub fn of_with(src: &Source, mode: Option<AirspyGain>, steps: Option<&[f64]>) -> Result<GainPlan, String> {
        let (kind, axes, moves, about): (PlanKind, Vec<Axis>, Vec<Move>, Vec<&str>) = match &src.kind {
            SourceKind::Rtlsdr { .. } => (
                PlanKind::Rtl,
                vec![Axis::of(Knob::Db, R82XX_DB.to_vec())],
                vec![mv(0, 4, Dir::Up), mv(0, 1, Dir::Up)],
                vec!["the tuner's gain table (its LNA and mixer together, the VGA fixed): about 5 dB steps, then one entry; its AGC off"],
            ),
            // An `rtl_tcp` source: its server's tuner's table, as the server reported the tuner (it is known once connected).
            SourceKind::Rtltcp { .. } => {
                let Some(steps) = steps.filter(|s| s.len() > 1) else {
                    return Err("its server hasn't reported a tuner with a gain table this knows (it is read when the server connects, and these are the R820T, R828D and E4000): wait for it to connect".into());
                };
                (
                    PlanKind::Rtl,
                    vec![Axis::of(Knob::Db, steps.to_vec())],
                    vec![mv(0, (steps.len() / 7).max(1), Dir::Up), mv(0, 1, Dir::Up)],
                    vec!["the server's tuner's gain table, as it reported it (its LNA and mixer together): a seventh of it at a time, then one entry; its AGC off"],
                )
            }
            // A SpyServer source: its server's gain steps, 0 to the highest it reported.
            SourceKind::Spyserver { .. } => {
                let Some(steps) = steps.filter(|s| s.len() > 1) else {
                    return Err("its server hasn't said how many gain steps it has (Setup asks it when its address is entered, and it is read again when the source connects): enter the address in Setup, or wait for it to connect".into());
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
        };
        if let SourceKind::Soapy { gain_db: None, .. } = &src.kind {
            return Err("this SoapySDR radio has no overall gain set (its gain is left as the device has it): set one in Setup first".into());
        }
        if matches!(&src.kind, SourceKind::Bladerf { agc: true, .. } | SourceKind::Usrp { agc: true, .. } | SourceKind::Soapy { agc: true, .. }) {
            return Err("its AGC is on, and switching that off takes a restart: turn AGC off in Setup, Stop and Start, then tune".into());
        }
        Ok(GainPlan { axes, moves, about: about.into_iter().map(String::from).collect(), kind })
    }

    /// Where `src`'s gain is, as a place on each axis (the nearest value).
    pub fn point(&self, src: &Source) -> Point {
        self.axes.iter().map(|a| a.nearest(self.read(src, a.knob))).collect()
    }

    fn read(&self, src: &Source, knob: Knob) -> f64 {
        let g = airspy_gains(&src.kind);
        match (&src.kind, knob, knob.stage()) {
            (SourceKind::Rtlsdr { gain_db, .. } | SourceKind::Rtltcp { gain_db, .. }, ..) => *gain_db,
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
            (SourceKind::Rtlsdr { gain_db, agc, .. } | SourceKind::Rtltcp { gain_db, agc, .. }, PlanKind::Rtl) => (*gain_db, *agc) = (v(0), false),
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
            let i = self.axes.iter().position(|a| a.knob == *k).ok_or_else(|| format!("this radio's stages are {}", self.axes.iter().map(|a| a.knob.key()).collect::<Vec<_>>().join(", ")))?;
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
    fn an_rtl_tcp_source_walks_the_table_of_the_tuner_its_server_reported() {
        let s = src(serde_json::json!({ "type": "rtltcp", "address": "pi", "gainDb": 30, "agc": true }));
        assert!(GainPlan::of(&s, None).unwrap_err().contains("wait for it to connect"), "no tuner reported yet");
        assert!(GainPlan::of_with(&s, None, Some(&[])).is_err());
        let plan = GainPlan::of_with(&s, None, Some(&R82XX_DB)).unwrap();
        let p = plan.point(&s);
        assert_eq!(R82XX_DB[p[0]], 29.7);
        let SourceKind::Rtltcp { gain_db, agc, .. } = plan.apply(&s, &p).kind else { panic!() };
        assert_eq!((gain_db, agc), (29.7, false));
        // An E4000's own 14 steps (dB), and coarser moves for a shorter table.
        let e4000 = [-1.0, 1.5, 4.0, 6.5, 9.0, 11.5, 14.0, 16.5, 19.0, 21.5, 24.0, 29.0, 34.0, 42.0];
        let plan = GainPlan::of_with(&s, None, Some(&e4000)).unwrap();
        assert_eq!(plan.axes[0].values, e4000);
        assert_eq!((plan.moves[0].step, plan.moves[1].step), (2, 1));
        assert_eq!((GainPlan::of_with(&s, None, Some(&R82XX_DB)).unwrap().moves[0].step), 4);
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
