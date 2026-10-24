//! In-band signalling: what a call's voice carries besides speech, and the
//! detectors that find it, each a [`Detector`] over the call's audio:
//!
//! - [`units`]: MDC1200 and FleetSync unit IDs (analog FM);
//! - [`subaudible`]: the CTCSS tone or DCS code under the voice (analog FM);
//! - [`audio`]: DTMF and KNOX digits, call progress tones and the single
//!   tones paging is made of (P25 Phase 1, analog, and a half-rate radio
//!   that sends no tone frames).
//!
//! A half-rate (AMBE+2) radio whose vocoder recognises a tone sends a tone
//! frame naming it (TIA-102.BABA-A §16) instead, which the vocoder plays
//! ([`crate::mbe::Decoder::tone`]) and the frame carries
//! ([`crate::trunk::frames::VoiceFrame::tone`]). A call gathers all of it in
//! one place ([`crate::trunk::inband::InBand`]). [`pattern`] makes its tones
//! into the signals they are (a page, a DTMF string, a Morse ID), [`signal`]
//! names those by a system's signals, and [`tables`] has the paging plans.

use std::fmt;
use std::ops::Range;

pub mod audio;
pub mod pattern;
pub mod signal;
pub mod subaudible;
pub mod tables;
pub mod units;

/// What a detector listens to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tap {
    /// The call's audio.
    Voice,
    /// FM: the audio before the voice high-pass (where CTCSS and DCS are).
    Subaudible,
}

/// Some of a call's audio, for its detectors.
pub struct Chunk<'a> {
    pub audio: &'a [f32],
    /// The call's sample `audio` starts at.
    pub at: usize,
    /// What the vocoder's tone frames already named (samples of the call's
    /// audio): a detector leaves those alone.
    pub named: &'a [Range<usize>],
}

/// Something a call's voice carried.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Found {
    /// A unit ID burst: who is talking.
    Unit(units::UnitId),
    /// The sub-audible tone or code, as now read (each new reading).
    Code(subaudible::ToneHeard),
    /// A tone that ended.
    Tone(ToneSeg),
}

/// A detector of one kind of in-band signalling, one per call.
pub trait Detector: Send {
    fn tap(&self) -> Tap {
        Tap::Voice
    }
    fn push(&mut self, chunk: &Chunk, out: &mut Vec<Found>);
    /// The call's audio is all in: what it was still in the middle of.
    fn finish(&mut self, _out: &mut Vec<Found>) {}
    /// In the middle of something since this sample of the call's audio:
    /// what came just before it may still belong with it.
    fn busy_since(&self) -> Option<usize> {
        None
    }
}

/// DTMF's and KNOX's low (row) and high (column) tones, Hz, and the key at
/// each row and column.
pub const DTMF_HZ: ([f32; 4], [f32; 4]) = ([697.0, 770.0, 852.0, 941.0], [1209.0, 1336.0, 1477.0, 1633.0]);
pub const KNOX_HZ: ([f32; 4], [f32; 4]) = ([606.0, 672.0, 743.0, 820.0], [1052.0, 1162.0, 1279.0, 1430.0]);
pub const KEYS: [[char; 4]; 4] = [['1', '2', '3', 'A'], ['4', '5', '6', 'B'], ['7', '8', '9', 'C'], ['*', '0', '#', 'D']];

/// A key's two tones on `grid` (DTMF_HZ or KNOX_HZ), Hz, low first.
pub fn key_hz(grid: &([f32; 4], [f32; 4]), key: char) -> Option<(f32, f32)> {
    (0..4).flat_map(|r| (0..4).map(move |c| (r, c))).find(|&(r, c)| KEYS[r][c] == key).map(|(r, c)| (grid.0[r], grid.1[c]))
}

/// What a tone is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToneKind {
    /// `0`–`9`, `A`–`D`, `*`, `#`.
    Dtmf(char),
    Knox(char),
    /// Its two frequencies, Hz, lower first (350+440 dial, 440+480 ringback, 480+620 busy).
    CallProgress(u16, u16),
    /// Hz.
    Single(f32),
}

/// Tone IDs 128–143 (DTMF) and 144–159 (KNOX): their digits, in ID order.
const DIGITS: [char; 16] = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', '*', '#'];

