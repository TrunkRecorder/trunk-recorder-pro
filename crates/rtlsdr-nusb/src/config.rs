//! Validated configuration, independent of USB access.

use crate::{Error, Result};
use std::marker::PhantomData;

/// Representation delivered to the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleFormat {
    /// Interleaved unsigned eight-bit I/Q pairs, as delivered by USB.
    RawIq,
    /// Complex floats, with each component mapped from `0..=255` to `-1..=1`.
    F32Iq,
}

mod private {
    pub trait Sealed {
        type Processor: crate::rx::processing::Processor;
    }
}
/// Sealed compile-time sample format used by devices and streams.
pub trait SampleMode: private::Sealed + Copy + core::fmt::Debug + Eq + 'static {
    /// Runtime equivalent of this mode.
    const FORMAT: SampleFormat;
}
/// Raw unsigned eight-bit complex samples.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RawIq;
/// Converted complex `f32` samples (the default).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct F32Iq;
impl private::Sealed for RawIq {
    type Processor = crate::rx::processing::RawProcessor;
}
impl private::Sealed for F32Iq {
    type Processor = crate::rx::processing::IqProcessor;
}
impl SampleMode for RawIq {
    const FORMAT: SampleFormat = SampleFormat::RawIq;
}
impl SampleMode for F32Iq {
    const FORMAT: SampleFormat = SampleFormat::F32Iq;
}

/// Tuner gain policy. Manual gain is approximate, using the reference driver's
/// measured 2.3 dB LNA and 1.2 dB mixer steps with a fixed VGA setting.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum GainConfig {
    /// Let the tuner control LNA and mixer gain automatically.
    #[default]
    Auto,
    /// Requested LNA + mixer gain in dB, in `0.0..=52.5`.
    Manual(f32),
}

/// Direct ADC input for frequencies below 28.8 MHz on non-V4 dongles.
/// Requires appropriate board wiring. Blog V4 uses its own upconverter instead.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DirectSampling {
    /// Use the RF tuner; reject frequencies below its supported range.
    #[default]
    Off,
    /// Use ADC I below 28.8 MHz, automatically returning to the tuner above it.
    I,
    /// Use ADC Q below 28.8 MHz, automatically returning to the tuner above it.
    Q,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub frequency_hz: u64,
    pub sample_rate_hz: u32,
    pub gain: GainConfig,
    pub correction_ppm: i32,
    pub bias_tee: bool,
    pub direct_sampling: DirectSampling,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            frequency_hz: 100_000_000,
            sample_rate_hz: 2_048_000,
            gain: GainConfig::Auto,
            correction_ppm: 0,
            bias_tee: false,
            direct_sampling: DirectSampling::Off,
        }
    }
}
impl Settings {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=1_766_000_000).contains(&self.frequency_hz) {
            return Err(Error::invalid_config(
                "frequency_hz",
                "must be in 1..=1766000000; the actual minimum depends on the tuner and direct sampling",
            ));
        }
        if !(900_001..=3_200_000).contains(&self.sample_rate_hz) {
            return Err(Error::invalid_config(
                "sample_rate_hz",
                "must be in 900001..=3200000 samples/s",
            ));
        }
        if let GainConfig::Manual(db) = self.gain
            && (!db.is_finite() || !(0.0..=52.5).contains(&db))
        {
            return Err(Error::invalid_config(
                "gain",
                "manual gain must be finite and in 0..=52.5 dB",
            ));
        }
        if !(-488..=488).contains(&self.correction_ppm) {
            return Err(Error::invalid_config(
                "correction_ppm",
                "must be in -488..=488 for the signed 14-bit correction register",
            ));
        }
        Ok(())
    }
}

