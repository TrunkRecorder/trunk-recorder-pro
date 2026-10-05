//! NXDN (NXDN Forum TS 1-A … 1-F): conventional, Type-C trunking (a
//! control channel) and Type-D (IDAS distributed trunking).
//!
//! ```text
//! channel IQ → dsp::C4fm (4FSK, 2400 or 4800 baud) → frame::Framer (FSW, 192-symbol frames)
//!   → descramble, LICH → channel (Viterbi, CRC: SACCH, FACCH1, UDCH, CAC)
//!   → layer3 messages (calls, grants, site information)
//!   → voice: AMBE+2 codewords (crate::ambe, as DMR) → mbe
//! ```

pub mod channel;
pub mod fec;
pub mod frame;
pub mod layer3;

use crate::dsp::c4fm::{C4fm, C4fmOptions};

/// The two NXDN rates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rate {
    /// 4800 bps, 2400 baud, 6.25 kHz channels (±1050 Hz outer deviation).
    N48,
    /// 9600 bps, 4800 baud, 12.5 kHz channels (±2400 Hz).
    N96,
}

impl Rate {
    pub fn baud(self) -> f64 {
        match self {
            Rate::N48 => 2400.0,
            Rate::N96 => 4800.0,
        }
    }
    /// One-sided channel filter cutoff, Hz.
    pub fn cutoff_hz(self) -> f64 {
        match self {
            Rate::N48 => 3400.0,
            Rate::N96 => 6250.0,
        }
    }
    /// Seconds per frame.
    pub fn frame_s(self) -> f64 {
        frame::FRAME_SYMBOLS as f64 / self.baud()
    }
    pub fn name(self) -> &'static str {
        match self {
            Rate::N48 => "nxdn48",
            Rate::N96 => "nxdn96",
        }
    }
    pub fn parse(s: &str) -> Option<Rate> {
        match s.to_ascii_lowercase().as_str() {
            "nxdn48" | "48" | "4800" => Some(Rate::N48),
            "nxdn96" | "96" | "9600" => Some(Rate::N96),
            _ => None,
        }
    }
    /// The 4FSK receiver for this rate.
    pub fn receiver(self, rate: f64) -> C4fm {
        C4fm::with_options(rate, C4fmOptions::nxdn(self.baud()))
    }
}
