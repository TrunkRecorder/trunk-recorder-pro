//! Demodulator initialization and complete hardware configuration.

use crate::{
    DeviceDescriptor, DirectSampling, Error, GainConfig, Result,
    config::Settings,
    protocol::Com,
    tuners::{IF_HZ, Tuner, TunerKind, XTAL_HZ},
    usb::ControlBackend,
};

const DEMOD_INIT: &[(u8, u8, u16, usize)] = &[
    (1, 1, 20, 1),
    (1, 1, 16, 1),
    (1, 21, 0, 1),
    (1, 22, 0, 1),
    (1, 23, 0, 1),
    (1, 24, 0, 1),
    (1, 25, 0, 1),
    (1, 26, 0, 1),
    (1, 27, 0, 1),
    (1, 28, 202, 1),
    (1, 29, 220, 1),
    (1, 30, 215, 1),
    (1, 31, 216, 1),
    (1, 32, 224, 1),
    (1, 33, 242, 1),
    (1, 34, 14, 1),
    (1, 35, 53, 1),
    (1, 36, 6, 1),
    (1, 37, 80, 1),
    (1, 38, 156, 1),
    (1, 39, 13, 1),
    (1, 40, 113, 1),
    (1, 41, 17, 1),
    (1, 42, 20, 1),
    (1, 43, 113, 1),
    (1, 44, 116, 1),
    (1, 45, 25, 1),
    (1, 46, 65, 1),
    (1, 47, 165, 1),
    (0, 25, 5, 1),
    (1, 147, 240, 1),
    (1, 148, 15, 1),
    (1, 17, 0, 1),
    (1, 4, 0, 1),
    (0, 97, 96, 1),
    (0, 6, 128, 1),
    (1, 177, 27, 1),
    (0, 13, 131, 1),
];

/// Values quantized to the receiver's integer dividers.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Applied {
    pub frequency_hz: u64,
    pub sample_rate_hz: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Hardware {
    pub tuner: Option<Tuner>,
    pub valid: bool,
    pub receiving: bool,
    direct: bool,
}

