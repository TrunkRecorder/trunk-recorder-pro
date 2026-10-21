//! Conventional channels (analog FM and AM, P25, DMR, NXDN): energy-detected,
//! so an idle channel costs almost nothing.
//!
//! ```text
//! every block, per channel, from the channelizer's own spectrum (no head):
//!   band power (±5 kHz) vs the local noise floor → SNR (dB)
//!   (AM: also its carrier's line, the strongest bin pair, against where
//!    noise alone puts it — ~9 dB more sensitive, as the carrier is one
//!    line — leaving out the steady lines of spurs; whichever reads higher)
//! SNR ≥ squelch → open a head with pre-roll (the air from before detection)
//!   → channel filter / carrier meter (±5.5 kHz: confirms the carrier, so FFT
//!     leakage from a strong neighbour doesn't make a call)
//!   → FM demod → 8 kHz audio            (fm; unit IDs from MDC1200 /
//!                                        FleetSync bursts in it, the CTCSS
//!                                        tone / DCS code from below it)
//!   → AM demod → 8 kHz audio            (am: airband)
//!   → receiver bank → voice tracker     (p25; talkgroup from link control)
//!   → 4FSK → DMR framer → both slots   (dmr; a call on each slot, talkgroup
//!                                        from link control)
//!   → 4FSK → NXDN framer               (nxdn48 / nxdn96; group and unit
//!                                        from VCALL, the RAN from the SACCH)
//! a call starts with the first audio (or P25 link control), ends when there
//! has been none for the call timeout; the head closes once the carrier has
//! been gone for CLOSE_HANG_S with no call.
//! ```
//!
//! Several rows on one frequency are one channel, each row with its own
//! access code ([`Access`]: a CTCSS tone or DCS code, a P25 NAC / talkgroup,
//! a DMR colour code / slot / talkgroup, an NXDN RAN / talkgroup), at most
//! one with none. Each transmission goes to the row whose code it carries
//! (the most specific), else to the row with none, else isn't recorded:
//! - FM: held until its tone is known. A frequency whose only row has no
//!   tone isn't held.
//! - P25: every frame carries the NAC, link control the talkgroup, so
//!   nothing is held.
//! - DMR: per slot. NXDN: by RAN and / or group.
//!
//! A call is filed under the row's talkgroup if it gives one, else the
//! talkgroup the air named, else the row's default (the frequency in kHz).
//! The air's talkgroup is kept beside it ([`Call::talkgroup_ota`]):
//! conventional radios mostly send one that says little, often 1, so a row
//! can file it under a number of its own.
//!
//! A detection the carrier meter doesn't confirm raises that channel's open
//! threshold to just above what triggered it, until the band quietens again,
//! so a neighbour's leakage doesn't reopen the head every block.
//!
//! The noise floor is per source: the median of each 1/64 of the band
//! (fewer, wider slices at low sample rates: [`floor_slices`]), smoothed over
//! time, which follows the SDR's passband shape and ignores signals filling
//! under half a slice.

use std::sync::Arc;

use num_complex::Complex32;

use super::calls::{Call, CallIds, CallManager, Flags, Voice, STUCK_CALL_S};
use super::engine::{Event, SystemConfig};
use super::frames::CallFrames;
use super::record::{Reception, Transmissions};
use super::voice::CipherId;
use super::talkgroups::{Talkgroup, Talkgroups};
use super::voice::TrackerOut;
use super::voice::{self, VoiceDecoder, VoiceKind, VoiceSpec};
use super::frames::VoiceFrame;
use crate::dsp::am;
use crate::dsp::fm::{self, ChannelFilter};
use crate::dsp::tones::{Tone, ToneDetector};
use crate::dsp::channelizer::{floor_slice, floor_slices};
use crate::dsp::{AnalogTuning, Channelizer, HeadId};
use crate::mbe;
use crate::nxdn::Rate;
use crate::alias::Alias;
use crate::p25::crypt::Keyring;
use crate::position::Position;
use crate::p25::diversity::BankConfig;

/// Half-width of the band the detector sums, Hz (a 12.5 kHz channel's signal).
const DETECT_HALF_BW: f64 = 5000.0;
/// Head filter cutoff (the carrier meter narrows it further), Hz.
const HEAD_CUTOFF_HZ: f64 = 7000.0;
/// Band power smoothing, s.
const DETECT_TAU_S: f64 = 0.02;
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
    /// Analog FM (12.5 kHz, or 25 kHz wide).
    Fm,
    /// Analog AM (airband).
    Am,
    /// P25 Phase 1 (C4FM or CQPSK).
    P25,
    /// DMR (Tier II): each of the two slots records its own calls.
    Dmr,
    /// NXDN at either rate.
    Nxdn(Rate),
}

impl ConvMode {
    /// What a call on a channel of this mode is on: slot `slot` (DMR), its colour code once heard.
    fn voice(self, slot: u8, color_code: Option<u8>) -> Voice {
        match self {
            ConvMode::Fm => Voice::Fm,
            ConvMode::Am => Voice::Am,
            ConvMode::P25 => Voice::P25,
            ConvMode::Dmr => Voice::Dmr { slot, color_code },
            ConvMode::Nxdn(r) => Voice::Nxdn(r),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ConvMode::Fm => "fm",
            ConvMode::Am => "am",
            ConvMode::P25 => "p25",
            ConvMode::Dmr => "dmr",
            ConvMode::Nxdn(r) => r.name(),
        }
    }

    /// The decoder a channel of this mode opens with.
    fn kind(self) -> VoiceKind {
        match self {
            ConvMode::Fm => VoiceKind::Analog,
            ConvMode::Am => VoiceKind::Am,
            ConvMode::P25 => VoiceKind::Fdma,
            ConvMode::Dmr => VoiceKind::Dmr,
            ConvMode::Nxdn(r) => VoiceKind::Nxdn(r),
        }
    }

    /// Analog (FM or AM): demodulated as [`ConvChannel::analog`] says.
    pub fn analog(self) -> bool {
        matches!(self, ConvMode::Fm | ConvMode::Am)
    }
}

#[derive(Clone, Debug)]
pub struct ConvChannel {
    pub freq_hz: f64,
    pub mode: ConvMode,
    /// The talkgroup number calls are filed under, if the row gives one
    /// (None: the air's, else `default_talkgroup`).
    pub talkgroup: Option<u32>,
    /// What a call is filed under when neither the row nor the air names a
    /// talkgroup: [`ConvChannel::default_talkgroup_at`].
    pub default_talkgroup: u32,
    /// Name, description, tag, group for the call record.
    pub info: Option<Talkgroup>,
    /// Open threshold, dB above the noise floor (None: [`ConvConfig::squelch_db`]).
    pub squelch_db: Option<f64>,
    /// Record only transmissions carrying this code (None: any — or, beside
    /// rows with codes on the frequency, the rest).
    pub access: Option<Access>,
    /// The system it belongs to (its place in the engine's systems, a
    /// conventional one): its rules, its calls' [`Call::system`].
    pub system: usize,
    /// Analog: how it's demodulated (rows sharing a frequency share it).
    pub analog: AnalogTuning,
}

/// How a row picks its transmissions out of a frequency's (the channel's
/// Code, Slot and Match TG columns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// FM: a CTCSS tone or DCS code.
    Tone(Tone),
    /// P25: whichever of network access code and talkgroup are given.
    P25 { nac: Option<u16>, tg: Option<u32> },
    /// DMR: whichever of colour code, slot (1, 2) and talkgroup are given.
    Dmr { cc: Option<u8>, slot: Option<u8>, tg: Option<u32> },
    /// NXDN: whichever of RAN and group (or unit) are given.
    Nxdn { ran: Option<u8>, tg: Option<u32> },
}

impl std::fmt::Display for Access {
    /// `151.4`, `D023N` (Trunk Recorder's); `NAC 293 TG 1`; `CC 1 TS 2 TG 201`.
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Access::Tone(t) => write!(f, "{t}"),
            Access::P25 { nac, tg } => {
                let parts: Vec<String> = [nac.map(|n| format!("NAC {n:03X}")), tg.map(|t| format!("TG {t}"))].into_iter().flatten().collect();
                write!(f, "{}", parts.join(" "))
            }
            Access::Dmr { cc, slot, tg } => {
                let parts: Vec<String> =
                    [cc.map(|c| format!("CC {c}")), slot.map(|s| format!("TS {s}")), tg.map(|t| format!("TG {t}"))].into_iter().flatten().collect();
                write!(f, "{}", parts.join(" "))
            }
            Access::Nxdn { ran, tg } => {
                let parts: Vec<String> = [ran.map(|r| format!("RAN {r}")), tg.map(|t| format!("TG {t}"))].into_iter().flatten().collect();
                write!(f, "{}", parts.join(" "))
            }
        }
    }
}

