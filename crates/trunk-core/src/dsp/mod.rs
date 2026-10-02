//! Signal processing: the shared channelizer, the P25 symbol receivers and
//! narrowband FM.

pub mod c4fm;
pub mod channelizer;
pub mod cqpsk;
pub mod fm;
pub mod msd;
pub mod signalling;
pub mod tones;

pub use channelizer::{Channelizer, HeadId};

/// One decided symbol from a receiver: the dibit, the channel-sample instant
/// it was sampled at (common to every receiver on the channel, so frames from
/// different receivers can be matched), and each bit's reliability (distance
/// from its decision boundary, ≥ 0; high bit first).
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
}