impl Hardware {
    pub(crate) fn validate_device(
        &self,
        config: &Settings,
        descriptor: &DeviceDescriptor,
    ) -> Result<()> {
        config.validate()?;
        if config.frequency_hz < XTAL_HZ
            && config.direct_sampling == DirectSampling::Off
            && !descriptor.blog_v4()
        {
            return Err(Error::invalid_config(
                "frequency_hz",
                "below 28.8 MHz requires Blog V4 or direct sampling",
            ));
        }
        if descriptor.blog_v4() && config.direct_sampling != DirectSampling::Off {
            return Err(Error::invalid_config(
                "direct_sampling",
                "Blog V4 uses its upconverter; select Off",
            ));
        }
        Ok(())
    }
    pub(crate) async fn configure<C: ControlBackend>(
        &mut self,
        control: &C,
        config: &Settings,
        descriptor: &DeviceDescriptor,
    ) -> Result<Applied> {
        self.validate_device(config, descriptor)?;
        let was_valid = std::mem::replace(&mut self.valid, false);
        let com = Com(control);
        let result = self
            .configure_inner(&com, config, descriptor, !was_valid)
            .await;
        // Always close the repeater on returned errors. Cancellation leaves the
        // hardware invalid; the next complete configuration reinitializes it.
        let close = com.i2c_gate(false).await;
        let result = result.and_then(|applied| close.map(|()| applied));
        self.valid = result.is_ok();
        result
    }
    async fn configure_inner<C: ControlBackend>(
        &mut self,
        com: &Com<'_, C>,
        config: &Settings,
        descriptor: &DeviceDescriptor,
        reinitialize: bool,
    ) -> Result<Applied> {
        if reinitialize {
            com.usb_u8(0x2000, 0x09).await?;
            com.usb_u16(0x2158, 0x0200).await?;
            com.receiver(false).await?;
            com.sys_write(0x300b, 0x22).await?;
            com.sys_write(0x3000, 0xe8).await?;
            for &(page, reg, value, width) in DEMOD_INIT {
                com.demod(page, reg, value, width).await?;
            }
            com.i2c_gate(true).await?;
            let kind = Tuner::detect(com).await?;
            if descriptor.blog_v4() && kind != TunerKind::R828D {
                return Err(Error::UnsupportedTuner);
            }
            self.tuner = Some(Tuner::new(kind, descriptor.blog_v4()));
            self.direct = false;
        }
        let crystal = corrected_crystal(config.correction_ppm);
        let tuner = self.tuner.as_mut().ok_or(Error::UnsupportedTuner)?;
        tuner.crystal_hz = crystal;
        let direct = config.frequency_hz < XTAL_HZ && config.direct_sampling != DirectSampling::Off;
        com.i2c_gate(true).await?;
        if reinitialize || (self.direct && !direct) {
            tuner.open(com).await?;
        }
        if direct {
            if !self.direct {
                tuner.standby(com).await?;
            }
            com.demod(1, 0xb1, 0x1a, 1).await?;
            // Enable both ADC inputs so the selected I/Q physical path is usable.
            com.demod(0, 0x08, 0xcd, 1).await?;
            com.demod(1, 0x15, 0, 1).await?;
            com.demod(
                0,
                0x06,
                if config.direct_sampling == DirectSampling::I {
                    0x80
                } else {
                    0x90
                },
                1,
            )
            .await?;
            com.demod(
                0,
                0x19,
                if config.gain == GainConfig::Auto {
                    0x25
                } else {
                    5
                },
                1,
            )
            .await?;
        } else {
            com.demod(1, 0xb1, 0x1a, 1).await?;
            com.demod(0, 0x08, 0x4d, 1).await?;
            com.demod(1, 0x15, 1, 1).await?;
            com.demod(0, 0x06, 0x80, 1).await?;
            com.demod(0, 0x19, 5, 1).await?;
            tuner.set_gain(com, config.gain).await?;
        }
        self.direct = direct;
        // Sampling correction is applied by these registers, so the resampler
        // divider below must continue to use the nominal crystal frequency.
        let ppm = correction_word(config.correction_ppm);
        com.demod(1, 0x3e, (ppm >> 8) & 0x3f, 1).await?;
        com.demod(1, 0x3f, ppm & 0xff, 1).await?;
        let (ratio, sample_rate_hz) = sample_rate(config.sample_rate_hz);
        com.demod(1, 0x9f, (ratio >> 16) as u16, 2).await?;
        com.demod(1, 0xa1, ratio as u16, 2).await?;
        // Tuner accesses precede closing/resetting the repeater.
        let frequency_hz = if direct {
            config.frequency_hz
        } else {
            tuner.tune(com, config.frequency_hz).await?
        };
        let if_frequency = if direct { config.frequency_hz } else { IF_HZ };
        let word = if_word(if_frequency, crystal);
        com.demod(1, 0x19, ((word >> 16) & 0x3f) as u16, 1).await?;
        com.demod(1, 0x1a, ((word >> 8) & 0xff) as u16, 1).await?;
        com.demod(1, 0x1b, (word & 0xff) as u16, 1).await?;
        com.demod(1, 1, 0x14, 1).await?;
        com.demod(1, 1, 0x10, 1).await?;
        com.gpio(0, config.bias_tee).await?;
        if reinitialize && self.receiving {
            com.receiver(true).await?;
        }
        Ok(Applied {
            frequency_hz: if direct {
                (((config.frequency_hz << 22) / crystal) * crystal) >> 22
            } else {
                frequency_hz
            },
            sample_rate_hz,
        })
    }
    pub(crate) async fn shutdown<C: ControlBackend>(&mut self, control: &C) -> Result<()> {
        let com = Com(control);
        let mut result = com.receiver(false).await;
        self.receiving = false;
        // Bias cleanup is independent of tuner health.
        result = result.and(com.gpio(0, false).await);
        if let Some(tuner) = &mut self.tuner {
            let gate = com.i2c_gate(true).await;
            if gate.is_ok() {
                result = result.and(tuner.standby(&com).await);
            }
            result = result.and(gate);
        }
        result = result.and(com.i2c_gate(false).await);
        // ADC I/Q and PLL off, demodulator reset released.
        result = result.and(com.sys_write(0x3000, 0x20).await);
        self.valid = false;
        result
    }
}