impl Access {
    /// A row's Code column (or the old Tone) as people write it, read for its mode:
    /// - FM: [`Tone::parse`] (`151.4 PL`, `023 DPL`, `D023N` …);
    /// - P25: `293`, `293 NAC`, `NAC 293`, `$293`, `0x293`; `F7E` / `F7F`
    ///   (a radio's "receive any") mean any; `NAC 293 TG 1`, `TG 1`;
    /// - DMR: `CC1`, `1`, RadioReference's `CC1 TS2 TG201` or
    ///   `CC 1 TG 201 SL 2` (its scan list is ignored), `Slot 2`;
    /// - NXDN: `RAN 5`, `5`, `RAN5 TG 201`.
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
        if mode == ConvMode::Am {
            return Err(format!("\"{}\": AM channels carry no tone or code — leave it empty", text.trim()));
        }
        if mode == ConvMode::P25 {
            let bad = || format!("\"{}\" isn't a NAC (up to 3 hex digits, like 293) or talkgroup (TG 1)", text.trim());
            // The NAC (hex) before TG, the talkgroup (decimal) after it.
            let up = up.replace([',', ':', '='], " ");
            let (front, tg) = match up.find("TG") {
                Some(i) => {
                    let t = up[i + 2..].trim();
                    (&up[..i], Some(t.parse::<u32>().ok().filter(|&t| t > 0 && t < 1 << 16).ok_or_else(|| format!("TG {t}: not a P25 talkgroup (1–65535)"))?))
                }
                None => (&up[..], None),
            };
            let body: String = front.split_whitespace().filter(|w| *w != "NAC").collect();
            let body = body.trim_start_matches('$');
            let body = body.strip_prefix("0X").unwrap_or(body);
            let nac = match body {
                "" if tg.is_some() => None,
                _ if body.is_empty() || body.len() > 3 => return Err(bad()),
                _ => Some(u16::from_str_radix(body, 16).map_err(|_| bad())?).filter(|&n| n != 0xf7e && n != 0xf7f),
            };
            return Ok((nac.is_some() || tg.is_some()).then_some(Access::P25 { nac, tg }));
        }
        let nxdn = matches!(mode, ConvMode::Nxdn(_));
        let bad = || {
            if nxdn {
                format!("\"{}\" isn't an NXDN RAN (RAN 5) or talkgroup (TG 201)", text.trim())
            } else {
                format!("\"{}\" isn't a DMR colour code (CC1), slot (TS2) or talkgroup (TG201)", text.trim())
            }
        };
        // Letters and digits apart: CC1 → CC 1.
        let mut spaced = String::new();
        for c in up.chars() {
            if spaced.chars().last().is_some_and(|l| l.is_ascii_digit() != c.is_ascii_digit() && l != ' ') && c != ' ' {
                spaced.push(' ');
            }
            spaced.push(if c == ',' || c == ':' || c == '=' { ' ' } else { c });
        }
        let words: Vec<&str> = spaced.split_whitespace().collect();
        let (mut cc, mut slot, mut tg, mut ran) = (None, None, None, None);
        let mut i = 0;
        while i < words.len() {
            let (key, val) = match words[i].parse::<u32>() {
                Ok(v) if i == 0 => (if nxdn { "RAN" } else { "CC" }, Some(v)),
                _ => (words[i], words.get(i + 1).and_then(|w| w.parse::<u32>().ok())),
            };
            let v = val.ok_or_else(bad)?;
            i += if words[i].parse::<u32>().is_ok() { 1 } else { 2 };
            match key {
                "RAN" if nxdn => ran = Some(u8::try_from(v).ok().filter(|&r| r <= 63).ok_or_else(|| format!("RAN {v}: a RAN is 0–63"))?),
                "TG" if nxdn => tg = Some(v).filter(|&t| t > 0 && t < 1 << 16).map(Some).ok_or_else(|| format!("TG {v}: not an NXDN group (1–65535)"))?,
                _ if nxdn => return Err(bad()),
                "CC" | "COLOR" | "COLOUR" => cc = Some(u8::try_from(v).ok().filter(|&c| c <= 15).ok_or_else(|| format!("CC {v}: a colour code is 0–15"))?),
                "TS" | "SLOT" => slot = Some(u8::try_from(v).ok().filter(|s| (1..=2).contains(s)).ok_or_else(|| format!("TS {v}: the slot is 1 or 2"))?),
                "TG" => tg = Some(v).filter(|&t| t > 0 && t < 1 << 24).map(Some).ok_or_else(|| format!("TG {v}: not a DMR talkgroup"))?,
                "SL" => {}
                _ => return Err(bad()),
            }
        }
        if nxdn {
            // RAN 0 means "any" to a receiver (TS 1-A table 6.3-3).
            let ran = ran.filter(|&r| r != 0);
            return Ok((ran.is_some() || tg.is_some()).then_some(Access::Nxdn { ran, tg }));
        }
        Ok((cc.is_some() || slot.is_some() || tg.is_some()).then_some(Access::Dmr { cc, slot, tg }))
    }

    /// A row's Code, Slot and Match TG together. `code` may name the slot
    /// or talkgroup too (RadioReference's `CC1 TS2 TG201`), if it agrees.
    pub fn join(mode: ConvMode, code: &str, slot: Option<u8>, tg: Option<u32>) -> Result<Option<Access>, String> {
        let a = Self::parse(mode, code)?;
        if slot.is_none() && tg.is_none() {
            return Ok(a);
        }
        let (_, s, t) = a.map_or((String::new(), None, None), Access::parts);
        if slot.zip(s).is_some_and(|(a, b)| a != b) || tg.zip(t).is_some_and(|(a, b)| a != b) {
            return Err(format!("\"{}\" names another slot or talkgroup than its own columns", code.trim()));
        }
        let (slot, tg) = (slot.or(s), tg.or(t));
        if slot.is_some() && mode != ConvMode::Dmr {
            return Err(format!("Slot {}: only DMR has slots — leave it empty", slot.unwrap()));
        }
        if slot.is_some_and(|s| !(1..=2).contains(&s)) {
            return Err(format!("Slot {}: the slot is 1 or 2", slot.unwrap()));
        }
        let max = match mode {
            ConvMode::Fm | ConvMode::Am => return Err(format!("Match TG {}: {} names no talkgroup — leave it empty", tg.unwrap(), if mode == ConvMode::Fm { "FM" } else { "AM" })),
            ConvMode::Dmr => 1 << 24,
            _ => 1 << 16,
        };
        if tg.is_some_and(|t| t == 0 || t >= max) {
            return Err(format!("Match TG {}: not a talkgroup (1–{})", tg.unwrap(), max - 1));
        }
        Ok(Some(match (mode, a) {
            (ConvMode::P25, a) => Access::P25 { nac: match a { Some(Access::P25 { nac, .. }) => nac, _ => None }, tg },
            (ConvMode::Dmr, a) => Access::Dmr { cc: match a { Some(Access::Dmr { cc, .. }) => cc, _ => None }, slot, tg },
            (_, a) => Access::Nxdn { ran: match a { Some(Access::Nxdn { ran, .. }) => ran, _ => None }, tg },
        }))
    }

    /// The other way: (Code, Slot, Match TG), the code bare (`151.4`,
    /// `293` for a NAC, `1` for a colour code, `5` for a RAN).
    pub fn parts(self) -> (String, Option<u8>, Option<u32>) {
        let bare = |v: Option<u32>| v.map_or(String::new(), |v| v.to_string());
        match self {
            Access::Tone(t) => (t.to_string(), None, None),
            Access::P25 { nac, tg } => (nac.map_or(String::new(), |n| format!("{n:03X}")), None, tg),
            Access::Dmr { cc, slot, tg } => (bare(cc.map(u32::from)), slot, tg),
            Access::Nxdn { ran, tg } => (bare(ran.map(u32::from)), None, tg),
        }
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
            Access::Nxdn { ran, tg } => ran.is_some() as usize + tg.is_some() as usize,
            Access::P25 { nac, tg } => nac.is_some() as usize + tg.is_some() as usize,
            Access::Tone(_) => 1,
        }
    }

    /// Whether a digital transmission (on `slot` 0 / 1) satisfies it; what
    /// isn't known yet doesn't.
    fn admits(self, nac: Option<u16>, slot: usize, cc: Option<u8>, tg: Option<u32>, ran: Option<u8>) -> bool {
        match self {
            Access::Tone(_) => false,
            Access::P25 { nac: n, tg: t } => n.is_none_or(|n| nac == Some(n)) && t.is_none_or(|t| tg == Some(t)),
            Access::Dmr { cc: c, slot: s, tg: t } => {
                c.is_none_or(|c| cc == Some(c)) && s.is_none_or(|s| s as usize == slot + 1) && t.is_none_or(|t| tg == Some(t))
            }
            Access::Nxdn { ran: r, tg: t } => r.is_none_or(|r| ran == Some(r)) && t.is_none_or(|t| tg == Some(t)),
        }
    }
}