/// Complete, statically validated receiver configuration.
///
/// Device-dependent frequency validation happens during open/configuration.
/// The first release supports sample rates in `900001..=3200000` samples/s;
/// rates above 2.4 MS/s may lose samples depending on the host and receiver.
#[derive(Clone, Debug, PartialEq)]
pub struct Config<M: SampleMode = F32Iq> {
    pub(crate) settings: Settings,
    mode: PhantomData<fn() -> M>,
}
impl<M: SampleMode> Default for Config<M> {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            mode: PhantomData,
        }
    }
}
impl Config<F32Iq> {
    /// Start building a configuration without touching hardware.
    pub fn builder() -> ConfigBuilder<F32Iq> {
        ConfigBuilder::default()
    }
}
impl<M: SampleMode> Config<M> {
    /// Requested center frequency in Hz.
    pub fn frequency_hz(&self) -> u64 {
        self.settings.frequency_hz
    }
    /// Requested complex sample rate in samples/s.
    pub fn sample_rate_hz(&self) -> u32 {
        self.settings.sample_rate_hz
    }
    /// Selected sample representation.
    pub fn sample_format(&self) -> SampleFormat {
        M::FORMAT
    }
    /// Gain policy.
    pub fn gain(&self) -> GainConfig {
        self.settings.gain
    }
    /// Crystal correction in parts per million.
    pub fn correction_ppm(&self) -> i32 {
        self.settings.correction_ppm
    }
    /// Whether RF bias power is requested.
    pub fn bias_tee(&self) -> bool {
        self.settings.bias_tee
    }
    /// Low-frequency direct sampling policy.
    pub fn direct_sampling(&self) -> DirectSampling {
        self.settings.direct_sampling
    }
}

/// Builder for reusable receiver configurations.
#[derive(Clone, Debug)]
pub struct ConfigBuilder<M: SampleMode = F32Iq> {
    config: Config<M>,
}
impl Default for ConfigBuilder<F32Iq> {
    fn default() -> Self {
        Self {
            config: Config::default(),
        }
    }
}
impl<M: SampleMode> ConfigBuilder<M> {
    /// Set center frequency in Hz (hardware-specific limits are checked on open).
    pub fn frequency_hz(mut self, value: u64) -> Self {
        self.config.settings.frequency_hz = value;
        self
    }
    /// Set the sample rate in `900001..=3200000` complex samples/s.
    pub fn sample_rate_hz(mut self, value: u32) -> Self {
        self.config.settings.sample_rate_hz = value;
        self
    }
    /// Set tuner gain policy.
    pub fn gain(mut self, value: GainConfig) -> Self {
        self.config.settings.gain = value;
        self
    }
    /// Set crystal correction in parts per million.
    pub fn correction_ppm(mut self, value: i32) -> Self {
        self.config.settings.correction_ppm = value;
        self
    }
    /// Enable or disable GPIO 0 RF bias power (off by default).
    pub fn bias_tee(mut self, value: bool) -> Self {
        self.config.settings.bias_tee = value;
        self
    }
    /// Select automatic low-frequency direct sampling behavior.
    pub fn direct_sampling(mut self, value: DirectSampling) -> Self {
        self.config.settings.direct_sampling = value;
        self
    }
    /// Validate and build the configuration.
    pub fn build(self) -> Result<Config<M>> {
        self.config.settings.validate()?;
        Ok(self.config)
    }
}
impl ConfigBuilder<F32Iq> {
    /// Deliver raw unsigned eight-bit I/Q blocks instead of complex floats.
    pub fn raw_iq(self) -> ConfigBuilder<RawIq> {
        ConfigBuilder {
            config: Config {
                settings: self.config.settings,
                mode: PhantomData,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_settings_are_rejected_before_open() {
        for hz in [0, 1_766_000_001, u64::MAX] {
            assert!(Config::builder().frequency_hz(hz).build().is_err());
        }
        for rate in [0, 900_000, 3_200_001, u32::MAX] {
            assert!(Config::builder().sample_rate_hz(rate).build().is_err());
        }
        for gain in [f32::NAN, f32::INFINITY, -0.1, 52.6] {
            assert!(
                Config::builder()
                    .gain(GainConfig::Manual(gain))
                    .build()
                    .is_err()
            );
        }
        for ppm in [-489, 489, i32::MIN, i32::MAX] {
            assert!(Config::builder().correction_ppm(ppm).build().is_err());
        }
    }
    #[test]
    fn sample_rate_limits_are_independent_of_ppm() {
        for ppm in [-488, -100, 0, 100, 488] {
            for (rate, accepted) in [
                (900_000, false),
                (900_001, true),
                (3_200_000, true),
                (3_200_001, false),
            ] {
                assert_eq!(
                    Config::builder()
                        .sample_rate_hz(rate)
                        .correction_ppm(ppm)
                        .build()
                        .is_ok(),
                    accepted
                );
            }
        }
    }
}