pub(crate) fn corrected_crystal(ppm: i32) -> u64 {
    (XTAL_HZ as i64 * (1_000_000 + i64::from(ppm)) / 1_000_000) as u64
}
pub(crate) fn correction_word(ppm: i32) -> u16 {
    (-((i64::from(ppm) * (1 << 24)).div_euclid(1_000_000)) & 0x3fff) as u16
}
pub(crate) fn sample_rate(rate: u32) -> (u32, u32) {
    let ratio = ((XTAL_HZ << 22) / u64::from(rate)) as u32 & 0x0ffffffc;
    (ratio, ((XTAL_HZ << 22) / u64::from(ratio)) as u32)
}
fn if_word(hz: u64, crystal: u64) -> u32 {
    (-(((hz << 22) / crystal) as i64) & 0x3fffff) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeTransport;
    #[test]
    fn ppm_changes_correction_registers_without_changing_resampler_divider() {
        let control = FakeTransport::default();
        let descriptor = DeviceDescriptor {
            index: 0,
            vid: 0x0bda,
            pid: 0x2838,
            serial: None,
            manufacturer: None,
            product: None,
        };
        let mut hardware = Hardware::default();
        for ppm in [-488, -100, 0, 100, 488] {
            control.state.control_out_requests.lock().unwrap().clear();
            let config = Settings {
                correction_ppm: ppm,
                ..Settings::default()
            };
            let applied =
                futures_lite::future::block_on(hardware.configure(&control, &config, &descriptor))
                    .unwrap();
            assert_eq!(applied.sample_rate_hz, 2_048_000);
            assert_eq!(
                hardware.tuner.as_ref().unwrap().crystal_hz,
                corrected_crystal(ppm)
            );
            let writes = control.state.control_out_requests.lock().unwrap();
            let register = |reg: u8| {
                writes
                    .iter()
                    .rev()
                    .find(|r| r.index == 0x11 && r.value == (u16::from(reg) << 8 | 0x20))
                    .unwrap()
                    .data
                    .as_slice()
            };
            // Osmocom's nominal 28.8 MHz / 2.048 MS/s divider is 0x03840000.
            assert_eq!(register(0x9f), [0x03, 0x84]);
            assert_eq!(register(0xa1), [0x00, 0x00]);
            let correction = (u16::from(register(0x3e)[0]) << 8) | u16::from(register(0x3f)[0]);
            assert_eq!(correction, correction_word(ppm));
        }
    }
    #[test]
    fn power_clock_precedes_demod_access_and_configuration_closes_i2c_gate() {
        let control = FakeTransport::default();
        let descriptor = DeviceDescriptor {
            index: 0,
            vid: 0x0bda,
            pid: 0x2838,
            serial: None,
            manufacturer: None,
            product: None,
        };
        let mut hardware = Hardware::default();
        let applied = futures_lite::future::block_on(hardware.configure(
            &control,
            &Settings::default(),
            &descriptor,
        ))
        .unwrap();
        assert_eq!(applied.sample_rate_hz, 2_048_000);
        assert!(applied.frequency_hz.abs_diff(100_000_000) < 100);
        let writes = control.state.control_out_requests.lock().unwrap();
        let clock = writes
            .iter()
            .position(|r| r.value == 0x3000 && r.data == [0xe8])
            .unwrap();
        let demod = writes.iter().position(|r| r.index < 0x20).unwrap();
        assert!(clock < demod);
        let last = writes.last().unwrap();
        assert_eq!(
            (last.value, last.index, last.data.as_slice()),
            (0x0120, 0x11, &[0x10][..])
        );
        assert!(hardware.valid);
    }
    #[test]
    fn divider_and_signed_correction_boundaries() {
        for ppm in [-488, -1, 0, 1, 488] {
            let word = correction_word(ppm);
            let signed = ((word << 2) as i16) >> 2;
            assert!((i64::from(signed) * 1_000_000 + i64::from(ppm) * (1 << 24)).abs() < 1_000_000);
            for rate in [900_001, 1_024_000, 2_048_000, 2_400_000, 3_200_000] {
                let (ratio, actual) = sample_rate(rate);
                assert_eq!(ratio & 3, 0);
                assert!(actual >= rate && actual - rate < 2);
            }
        }
    }
}
