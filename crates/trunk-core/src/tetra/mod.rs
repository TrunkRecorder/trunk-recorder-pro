//! TETRA trunked mode, downlink (ETSI EN 300 392-2): the control channel, and voice.
//!
//! ```text
//! channel IQ → dsp::Cqpsk (π/4-DQPSK, 18 000 baud) → burst::Framer (255-symbol slots)
//!   → fec (descramble, de-interleave, Viterbi, CRC; the AACH's Reed-Muller code)
//!   → mac (SYNC, SYSINFO, MAC-RESOURCE, fragments) → cmce (calls; mm, mle) → cell::Event
//!   → site (calls followed) → trunk::Message
//!   traffic slots → speech (the codec's frames) → acelp (audio; its tables supplied) → voice
//! ```
//!
//! The layouts are the standard's, checked against dsd-neo's decoder (GPL-3.0-or-later,
//! arancormonk and contributors) and the vectors its tests carry.

pub mod acelp;
pub mod burst;
pub mod cell;
pub mod cmce;
pub mod fec;
pub mod mac;
pub mod mle;
pub mod mm;
pub mod speech;
pub mod synth;

pub use mac::CellTime;

use crate::dsp::cqpsk::{Cqpsk, Options};

pub const BAUD: f64 = 18_000.0;
/// One-sided channel filter cutoff, Hz: 25 kHz channels, occupied to 18 000 × 1.35 / 2 = 12 150.
pub const CUTOFF_HZ: f64 = 13_000.0;
/// The lowest channel rate to receive at: three samples a symbol.
pub const MIN_RATE: f64 = 54_000.0;

/// The receiver for a channel sampled at `rate`. The framer finds the syncs, and tells it ([`Cqpsk::synced`]).
pub fn receiver(rate: f64) -> Cqpsk {
    Cqpsk::new(rate, Options { baud: BAUD, external_sync: true, ..Default::default() })
}
