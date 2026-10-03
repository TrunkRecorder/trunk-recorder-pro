//! Conventional channels (analog NBFM, P25): energy-detected, so an idle
//! channel costs almost nothing.
//!
//! ```text
//! every block, per channel, from the channelizer's own spectrum (no head):
//!   band power (±5 kHz) vs the local noise floor → SNR (dB)
//! SNR ≥ squelch → open a head with pre-roll (the air from before detection)
//!   → channel filter / carrier meter (±5.5 kHz: confirms the carrier, so FFT
//!     leakage from a strong neighbour doesn't make a call)
//!   → NBFM demod → 8 kHz audio          (fm; unit IDs from MDC1200 /
//!                                        FleetSync bursts in it, the CTCSS
//!                                        tone / DCS code from below it)
//!   → receiver bank → voice tracker     (p25; talkgroup from link control)
//!   → 4FSK → DMR framer → both slots   (dmr; a call on each slot, talkgroup
//!                                        from link control)
//! a call starts with the first audio (or P25 link control), ends when there
//! has been none for the call timeout; the head closes once the carrier has
//! been gone for CLOSE_HANG_S with no call.
//! ```
//!
//! Several rows on one frequency are one channel, each row with its own
//! access code ([`Access`]: a CTCSS tone or DCS code, a P25 NAC, a DMR colour
//! code / slot / talkgroup), at most one with none. Each transmission goes
//! to the row whose code it carries (the most specific, for DMR), else to
//! the row with none, else isn't recorded:
//! - FM: held until its tone is known, then filed under that row's
//!   talkgroup, as P25 files a call under the talkgroup its link control
//!   names. A frequency whose only row has no tone isn't held.
//! - P25: every frame carries the NAC, so nothing is held; on a frequency
//!   with several rows, filed under the row's talkgroup (conventional radios
//!   mostly send a talkgroup that says little, often 1).
//! - DMR: per slot; filed under the talkgroup on the air, as always (the row
//!   gives the names).
//!
//! A detection the carrier meter doesn't confirm raises that channel's open
//! threshold to just above what triggered it, until the band quietens again,
//! so a neighbour's leakage doesn't reopen the head every block.
//!
//! The noise floor is per source: the median of each 1/64 of the band,
//! smoothed over time, which follows the SDR's passband shape and ignores
//! signals filling under half a slice.

use num_complex::Complex32;

use super::calls::{conventional_index, conventional_system, Call, CallId, CallManager, CallSource};
use super::frames::CallFrames;
use super::record::{Reception, Transmissions};
use super::talkgroups::Talkgroup;
use super::voice::TrackerOut;
use super::voice::{self, VoiceDecoder, VoiceKind, VoiceSpec};
use super::frames::VoiceFrame;
use crate::dsp::fm::{self, ChannelFilter};
use crate::dsp::tones::{Tone, ToneDetector};
use crate::dsp::{Channelizer, HeadId};
use crate::mbe;
use crate::p25::alias::Alias;
use crate::p25::diversity::BankConfig;

/// Half-width of the band the detector sums, Hz (a 12.5 kHz channel's signal).
const DETECT_HALF_BW: f64 = 5000.0;
/// Head filter cutoff (the carrier meter narrows it further), Hz.
const HEAD_CUTOFF_HZ: f64 = 7000.0;
/// Band power smoothing, s.
const DETECT_TAU_S: f64 = 0.02;
/// Noise floor slices per source.
const FLOOR_SLICES: usize = 64;
/// Noise floor refresh, s, and its smoothing per refresh.
const FLOOR_EVERY_S: f64 = 0.1;
const FLOOR_ALPHA: f64 = 0.3;
/// Carrier-meter threshold below the open threshold (hysteresis), dB.
const CLOSE_BELOW_DB: f64 = 3.0;
/// After a detection the carrier meter didn't confirm, reopen only this far
/// above the level that caused it, dB.
const FALSE_OPEN_RAISE_DB: f64 = 3.0;
/// A head with no call closes once the carrier has been gone this long, s.
const CLOSE_HANG_S: f64 = 0.5;
/// A held transmission with no tone yet goes to the row without one after this, s.
const TONE_WAIT_S: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvMode {
    /// Analog narrowband FM (12.5 kHz).
    Fm,
    /// P25 Phase 1 (C4FM or CQPSK).
    P25,
    /// DMR (Tier II): each of the two slots records its own calls.
    Dmr,
}

impl ConvMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ConvMode::Fm => "fm",
            ConvMode::P25 => "p25",
            ConvMode::Dmr => "dmr",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConvChannel {
    pub freq_hz: f64,
    pub mode: ConvMode,
    /// The talkgroup number calls are filed under (P25: when the air names none).
    pub talkgroup: u32,
    /// Name, description, tag, group for the call record.
    pub info: Option<Talkgroup>,
    /// Open threshold, dB above the noise floor (None: [`ConvConfig::squelch_db`]).
    pub squelch_db: Option<f64>,
    /// Record only transmissions carrying this code (None: any — or, beside
    /// rows with codes on the frequency, the rest).
    pub access: Option<Access>,
    /// The conventional system it belongs to (from 0): its rules, its calls'
    /// [`Call::system`] ([`conventional_system`]).
    pub system: usize,
}

/// How a row picks its transmissions out of a frequency's (the channel's
/// Tone column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// FM: a CTCSS tone or DCS code.
    Tone(Tone),
    /// P25: the network access code.
    Nac(u16),
    /// DMR: whichever of colour code, slot (1, 2) and talkgroup are given.
    Dmr { cc: Option<u8>, slot: Option<u8>, tg: Option<u32> },
}

impl std::fmt::Display for Access {
    /// `151.4`, `D023N` (Trunk Recorder's); `NAC 293`; `CC 1 TS 2 TG 201`.
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Access::Tone(t) => write!(f, "{t}"),
            Access::Nac(n) => write!(f, "NAC {n:03X}"),
            Access::Dmr { cc, slot, tg } => {
                let parts: Vec<String> =
                    [cc.map(|c| format!("CC {c}")), slot.map(|s| format!("TS {s}")), tg.map(|t| format!("TG {t}"))].into_iter().flatten().collect();
                write!(f, "{}", parts.join(" "))
            }
        }
    }
}

