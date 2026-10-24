//! In-band tones: DTMF and KNOX digits, call progress tones, and the single
//! tones paging is made of, sent in a call's voice. (Sub-audible tones,
//! CTCSS and DCS, are [`super::tones`].)
//!
//! A half-rate (AMBE+2) radio whose vocoder recognises a tone sends a tone
//! frame naming it (TIA-102.BABA-A §16), read exactly from the frame's bits
//! ([`ToneKind::from_tone_id`]). Full rate (IMBE) has no tone frames, and
//! not every half-rate vocoder looks for tones; analog has no frames at all.

use std::fmt;

pub mod audio;
pub mod pattern;
pub mod signal;
pub mod tables;

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
                ToneKind::Dtmf(c) | ToneKind::Knox(c) => {
                    let k = DIGITS.iter().position(|&d| d == c).unwrap();
                    let (lo, hi) = if id < 144 {
                        ([941, 697, 697, 697, 770, 770, 770, 852, 852, 852, 697, 770, 852, 941, 941, 941], [1336, 1209, 1336, 1477, 1209, 1336, 1477, 1209, 1336, 1477, 1633, 1633, 1633, 1633, 1209, 1477])
                    } else {
                        ([820, 606, 606, 606, 672, 672, 672, 743, 743, 743, 606, 672, 743, 820, 820, 820], [1162, 1052, 1162, 1279, 1052, 1162, 1279, 1052, 1162, 1279, 1430, 1430, 1430, 1430, 1052, 1279])
                    };
                    (lo[k] as f64, hi[k] as f64)
                }
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
