//! Signal processing: the shared channelizer, the P25 symbol receivers and
//! analog FM and AM.

pub mod am;
pub mod c4fm;
pub mod channelizer;
pub mod cqpsk;
pub mod filters;
pub mod fm;
pub mod inband;
pub mod levels;
pub mod msd;
pub mod real_iq;
pub mod rolloff;
pub mod squelch;

pub use channelizer::{Channelizer, HeadId};
pub use squelch::SquelchTiming;

/// How an analog channel is demodulated (a conventional system and its
/// channels choose; trunked analog calls take [`AnalogTuning::default`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalogTuning {
    /// One-sided passband of the channel filter / carrier meter, Hz.
    pub half_bw_hz: f64,
    /// FM: de-emphasis time constant, s (0: none).
    pub deemph_tau_s: f64,
    /// AM: how fast the audio level follows the carrier, s.
    pub level_tau_s: f64,
    /// AM: the audio high-pass's corner, Hz (0: none).
    pub low_cut_hz: f64,
    pub squelch: SquelchTiming,
    /// FM: leave out data bursts (EDACS's 9600 baud signalling at the start
    /// of a transmission, on the voice channel): [`fm::DataBurst`].
    pub data_mute: bool,
}

impl Default for AnalogTuning {
    /// Narrowband FM (12.5 kHz), 750 µs de-emphasis.
    fn default() -> Self {
        AnalogTuning { half_bw_hz: fm::CHANNEL_HALF_BW, deemph_tau_s: fm::DEEMPH_TAU, level_tau_s: am::LEVEL_FAST_S, low_cut_hz: am::LOW_CUT_HZ, squelch: SquelchTiming::default(), data_mute: false }
    }
}

impl AnalogTuning {
    /// AM at `freq_hz`: a 25 kHz channel's filter on VHF airband; wider from
    /// 225 MHz (UHF military air), where transmitters stray further.
    pub fn am(freq_hz: f64) -> Self {
        AnalogTuning { half_bw_hz: am::default_half_bw(freq_hz), ..Default::default() }
    }
}

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