impl ToneKind {
    /// A tone frame's tone (TIA-102.BABA-A Table 19). None: a reserved ID,
    /// or 255 (no tone: zero amplitude).
    pub fn from_tone_id(id: u8) -> Option<ToneKind> {
        Some(match id {
            5..=122 => ToneKind::Single(31.25 * id as f32),
            128..=143 => ToneKind::Dtmf(DIGITS[id as usize - 128]),
            144..=159 => ToneKind::Knox(DIGITS[id as usize - 144]),
            160 => ToneKind::CallProgress(350, 440),
            161 => ToneKind::CallProgress(440, 480),
            162 => ToneKind::CallProgress(480, 620),
            163 => ToneKind::CallProgress(350, 490),
            _ => return None,
        })
    }

    /// The call JSON's `type`.
    pub fn name(&self) -> &'static str {
        match self {
            ToneKind::Dtmf(_) => "dtmf",
            ToneKind::Knox(_) => "knox",
            ToneKind::CallProgress(..) => "call_progress",
            ToneKind::Single(_) => "tone",
        }
    }
}

/// The call JSON's `value`: `5`, `350+440`, `343.75`.
impl fmt::Display for ToneKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ToneKind::Dtmf(c) | ToneKind::Knox(c) => write!(f, "{c}"),
            ToneKind::CallProgress(a, b) => write!(f, "{a}+{b}"),
            ToneKind::Single(hz) => write!(f, "{}", (hz * 100.0).round() / 100.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Named by the vocoder's tone frames.
    Frame,
    /// Found in the audio.
    Audio,
}

impl Origin {
    pub fn name(self) -> &'static str {
        match self {
            Origin::Frame => "frame",
            Origin::Audio => "audio",
        }
    }
}

/// One tone, unbroken, in a call's audio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneSeg {
    pub kind: ToneKind,
    /// Where it starts in the call's audio, and how long it lasts: samples.
    pub start: usize,
    pub len: usize,
    /// Tone frames: the loudest level they named (AD, 0–127).
    pub level: Option<u8>,
    pub origin: Origin,
}

/// A tone frame's level AD as dBm0: 127 is +3.17 dBm0, 0.711 dB a step
/// (TIA-102.BABA-A §16.2).
pub fn level_dbm0(ad: u8) -> f64 {
    3.17 - 0.711 * (127 - ad.min(127)) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_ids_name_table_19() {
        assert_eq!(ToneKind::from_tone_id(128), Some(ToneKind::Dtmf('0')));
        assert_eq!(ToneKind::from_tone_id(143), Some(ToneKind::Dtmf('#')));
        assert_eq!(ToneKind::from_tone_id(138), Some(ToneKind::Dtmf('A')));
        assert_eq!(ToneKind::from_tone_id(145), Some(ToneKind::Knox('1')));
        assert_eq!(ToneKind::from_tone_id(162), Some(ToneKind::CallProgress(480, 620)));
        assert_eq!(ToneKind::from_tone_id(5), Some(ToneKind::Single(156.25)));
        assert_eq!(ToneKind::from_tone_id(11).map(|k| k.to_string()), Some("343.75".into()));
        for id in [0, 4, 123, 127, 164, 254, 255] {
            assert_eq!(ToneKind::from_tone_id(id), None, "{id}");
        }
    }

    /// Each dual tone's Annex T harmonics land near Table 19's frequencies:
    /// two harmonics of one fundamental only come close (KNOX's 1279 Hz
    /// comes out 1286–1299 Hz, ringback 425 + 496 Hz), so Table 19 names a
    /// tone and the vocoder's playback of it is that far off.
    #[test]
    fn dual_tones_match_annex_t() {
        for id in 128..=163u8 {
            let (f0, a, b) = crate::mbe::annex_t(id).unwrap();
            let want = match ToneKind::from_tone_id(id).unwrap() {
                ToneKind::Dtmf(c) => key_hz(&DTMF_HZ, c).map(|(a, b)| (a as f64, b as f64)).unwrap(),
                ToneKind::Knox(c) => key_hz(&KNOX_HZ, c).map(|(a, b)| (a as f64, b as f64)).unwrap(),
                ToneKind::CallProgress(lo, hi) => (lo as f64, hi as f64),
                ToneKind::Single(_) => unreachable!(),
            };
            let got = (f0 * a as f64, f0 * b as f64);
            let near = |g: f64, w: f64| (g / w - 1.0).abs() < 0.035;
            assert!(near(got.0, want.0) && near(got.1, want.1), "{id}: {got:?} vs {want:?}");
        }
    }

    #[test]
    fn levels_span_the_standards_range() {
        assert!((level_dbm0(127) - 3.17).abs() < 1e-9);
        assert!((level_dbm0(0) + 87.13).abs() < 0.01);
    }
}