impl Access {
    /// A row's Tone column as people write it, read for its mode:
    /// - FM: [`Tone::parse`] (`151.4 PL`, `023 DPL`, `D023N` …);
    /// - P25: `293`, `293 NAC`, `NAC 293`, `$293`, `0x293`; `F7E` / `F7F`
    ///   (a radio's "receive any") mean any;
    /// - DMR: `CC1`, `1`, RadioReference's `CC1 TS2 TG201` or
    ///   `CC 1 TG 201 SL 2` (its scan list is ignored), `Slot 2`.
    ///
    /// Empty, `0` and `S` (Trunk Recorder's search): any.
    pub fn parse(mode: ConvMode, text: &str) -> Result<Option<Access>, String> {
        let up = text.trim().to_uppercase();
        if mode == ConvMode::Fm {
            return Ok(Tone::parse(text)?.map(Access::Tone));
        }
        if ["", "0", "S", "ANY", "NONE", "SEARCH"].contains(&up.as_str()) {
            return Ok(None);
        }
        if mode == ConvMode::P25 {
            let bad = || format!("\"{}\" isn't a NAC (up to 3 hex digits, like 293)", text.trim());
            let body: String = up.split_whitespace().filter(|w| *w != "NAC").collect();
            let body = body.trim_start_matches('$');
            let body = body.strip_prefix("0X").unwrap_or(body);
            if body.is_empty() || body.len() > 3 {
                return Err(bad());
            }
            let n = u16::from_str_radix(body, 16).map_err(|_| bad())?;
            return Ok((n != 0xf7e && n != 0xf7f).then_some(Access::Nac(n)));
        }
        let bad = || format!("\"{}\" isn't a DMR colour code (CC1), slot (TS2) or talkgroup (TG201)", text.trim());
        // Letters and digits apart: CC1 → CC 1.
        let mut spaced = String::new();
        for c in up.chars() {
            if spaced.chars().last().is_some_and(|l| l.is_ascii_digit() != c.is_ascii_digit() && l != ' ') && c != ' ' {
                spaced.push(' ');
            }
            spaced.push(if c == ',' || c == ':' || c == '=' { ' ' } else { c });
        }
        let words: Vec<&str> = spaced.split_whitespace().collect();
        let (mut cc, mut slot, mut tg) = (None, None, None);
        let mut i = 0;
        while i < words.len() {
            let (key, val) = match words[i].parse::<u32>() {
                Ok(v) if i == 0 => ("CC", Some(v)),
                _ => (words[i], words.get(i + 1).and_then(|w| w.parse::<u32>().ok())),
            };
            let v = val.ok_or_else(bad)?;
            i += if words[i].parse::<u32>().is_ok() { 1 } else { 2 };
            match key {
                "CC" | "COLOR" | "COLOUR" => cc = Some(u8::try_from(v).ok().filter(|&c| c <= 15).ok_or_else(|| format!("CC {v}: a colour code is 0–15"))?),
                "TS" | "SLOT" => slot = Some(u8::try_from(v).ok().filter(|s| (1..=2).contains(s)).ok_or_else(|| format!("TS {v}: the slot is 1 or 2"))?),
                "TG" => tg = Some(v).filter(|&t| t > 0 && t < 1 << 24).map(Some).ok_or_else(|| format!("TG {v}: not a DMR talkgroup"))?,
                "SL" => {}
                _ => return Err(bad()),
            }
        }
        Ok((cc.is_some() || slot.is_some() || tg.is_some()).then_some(Access::Dmr { cc, slot, tg }))
    }

    /// Two rows can't tell these apart.
    fn same(self, other: Access) -> bool {
        match (self, other) {
            (Access::Tone(a), Access::Tone(b)) => a.matches(b),
            _ => self == other,
        }
    }

    /// How much of a transmission this names (the most specific row wins).
    fn fields(self) -> usize {
        match self {
            Access::Dmr { cc, slot, tg } => cc.is_some() as usize + slot.is_some() as usize + tg.is_some() as usize,
            _ => 1,
        }
    }

    /// Whether a digital transmission (on `slot` 0 / 1) satisfies it; what
    /// isn't known yet doesn't.
    fn admits(self, nac: Option<u16>, slot: usize, cc: Option<u8>, tg: Option<u32>) -> bool {
        match self {
            Access::Tone(_) => false,
            Access::Nac(n) => nac == Some(n),
            Access::Dmr { cc: c, slot: s, tg: t } => {
                c.is_none_or(|c| cc == Some(c)) && s.is_none_or(|s| s as usize == slot + 1) && t.is_none_or(|t| tg == Some(t))
            }
        }
    }
}

impl ConvChannel {
    /// A channel filed under its default talkgroup: the frequency in kHz
    /// (154.430 MHz → 154430), stable however the list is ordered.
    pub fn new(freq_hz: f64, mode: ConvMode) -> Self {
        ConvChannel { freq_hz, mode, talkgroup: Self::default_talkgroup(freq_hz), info: None, squelch_db: None, access: None, system: 0 }
    }

    pub fn default_talkgroup(freq_hz: f64) -> u32 {
        (freq_hz / 1000.0).round() as u32
    }