impl ConvChannel {
    /// A channel with no talkgroup of its own: filed under the air's, else
    /// the frequency in kHz (154.430 MHz → 154430), stable however the list
    /// is ordered.
    pub fn new(freq_hz: f64, mode: ConvMode) -> Self {
        let analog = if mode == ConvMode::Am { AnalogTuning::am(freq_hz) } else { AnalogTuning::default() };
        let default_talkgroup = Self::default_talkgroup(freq_hz);
        ConvChannel { freq_hz, mode, talkgroup: None, default_talkgroup, info: None, squelch_db: None, access: None, system: 0, analog }
    }

    /// Its names, when the number they go with is known before a call is
    /// heard: its TG Number, or an analog row's default (the air names
    /// none). None when the number comes from the air, or it has no names.
    pub fn names_ahead(&self) -> Option<&Talkgroup> {
        let known = self.talkgroup.is_some() || matches!(self.mode, ConvMode::Fm | ConvMode::Am);
        self.info.as_ref().filter(|_| known)
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


/// What the conventional channels did: the engine's events as they are,
/// and what the engine itself acts on.
pub enum ConvOut {
    /// A call started or changed, its audio, a radio's position, a
    /// transmission no row took ([`Event::ConvSkipped`]).
    Event(Event),
    /// A call is over: the engine saves it. `recorder_num`: the index of the
    /// channel (frequency) it came on.
    End { call: Call, audio: Vec<f32>, frames: CallFrames, recorder_num: u32, tx: Transmissions, reception: Reception, freq_error_hz: Option<f64> },
    /// A radio's talker alias, heard on a P25 channel of system `.0` ([`Call::system`]).
    Alias(u16, Alias),
    /// A key of system `.0` that decrypts its voice to noise.
    WrongKey(u16, CipherId),
}

/// What a conventional call carried, as a row's code would say it: the
/// CTCSS / DCS heard ("" for none), the NAC and talkgroup, the DMR colour
/// code, slot and talkgroup, the NXDN RAN and talkgroup (the air's, not
/// the one it was filed under) — for finding a frequency's codes. None for
/// a trunked call.
pub fn heard_code(call: &Call) -> Option<String> {
    if call.site.is_some() {
        return None;
    }
    match call.voice {
        Voice::Fm | Voice::Am => Some(call.tone.map_or(String::new(), |h| h.tone.to_string())),
        // (RAN 0 is "any": a row can't ask for it.)
        Voice::Nxdn(_) => Some(Access::Nxdn { ran: call.ran.filter(|&r| r != 0), tg: call.talkgroup_ota }.to_string()),
        Voice::Dmr { slot, color_code: cc @ Some(_) } => Some(Access::Dmr { cc, slot: Some(slot + 1), tg: call.talkgroup_ota }.to_string()),
        _ => call.nac.map(|n| Access::P25 { nac: Some(n), tg: call.talkgroup_ota }.to_string()),
    }
}

/// What one slot of an open channel heard in one run (FM and P25: slot 0 only).
#[derive(Default)]
struct Heard {
    audio: Vec<f32>,
    /// FM: `audio` before the voice high-pass (for the tone detector).
    low: Vec<f32>,
    frames: Vec<VoiceFrame>,
    /// (source, emergency, encrypted) from link control.
    infos: Vec<(Option<u32>, Flags)>,
    /// The cipher the encryption sync named, if it did.
    cipher: Option<CipherId>,
    /// The tracker decrypted voice.
    decrypted: bool,
    /// Talkers' GPS positions.
    positions: Vec<Position>,
    /// Air time of the first voice frame, and the end of the last (digital).
    air: Option<(f64, f64)>,
    /// The talkgroup the air named.
    tg: Option<u32>,
    color_code: Option<u8>,
    /// NXDN: the RAN its SACCH carried.
    ran: Option<u8>,
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
        self.cipher = other.cipher.or(self.cipher);
        self.decrypted |= other.decrypted;
        self.positions.append(&mut other.positions);
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
    /// How far off its channel its carrier came in, Hz: the sum of the
    /// measurements and their number (as a trunked call's).
    freq_error: (f64, u32),
    /// The talkgroup the air named, once it has ([`Call::talkgroup_ota`]).
    air_tg: Option<u32>,
    /// The row it is filed by.
    row: usize,
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
    /// Per slot: a cipher heard ahead of any voice (a P25 HDU in a run of
    /// its own), for the voice that follows it.
    cipher_ahead: [Option<CipherId>; 2],
}

/// One frequency, and the rows on it (what they share is checked: [`Conventional::check`]).
struct Chan {
    freq_hz: f64,
    mode: ConvMode,
    /// The system its rows belong to ([`ConvChannel::system`]).
    system: usize,
    analog: AnalogTuning,
    shape: ChannelShape,
    /// The lowest open threshold any row sets (None: the default).
    squelch_db: Option<f64>,
    /// Every row on the frequency (the first: what a transmission no row names is filed under).
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
    /// AM: its carrier detector, and the spurs it has learned (as the open decoder last heard of them).
    watch: Option<am::LineWatch>,
    ignored: Vec<(f64, f64)>,
    bar_db: f64,
    raised: bool,
    open: Option<Open>,
}

impl Chan {
    fn base_db(&self, dflt: f64) -> f64 {
        self.squelch_db.unwrap_or(dflt)
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
    /// Keep each call's vocoder frames (see [`super::frames`]).
    capture_frames: bool,
    chans: Vec<Chan>,
    floors: Vec<Floor>,
}

/// A channel's widths, from its mode (and an analog one's bandwidth), worked out once.
#[derive(Clone, Copy, Debug)]
struct ChannelShape {
    /// Half-width of the band the detector sums, Hz.
    detect_half_bw: f64,
    /// The head's filter cutoff, Hz.
    head_cutoff_hz: f64,
    /// The carrier meter's noise bandwidth, Hz.
    meter_bw: f64,
}

impl ChannelShape {
    fn of(mode: ConvMode, analog: &AnalogTuning) -> Self {
        // An analog channel's widths follow its bandwidth, from the 12.5 kHz FM channel's.
        let wider = |base: f64| analog.half_bw_hz + (base - fm::CHANNEL_HALF_BW);
        match mode {
            m if m.analog() => ChannelShape { detect_half_bw: wider(DETECT_HALF_BW), head_cutoff_hz: wider(HEAD_CUTOFF_HZ), meter_bw: 2.0 * analog.half_bw_hz },
            // Digital: the head as a trunked channel of its kind opens.
            m => ChannelShape { detect_half_bw: if m == ConvMode::Nxdn(Rate::N48) { 3125.0 } else { DETECT_HALF_BW }, head_cutoff_hz: m.kind().cutoff_hz(), meter_bw: ChannelFilter::noise_bandwidth() },
        }
    }
}

/// Whether `channels` can run together (see [`Conventional`]'s rows rules).
pub fn check_channels(channels: &[ConvChannel]) -> Result<(), String> {
    Conventional::check(channels)
}

/// What a channel's calls need from their system's configuration.
struct CallRules<'a> {
    call_timeout_s: f64,
    /// A call longer than this is concluded and a new one started (0: never), s.
    max_call_s: f64,
    /// Keep each call's vocoder frames (see [`super::frames`]).
    capture_frames: bool,
    /// The system's talkgroup names.
    talkgroups: &'a Talkgroups,
    /// Its encryption keys.
    keys: Option<&'a Arc<Keyring>>,
}

impl<'a> CallRules<'a> {
    fn of(sys: &'a SystemConfig, capture_frames: bool) -> Self {
        CallRules { call_timeout_s: sys.calls.call_timeout_s, max_call_s: sys.calls.max_call_s, capture_frames, talkgroups: &sys.talkgroups, keys: sys.keys.as_ref() }
    }
}

impl Conventional {
    /// `sources`: (centre, rate, usable half-width) of each source. Channels
    /// outside every source are an error.
    pub fn new(channels: &[ConvChannel], sources: &[(f64, f64, f64)], cfg: ConvConfig, bank_cfg: BankConfig, capture_frames: bool) -> Result<Self, String> {
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
            let c = &rows[0];
            let squelch_db = rows.iter().filter_map(|r| r.squelch_db).reduce(f64::min);
            let routed = rows.len() > 1 || c.access.is_some();
            let Some(src) = sources.iter().position(|&(center, _, half)| (c.freq_hz - center).abs() <= half) else {
                outside.push(format!("{:.5}", c.freq_hz / 1e6));
                continue;
            };
            let (center, rate, _) = sources[src];
            let offset_hz = c.freq_hz - center;
            let n = floor_slices(rate);
            let slice = floor_slice(n, rate, offset_hz);
            let bar = squelch_db.unwrap_or(cfg.squelch_db);
            let (freq_hz, mode, system, analog, shape) = (c.freq_hz, c.mode, c.system, c.analog, ChannelShape::of(c.mode, &c.analog));
            chans.push(Chan { freq_hz, mode, system, analog, shape, squelch_db, rows, routed, skipped: [None, None], source: src, offset_hz, slice, power: 0.0, watch: (mode == ConvMode::Am).then(am::LineWatch::default), ignored: Vec::new(), bar_db: bar, raised: false, open: None });
        }
        if !outside.is_empty() {
            return Err(format!("Conventional channel(s) outside every source's bandwidth: {} MHz — move a center frequency or disable them.", outside.join(", ")));
        }
        let floors = sources
            .iter()
            .map(|&(_, rate, _)| {
                let n = floor_slices(rate);
                Floor { slices: vec![0.0; n], scratch: vec![0.0; n], next_s: 0.0, primed: false }
            })
            .collect();
        Ok(Conventional { cfg, bank_cfg, capture_frames, chans, floors })
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
        if rows.iter().any(|r| r.analog.half_bw_hz != rows[0].analog.half_bw_hz) {
            return Err(format!("Conventional channel {mhz} MHz is listed with different bandwidths — rows sharing a frequency need the same one."));
        }
        if rows[0].mode == ConvMode::Am && rows.len() > 1 {
            return Err(format!("Conventional channel {mhz} MHz is listed twice — AM has no tone or code to split a frequency by."));
        }
        let what = match rows[0].mode {
            ConvMode::Fm => "tone",
            ConvMode::Am => "code",
            ConvMode::P25 => "NAC or talkgroup",
            ConvMode::Dmr => "colour code, slot or talkgroup",
            ConvMode::Nxdn(_) => "RAN or talkgroup",
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

    /// The keys of system `system` changed: its open channels decrypt with them from now on.
    pub fn set_system_keys(&mut self, system: usize, keys: Option<&Arc<Keyring>>) {
        for o in self.chans.iter_mut().filter(|c| c.system == system).filter_map(|c| c.open.as_mut()) {
            o.voice.set_keys(keys.cloned());
        }
    }

    /// Heads open now.
    pub fn open_count(&self) -> usize {
        self.chans.iter().filter(|c| c.open.is_some()).count()
    }

    /// Each channel: its frequency, its system, and its calls in progress.
    pub fn channels(&self) -> impl Iterator<Item = (f64, usize, usize)> + '_ {
        self.chans.iter().map(|c| (c.freq_hz, c.system, c.open.as_ref().map_or(0, |o| o.live.iter().flatten().count())))
    }

    /// Calls in progress.
    pub fn calls(&self) -> impl Iterator<Item = &Call> {
        self.chans.iter().filter_map(|c| c.open.as_ref()).flat_map(|o| o.live.iter().flatten().map(|l| &l.call))
    }

    /// After a block ran on `source` (whose clock reads `now_s`).
    /// `ids`: the engine's call ids. `systems`: the engine's (a channel's `system` indexes them).
    /// `gap`: the block is silence standing in for samples that never came
    /// ([`super::Engine::push_gap`]): calls open run on, nothing new opens,
    /// and the noise floor is left as it was.
    pub fn on_block<'s>(&mut self, source: usize, chz: &mut Channelizer, now_s: f64, gap: bool, ids: &CallIds, system: &dyn Fn(usize) -> &'s SystemConfig, out: &mut Vec<ConvOut>) {
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
        let mut bins = Vec::new();
        for (idx, ch) in self.chans.iter_mut().enumerate() {
            if ch.source != source {
                continue;
            }
            let rules = &CallRules::of(system(ch.system), self.capture_frames);
            let floor = self.floors[source].slices[ch.slice].max(1e-30);
            let bp = chz.band_power(ch.offset_hz, ch.shape.detect_half_bw);
            ch.power = if ch.power == 0.0 { bp } else { ch.power + a * (bp - ch.power) };
            let snr_db = if gap {
                f64::NEG_INFINITY
            } else if let Some(w) = &mut ch.watch {
                // AM: its carrier's line, above where noise alone puts the strongest bins
                // (steady lines left out), or its band power, whichever is the stronger.
                let first = chz.band_bins(ch.offset_hz, ch.shape.detect_half_bw, &mut bins);
                w.update(&bins, first, chz.bin_hz(), floor, chz.block_seconds(), a).max(10.0 * (ch.power / floor).log10())
            } else {
                10.0 * (ch.power / floor).log10()
            };
            let base = ch.base_db(dflt);
            let meter_thr = chz.threshold(floor, ch.shape.meter_bw, (base - CLOSE_BELOW_DB).max(3.0));
            if ch.open.is_none() {
                if ch.raised && snr_db < base {
                    ch.raised = false;
                    ch.bar_db = base;
                }
                if snr_db >= ch.bar_db {
                    Self::open(ch, chz, now_s, self.cfg.preroll_s, self.bank_cfg, self.cfg.vocoder, meter_thr, idx as u32, ids, rules, out);
                }
                continue;
            }
            let Some(iq) = chz.output(ch.open.as_ref().unwrap().head).map(|v| v.to_vec()) else { continue };
            if let Some(w) = &ch.watch {
                // A spur learned while it's open isn't listened to either.
                let ignore = w.ignored();
                if ignore != ch.ignored {
                    ch.open.as_mut().unwrap().voice.set_ignore(&ignore);
                    ch.ignored = ignore;
                }
            }
            let max_call_s = if rules.max_call_s > 0.0 { rules.max_call_s } else { STUCK_CALL_S };
            Self::run(ch, &iq, now_s, meter_thr, idx as u32, ids, rules, max_call_s, out);
            // Reception: the channel's power while its carrier is up, against the floor (both as a channel's head would see them).
            if snr_db >= base {
                let bw = ch.shape.meter_bw;
                let (sig, noise) = (chz.noise_in_band(ch.power, bw), chz.noise_in_band(floor, bw));
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
    fn open(ch: &mut Chan, chz: &mut Channelizer, now_s: f64, preroll_s: f64, bank_cfg: BankConfig, vocoder: mbe::Profile, meter_thr: f32, num: u32, ids: &CallIds, rules: &CallRules, out: &mut Vec<ConvOut>) {
        if let Some(w) = &ch.watch {
            ch.ignored = w.ignored();
        }
        let (head, pre, start_sample) = chz.add_head(ch.offset_hz, ch.shape.head_cutoff_hz, preroll_s);
        let rate = chz.output_rate();
        let kind = ch.mode.kind();
        let voice = voice::build(&VoiceSpec {
            kind,
            rate,
            // (On the engine's clock: `now_s` is this source's, offset included.)
            t0: start_sample as f64 / chz.fs() + (now_s - chz.sample_position() as f64 / chz.fs()),
            seed: ch.freq_hz as u32,
            vocoder,
            bank: bank_cfg,
            // (FM: set on every run, from the noise floor.)
            squelch: meter_thr,
            // For the tone detector.
            subaudible: ch.mode == ConvMode::Fm,
            analog: ch.analog,
            ignore_hz: ch.ignored.clone(),
            keys: rules.keys.cloned(),
        });
        let meter = (!ch.mode.analog()).then(|| ChannelFilter::new(rate));
        ch.open = Some(Open { head, voice, meter, opened_s: now_s, carrier_seen: false, last_carrier_s: now_s, live: [None, None], tx: None, cipher_ahead: [None; 2] });
        Self::run(ch, &pre, now_s, meter_thr, num, ids, rules, 0.0, out);
    }

    /// Run the open channel's receiver over `iq` (air up to `now_s`).
    #[allow(clippy::too_many_arguments)]
    fn run(ch: &mut Chan, iq: &[Complex32], now_s: f64, meter_thr: f32, num: u32, ids: &CallIds, rules: &CallRules, max_call_s: f64, out: &mut Vec<ConvOut>) {
        let o = ch.open.as_mut().unwrap();
        let mut heard: [Heard; 2] = Default::default();
        // Digital: the carrier by the meter; analog: by the decoder's squelch, at the same threshold.
        let metered = o.meter.as_mut().map(|m| m.meter(iq) > meter_thr);
        o.voice.set_squelch(meter_thr);
        let mut vout = Vec::new();
        o.voice.push(iq, &mut vout);
        let carrier = metered.or(o.voice.carrier()).unwrap_or(false);
        // Its calls' frequency error, while the carrier is up and the receiver can tell.
        if let Some(off) = o.voice.offset_hz().filter(|_| carrier) {
            for l in o.live.iter_mut().flatten() {
                l.freq_error.0 += off as f64;
                l.freq_error.1 += 1;
            }
        }
        for v in vout {
            let h = &mut heard[v.slot as usize & 1];
            match v.out {
                TrackerOut::Audio(a, f) => {
                    h.audio.extend_from_slice(&a);
                    h.frames.push(f);
                }
                TrackerOut::AnalogAudio(a) => h.audio.extend_from_slice(&a),
                TrackerOut::Subaudible(l) => h.low.extend_from_slice(&l),
                TrackerOut::Info { source, flags } => h.infos.push((source, flags)),
                TrackerOut::Cipher(id) => h.cipher = Some(id),
                TrackerOut::Decrypted => h.decrypted = true,
                TrackerOut::Alias(a) => out.push(ConvOut::Alias(ch.system as u16, a)),
                TrackerOut::Position(p) => h.positions.push(p),
                TrackerOut::WrongKey(c) => out.push(ConvOut::WrongKey(ch.system as u16, c)),
                TrackerOut::AliasLc(_) => {}
                TrackerOut::BaseStationId(call_sign) => out.push(ConvOut::Event(Event::CallSign { system: ch.system as u16, freq_hz: ch.freq_hz.round() as u64, call_sign })),
            }
        }
        for (slot, h) in heard.iter_mut().enumerate() {
            let a = o.voice.air(slot as u8);
            (h.air, h.tg, h.nac, h.color_code, h.ran) = (a.air, a.talkgroup, a.nac, a.color_code, a.ran);
            // A run with nothing but the cipher is dropped (no voice, no
            // link control): keep the cipher for the voice after it.
            let ahead = &mut o.cipher_ahead[slot];
            if h.audio.is_empty() && h.infos.is_empty() {
                *ahead = h.cipher.or(*ahead);
            } else {
                h.cipher = h.cipher.or(ahead.take());
            }
            if !carrier && h.audio.is_empty() {
                *ahead = None;
            }
        }
        if ch.routed && ch.mode == ConvMode::Fm {
            if let Some(code) = Self::route(&ch.rows, &mut o.tx, &mut heard[0], carrier) {
                Self::skip(&mut ch.skipped[0], ch.freq_hz, code, now_s, rules, out);
            }
        }
        if carrier {
            o.carrier_seen = true;
            o.last_carrier_s = now_s;
        }
        if ch.routed && ch.mode != ConvMode::Fm {
            for (slot, h) in heard.iter_mut().enumerate() {
                if let Some(code) = Self::route_digital(&ch.rows, slot, h) {
                    Self::skip(&mut ch.skipped[slot], ch.freq_hz, code, now_s, rules, out);
                }
            }
        }
        for (slot, h) in heard.into_iter().enumerate() {
            Self::slot_call(ch, slot, h, now_s, num, ids, rules, max_call_s, out);
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
            out.push(ConvOut::Event(Event::ConvSkipped { freq_hz: freq_hz.round() as u64, code: code.clone() }));
        }
        *last = Some((code, now_s));
    }

    fn tone_code(tones: &ToneDetector) -> String {
        tones.heard().map_or(String::new(), |h| h.tone.to_string())
    }

    /// P25 / DMR / NXDN: the row this run's traffic on `slot` is for (none:
    /// it is dropped, and its code returned).
    fn route_digital(rows: &[ConvChannel], slot: usize, h: &mut Heard) -> Option<String> {
        if h.audio.is_empty() && h.infos.is_empty() {
            return None;
        }
        let best = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.access.is_none_or(|a| a.admits(h.nac, slot, h.color_code, h.tg, h.ran)))
            .max_by_key(|(i, r)| (r.access.map_or(0, Access::fields), std::cmp::Reverse(*i)));
        match best {
            // Only voice counts as a transmission lost (as only calls with audio are kept).
            None if h.audio.is_empty() => {
                *h = Heard::default();
                None
            }
            None => {
                let code = match rows[0].mode {
                    ConvMode::P25 => Access::P25 { nac: h.nac, tg: h.tg }.to_string(),
                    ConvMode::Nxdn(_) => Access::Nxdn { ran: h.ran.filter(|&r| r != 0), tg: h.tg }.to_string(),
                    _ => Access::Dmr { cc: h.color_code, slot: Some(slot as u8 + 1), tg: h.tg }.to_string(),
                };
                *h = Heard::default();
                Some(code)
            }
            Some((i, _)) => {
                h.row = Some(i);
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
    fn slot_call(ch: &mut Chan, slot: usize, h: Heard, now_s: f64, num: u32, ids: &CallIds, rules: &CallRules, max_call_s: f64, out: &mut Vec<ConvOut>) {
        if h.audio.is_empty() && h.infos.is_empty() {
            return;
        }
        let o = ch.open.as_mut().unwrap();
        // The talkgroup: the row's, else the air's, else the row's default.
        let r = h.row.unwrap_or(0);
        let row = &ch.rows[r];
        let air_tg = h.tg;
        let tg = row.talkgroup.or(air_tg).unwrap_or(row.default_talkgroup);
        if let Some(l) = &o.live[slot] {
            let too_long = max_call_s > 0.0 && now_s - l.call.start_s > max_call_s;
            // A different talkgroup on the air, or another row's
            // transmission, is a new call (digital traffic that hasn't named
            // its talkgroup yet stays with the call it follows); the first
            // talkgroup named just labels the call it arrived in.
            let new_tg = l.air_tg.is_some() && air_tg.is_some_and(|t| Some(t) != l.air_tg);
            let new_row = l.row != r && (ch.mode.analog() || air_tg.is_some());
            if too_long || new_tg || new_row {
                let l = o.live[slot].take().unwrap();
                Self::end(l, num, out);
            } else if air_tg.is_some() && l.air_tg.is_none() {
                let info = Self::names(row, tg, rules.talkgroups);
                let l = o.live[slot].as_mut().unwrap();
                l.air_tg = air_tg;
                l.call.talkgroup_ota = air_tg;
                if l.call.talkgroup != tg {
                    l.call.talkgroup = tg;
                    l.call.talkgroup_info = info;
                }
                out.push(ConvOut::Event(Event::CallUpdate(l.call.clone())));
            }
        }
        if o.live[slot].is_none() {
            let dur = h.audio.len() as f64 / fm::AUDIO_RATE;
            let start = h.air.map_or_else(|| (now_s - dur).max(o.opened_s - 0.05), |a| a.0);
            let call = Call {
                site: None,
                system: ch.system as u16,
                twin: false,
                id: ids.next(),
                talkgroup: tg,
                talkgroup_ota: air_tg,
                freq_hz: ch.freq_hz.round() as u64,
                voice: ch.mode.voice(slot as u8, h.color_code),
                unit_to_unit: false,
                recording: true,
                followed: false,
                reason: None,
                encrypted: false,
                cipher: None,
                decrypted: false,
                emergency: false,
                priority: 0,
                duplex: false,
                mode: false,
                start_s: start,
                last_update_s: now_s,
                heard_s: now_s,
                last_audio_s: now_s,
                sources: Vec::new(),
                talkgroup_info: Self::names(row, tg, rules.talkgroups),
                patched_talkgroups: Vec::new(),
                ran: h.ran,
                nac: h.nac,
                tone: None,
                tone_set: match row.access {
                    Some(Access::Tone(t)) => Some(t),
                    _ => None,
                },
            };
            out.push(ConvOut::Event(Event::CallStart(call.clone())));
            o.live[slot] = Some(Live { call, audio: Vec::new(), frames: CallFrames::new(rules.capture_frames), tx: Transmissions::default(), reception: Reception::default(), freq_error: (0.0, 0), air_tg, row: r, tones: (ch.mode == ConvMode::Fm).then(Box::default) });
        }
        let l = o.live[slot].as_mut().unwrap();
        l.call.last_update_s = now_s;
        l.call.heard_s = now_s;
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
        if l.call.ran.is_none() && h.ran.is_some() {
            l.call.ran = h.ran;
            changed = true;
        }
        if let (Voice::Dmr { color_code: cc @ None, .. }, Some(heard)) = (&mut l.call.voice, h.color_code) {
            *cc = Some(heard);
            changed = true;
        }
        if h.cipher.is_some() && l.call.cipher != h.cipher {
            l.call.cipher = h.cipher;
            changed = true;
        }
        for (src, flags) in h.infos {
            changed |= CallManager::note_info(&mut l.call, Some(&mut l.tx), src, flags, now_s);
        }
        if h.decrypted {
            changed |= CallManager::note_decrypted(&mut l.call, Some(&mut l.tx), now_s);
        }
        for p in h.positions {
            if CallManager::note_position(&mut l.call, &p) {
                out.push(ConvOut::Event(Event::UnitPosition { system: l.call.system, unit: p.unit, lat: p.lat, lon: p.lon, talkgroup: l.call.talkgroup }));
                changed = true;
            }
        }
        if changed {
            out.push(ConvOut::Event(Event::CallUpdate(l.call.clone())));
        }
        // (An encrypted transmission's audio, unless decrypted, is left out
        // when the call is saved, as a trunked call's is; it isn't played live.)
        if !h.audio.is_empty() {
            l.tx.note(l.audio.len(), now_s);
            l.audio.extend_from_slice(&h.audio);
            for f in h.frames {
                l.frames.push(f);
            }
            if !l.tx.current_dropped() {
                out.push(ConvOut::Event(Event::Audio { call_id: l.call.id, system: l.call.system, talkgroup: l.call.talkgroup, samples: h.audio, decrypted: l.call.encrypted || l.call.decrypted }));
            }
        }
    }

    /// The call's names: the row's, if it has any (filed under `tg`); else
    /// the system's talkgroup file's for `tg`.
    fn names(row: &ConvChannel, tg: u32, talkgroups: &Talkgroups) -> Option<Talkgroup> {
        row.info.clone().map(|t| Talkgroup { number: tg, ..t }).or_else(|| talkgroups.get(&tg).cloned())
    }

    fn end(l: Live, num: u32, out: &mut Vec<ConvOut>) {
        let freq_error_hz = (l.freq_error.1 > 0).then(|| l.freq_error.0 / l.freq_error.1 as f64);
        out.push(ConvOut::End { call: l.call, audio: l.audio, frames: l.frames, recorder_num: num, tx: l.tx, reception: l.reception, freq_error_hz });
    }

    /// End of input: flush the receivers and end every call.
    pub fn finish(&mut self, out: &mut Vec<ConvOut>) {
        for (idx, ch) in self.chans.iter_mut().enumerate() {
            let Some(o) = ch.open.as_mut() else { continue };
            let mut vout = Vec::new();
            o.voice.flush(&mut vout);
            for v in vout {
                let Some(l) = o.live[v.slot as usize & 1].as_mut() else { continue };
                if let TrackerOut::Audio(a, f) = v.out {
                    l.audio.extend_from_slice(&a);
                    l.frames.push(f);
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
    use crate::trunk::{CallConfig, SystemConfig};
    use std::f64::consts::PI;

    struct Tx {
        offset_hz: f64,
        /// Power relative to the noise in a 10 kHz band, dB.
        snr_db: f64,
        on: Vec<(f64, f64)>,
        ph: f64,
        /// AM (50 % at 1 kHz) instead of FM.
        am: bool,
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

    fn conv_system(name: &str) -> SystemConfig {
        SystemConfig { calls: CallConfig { call_timeout_s: 1.0, ..Default::default() }, ..SystemConfig::conventional(name) }
    }

    fn run_systems(fs: f64, secs: f64, txs: &mut [Tx], channels: Vec<ConvChannel>, systems: Vec<SystemConfig>) -> Run {
        run_gapped(fs, secs, txs, channels, systems, None)
    }

    /// [`run_systems`], with `gap` = (air time, seconds): that long of
    /// samples that never came ([`Engine::push_gap`]) at that point of the air.
    fn run_gapped(fs: f64, secs: f64, txs: &mut [Tx], channels: Vec<ConvChannel>, systems: Vec<SystemConfig>, mut gap: Option<(f64, f64)>) -> Run {
        let center = 155_000_000.0;
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ }],
            conventional: channels,
            systems,
            ..Default::default()
        };
        let e = Engine::new(cfg).unwrap();
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
                    let dev = if tx.am { 0.0 } else { tx.audio.get(i).map_or(2500.0 * (2.0 * PI * 1000.0 * t).sin(), |&a| 5000.0 * a as f64) };
                    tx.ph += 2.0 * PI * (tx.offset_hz + dev) / fs;
                    if on.is_some() {
                        let p = sigma2 * 10_000.0 / fs * 10f64.powf(tx.snr_db / 10.0);
                        let env = if tx.am { 1.0 + 0.5 * (2.0 * PI * 1000.0 * t).sin() } else { 1.0 };
                        x += Complex32::from_polar((p.sqrt() * env) as f32, tx.ph as f32);
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
                    Event::Concluded(k) => out.push((k.call, k.audio.len(), k.record.to_json())),
                    Event::CallStart(_) => starts += 1,
                    Event::ConvSkipped { freq_hz, code } => skipped.push((freq_hz, code)),
                    _ => {}
                }
            }
        }
        e.finish();
        for ev in e.drain_events() {
            if let Event::Concluded(k) = ev {
                out.push((k.call, k.audio.len(), k.record.to_json()));
            }
        }
        (out, starts, skipped)
    }

    fn fm(freq_hz: f64, tg: u32) -> ConvChannel {
        ConvChannel { talkgroup: Some(tg), ..ConvChannel::new(freq_hz, ConvMode::Fm) }
    }

    /// An encrypted P25 transmission on a conventional channel whose system
    /// has the key: one call, decrypted, naming its cipher (the HDU that
    /// carries it is often decoded in a run of its own, ahead of the voice).
    #[test]
    fn p25_decrypted_calls_name_their_cipher() {
        // Low rates too: their noise-floor slices once filled with the signal,
        // and no channel opened.
        for (fs, off) in [(2_400_000.0, 200_000.0), (480_000.0, 50_000.0), (240_000.0, 30_000.0)] {
            p25_decrypted_call_at(fs, off);
        }
    }

    fn p25_decrypted_call_at(fs: f64, off: f64) {
        use crate::p25::crypt::{Keyring, ALGID_AES_256};
        use crate::p25::synth::{dibits, imbe, modulate, Tx};
        use crate::p25::voice::EncryptionSync;
        let (center, freq) = (155_000_000.0, 155_000_000.0 + off);
        let key = vec![0x5a; 32];
        let es = EncryptionSync { algid: ALGID_AES_256, keyid: 0x4319, mi: [9, 8, 7, 6, 5, 4, 3, 2, 0] };
        let tx = Tx { nac: 0x293, talkgroup: 716, source: 42, cipher: Some((es, key.clone())), voice: (0..72).map(imbe).collect() };
        let mut x = 3u32;
        let mut pad = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 30) as u8
                })
                .collect()
        };
        let mut d = pad(2400);
        d.extend(dibits(&tx.frames()));
        d.extend(pad(2400));
        let mut ph = 0.0;
        let mut iq = vec![Complex32::default(); (fs * 0.5) as usize];
        iq.extend(modulate(&d, fs, freq - center, 0.3, &mut ph));
        iq.extend(vec![Complex32::default(); (fs * 3.0) as usize]);
        crate::nxdn::synth::add_noise(&mut iq, 0.003, &mut 9);
        let mut keys = Keyring::default();
        keys.add(ALGID_AES_256, Some(0x4319), key, vec![]).unwrap();
        let sys = SystemConfig { keys: Some(Arc::new(keys)), ..conv_system("conv") };
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ }],
            conventional: vec![ConvChannel::new(freq, ConvMode::P25)],
            systems: vec![sys],
            ..Default::default()
        };
        let e = Engine::new(cfg).unwrap();
        for c in iq.chunks(32_768) {
            e.push_iq(0, c);
        }
        e.finish();
        let calls: Vec<Call> = e.drain_events().into_iter().filter_map(|ev| if let Event::Concluded(k) = ev { Some(k.call) } else { None }).collect();
        let [c] = &calls[..] else { panic!("{fs} S/s: {} calls", calls.len()) };
        assert_eq!((c.talkgroup, c.talkgroup_ota), (716, Some(716)));
        assert!(c.encrypted && c.decrypted);
        assert_eq!(c.cipher, Some(CipherId { algid: ALGID_AES_256, keyid: 0x4319 }));
    }

    #[test]
    fn conventional_systems_keep_their_own_names_and_rules() {
        let (fs, c) = (2_400_000.0, 155_000_000.0);
        let mut txs = vec![
            Tx { offset_hz: 200_000.0, snr_db: 30.0, on: vec![(0.5, 2.5)], ph: 0.0, am: false, audio: Vec::new() },
            Tx { offset_hz: 312_500.0, snr_db: 30.0, on: vec![(0.5, 2.5)], ph: 0.0, am: false, audio: Vec::new() },
            Tx { offset_hz: -200_000.0, snr_db: 30.0, on: vec![(0.5, 2.5)], ph: 0.0, am: false, audio: Vec::new() },
        ];
        let police = ConvChannel { system: 1, ..fm(c + 312_500.0, 2) };
        let ems = ConvChannel { system: 2, ..fm(c - 200_000.0, 3) };
        // EMS keeps only calls of 3 s and more.
        let mut strict = conv_system("ems");
        strict.save.min_call_s = 3.0;
        let (calls, _, _) = run_systems(fs, 3.5, &mut txs, vec![fm(c + 200_000.0, 1), police, ems], vec![conv_system("fire"), conv_system("police"), strict]);
        let mut got: Vec<(u32, u16, bool)> = calls.iter().map(|(k, _, j)| (k.talkgroup, k.system, j.contains(r#""short_name":"fire""#) || j.contains(r#""short_name":"police""#))).collect();
        got.sort();
        assert_eq!(got, [(1, 0, true), (2, 1, true)], "EMS's 2 s call is under its minimum");
        assert!(calls.iter().any(|(k, _, j)| k.talkgroup == 2 && j.contains(r#""short_name":"police""#)));
        assert!(calls.iter().all(|(k, _, _)| k.site.is_none()), "a conventional call has no site");
    }

    /// Airband: AM calls on 25 kHz channels beside each other, one 2 kHz off
    /// frequency, one weak, an FM call beside them, and a strong AM neighbour
    /// that isn't a channel. (The source's centre is 155 MHz, so the AM
    /// channels take the VHF filter, as 118–137 MHz would. AM channels open
    /// from 3 s, once they know the spurs.)
    #[test]
    fn am_calls_detected_beside_fm_and_neighbours() {
        let fs = 2_400_000.0;
        let c = 155_000_000.0;
        let am = |off: f64, snr_db: f64, on: Vec<(f64, f64)>| Tx { offset_hz: off, snr_db, on, ph: 0.0, am: true, audio: Vec::new() };
        let mut txs = vec![
            // Two transmissions 1.5 s apart (> the 1 s call timeout): two calls, 2 kHz off frequency.
            am(202_000.0, 30.0, vec![(3.5, 5.5), (7.0, 8.0)]),
            // A weak one, 15 dB above noise, on the next channel up.
            am(225_000.0, 15.0, vec![(4.0, 6.0)]),
            // A very strong neighbour (not a configured channel) 25 kHz from channel 3.
            am(-75_000.0, 50.0, vec![(3.2, 8.5)]),
            Tx { offset_hz: 300_000.0, snr_db: 25.0, on: vec![(1.0, 2.0)], ph: 0.0, am: false, audio: Vec::new() },
        ];
        let air = |freq: f64, tg: u32| ConvChannel { talkgroup: Some(tg), ..ConvChannel::new(freq, ConvMode::Am) };
        let chans = vec![air(c + 200_000.0, 1), air(c + 225_000.0, 2), air(c - 50_000.0, 3), fm(c + 300_000.0, 4)];
        let (calls, _, _) = run(fs, 9.0, &mut txs, chans);
        let mut got: Vec<(u32, bool, f64)> = calls.iter().map(|(k, n, _)| (k.talkgroup, k.voice.analog(), *n as f64 / 8000.0)).collect();
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let tgs: Vec<u32> = got.iter().map(|g| g.0).collect();
        assert_eq!(tgs, [1, 1, 2, 4], "{got:?}");
        assert!(got.iter().all(|g| g.1), "all analog: {got:?}");
        // Each about as long as it was on the air.
        for (g, want) in got.iter().zip([1.0, 2.0, 2.0, 1.0]) {
            assert!((g.2 - want).abs() < 0.2, "talkgroup {}: {:.2} s of audio, wanted {want}", g.0, g.2);
        }
    }

    /// A distant aircraft: AM 3 dB over the noise in its channel, 5 dB under
    /// the 8 dB squelch by band power, opens by its carrier and is recorded
    /// whole; an idle AM channel beside it never opens on the noise.
    #[test]
    fn weak_am_opens_by_its_carrier() {
        let (fs, c) = (2_400_000.0, 155_000_000.0);
        let mut txs = vec![Tx { offset_hz: 201_000.0, snr_db: 3.0, on: vec![(3.5, 6.0)], ph: 0.0, am: true, audio: Vec::new() }];
        let air = |freq: f64, tg: u32| ConvChannel { talkgroup: Some(tg), ..ConvChannel::new(freq, ConvMode::Am) };
        let (calls, starts, _) = run(fs, 7.5, &mut txs, vec![air(c + 200_000.0, 1), air(c + 250_000.0, 2)]);
        let got: Vec<(u32, f64)> = calls.iter().map(|(k, n, _)| (k.talkgroup, *n as f64 / 8000.0)).collect();
        assert_eq!(starts, 1, "{got:?}");
        let [(1, secs)] = got[..] else { panic!("{got:?}") };
        assert!((secs - 2.5).abs() < 0.25, "{secs:.2} s of 2.5 s");
    }

    /// A spur (a steady, unmodulated line) in an AM channel from the start,
    /// 4 dB over the channel's noise (under the squelch by band power, ~20 dB
    /// over it as a line): learned in the warm-up, it never opens the channel
    /// nor holds open the call of an aircraft beside it, 3 kHz away.
    #[test]
    fn am_spurs_are_learned() {
        let (fs, c) = (2_400_000.0, 155_000_000.0);
        let spur = |off: f64, on: (f64, f64)| Tx { offset_hz: off, snr_db: 4.0, on: vec![on], ph: 0.0, am: false, audio: vec![0.0; 8000 * 20] };
        let mut txs = vec![
            spur(200_000.0 - 3_000.0, (0.0, 12.0)),
            Tx { offset_hz: 200_000.0, snr_db: 15.0, on: vec![(6.0, 8.0)], ph: 0.0, am: true, audio: Vec::new() },
            // A channel with only a spur.
            spur(251_200.0, (0.0, 12.0)),
        ];
        let air = |freq: f64, tg: u32| ConvChannel { talkgroup: Some(tg), ..ConvChannel::new(freq, ConvMode::Am) };
        let (calls, starts, _) = run(fs, 12.0, &mut txs, vec![air(c + 200_000.0, 1), air(c + 250_000.0, 2)]);
        let got: Vec<(u32, f64)> = calls.iter().map(|(k, n, _)| (k.talkgroup, *n as f64 / 8000.0)).collect();
        assert_eq!(starts, 1, "{got:?}");
        let [(1, secs)] = got[..] else { panic!("{got:?}") };
        assert!((secs - 2.0).abs() < 0.25, "{secs:.2} s of 2 s");
    }

    #[test]
    fn am_rows_and_codes() {
        let f = 121_500_000.0;
        assert!(Access::parse(ConvMode::Am, "").unwrap().is_none());
        assert!(Access::parse(ConvMode::Am, "151.4").is_err());
        let a = ConvChannel::new(f, ConvMode::Am);
        assert_eq!(a.analog.half_bw_hz, crate::dsp::am::HALF_BW_VHF);
        assert_eq!(ConvChannel::new(243e6, ConvMode::Am).analog.half_bw_hz, crate::dsp::am::HALF_BW_UHF);
        let twice = Conventional::check(&[a.clone(), ConvChannel { talkgroup: Some(9), ..a.clone() }]).unwrap_err();
        assert!(twice.contains("AM has no tone"), "{twice}");
        let mut narrow = ConvChannel::new(f, ConvMode::Fm);
        narrow.access = Access::parse(ConvMode::Fm, "100.0").unwrap();
        let mut wide = ConvChannel::new(f, ConvMode::Fm);
        wide.analog.half_bw_hz = 8000.0;
        let bw = Conventional::check(&[narrow, wide]).unwrap_err();
        assert!(bw.contains("different bandwidths"), "{bw}");
    }

    #[test]
    fn fm_calls_detected_split_and_leakage_ignored() {
        let fs = 2_400_000.0;
        let c = 155_000_000.0;
        let mut txs = vec![
            // Two transmissions 1.5 s apart (> the 1 s call timeout): two calls.
            Tx { offset_hz: 200_000.0, snr_db: 30.0, on: vec![(0.5, 2.5), (4.0, 5.0)], ph: 0.0, am: false, audio: Vec::new() },
            // A weak one, 15 dB above noise.
            Tx { offset_hz: 312_500.0, snr_db: 15.0, on: vec![(1.0, 3.0)], ph: 0.0, am: false, audio: Vec::new() },
            // A very strong neighbour (not a configured channel) 12.5 kHz from channel 3.
            Tx { offset_hz: -100_000.0, snr_db: 50.0, on: vec![(0.2, 5.5)], ph: 0.0, am: false, audio: Vec::new() },
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
        let mut txs = vec![Tx { offset_hz: 200_000.0, snr_db: 30.0, on: vec![(0.5, 2.0)], ph: 0.0, am: false, audio: Vec::new() }];
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
        let mut txs = vec![Tx { offset_hz: 200_000.0, snr_db: 25.0, on: vec![(0.5, 0.5 + secs)], ph: 0.0, am: false, audio }];
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
            Tx { offset_hz: 200_000.0, snr_db: 25.0, on: vec![(0.5, 2.5)], ph: 0.0, am: false, audio: ctcss },
            Tx { offset_hz: 300_000.0, snr_db: 25.0, on: vec![(0.5, 2.5)], ph: 0.0, am: false, audio: dcs },
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
            txs.iter().map(|&(at, tone)| Tx { offset_hz: 200_000.0, snr_db: 25.0, on: vec![(at, at + 1.5)], ph: 0.0, am: false, audio: voice_over(tone, 1.5) }).collect();
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
        let nac = |n| Ok(Some(Access::P25 { nac: Some(n), tg: None }));
        for t in ["293", "293 NAC", "NAC 293", "$293", "0x293", "nac 293"] {
            assert_eq!(p25(t), nac(0x293), "{t}");
        }
        assert_eq!(p25("1a2"), nac(0x1a2));
        for t in ["", "F7E", "$F7F", "S"] {
            assert_eq!(p25(t), Ok(None), "{t}");
        }
        assert!(p25("1234").is_err() && p25("xyz").is_err() && p25("NAC").is_err() && p25("TG 70000").is_err());
        assert_eq!(p25("NAC 293 TG 1"), Ok(Some(Access::P25 { nac: Some(0x293), tg: Some(1) })));
        assert_eq!(p25("293, TG1"), Ok(Some(Access::P25 { nac: Some(0x293), tg: Some(1) })));
        assert_eq!(p25("TG 46"), Ok(Some(Access::P25 { nac: None, tg: Some(46) })));
        assert_eq!(p25("F7E TG 46"), Ok(Some(Access::P25 { nac: None, tg: Some(46) })));
        assert_eq!(Access::P25 { nac: Some(0x2a), tg: None }.to_string(), "NAC 02A");
        assert_eq!(Access::P25 { nac: Some(0x2a), tg: Some(7) }.to_string(), "NAC 02A TG 7");
        // Code, Slot and Match TG: joined, and split back.
        let join = Access::join;
        assert_eq!(join(ConvMode::P25, "293", None, Some(1)), Ok(Some(Access::P25 { nac: Some(0x293), tg: Some(1) })));
        assert_eq!(join(ConvMode::P25, "", None, None), Ok(None));
        assert_eq!(join(ConvMode::Dmr, "CC1", Some(2), Some(201)), Ok(Some(Access::Dmr { cc: Some(1), slot: Some(2), tg: Some(201) })));
        assert_eq!(join(ConvMode::Dmr, "CC1 TS2 TG201", Some(2), None), Ok(Some(Access::Dmr { cc: Some(1), slot: Some(2), tg: Some(201) })));
        assert!(join(ConvMode::Dmr, "CC1 TS2", Some(1), None).is_err(), "two slots");
        assert!(join(ConvMode::P25, "293", Some(1), None).is_err(), "P25 has no slots");
        assert!(join(ConvMode::Fm, "151.4", None, Some(1)).is_err(), "FM names no talkgroup");
        assert!(join(ConvMode::Am, "", None, Some(1)).is_err());
        assert!(join(ConvMode::Nxdn(crate::nxdn::Rate::N48), "5", None, Some(70_000)).is_err());
        for (mode, a) in [
            (ConvMode::P25, Access::P25 { nac: Some(0x293), tg: Some(1) }),
            (ConvMode::P25, Access::P25 { nac: None, tg: Some(1) }),
            (ConvMode::Dmr, Access::Dmr { cc: Some(1), slot: Some(2), tg: None }),
            (ConvMode::Nxdn(crate::nxdn::Rate::N48), Access::Nxdn { ran: Some(5), tg: Some(201) }),
            (ConvMode::Fm, Access::parse(ConvMode::Fm, "D023N").unwrap().unwrap()),
        ] {
            let (code, slot, tg) = a.parts();
            assert_eq!(join(mode, &code, slot, tg), Ok(Some(a)), "{a}");
        }
        assert_eq!(Access::P25 { nac: Some(0x293), tg: Some(1) }.parts(), ("293".into(), None, Some(1)));
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

    /// Unencrypted P25 transmissions `(start s, NAC, talkgroup)` on the
    /// frequency of `rows` (155.05 MHz): the calls concluded with their JSON,
    /// and the codes no row took.
    fn p25_calls(txs: &[(f64, u16, u16)], rows: Vec<ConvChannel>, secs: f64) -> (Vec<(Call, String)>, Vec<String>) {
        use crate::p25::synth::{dibits, imbe, modulate, Tx};
        let (fs, center, freq) = (480_000.0, 155_000_000.0, 155_050_000.0);
        let mut x = 3u32;
        let mut pad = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 30) as u8
                })
                .collect()
        };
        let mut iq = vec![Complex32::default(); (fs * secs) as usize];
        for &(at, nac, talkgroup) in txs {
            let tx = Tx { nac, talkgroup, source: 42, cipher: None, voice: (0..36).map(imbe).collect() };
            let mut d = pad(1200);
            d.extend(dibits(&tx.frames()));
            d.extend(pad(1200));
            let mut ph = 0.0;
            for (k, v) in modulate(&d, fs, freq - center, 0.3, &mut ph).into_iter().enumerate() {
                if let Some(s) = iq.get_mut((at * fs) as usize + k) {
                    *s += v;
                }
            }
        }
        crate::nxdn::synth::add_noise(&mut iq, 0.003, &mut 9);
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ }],
            conventional: rows.into_iter().map(|r| ConvChannel { freq_hz: freq, ..r }).collect(),
            systems: vec![conv_system("conv")],
            ..Default::default()
        };
        let e = Engine::new(cfg).unwrap();
        for c in iq.chunks(32_768) {
            e.push_iq(0, c);
        }
        e.finish();
        let (mut calls, mut skipped) = (Vec::new(), Vec::new());
        for ev in e.drain_events() {
            match ev {
                Event::Concluded(k) => calls.push((k.call.clone(), k.record.to_json())),
                Event::ConvSkipped { code, .. } => skipped.push(code),
                _ => {}
            }
        }
        (calls, skipped)
    }

    /// A P25 row: its code (NAC / talkgroup), the number it files under, its name.
    fn p25_row(code: &str, tg: Option<u32>, name: &str) -> ConvChannel {
        let info = (!name.is_empty()).then(|| Talkgroup { alpha_tag: name.into(), ..Default::default() });
        ConvChannel { access: Access::parse(ConvMode::P25, code).unwrap(), talkgroup: tg, info, ..ConvChannel::new(155_050_000.0, ConvMode::P25) }
    }

    /// (filed under, the air's, name) of each call.
    fn filed(calls: &[(Call, String)]) -> Vec<(u32, Option<u32>, String)> {
        calls.iter().map(|(c, _)| (c.talkgroup, c.talkgroup_ota, c.talkgroup_info.as_ref().map_or(String::new(), |t| t.alpha_tag.clone()))).collect()
    }

    #[test]
    fn p25_rows_file_under_their_number_and_keep_the_airs() {
        // No number: the air's, which the record names as the air's too.
        let (calls, _) = p25_calls(&[(0.3, 0x293, 1)], vec![p25_row("", None, "")], 3.0);
        assert_eq!(filed(&calls), [(1, Some(1), String::new())]);
        assert!(calls[0].1.contains(r#""talkgroup":1,"talkgroup_ota":1,"#), "{}", calls[0].1);
        // The NAC the frames carried, and what kind of call it was.
        assert!(calls[0].1.contains(r#""nac":659,"#) && calls[0].1.contains(r#""system_type":"conventional","voice_mode":"p25","#), "{}", calls[0].1);
        // A number of its own: filed under it, the air's kept.
        let (calls, _) = p25_calls(&[(0.3, 0x293, 1)], vec![p25_row("", Some(46012), "Townville PD")], 3.0);
        assert_eq!(filed(&calls), [(46012, Some(1), "Townville PD".into())]);
        assert!(calls[0].1.contains(r#""talkgroup":46012,"talkgroup_ota":1,"#), "{}", calls[0].1);
    }

    #[test]
    fn p25_rows_split_a_frequency_by_talkgroup() {
        let rows = vec![p25_row("293 TG 1", Some(45351), "Townville PW"), p25_row("293 TG 2", Some(45353), "Townville Parks"), p25_row("293", None, "Townville (other)")];
        let txs = [(0.3, 0x293, 1), (3.3, 0x293, 2), (6.3, 0x293, 7), (9.3, 0x4f1, 1)];
        let (calls, skipped) = p25_calls(&txs, rows, 12.0);
        assert_eq!(
            filed(&calls),
            [(45351, Some(1), "Townville PW".into()), (45353, Some(2), "Townville Parks".into()), (7, Some(7), "Townville (other)".into())]
        );
        assert_eq!(skipped, ["NAC 4F1 TG 1"]);
    }

    #[test]
    fn a_new_talkgroup_on_a_folding_row_is_a_new_call() {
        // Back to back, within the call timeout: two calls, one number.
        let (calls, _) = p25_calls(&[(0.3, 0x293, 1), (1.5, 0x293, 2)], vec![p25_row("", Some(46012), "")], 4.0);
        assert_eq!(filed(&calls), [(46012, Some(1), String::new()), (46012, Some(2), String::new())]);
    }

    #[test]
    fn analog_records_name_no_air_talkgroup() {
        let mut txs = [Tx { offset_hz: 200_000.0, snr_db: 30.0, on: vec![(0.5, 2.0)], ph: 0.0, am: false, audio: Vec::new() }];
        let (calls, ..) = run(2_400_000.0, 4.0, &mut txs, vec![fm(155.2e6, 5)]);
        let [(c, _, json)] = &calls[..] else { panic!("{} calls", calls.len()) };
        assert_eq!((c.talkgroup, c.talkgroup_ota), (5, None));
        assert!(!json.contains("talkgroup_ota"), "{json}");
        assert!(json.contains(r#""nac":-1,"#) && json.contains(r#""voice_mode":"fm","#), "{json}");
    }
}
