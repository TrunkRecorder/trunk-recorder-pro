//! What a source's samples go into: a [`Channelizer`] (a radio's IQ, which
//! every kind of channel is cut from) or an [`AudioIn`] (a stream's audio,
//! demodulated already, which only conventional channels with no frequency
//! hear). The engine's sources and the conventional channels both hold this
//! one type; what is the same of the two (time, blocks, the noise floor) is
//! asked here.

use super::audio_in::{AudioIn, Stereo};
use super::channelizer::{floor_slices, Channelizer};

/// How many slices the noise floor of a source is taken in: a radio's band's
/// at `fs` ([`floor_slices`]); a stream's (`audio`) mix, left and right.
pub fn slices(audio: bool, fs: f64) -> usize {
    if audio { Stereo::ALL.len() } else { floor_slices(fs) }
}

pub enum Front {
    Air(Channelizer),
    Audio(AudioIn),
}

impl Front {
    /// Samples in per second, Hz.
    pub fn fs(&self) -> f64 {
        match self {
            Front::Air(c) => c.fs(),
            Front::Audio(a) => a.fs(),
        }
    }

    /// Samples taken so far: the next block's start.
    pub fn sample_position(&self) -> u64 {
        match self {
            Front::Air(c) => c.sample_position(),
            Front::Audio(a) => a.sample_position(),
        }
    }

    /// Seconds one block is.
    pub fn block_seconds(&self) -> f64 {
        match self {
            Front::Air(c) => c.block_seconds(),
            Front::Audio(a) => a.block_seconds(),
        }
    }

    /// A channel's sample rate, Hz.
    pub fn output_rate(&self) -> f64 {
        match self {
            Front::Air(c) => c.output_rate(),
            Front::Audio(a) => a.fs(),
        }
    }

    /// How many slices its noise floor is taken in ([`slices`]).
    pub fn floor_slices(&self) -> usize {
        slices(matches!(self, Front::Audio(_)), self.fs())
    }

    /// The power a channel's head (or a stream's audio) carries of noise of
    /// `floor` per slice, over `bandwidth_hz` ([`Channelizer::noise_in_band`]):
    /// a stream's floor is its audio's power already.
    pub fn noise_in_band(&self, floor: f64, bandwidth_hz: f64) -> f64 {
        match self {
            Front::Air(c) => c.noise_in_band(floor, bandwidth_hz),
            Front::Audio(_) => floor,
        }
    }

    /// The power `db` above it: what a squelch or carrier meter compares against ([`Channelizer::threshold`]).
    pub fn threshold(&self, floor: f64, bandwidth_hz: f64, db: f64) -> f32 {
        (self.noise_in_band(floor, bandwidth_hz) * 10f64.powf(db / 10.0)) as f32
    }

    /// Its noise floor, slice by slice ([`Channelizer::noise_profile`], [`AudioIn::noise_profile`]).
    pub fn noise_profile(&self, out: &mut [f64]) {
        match self {
            Front::Air(c) => c.noise_profile(out),
            Front::Audio(a) => a.noise_profile(out),
        }
    }

    /// A radio's channelizer (None: a stream).
    pub fn air(&self) -> Option<&Channelizer> {
        match self {
            Front::Air(c) => Some(c),
            Front::Audio(_) => None,
        }
    }

    pub fn air_mut(&mut self) -> Option<&mut Channelizer> {
        match self {
            Front::Air(c) => Some(c),
            Front::Audio(_) => None,
        }
    }
}
