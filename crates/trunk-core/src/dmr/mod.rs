//! DMR (ETSI TS 102 361): Tier II conventional and the trunked variants
//! built on it. Both slots of a carrier come out of one receiver:
//!
//! ```text
//! channel IQ → dsp::C4fm (4800 baud 4FSK, as P25) → burst::Framer (sync, 30 ms grid)
//!   → slot::Channel (CACH names the slot) → SlotDecoder × 2
//!       → voice: AMBE+2 codewords (the P25 Phase 2 FEC and vocoder)
//!       → link control (talkgroup, source, privacy), CSBKs
//! ```

pub mod burst;
pub mod fec;
pub mod slot;
#[cfg(test)]
pub mod synth;
pub mod trunking;
pub mod voice;

pub use burst::{Burst, Framer, SyncKind};
pub use slot::{Channel, Csbk, Lc, LcFrom, SlotDecoder, SlotEvent};
pub use trunking::{DmrConfig, Site, Variant};
pub use voice::DmrVoice;

/// One-sided channel filter cutoff, Hz (12.5 kHz channel, ±1.944 kHz deviation).
pub const CHANNEL_CUTOFF_HZ: f64 = 6250.0;