    /// The default for the `k`th row on a frequency (from 0): the frequency
    /// in kHz, then that with a digit added (154325, 1543251, 1543252 …).
    pub fn default_talkgroup_at(freq_hz: f64, k: usize) -> u32 {
        let khz = Self::default_talkgroup(freq_hz);
        if k == 0 { khz } else { khz.saturating_mul(10).saturating_add(k as u32) }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ConvConfig {
    /// Default open threshold, dB above the noise floor.
    pub squelch_db: f64,
    /// Air replayed from before detection, s (capped by the engine's history).
    pub preroll_s: f64,
    /// The IMBE vocoder for P25 channels.
    pub vocoder: mbe::Profile,
}

impl Default for ConvConfig {
    fn default() -> Self {
        ConvConfig { squelch_db: 8.0, preroll_s: 0.3, vocoder: mbe::Profile::Enhanced }
    }
}

/// A carrier stuck on still ends a call this long, s (with no length limit set).
pub const STUCK_CALL_S: f64 = 600.0;

/// What the conventional channels did, for the engine to report.
pub enum ConvOut {
    Start(Call),
    Update(Call),
    Audio { call_id: CallId, system: u16, talkgroup: u32, samples: Vec<f32> },
    End { call: Call, audio: Vec<f32>, frames: CallFrames, recorder_num: u32, tx: Transmissions, reception: Reception },
    /// A radio's talker alias, heard on a P25 channel of conventional system `.0` ([`Call::system`]).
    Alias(u16, Alias),
    /// A transmission no row took, and the code it carried (as a row's
    /// Tone would say it; "" for none).
    Skipped { freq_hz: u64, code: String },
}

/// What a conventional call carried, as a row's Tone would say it: the
/// CTCSS / DCS heard ("" for none), the NAC, or the DMR colour code, slot
/// and talkgroup — for finding a frequency's codes. None for a trunked call.
pub fn heard_code(call: &Call) -> Option<String> {
    conventional_index(call.system)?;
    if call.analog {
        return Some(call.tone.map_or(String::new(), |h| h.tone.to_string()));
    }
    if call.color_code.is_some() {
        return Some(Access::Dmr { cc: call.color_code, slot: Some(call.tdma_slot + 1), tg: Some(call.talkgroup) }.to_string());
    }
    call.nac.map(|n| Access::Nac(n).to_string())
}

/// What one slot of an open channel heard in one run (FM and P25: slot 0 only).
#[derive(Default)]
struct Heard {
    audio: Vec<f32>,
    /// FM: `audio` before the voice high-pass (for the tone detector).
    low: Vec<f32>,
    frames: Vec<VoiceFrame>,
    /// (source, emergency, encrypted) from link control.
    infos: Vec<(Option<u32>, bool, bool)>,
    /// Air time of the first voice frame, and the end of the last (digital).
    air: Option<(f64, f64)>,
    /// The talkgroup the air named (FM with tones: the row's).
    tg: Option<u32>,
    color_code: Option<u8>,
    /// P25: the NAC its frames carried.
    nac: Option<u16>,
    /// The row it is for, on a frequency with codes.
    row: Option<usize>,
}

impl Heard {
    /// FM: take `other`'s audio and unit IDs after this one's.
    fn append(&mut self, other: &mut Heard) {
        self.audio.append(&mut other.audio);
        self.low.append(&mut other.low);
        self.infos.append(&mut other.infos);
    }
}

/// FM on a frequency with tones: the transmission being held.
#[derive(Default)]
struct Tx {
    tones: ToneDetector,
    /// What it brought before its row was known.
    held: Heard,
    secs: f64,
    /// Its row, once known (`Some(None)`: no row takes it).
    row: Option<Option<usize>>,
}

struct Live {
    call: Call,
    audio: Vec<f32>,
    /// P25: the vocoder frames behind `audio`.
    frames: CallFrames,
    /// Where each transmission starts in `audio`.
    tx: Transmissions,
    /// How strong it came in.
    reception: Reception,
    /// The talkgroup came from P25 link control (not the channel's default).
    tg_from_air: bool,
    /// FM: what tone the call carries.
    tones: Option<Box<ToneDetector>>,
}

struct Open {
    head: HeadId,
    /// Its decoder ([`super::voice`]: the same as trunked calls').
    voice: Box<dyn VoiceDecoder>,
    /// Digital: the carrier meter (FM: the decoder's squelch tells).
    meter: Option<ChannelFilter>,
    opened_s: f64,
    carrier_seen: bool,
    last_carrier_s: f64,
    /// The call on each slot (FM and P25: slot 0).
    live: [Option<Live>; 2],
    tx: Option<Box<Tx>>,
}

struct Chan {
    /// The first row (with the lowest squelch any row sets).
    cfg: ConvChannel,
    /// Every row on the frequency (only FM has more than one).
    rows: Vec<ConvChannel>,
    /// Transmissions wait for their tone.
    routed: bool,
    /// Per slot: the code last dropped, and when — reported once per call's
    /// worth (a pause shorter than the call timeout continues it), as a
    /// recorded call would be counted.
    skipped: [Option<(String, f64)>; 2],
    source: usize,
    offset_hz: f64,
    slice: usize,
    power: f64,
    bar_db: f64,
    raised: bool,
    open: Option<Open>,
}

impl Chan {
    fn base_db(&self, dflt: f64) -> f64 {
        self.cfg.squelch_db.unwrap_or(dflt)
    }
}

struct Floor {
    slices: Vec<f64>,
    scratch: Vec<f64>,
    next_s: f64,
    primed: bool,
}

pub struct Conventional {
    cfg: ConvConfig,
    bank_cfg: BankConfig,
    chans: Vec<Chan>,
    floors: Vec<Floor>,
}

/// Whether `channels` can run together (see [`Conventional`]'s rows rules).
pub fn check_channels(channels: &[ConvChannel]) -> Result<(), String> {
    Conventional::check(channels)
}

/// What the calls need from the engine's configuration.
pub struct CallRules {
    pub call_timeout_s: f64,
    /// A call longer than this is concluded and a new one started (0: never), s.
    pub max_call_s: f64,
    pub record_encrypted: bool,
    /// Keep each call's vocoder frames (see [`super::frames`]).
    pub capture_frames: bool,
}

impl Conventional {
    /// `sources`: (centre, rate) of each source. Channels outside every
    /// source are an error.
    pub fn new(channels: &[ConvChannel], sources: &[(f64, f64)], cfg: ConvConfig, bank_cfg: BankConfig, usable: f64) -> Result<Self, String> {
        let mut chans = Vec::new();
        let mut outside = Vec::new();
        let mut groups: Vec<Vec<ConvChannel>> = Vec::new();
        for c in channels {
            match groups.iter_mut().find(|g| (g[0].freq_hz - c.freq_hz).abs() < 1.0) {
                Some(g) => g.push(c.clone()),
                None => groups.push(vec![c.clone()]),
            }
        }
        for rows in groups {
            Self::check_rows(&rows)?;
            let mut c = rows[0].clone();
            c.squelch_db = rows.iter().filter_map(|r| r.squelch_db).reduce(f64::min);
            let c = &c;
            let routed = rows.len() > 1 || c.access.is_some();
            let Some(src) = sources.iter().position(|&(center, rate)| (c.freq_hz - center).abs() <= rate / 2.0 * usable) else {
                outside.push(format!("{:.5}", c.freq_hz / 1e6));
                continue;
            };
            let (center, rate) = sources[src];
            let offset_hz = c.freq_hz - center;
            let slice = (((offset_hz + rate / 2.0) / rate * FLOOR_SLICES as f64) as usize).min(FLOOR_SLICES - 1);
            let bar = c.squelch_db.unwrap_or(cfg.squelch_db);
            chans.push(Chan { cfg: c.clone(), rows, routed, skipped: [None, None], source: src, offset_hz, slice, power: 0.0, bar_db: bar, raised: false, open: None });
        }
        if !outside.is_empty() {
            return Err(format!("Conventional channel(s) outside every source's bandwidth: {} MHz — move a center frequency or disable them.", outside.join(", ")));
        }
        let floors = sources.iter().map(|_| Floor { slices: vec![0.0; FLOOR_SLICES], scratch: vec![0.0; FLOOR_SLICES], next_s: 0.0, primed: false }).collect();
        Ok(Conventional { cfg, bank_cfg, chans, floors })
    }

    /// Whether `channels` can run together: rows on one frequency have one
    /// mode, and no two are for the same code (or none).
    pub fn check(channels: &[ConvChannel]) -> Result<(), String> {
        let mut freqs: Vec<f64> = Vec::new();
        for c in channels {
            if !freqs.iter().any(|f| (f - c.freq_hz).abs() < 1.0) {
                freqs.push(c.freq_hz);
                let rows: Vec<ConvChannel> = channels.iter().filter(|r| (r.freq_hz - c.freq_hz).abs() < 1.0).cloned().collect();
                Self::check_rows(&rows)?;
            }
        }
        Ok(())
    }

    fn check_rows(rows: &[ConvChannel]) -> Result<(), String> {
        let mhz = format!("{:.5}", rows[0].freq_hz / 1e6);
        if rows.iter().any(|r| r.system != rows[0].system) {
            return Err(format!("Conventional channel {mhz} MHz is in two conventional systems — a frequency belongs to one."));
        }
        if rows.iter().any(|r| r.mode != rows[0].mode) {
            return Err(format!("Conventional channel {mhz} MHz is listed with different modes — rows sharing a frequency need the same one."));
        }
        let what = match rows[0].mode {
            ConvMode::Fm => "tone",
            ConvMode::P25 => "NAC",
            ConvMode::Dmr => "colour code, slot or talkgroup",
        };
        for (i, a) in rows.iter().enumerate() {
            for b in &rows[..i] {
                match (a.access, b.access) {
                    (None, None) => {
                        return Err(format!("Conventional channel {mhz} MHz is listed twice without a {what} — give each row its own (one may have none)."));
                    }
                    (Some(x), Some(y)) if x.same(y) => {
                        let same = if x == y { format!("{x}") } else { format!("{y} and {x} (the same signal)") };
                        return Err(format!("Conventional channel {mhz} MHz has two rows for {same}."));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.chans.is_empty()
    }

    /// Heads open now.
    pub fn open_count(&self) -> usize {
        self.chans.iter().filter(|c| c.open.is_some()).count()
    }

    /// Calls in progress.
    pub fn calls(&self) -> impl Iterator<Item = &Call> {
        self.chans.iter().filter_map(|c| c.open.as_ref()).flat_map(|o| o.live.iter().flatten().map(|l| &l.call))
    }

    /// After a block ran on `source` (whose clock reads `now_s`).
    /// `calls` and `rules`: each conventional system's (a channel's `system` indexes them).
    /// `gap`: the block is silence standing in for samples that never came
    /// ([`super::Engine::push_gap`]): calls open run on, nothing new opens,
    /// and the noise floor is left as it was.
    pub fn on_block(&mut self, source: usize, chz: &mut Channelizer, now_s: f64, gap: bool, calls: &mut [CallManager], rules: &[CallRules], out: &mut Vec<ConvOut>) {
        if !self.chans.iter().any(|c| c.source == source) {
            return;
        }
        let fl = &mut self.floors[source];
        if now_s >= fl.next_s && !gap {
            chz.noise_profile(&mut fl.scratch);
            if fl.primed {
                for (s, v) in fl.slices.iter_mut().zip(&fl.scratch) {
                    *s += FLOOR_ALPHA * (v - *s);
                }
            } else {
                fl.slices.copy_from_slice(&fl.scratch);
                fl.primed = true;
            }
            fl.next_s = now_s + FLOOR_EVERY_S;
        }
        let a = 1.0 - (-chz.block_seconds() / DETECT_TAU_S).exp();
        let dflt = self.cfg.squelch_db;
        for (idx, ch) in self.chans.iter_mut().enumerate() {
            if ch.source != source {
                continue;
            }
            let (calls, rules) = (&mut calls[ch.cfg.system], &rules[ch.cfg.system]);
            let floor = self.floors[source].slices[ch.slice].max(1e-30);
            let bp = chz.band_power(ch.offset_hz, DETECT_HALF_BW);
            ch.power = if ch.power == 0.0 { bp } else { ch.power + a * (bp - ch.power) };
            let snr_db = if gap { f64::NEG_INFINITY } else { 10.0 * (ch.power / floor).log10() };
            let base = ch.base_db(dflt);
            let meter_thr = (chz.noise_in_band(floor, ChannelFilter::noise_bandwidth()) * 10f64.powf((base - CLOSE_BELOW_DB).max(3.0) / 10.0)) as f32;
            if ch.open.is_none() {
                if ch.raised && snr_db < base {
                    ch.raised = false;
                    ch.bar_db = base;
                }
                if snr_db >= ch.bar_db {
                    Self::open(ch, chz, now_s, self.cfg.preroll_s, self.bank_cfg, self.cfg.vocoder, meter_thr, idx as u32, calls, rules, out);
                }
                continue;
            }
            let Some(iq) = chz.output(ch.open.as_ref().unwrap().head).map(|v| v.to_vec()) else { continue };
            let max_call_s = if rules.max_call_s > 0.0 { rules.max_call_s } else { STUCK_CALL_S };
            Self::run(ch, &iq, now_s, meter_thr, idx as u32, calls, rules, max_call_s, out);
            // Reception: the channel's power while its carrier is up, against the floor (both as a channel's head would see them).
            if snr_db >= base {
                let (sig, noise) = (chz.noise_in_band(ch.power, ChannelFilter::noise_bandwidth()), chz.noise_in_band(floor, ChannelFilter::noise_bandwidth()));
                for l in ch.open.as_mut().unwrap().live.iter_mut().flatten() {
                    if now_s - l.call.last_audio_s < 0.5 {
                        l.reception.signal(sig);
                        l.reception.noise(noise);
                    }
                }
            }
            // Wind down.
            let o = ch.open.as_mut().unwrap();
            for live in o.live.iter_mut() {
                if live.as_ref().is_some_and(|l| now_s - l.call.last_audio_s > rules.call_timeout_s) {
                    Self::end(live.take().unwrap(), idx as u32, out);
                }
            }
            let o = ch.open.as_ref().unwrap();
            if o.live.iter().all(Option::is_none) && now_s - o.last_carrier_s.max(o.opened_s) > CLOSE_HANG_S {
                if !o.carrier_seen {
                    // The meter never confirmed it: leakage or a spur. Wait
                    // for the band to rise further (or fall back) first.
                    ch.bar_db = snr_db + FALSE_OPEN_RAISE_DB;
                    ch.raised = true;
                }
                let o = ch.open.take().unwrap();
                chz.remove_head(o.head);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open(ch: &mut Chan, chz: &mut Channelizer, now_s: f64, preroll_s: f64, bank_cfg: BankConfig, vocoder: mbe::Profile, meter_thr: f32, num: u32, calls: &mut CallManager, rules: &CallRules, out: &mut Vec<ConvOut>) {
        let (head, pre, start_sample) = chz.add_head(ch.offset_hz, HEAD_CUTOFF_HZ, preroll_s);
        let rate = chz.output_rate();
        let kind = match ch.cfg.mode {
            ConvMode::Fm => VoiceKind::Analog,
            ConvMode::P25 => VoiceKind::Fdma,
            ConvMode::Dmr => VoiceKind::Dmr,
        };
        let voice = voice::build(&VoiceSpec {
            kind,
            rate,
            // (On the engine's clock: `now_s` is this source's, offset included.)
            t0: start_sample as f64 / chz.fs() + (now_s - chz.sample_position() as f64 / chz.fs()),
            seed: ch.cfg.freq_hz as u32,
            vocoder,
            bank: bank_cfg,
            // (FM: set on every run, from the noise floor.)
            squelch: meter_thr,
            // For the tone detector.
            subaudible: true,
        });
        let meter = (kind != VoiceKind::Analog).then(|| ChannelFilter::new(rate));
        ch.open = Some(Open { head, voice, meter, opened_s: now_s, carrier_seen: false, last_carrier_s: now_s, live: [None, None], tx: None });
        Self::run(ch, &pre, now_s, meter_thr, num, calls, rules, 0.0, out);
    }

    /// Run the open channel's receiver over `iq` (air up to `now_s`).
    #[allow(clippy::too_many_arguments)]
    fn run(ch: &mut Chan, iq: &[Complex32], now_s: f64, meter_thr: f32, num: u32, calls: &mut CallManager, rules: &CallRules, max_call_s: f64, out: &mut Vec<ConvOut>) {
        let o = ch.open.as_mut().unwrap();
        let mut heard: [Heard; 2] = Default::default();
        // Digital: the carrier by the meter; FM: by the decoder's squelch, at the same threshold.
        let metered = o.meter.as_mut().map(|m| m.meter(iq) > meter_thr);
        o.voice.set_squelch(meter_thr);
        let mut vout = Vec::new();
        o.voice.push(iq, &mut vout);
        let carrier = metered.or(o.voice.carrier()).unwrap_or(false);
        for v in vout {
            let h = &mut heard[v.slot as usize & 1];
            match v.out {
                TrackerOut::Audio(a, f) => {
                    h.audio.extend_from_slice(&a);
                    h.frames.push(f);
                }
                TrackerOut::AnalogAudio(a) => h.audio.extend_from_slice(&a),
                TrackerOut::Subaudible(l) => h.low.extend_from_slice(&l),
                TrackerOut::Info { source, emergency, encrypted } => h.infos.push((source, emergency, encrypted)),
                TrackerOut::Alias(a) => out.push(ConvOut::Alias(conventional_system(ch.cfg.system), a)),
            }
        }
        for (slot, h) in heard.iter_mut().enumerate() {
            let a = o.voice.air(slot as u8);
            (h.air, h.tg, h.nac, h.color_code) = (a.air, a.talkgroup, a.nac, a.color_code);
        }
        if ch.routed && ch.cfg.mode == ConvMode::Fm {
            if let Some(code) = Self::route(&ch.rows, &mut o.tx, &mut heard[0], carrier) {
                Self::skip(&mut ch.skipped[0], ch.cfg.freq_hz, code, now_s, rules, out);
            }
        }
        if carrier {
            o.carrier_seen = true;
            o.last_carrier_s = now_s;
        }
        if ch.routed && ch.cfg.mode != ConvMode::Fm {
            for (slot, h) in heard.iter_mut().enumerate() {
                if let Some(code) = Self::route_digital(&ch.rows, slot, h) {
                    Self::skip(&mut ch.skipped[slot], ch.cfg.freq_hz, code, now_s, rules, out);
                }
            }
        }
        for (slot, h) in heard.into_iter().enumerate() {
            Self::slot_call(ch, slot, h, now_s, num, calls, rules, max_call_s, out);
        }
    }

    /// FM with tones: hold the transmission until its tone names its row (or
    /// none), then pass it on as that row's (`h`: this run's; emptied while
    /// held, or when no row takes it). A transmission ends with the carrier.
    /// Returns the code of a transmission no row takes, on each run of it.
    fn route(rows: &[ConvChannel], tx: &mut Option<Box<Tx>>, h: &mut Heard, carrier: bool) -> Option<String> {
        if h.audio.is_empty() && h.infos.is_empty() {
            if !carrier {
                // Over before it could be told: by what was heard, else no tone.
                if let Some(mut t) = tx.take().filter(|t| t.row.is_none()) {
                    match Self::pick(rows, &t.tones, true) {
                        Some(Some(r)) => {
                            *h = std::mem::take(&mut t.held);
                            h.tg = Some(rows[r].talkgroup);
                            h.row = Some(r);
                        }
                        _ => return Some(Self::tone_code(&t.tones)),
                    }
                }
            }
            return None;
        }
        let t = tx.get_or_insert_with(Box::default);
        t.tones.push(&h.low);
        t.secs += h.audio.len() as f64 / fm::AUDIO_RATE;
        if t.row.is_none() {
            t.held.append(h);
            t.row = Self::pick(rows, &t.tones, t.secs >= TONE_WAIT_S);
            if t.row.is_some() {
                *h = std::mem::take(&mut t.held);
            }
        }
        match t.row {
            Some(Some(r)) => {
                h.tg = Some(rows[r].talkgroup);
                h.row = Some(r);
                None
            }
            Some(None) => {
                *h = Heard::default();
                Some(Self::tone_code(&t.tones))
            }
            None => None,
        }
    }

    /// Report traffic no row took: once for a new code, or for the same
    /// after a pause longer than the call timeout.
    fn skip(last: &mut Option<(String, f64)>, freq_hz: f64, code: String, now_s: f64, rules: &CallRules, out: &mut Vec<ConvOut>) {
        if last.as_ref().is_none_or(|(c, t)| *c != code || now_s - t > rules.call_timeout_s) {
            out.push(ConvOut::Skipped { freq_hz: freq_hz.round() as u64, code: code.clone() });
        }
        *last = Some((code, now_s));
    }

    fn tone_code(tones: &ToneDetector) -> String {
        tones.heard().map_or(String::new(), |h| h.tone.to_string())
    }

    /// P25 / DMR: the row this run's traffic on `slot` is for (none: it is
    /// dropped, and its code returned). Several P25 rows file under the
    /// row's talkgroup.
    fn route_digital(rows: &[ConvChannel], slot: usize, h: &mut Heard) -> Option<String> {
        if h.audio.is_empty() && h.infos.is_empty() {
            return None;
        }
        let best = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.access.is_none_or(|a| a.admits(h.nac, slot, h.color_code, h.tg)))
            .max_by_key(|(i, r)| (r.access.map_or(0, Access::fields), std::cmp::Reverse(*i)));
        match best {
            // Only voice counts as a transmission lost (as only calls with audio are kept).
            None if h.audio.is_empty() => {
                *h = Heard::default();
                None
            }
            None => {
                let code = match rows[0].mode {
                    ConvMode::P25 => h.nac.map_or(String::new(), |n| Access::Nac(n).to_string()),
                    _ => Access::Dmr { cc: h.color_code, slot: Some(slot as u8 + 1), tg: h.tg }.to_string(),
                };
                *h = Heard::default();
                Some(code)
            }
            Some((i, r)) => {
                h.row = Some(i);
                if r.mode == ConvMode::P25 && rows.len() > 1 {
                    h.tg = Some(r.talkgroup);
                }
                None
            }
        }
    }

    /// The row a transmission is for: the one whose tone it carries, else
    /// (once a tone is heard, or `last`) the one with none, if any.
    fn pick(rows: &[ConvChannel], tones: &ToneDetector, last: bool) -> Option<Option<usize>> {
        let heard = tones.heard();
        let carries = |r: &ConvChannel, h: Tone| matches!(r.access, Some(Access::Tone(t)) if t.matches(h));
        if let Some(i) = heard.and_then(|h| rows.iter().position(|r| carries(r, h.tone))) {
            return Some(Some(i));
        }
        (heard.is_some() || last).then(|| rows.iter().position(|r| r.access.is_none()))
    }

    /// Start, relabel, split or feed the call on one slot with what it heard.
    #[allow(clippy::too_many_arguments)]
    fn slot_call(ch: &mut Chan, slot: usize, h: Heard, now_s: f64, num: u32, calls: &mut CallManager, rules: &CallRules, max_call_s: f64, out: &mut Vec<ConvOut>) {
        if h.audio.is_empty() && h.infos.is_empty() {
            return;
        }
        let o = ch.open.as_mut().unwrap();
        // The talkgroup: link control's (or the row's, see route), else the row's.
        let row = h.row.map_or(&ch.cfg, |r| &ch.rows[r]);
        let air_tg = h.tg;
        let tg = air_tg.unwrap_or(row.talkgroup);
        if let Some(l) = &o.live[slot] {
            let too_long = max_call_s > 0.0 && now_s - l.call.start_s > max_call_s;
            // A different talkgroup on the air is a new call; the first one
            // named just labels the call it arrived in.
            let new_tg = l.tg_from_air && air_tg.is_some_and(|t| t != l.call.talkgroup);
            if too_long || new_tg {
                let l = o.live[slot].take().unwrap();
                Self::end(l, num, out);
            } else if air_tg.is_some() && !l.tg_from_air {
                let info = Self::info_for(&ch.rows, row, tg, calls);
                let l = o.live[slot].as_mut().unwrap();
                l.tg_from_air = true;
                if l.call.talkgroup != tg {
                    l.call.talkgroup = tg;
                    l.call.talkgroup_info = info;
                    out.push(ConvOut::Update(l.call.clone()));
                }
            }
        }
        if o.live[slot].is_none() {
            let dur = h.audio.len() as f64 / fm::AUDIO_RATE;
            let start = h.air.map_or_else(|| (now_s - dur).max(o.opened_s - 0.05), |a| a.0);
            let call = Call {
                system: conventional_system(ch.cfg.system),
                id: calls.allocate_id(),
                talkgroup: tg,
                freq_hz: ch.cfg.freq_hz.round() as u64,
                phase2_tdma: false,
                tdma_slot: slot as u8,
                unit_to_unit: false,
                recording: true,
                reason: None,
                encrypted: false,
                emergency: false,
                priority: 0,
                duplex: false,
                mode: false,
                analog: ch.cfg.mode == ConvMode::Fm,
                start_s: start,
                last_update_s: now_s,
                last_audio_s: now_s,
                sources: Vec::new(),
                talkgroup_info: Self::info_for(&ch.rows, row, tg, calls),
                patched_talkgroups: Vec::new(),
                color_code: h.color_code,
                nac: h.nac,
                tone: None,
                tone_set: match row.access {
                    Some(Access::Tone(t)) => Some(t),
                    _ => None,
                },
            };
            out.push(ConvOut::Start(call.clone()));
            o.live[slot] = Some(Live { call, audio: Vec::new(), frames: CallFrames::new(rules.capture_frames), tx: Transmissions::default(), reception: Reception::default(), tg_from_air: air_tg.is_some(), tones: (ch.cfg.mode == ConvMode::Fm).then(Box::default) });
        }
        let l = o.live[slot].as_mut().unwrap();
        l.call.last_update_s = now_s;
        l.call.last_audio_s = h.air.map_or(now_s, |a| a.1);
        let mut changed = false;
        if let Some(t) = &mut l.tones {
            t.push(&h.low);
            let heard = t.heard();
            // A new tone is news; a firmer reading of the same one isn't.
            changed |= heard.map(|h| h.tone) != l.call.tone.map(|h| h.tone);
            l.call.tone = heard;
        }
        if l.call.nac.is_none() && h.nac.is_some() {
            l.call.nac = h.nac;
        }
        if l.call.color_code.is_none() && h.color_code.is_some() {
            l.call.color_code = h.color_code;
            changed = true;
        }
        for (src, emergency, encrypted) in h.infos {
            if encrypted && !l.call.encrypted {
                l.call.encrypted = true;
                changed = true;
            }
            if emergency && !l.call.emergency {
                l.call.emergency = true;
                changed = true;
            }
            if let Some(s) = src {
                if s != 0 && l.call.sources.last().is_none_or(|x| x.src != s) {
                    l.call.sources.push(CallSource { src: s, time_s: now_s, emergency });
                    changed = true;
                }
            }
        }
        if changed {
            out.push(ConvOut::Update(l.call.clone()));
        }
        if !h.audio.is_empty() && !(l.call.encrypted && !rules.record_encrypted) {
            l.tx.note(l.audio.len(), now_s);
            l.audio.extend_from_slice(&h.audio);
            for f in h.frames {
                l.frames.push(f);
            }
            out.push(ConvOut::Audio { call_id: l.call.id, system: l.call.system, talkgroup: l.call.talkgroup, samples: h.audio });
        }
    }

    /// The call's names: the row's filed under `tg`; else the talkgroup
    /// list's; else the names of the row it came on.
    fn info_for(rows: &[ConvChannel], row: &ConvChannel, tg: u32, calls: &CallManager) -> Option<Talkgroup> {
        let named = |r: &ConvChannel| r.info.clone().map(|t| Talkgroup { number: tg, ..t });
        let listed = calls.talkgroups.get(&tg).cloned();
        match rows.iter().find(|r| r.talkgroup == tg) {
            Some(r) => named(r).or(listed),
            None => listed.or_else(|| named(row)),
        }
    }

    fn end(l: Live, num: u32, out: &mut Vec<ConvOut>) {
        out.push(ConvOut::End { call: l.call, audio: l.audio, frames: l.frames, recorder_num: num, tx: l.tx, reception: l.reception });
    }

    /// End of input: flush the receivers and end every call.
    pub fn finish(&mut self, rules: &[CallRules], out: &mut Vec<ConvOut>) {
        for (idx, ch) in self.chans.iter_mut().enumerate() {
            let rules = &rules[ch.cfg.system];
            let Some(o) = ch.open.as_mut() else { continue };
            let mut vout = Vec::new();
            o.voice.flush(&mut vout);
            for v in vout {
                let Some(l) = o.live[v.slot as usize & 1].as_mut() else { continue };
                if let TrackerOut::Audio(a, f) = v.out {
                    if !(l.call.encrypted && !rules.record_encrypted) {
                        l.audio.extend_from_slice(&a);
                        l.frames.push(f);
                    }
                }
            }
            let o = ch.open.take().unwrap();
            for l in o.live.into_iter().flatten() {
                Self::end(l, idx as u32, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trunk::engine::{Engine, EngineConfig, Event, SourceConfig};
    use crate::trunk::{CallConfig, CONVENTIONAL};
    use std::f64::consts::PI;

    struct Tx {
        offset_hz: f64,
        /// Power relative to the noise in a 10 kHz band, dB.
        snr_db: f64,
        on: Vec<(f64, f64)>,
        ph: f64,
        /// 8 kHz modulation from each key-up (±1: 5 kHz), then the tone.
        audio: Vec<f32>,
    }

    /// `secs` of air at `fs`: FM transmissions (1 kHz tone, 2.5 kHz deviation, or `audio`) in Gaussian noise.
    /// (concluded calls, call starts, (frequency, code) of transmissions no row took).
    /// (concluded calls with their audio length and JSON, call starts, (frequency, code) of transmissions no row took).
    type Run = (Vec<(Call, usize, String)>, usize, Vec<(u64, String)>);

    fn run(fs: f64, secs: f64, txs: &mut [Tx], channels: Vec<ConvChannel>) -> Run {
        run_systems(fs, secs, txs, channels, vec![conv_system("conv")])
    }

    fn conv_system(name: &str) -> crate::trunk::ConvSystem {
        crate::trunk::ConvSystem { short_name: name.into(), calls: CallConfig { call_timeout_s: 1.0, ..Default::default() }, ..Default::default() }
    }

    fn run_systems(fs: f64, secs: f64, txs: &mut [Tx], channels: Vec<ConvChannel>, conv_systems: Vec<crate::trunk::ConvSystem>) -> Run {
        run_gapped(fs, secs, txs, channels, conv_systems, None)
    }

    /// [`run_systems`], with `gap` = (air time, seconds): that long of
    /// samples that never came ([`Engine::push_gap`]) at that point of the air.
    fn run_gapped(fs: f64, secs: f64, txs: &mut [Tx], channels: Vec<ConvChannel>, conv_systems: Vec<crate::trunk::ConvSystem>, mut gap: Option<(f64, f64)>) -> Run {
        let center = 155_000_000.0;
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false }],
            conventional: channels,
            conv_systems,
            ..Default::default()
        };
        let mut e = Engine::new(cfg).unwrap();
        let sigma2 = 0.01f64;
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut u = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            ((rng >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        let total = (fs * secs) as usize;
        let chunk = 32768;
        let mut buf = vec![Complex32::default(); chunk];
        let (mut out, mut starts, mut skipped) = (Vec::new(), 0, Vec::new());
        let mut i0 = 0;
        while i0 < total {
            let n = chunk.min(total - i0);
            for (k, v) in buf[..n].iter_mut().enumerate() {
                let (a, b) = (u(), u());
                let r = ((-a.ln()) * sigma2).sqrt();
                let mut x = Complex32::from_polar(r as f32, (2.0 * PI * b) as f32);
                let t = (i0 + k) as f64 / fs;
                for tx in txs.iter_mut() {
                    let on = tx.on.iter().find(|&&(s, e)| t >= s && t < e);
                    let i = on.map_or(usize::MAX, |&(s, _)| ((t - s) * 8000.0) as usize);
                    let dev = tx.audio.get(i).map_or(2500.0 * (2.0 * PI * 1000.0 * t).sin(), |&a| 5000.0 * a as f64);
                    tx.ph += 2.0 * PI * (tx.offset_hz + dev) / fs;
                    if on.is_some() {
                        let p = sigma2 * 10_000.0 / fs * 10f64.powf(tx.snr_db / 10.0);
                        x += Complex32::from_polar(p.sqrt() as f32, tx.ph as f32);
                    }
                }
                *v = x;
            }
            e.push_iq(0, &buf[..n]);
            i0 += n;
            if let Some((_, len)) = gap.filter(|&(at, _)| i0 as f64 / fs >= at) {
                e.push_gap(0, (len * fs) as u64);
                gap = None;
            }
            for ev in e.drain_events() {
                match ev {
                    Event::Concluded(k) => out.push((k.call, k.audio.len(), k.json)),
                    Event::CallStart(_) => starts += 1,
                    Event::ConvSkipped { freq_hz, code } => skipped.push((freq_hz, code)),
                    _ => {}
                }
            }
        }
        e.finish();
        for ev in e.drain_events() {
            if let Event::Concluded(k) = ev {
                out.push((k.call, k.audio.len(), k.json));
            }
        }
        (out, starts, skipped)
    }

    fn fm(freq_hz: f64, tg: u32) -> ConvChannel {
        ConvChannel { freq_hz, mode: ConvMode::Fm, talkgroup: tg, info: None, squelch_db: None, access: None, system: 0 }
    }

    #[test]
    fn conventional_systems_keep_their_own_names_and_rules() {
        let (fs, c) = (2_400_000.0, 155_000_000.0);
        let mut txs = vec![
            Tx { offset_hz: 200_000.0, snr_db: 30.0, on: vec![(0.5, 2.5)], ph: 0.0, audio: Vec::new() },
            Tx { offset_hz: 312_500.0, snr_db: 30.0, on: vec![(0.5, 2.5)], ph: 0.0, audio: Vec::new() },
            Tx { offset_hz: -200_000.0, snr_db: 30.0, on: vec![(0.5, 2.5)], ph: 0.0, audio: Vec::new() },
        ];
        let police = ConvChannel { system: 1, ..fm(c + 312_500.0, 2) };
        let ems = ConvChannel { system: 2, ..fm(c - 200_000.0, 3) };
        // EMS keeps only calls of 3 s and more.
        let mut strict = conv_system("ems");
        strict.save.min_call_s = 3.0;
        let (calls, _, _) = run_systems(fs, 3.5, &mut txs, vec![fm(c + 200_000.0, 1), police, ems], vec![conv_system("fire"), conv_system("police"), strict]);
        let mut got: Vec<(u32, u16, bool)> = calls.iter().map(|(k, _, j)| (k.talkgroup, k.system, j.contains(r#""short_name":"fire""#) || j.contains(r#""short_name":"police""#))).collect();
        got.sort();
        assert_eq!(got, [(1, CONVENTIONAL, true), (2, CONVENTIONAL - 1, true)], "EMS's 2 s call is under its minimum");
        assert!(calls.iter().any(|(k, _, j)| k.talkgroup == 2 && j.contains(r#""short_name":"police""#)));
        assert_eq!(conventional_index(CONVENTIONAL - 1), Some(1));
        assert_eq!(conventional_index(3), None);
    }

    #[test]
    fn fm_calls_detected_split_and_leakage_ignored() {
        let fs = 2_400_000.0;
        let c = 155_000_000.0;
        let mut txs = vec![
            // Two transmissions 1.5 s apart (> the 1 s call timeout): two calls.
            Tx { offset_hz: 200_000.0, snr_db: 30.0, on: vec![(0.5, 2.5), (4.0, 5.0)], ph: 0.0, audio: Vec::new() },
            // A weak one, 15 dB above noise.
            Tx { offset_hz: 312_500.0, snr_db: 15.0, on: vec![(1.0, 3.0)], ph: 0.0, audio: Vec::new() },
            // A very strong neighbour (not a configured channel) 12.5 kHz from channel 3.
            Tx { offset_hz: -100_000.0, snr_db: 50.0, on: vec![(0.2, 5.5)], ph: 0.0, audio: Vec::new() },
        ];
        let chans = vec![fm(c + 200_000.0, 1), fm(c + 312_500.0, 2), fm(c - 87_500.0, 3), fm(c - 400_000.0, 4)];
        let (calls, starts, _) = run(fs, 6.0, &mut txs, chans);
        let by_tg = |tg: u32| calls.iter().filter(|(c, _, _)| c.talkgroup == tg).collect::<Vec<_>>();
        let a = by_tg(1);
        assert_eq!(a.len(), 2, "channel 1 calls: {:?}", calls.iter().map(|(c, n, _)| (c.talkgroup, c.start_s, *n)).collect::<Vec<_>>());
        let secs = |n: usize| n as f64 / 8000.0;
        assert!((secs(a[0].1) - 2.0).abs() < 0.15, "first call {:.2} s of audio", secs(a[0].1));
        assert!((secs(a[1].1) - 1.0).abs() < 0.15, "second call {:.2} s of audio", secs(a[1].1));
        assert!((a[0].0.start_s - 0.5).abs() < 0.1, "first call starts at {:.2} s", a[0].0.start_s);
        assert!(a[0].2.contains("\"audio_type\":\"analog\""));
        assert!(a[0].2.contains("\"error_count\":0,\"spike_count\":0}],\"errorList\":[],\"srcList\":["), "{}", a[0].2);
        let w = by_tg(2);
        assert_eq!(w.len(), 1);
        assert!((secs(w[0].1) - 2.0).abs() < 0.2, "weak call {:.2} s", secs(w[0].1));
        assert!(by_tg(3).is_empty(), "leakage from the neighbour made a call: {:?}", by_tg(3).iter().map(|(c, n, _)| (c.start_s, c.last_audio_s, *n)).collect::<Vec<_>>());
        assert!(by_tg(4).is_empty());
        assert_eq!(starts, 3);
    }

    /// The radio goes quiet for 5 s in the middle of a transmission: time
    /// goes on through the gap, so the call ends then (its 1 s timeout), the
    /// rest of the transmission is a call of its own at the right time, and
    /// the noise when the radio comes back opens nothing.
    #[test]
    fn a_gap_in_the_samples_moves_time_on() {
        let fs = 2_400_000.0;
        let c = 155_000_000.0;
        let mut txs = vec![Tx { offset_hz: 200_000.0, snr_db: 30.0, on: vec![(0.5, 2.0)], ph: 0.0, audio: Vec::new() }];
        let chans = vec![fm(c + 200_000.0, 1), fm(c + 312_500.0, 2), fm(c - 400_000.0, 3)];
        let (calls, starts, _) = run_gapped(fs, 4.0, &mut txs, chans, vec![conv_system("conv")], Some((1.2, 5.0)));
        let got: Vec<(u32, f64, f64)> = calls.iter().map(|(c, n, _)| (c.talkgroup, c.start_s, *n as f64 / 8000.0)).collect();
        assert_eq!(starts, 2, "calls: {got:?}");
        assert!(got.iter().all(|&(tg, _, _)| tg == 1), "the gap opened another channel: {got:?}");
        let (first, second) = (got[0], got[1]);
        assert!((first.1 - 0.5).abs() < 0.1 && (first.2 - 0.7).abs() < 0.2, "first call: {first:?}");
        // Air time 1.2 s + 5 s of gap.
        assert!((second.1 - 6.2).abs() < 0.2 && (second.2 - 0.8).abs() < 0.2, "second call: {second:?}");
    }

    #[test]
    fn fm_unit_ids_from_signalling() {
        use crate::dsp::signalling::tests::{ffsk, fs_bits, mdc_bits, mdc_tones};
        // MDC1200 emergency at key-up, voice, a FleetSync 2400 ID at unkey.
        let mut audio = vec![0.0f32; 400];
        audio.extend(ffsk(&mdc_tones(&mdc_bits(0x00, 0x80, 0x1234)), 1200.0, 1800.0, 1200.0, 0.5));
        audio.extend((0..8000).map(|i| (2.0 * PI * 1000.0 * i as f64 / 8000.0).sin() as f32 * 0.5));
        audio.extend(ffsk(&fs_bits(101, 1234), 1200.0, 2400.0, 2400.0, 0.5));
        let secs = audio.len() as f64 / 8000.0;
        let c = 155_000_000.0;
        let mut txs = vec![Tx { offset_hz: 200_000.0, snr_db: 25.0, on: vec![(0.5, 0.5 + secs)], ph: 0.0, audio }];
        let (calls, _, _) = run(2_400_000.0, 3.5, &mut txs, vec![fm(c + 200_000.0, 1)]);
        assert_eq!(calls.len(), 1);
        let call = &calls[0].0;
        assert_eq!(call.sources.iter().map(|s| s.src).collect::<Vec<_>>(), vec![0x1234, 1011234]);
        assert!(call.emergency);
        assert!(calls[0].2.contains("\"src\":4660"), "{}", calls[0].2);
    }

    #[test]
    fn fm_tone_heard() {
        use crate::dsp::tones::{tests::dcs_dev, Tone};
        let c = 155_000_000.0;
        // 2 s of voice over a 151.4 Hz tone (500 Hz deviation), and over D023N (700 Hz).
        let voice = |i: usize| (2.0 * PI * 1000.0 * i as f64 / 8000.0).sin() * 0.5;
        let ctcss: Vec<f32> = (0..16_000).map(|i| (voice(i) + 0.1 * (2.0 * PI * 151.4 * i as f64 / 8000.0).sin()) as f32).collect();
        let dcs: Vec<f32> = dcs_dev(23, false, 700.0, 2.0, 8000.0).iter().enumerate().map(|(i, d)| (voice(i) + d / 5000.0) as f32).collect();
        let mut txs = vec![
            Tx { offset_hz: 200_000.0, snr_db: 25.0, on: vec![(0.5, 2.5)], ph: 0.0, audio: ctcss },
            Tx { offset_hz: 300_000.0, snr_db: 25.0, on: vec![(0.5, 2.5)], ph: 0.0, audio: dcs },
        ];
        let (calls, _, _) = run(2_400_000.0, 4.0, &mut txs, vec![fm(c + 200_000.0, 1), fm(c + 300_000.0, 2)]);
        let tone = |tg: u32| calls.iter().find(|(c, _, _)| c.talkgroup == tg).and_then(|(c, _, _)| c.tone).map(|h| h.tone);
        assert_eq!(tone(1), Some(Tone::Ctcss(1514)));
        assert_eq!(tone(2), Some(Tone::Dcs(23, false)));
        let json = &calls.iter().find(|(c, _, _)| c.talkgroup == 1).unwrap().2;
        assert!(json.contains("\"tone_mode\":\"search\",\"tone_detected\":\"151.4\",\"tone_confidence\":"), "{json}");
    }

    /// 8 kHz modulation: `secs` of a 1 kHz voice over `tone`.
    fn voice_over(tone: Option<Tone>, secs: f64) -> Vec<f32> {
        use crate::dsp::tones::tests::dcs_dev;
        let n = (secs * 8000.0) as usize;
        let sub: Vec<f64> = match tone {
            Some(Tone::Ctcss(t)) => (0..n).map(|i| 500.0 * (2.0 * PI * t as f64 / 10.0 * i as f64 / 8000.0).sin()).collect(),
            Some(Tone::Dcs(c, inv)) => dcs_dev(c, inv, 700.0, secs, 8000.0),
            None => vec![0.0; n],
        };
        (0..n).map(|i| ((2.0 * PI * 1000.0 * i as f64 / 8000.0).sin() * 0.5 + sub[i] / 5000.0) as f32).collect()
    }

    /// Transmissions on one frequency, each (start s, tone), 1.5 s long.
    /// (talkgroup, seconds) of each call, and the codes of transmissions no row took.
    fn shared_skipped(rows: Vec<ConvChannel>, txs: &[(f64, Option<Tone>)], secs: f64) -> (Vec<(u32, f64)>, Vec<String>) {
        let mut t: Vec<Tx> =
            txs.iter().map(|&(at, tone)| Tx { offset_hz: 200_000.0, snr_db: 25.0, on: vec![(at, at + 1.5)], ph: 0.0, audio: voice_over(tone, 1.5) }).collect();
        let (calls, _, skipped) = run(2_400_000.0, secs, &mut t, rows);
        (calls.iter().map(|(c, n, _)| (c.talkgroup, *n as f64 / 8000.0)).collect(), skipped.into_iter().map(|s| s.1).collect())
    }

    fn shared(rows: Vec<ConvChannel>, txs: &[(f64, Option<Tone>)], secs: f64) -> Vec<(u32, f64)> {
        shared_skipped(rows, txs, secs).0
    }

    fn toned(tone: &str, tg: u32) -> ConvChannel {
        ConvChannel { access: Access::parse(ConvMode::Fm, tone).unwrap(), ..fm(155_200_000.0, tg) }
    }

    #[test]
    fn rows_sharing_a_frequency_split_it_by_tone() {
        let ct = Some(Tone::Ctcss(1514));
        let dcs = Some(Tone::Dcs(23, false));
        let other = Some(Tone::Ctcss(1273));
        let rows = vec![toned("151.4", 1), toned("D023N", 2), toned("", 3)];
        let calls = shared(rows.clone(), &[(0.5, ct), (3.5, dcs), (6.5, None), (9.5, other)], 12.0);
        assert_eq!(calls.iter().map(|c| c.0).collect::<Vec<_>>(), vec![1, 2, 3, 3], "{calls:?}");
        // Nothing lost while the tone was found (all but the gate's attack).
        assert!(calls.iter().all(|c| c.1 > 1.4), "{calls:?}");
        // Back to back (0.5 s apart, inside the call timeout): still two calls.
        let calls = shared(rows, &[(0.5, ct), (2.5, dcs)], 5.0);
        assert_eq!(calls.iter().map(|c| c.0).collect::<Vec<_>>(), vec![1, 2], "{calls:?}");
        // No row without a tone: the others aren't recorded, but are reported (once each).
        let (calls, skipped) = shared_skipped(vec![toned("151.4", 1), toned("D023N", 2)], &[(0.5, other), (3.5, None), (6.5, dcs)], 9.0);
        assert_eq!(calls.iter().map(|c| c.0).collect::<Vec<_>>(), vec![2], "{calls:?}");
        assert_eq!(skipped, ["127.3", ""]);
    }

    #[test]
    fn access_codes_as_people_write_them() {
        let p25 = |t: &str| Access::parse(ConvMode::P25, t);
        for t in ["293", "293 NAC", "NAC 293", "$293", "0x293", "nac 293"] {
            assert_eq!(p25(t), Ok(Some(Access::Nac(0x293))), "{t}");
        }
        assert_eq!(p25("1a2"), Ok(Some(Access::Nac(0x1a2))));
        for t in ["", "F7E", "$F7F", "S"] {
            assert_eq!(p25(t), Ok(None), "{t}");
        }
        assert!(p25("1234").is_err() && p25("xyz").is_err());
        assert_eq!(Access::Nac(0x2a).to_string(), "NAC 02A");
        let dmr = |t: &str| Access::parse(ConvMode::Dmr, t);
        let d = |cc, slot, tg| Ok(Some(Access::Dmr { cc, slot, tg }));
        assert_eq!(dmr("CC1"), d(Some(1), None, None));
        assert_eq!(dmr("1"), d(Some(1), None, None));
        assert_eq!(dmr("CC1 TS2 TG201"), d(Some(1), Some(2), Some(201)));
        assert_eq!(dmr("CC 1 TG 201 SL 2"), d(Some(1), None, Some(201)));
        assert_eq!(dmr("cc 7, slot 1"), d(Some(7), Some(1), None));
        assert_eq!(dmr("TG 3100"), d(None, None, Some(3100)));
        assert_eq!(dmr(""), Ok(None));
        assert_eq!(dmr("CC16"), Err("CC 16: a colour code is 0–15".into()));
        assert_eq!(dmr("TS3"), Err("TS 3: the slot is 1 or 2".into()));
        assert!(dmr("CC").is_err() && dmr("foo 2").is_err());
        assert_eq!(dmr("cc1 ts2 tg201").unwrap().unwrap().to_string(), "CC 1 TS 2 TG 201");
        assert_eq!(Access::parse(ConvMode::Fm, "023 DPL"), Ok(Some(Access::Tone(Tone::Dcs(23, false)))));
    }

    #[test]
    fn rows_that_cant_share() {
        let p25 = ConvChannel { mode: ConvMode::P25, ..toned("", 9) };
        assert!(check_channels(&[toned("151.4", 1), p25.clone()]).unwrap_err().contains("different modes"));
        assert!(check_channels(&[p25.clone(), p25.clone()]).unwrap_err().contains("listed twice without a NAC"));
        let nac = |t: &str| ConvChannel { access: Access::parse(ConvMode::P25, t).unwrap(), ..p25.clone() };
        assert!(check_channels(&[nac("293"), nac("$293")]).unwrap_err().contains("two rows for NAC 293"));
        assert!(check_channels(&[nac("293"), nac("1A2"), p25.clone()]).is_ok());
        assert!(check_channels(&[toned("151.4", 1), toned("151.4", 2)]).unwrap_err().contains("two rows for 151.4"));
        assert!(check_channels(&[toned("151.4", 1), toned("", 2), toned("D023N", 3)]).is_ok());
    }
}
