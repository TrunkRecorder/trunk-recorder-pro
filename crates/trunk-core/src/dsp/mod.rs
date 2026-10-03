//! Signal processing: the shared channelizer, the P25 symbol receivers and
//! narrowband FM.

pub mod c4fm;
pub mod channelizer;
pub mod cqpsk;
pub mod filters;
pub mod fm;
pub mod msd;
pub mod rolloff;
pub mod signalling;
pub mod tones;

pub use channelizer::{Channelizer, HeadId};

/// One decided symbol from a receiver: the dibit, the channel-sample instant
/// it was sampled at (common to every receiver on the channel, so frames from
/// different receivers can be matched), and each bit's reliability (distance
/// from its decision boundary, ≥ 0; high bit first). A negative reliability
/// means none: soft decoding is off (the P25 framer then leaves
/// [`crate::p25::Frame::soft`] empty).
#[derive(Clone, Copy, Debug, Default)]
pub struct Symbol {
    pub dibit: u8,
    pub sample: f64,
    pub rel_hi: f32,
    pub rel_lo: f32,
}

/// A symbol receiver: channel IQ in, decided symbols out.
pub trait Receiver {
    fn push(&mut self, iq: &[crate::Complex32], out: &mut Vec<Symbol>);
    /// How far above the channel's centre the carrier is, Hz, when the
    /// receiver can tell (None while it hasn't locked onto a clean signal).
    fn offset_hz(&self) -> Option<f32> {
        None
    }
    /// How cleanly the symbols come out, when the receiver can tell: level
    /// step over spread (~10 and up clean, ~1 noise). For the dashboard.
    fn quality(&self) -> Option<f32> {
        None
    }
}
