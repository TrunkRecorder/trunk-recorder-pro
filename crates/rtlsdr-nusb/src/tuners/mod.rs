//! Tuner control: Rafael Micro R820T/R828D in [`r82xx`], Elonics E4000 in
//! [`e4k`], their registers through [`i2c::Registers`]. The caller
//! serializes the entire I2C operation.

mod e4k;
mod i2c;
mod r82xx;
mod registers;
use crate::{Error, GainConfig, Result, protocol::Com, usb::ControlBackend};
use e4k::E4000;
use r82xx::R82xx;

pub(crate) const XTAL_HZ: u64 = 28_800_000;

/// Detected tuner family. R820T-compatible revisions share the same chip ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TunerKind {
    /// R820T/R820T2/R860-compatible tuner at I2C address 0x34.
    R820T,
    /// R828D tuner at I2C address 0x74.
    R828D,
    /// Elonics E4000 tuner at I2C address 0xC8 (zero IF; roughly 52-2200 MHz
    /// with a gap near 1100-1250 MHz where its PLL does not lock).
    E4000,
}

/// Each tuner's I2C address, the register that identifies it and what it
/// holds there, in the order probed: the R82xx first, so their dongles see
/// no extra NAKed request.
const PROBES: [(TunerKind, u8, u8, u8); 3] = [(TunerKind::R820T, 0x34, 0, 0x69), (TunerKind::R828D, 0x74, 0, 0x69), (TunerKind::E4000, 0xc8, 2, 0x40)];

impl TunerKind {
    /// The gain a manual `db` is set to: the tuner's nearest step, dB.
    pub fn gain_db(self, db: f32) -> f32 {
        let tenths = match self {
            TunerKind::R820T | TunerKind::R828D => r82xx::nearest_gain(db),
            TunerKind::E4000 => e4k::nearest_gain(db).0,
        };
        tenths as f32 / 10.0
    }

    /// Its 8-bit I2C address.
    pub(crate) fn address(self) -> u8 {
        PROBES.iter().find(|p| p.0 == self).expect("every kind is probed").1
    }
}

/// What a tuner hands the demodulator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Output {
    /// A real IF at this frequency (the R82xx: 3.57 MHz).
    LowIf(u64),
    /// I and Q at baseband (the E4000).
    ZeroIf,
}

/// The dongle's tuner.
#[derive(Debug)]
pub(crate) enum Tuner {
    R82xx(R82xx),
    E4000(E4000),
}

/// `$body` on whichever tuner it is, named `$t`.
macro_rules! each {
    ($self:expr, $t:ident => $body:expr) => {
        match $self {
            Tuner::R82xx($t) => $body,
            Tuner::E4000($t) => $body,
        }
    };
}

impl Tuner {
    /// Which tuner answers. A tuner that isn't there NAKs, which the RTL2832U
    /// reports as a STALL; Chrome's WebUSB on Windows reports that STALL as a
    /// NetworkError, which nusb maps to Disconnected: also taken as "not
    /// here" while probing. Only if no tuner answers at all is a disconnect
    /// the answer.
    pub async fn detect<C: ControlBackend>(com: &Com<'_, C>) -> Result<TunerKind> {
        let mut disconnected = None;
        for (kind, address, reg, id) in PROBES {
            match com.i2c_read(address, reg, 1).await {
                Ok(data) if data[0] == id => return Ok(kind),
                Ok(_) | Err(Error::Transfer(nusb::transfer::TransferError::Stall)) => {}
                Err(error @ Error::Transfer(nusb::transfer::TransferError::Disconnected)) => disconnected = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(disconnected.unwrap_or(Error::UnsupportedTuner))
    }
    pub fn new(kind: TunerKind, blog_v4: bool) -> Self {
        match kind {
            TunerKind::E4000 => Tuner::E4000(E4000::new()),
            _ => Tuner::R82xx(R82xx::new(kind, blog_v4)),
        }
    }
    pub fn kind(&self) -> TunerKind {
        match self {
            Tuner::R82xx(t) => t.kind,
            Tuner::E4000(_) => TunerKind::E4000,
        }
    }
    /// The reference its PLL runs from: the RTL2832U's crystal, ppm-corrected.
    #[cfg(test)]
    pub fn crystal_hz(&self) -> u64 {
        each!(self, t => t.crystal_hz)
    }
    pub fn set_crystal_hz(&mut self, hz: u64) {
        each!(self, t => t.crystal_hz = hz)
    }
    pub fn output(&self) -> Output {
        match self {
            Tuner::R82xx(_) => Output::LowIf(r82xx::IF_HZ),
            Tuner::E4000(_) => Output::ZeroIf,
        }
    }
    pub async fn open<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<()> {
        each!(self, t => t.open(com).await)
    }
    pub async fn standby<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<()> {
        each!(self, t => t.standby(com).await)
    }
    pub async fn set_gain<C: ControlBackend>(&mut self, com: &Com<'_, C>, gain: GainConfig) -> Result<()> {
        each!(self, t => t.set_gain(com, gain).await)
    }
    /// Tune to `hz`: the frequency the samples are centred on now.
    pub async fn tune<C: ControlBackend>(&mut self, com: &Com<'_, C>, hz: u64) -> Result<u64> {
        each!(self, t => t.tune(com, hz).await)
    }
    /// Its IF filters for `hz` of bandwidth (the sample rate). The E4000's
    /// are driven, as librtlsdr does; the R82xx keep theirs.
    pub async fn set_bandwidth<C: ControlBackend>(&mut self, com: &Com<'_, C>, hz: u64) -> Result<()> {
        match self {
            Tuner::R82xx(_) => Ok(()),
            Tuner::E4000(t) => t.set_bandwidth(com, hz).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeTransport;
    use std::sync::atomic::Ordering;
    #[test]
    fn every_tuner_is_found_and_missing_tuners_are_rejected() {
        let control = FakeTransport::default();
        for kind in [TunerKind::R820T, TunerKind::R828D, TunerKind::E4000] {
            control.state.tuner_address.store(kind.address(), Ordering::SeqCst);
            assert_eq!(futures_lite::future::block_on(Tuner::detect(&Com(&control))).unwrap(), kind);
        }
        control.state.tuner_address.store(0, Ordering::SeqCst);
        assert!(matches!(futures_lite::future::block_on(Tuner::detect(&Com(&control))), Err(Error::UnsupportedTuner)));
    }
    /// WebUSB on Windows: absent tuners' NAKs arrive as "disconnected". The
    /// tuner is still found behind them; with nothing answering, the
    /// disconnect is the error.
    #[test]
    fn probe_naks_reported_as_disconnects_are_passed_over() {
        let control = FakeTransport::default();
        control.state.disconnect_absent_i2c.store(true, Ordering::SeqCst);
        for kind in [TunerKind::E4000, TunerKind::R828D] {
            control.state.tuner_address.store(kind.address(), Ordering::SeqCst);
            assert_eq!(futures_lite::future::block_on(Tuner::detect(&Com(&control))).unwrap(), kind);
        }
        control.state.tuner_address.store(0, Ordering::SeqCst);
        assert!(matches!(
            futures_lite::future::block_on(Tuner::detect(&Com(&control))),
            Err(Error::Transfer(nusb::transfer::TransferError::Disconnected))
        ));
    }
}
