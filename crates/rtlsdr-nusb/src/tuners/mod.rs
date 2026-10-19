//! Rafael Micro tuner control. The caller serializes the entire I2C operation.

mod registers;
use crate::{Error, GainConfig, Result, protocol::Com, usb::ControlBackend};
use registers::*;

pub(crate) const IF_HZ: u64 = 3_570_000;
pub(crate) const XTAL_HZ: u64 = 28_800_000;

/// Detected tuner family. R820T-compatible revisions share the same chip ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TunerKind {
    /// R820T/R820T2/R860-compatible tuner at I2C address 0x34.
    R820T,
    /// R828D tuner at I2C address 0x74.
    R828D,
}

#[derive(Debug)]
pub(crate) struct Tuner {
    pub kind: TunerKind,
    pub blog_v4: bool,
    pub crystal_hz: u64,
    shadow: [u8; 27],
    input: Option<u8>,
}
impl Tuner {
    pub fn new(kind: TunerKind, blog_v4: bool) -> Self {
        Self {
            kind,
            blog_v4,
            crystal_hz: XTAL_HZ,
            shadow: INITIAL,
            input: None,
        }
    }
    fn address(&self) -> u8 {
        if self.kind == TunerKind::R820T {
            0x34
        } else {
            0x74
        }
    }
    pub async fn detect<C: ControlBackend>(com: &Com<'_, C>) -> Result<TunerKind> {
        for (address, kind) in [(0x34, TunerKind::R820T), (0x74, TunerKind::R828D)] {
            match com.i2c_read(address, 0, 1).await {
                Ok(data) if data[0] == 0x69 => return Ok(kind),
                Ok(_) | Err(Error::Transfer(nusb::transfer::TransferError::Stall)) => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::UnsupportedTuner)
    }
    pub async fn open<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<()> {
        self.input = None;
        for (i, value) in INITIAL.iter().copied().enumerate() {
            com.i2c_write(self.address(), i as u8 + 5, value).await?;
            self.shadow[i] = value;
        }
        self.write_many(com, INIT_BEFORE_CAL).await?;
        let cap = self.calibrate(com).await?;
        self.write(com, 0x0a, 0x10 | cap, 0x1f).await?;
        self.write_many(com, INIT_AFTER_CAL).await
    }
    pub async fn standby<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<()> {
        // Try every independent power-down write even if one transfer fails.
        let mut result = Ok(());
        for &(reg, value, mask) in STANDBY {
            let next = self.write(com, reg, value, mask).await;
            result = result.and(next);
        }
        result
    }
    pub async fn set_gain<C: ControlBackend>(
        &mut self,
        com: &Com<'_, C>,
        gain: GainConfig,
    ) -> Result<()> {
        match gain {
            GainConfig::Auto => {
                self.write_many(
                    com,
                    &[(0x05, 0, 0x10), (0x07, 0x10, 0x10), (0x0c, 0x0b, 0x9f)],
                )
                .await
            }
            GainConfig::Manual(db) => {
                let (lna, mixer) = gain_steps(db);
                self.write_many(
                    com,
                    &[
                        (0x05, 0x10, 0x10),
                        (0x07, 0, 0x10),
                        (0x0c, 8, 0x9f),
                        (0x05, lna, 0x0f),
                        (0x07, mixer, 0x0f),
                    ],
                )
                .await
            }
        }
    }
    pub async fn tune<C: ControlBackend>(&mut self, com: &Com<'_, C>, hz: u64) -> Result<u64> {
        let upconvert = if self.blog_v4 && hz < XTAL_HZ {
            XTAL_HZ
        } else {
            0
        };
        let lo = hz + upconvert + IF_HZ;
        let &(_, drain, filter, corner) = STANDARD_MUX
            .iter()
            .rev()
            .find(|row| row.0 <= lo)
            .expect("table starts at zero");
        // V4 notch filters follow antenna RF, not the LO used by the mux.
        let drain = if self.blog_v4 {
            match hz {
                0..=2_200_000 | 85_000_000..=112_000_000 | 172_000_000..=242_000_000 => 0,
                _ => 8,
            }
        } else {
            drain
        };
        self.write_many(
            com,
            &[
                (0x17, drain, 8),
                (0x1a, filter, 0xc3),
                (0x1b, corner, 0xff),
                (0x10, 0, 0x0b),
                (0x08, 0, 0x3f),
                (0x09, 0, 0x3f),
            ],
        )
        .await?;
        let actual_lo = self.set_pll(com, lo).await?;
        if self.kind == TunerKind::R828D {
            let input = if self.blog_v4 {
                v4_input(hz)
            } else {
                u8::from(hz <= 345_000_000)
            };
            // The HF upconverter path does not benefit from the tracking filter.
            // Reapply the bypass on every tune because the mux resets it above.
            if self.blog_v4 && input == 2 {
                self.write_many(com, &[(0x1a, 0x40, 0xc3), (0x1b, 0, 0xff)])
                    .await?;
            }
            if self.input != Some(input) {
                if self.blog_v4 {
                    self.write(com, 0x06, if input == 2 { 8 } else { 0 }, 8)
                        .await?;
                    self.write(
                        com,
                        0x05,
                        match input {
                            0 => 0,
                            1 => 0x60,
                            _ => 0x20,
                        },
                        0x60,
                    )
                    .await?;
                    com.gpio(5, input != 2).await?;
                } else {
                    self.write(com, 0x05, if input == 0 { 0 } else { 0x60 }, 0x60)
                        .await?;
                }
                self.input = Some(input);
            }
        }
        Ok(actual_lo.saturating_sub(IF_HZ + upconvert))
    }
    async fn calibrate<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<u8> {
        for attempt in 0..2 {
            self.write_many(com, &[(0x0b, 0x60, 0x60), (0x0f, 4, 4), (0x10, 0, 3)])
                .await?;
            self.set_pll(com, 56_000_000).await?;
            self.write_many(com, &[(0x0b, 0x10, 0x10), (0x0b, 0, 0x10), (0x0f, 0, 4)])
                .await?;
            let code = self.read(com, 5).await?[4] & 15;
            let code = if code == 15 { 0 } else { code };
            // Codes 0 and 15 are invalid; keep the first valid calibration.
            if code != 0 || attempt == 1 {
                return Ok(code);
            }
        }
        unreachable!()
    }
    async fn set_pll<C: ControlBackend>(&mut self, com: &Com<'_, C>, hz: u64) -> Result<u64> {
        self.write_many(com, &[(0x10, 0, 0x10), (0x1a, 0, 0x0c), (0x12, 0x80, 0xe0)])
            .await?;
        let fine = (self.read(com, 5).await?[4] >> 4) & 3;
        let plan = pll(
            hz,
            self.crystal_hz,
            fine,
            if self.kind == TunerKind::R820T { 2 } else { 1 },
        )?;
        self.write_many(
            com,
            &[
                (0x10, plan.divider << 5, 0xe0),
                (0x14, plan.integer, 0xff),
                (0x12, if plan.sdm == 0 { 8 } else { 0 }, 8),
                (0x16, (plan.sdm >> 8) as u8, 0xff),
                (0x15, plan.sdm as u8, 0xff),
            ],
        )
        .await?;
        for attempt in 0..2 {
            if self.read(com, 3).await?[2] & 0x40 != 0 {
                self.write(com, 0x1a, 8, 8).await?;
                return Ok(plan.actual_hz);
            }
            if attempt == 0 {
                self.write(com, 0x12, 0x60, 0xe0).await?;
            }
        }
        Err(Error::PllUnlocked)
    }
    async fn read<C: ControlBackend>(&self, com: &Com<'_, C>, count: u16) -> Result<Vec<u8>> {
        Ok(com
            .i2c_read(self.address(), 0, count)
            .await?
            .into_iter()
            .map(u8::reverse_bits)
            .collect())
    }
    async fn write<C: ControlBackend>(
        &mut self,
        com: &Com<'_, C>,
        reg: u8,
        value: u8,
        mask: u8,
    ) -> Result<()> {
        let offset = usize::from(reg - 5);
        let value = (self.shadow[offset] & !mask) | (value & mask);
        com.i2c_write(self.address(), reg, value).await?;
        self.shadow[offset] = value;
        Ok(())
    }
    async fn write_many<C: ControlBackend>(
        &mut self,
        com: &Com<'_, C>,
        rows: &[(u8, u8, u8)],
    ) -> Result<()> {
        for &(reg, value, mask) in rows {
            self.write(com, reg, value, mask).await?;
        }
        Ok(())
    }
}

pub(crate) fn v4_input(hz: u64) -> u8 {
    if hz < XTAL_HZ {
        2
    } else if hz < 250_000_000 {
        1
    } else {
        0
    }
}
fn gain_steps(db: f32) -> (u8, u8) {
    let full = (db / 3.5).floor().clamp(0.0, 15.0) as u8;
    let half = u8::from(full < 15 && db - 3.5 * f32::from(full) >= 2.3);
    (full + half, full)
}
#[derive(Debug)]
struct Pll {
    divider: u8,
    integer: u8,
    sdm: u16,
    actual_hz: u64,
}
fn pll(hz: u64, crystal: u64, fine: u8, power_ref: u8) -> Result<Pll> {
    if hz == 0 || hz > 1_770_000_000 || crystal == 0 {
        return Err(Error::PllUnlocked);
    }
    let div = (1_770_000_000 / hz).ilog2().min(6) as i32;
    let mix = 1u64 << (div + 1);
    let divider = div
        + match fine.cmp(&power_ref) {
            std::cmp::Ordering::Less => 1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => -1,
        };
    if !(0..=7).contains(&divider) {
        return Err(Error::PllUnlocked);
    }
    let vco = hz * mix;
    let integer = vco / (2 * crystal);
    if !(13..=63).contains(&integer) {
        return Err(Error::PllUnlocked);
    }
    let sdm = ((vco % (2 * crystal)) * 32768 / crystal).min(65535) as u16;
    Ok(Pll {
        divider: divider as u8,
        integer: ((integer - 13) / 4 + (((integer - 13) % 4) << 6)) as u8,
        sdm,
        actual_hz: (2 * crystal * (integer * 65536 + u64::from(sdm))) / (65536 * mix),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeTransport;
    use std::sync::atomic::Ordering;
    #[test]
    fn v4_hf_bypasses_tracking_filter_on_retunes_and_restores_it_above_hf() {
        let control = FakeTransport::default();
        control.state.tuner_address.store(0x74, Ordering::SeqCst);
        let mut tuner = Tuner::new(TunerKind::R828D, true);
        for (hz, filter, corner) in [
            (7_100_000, 0x40, 0),
            (14_200_000, 0x40, 0),
            (28_799_999, 0x40, 0),
            (28_800_000, 0x02, 0xdf),
            (100_000_000, 0x02, 0x34),
            (433_920_000, 0x41, 0),
            (7_100_000, 0x40, 0),
        ] {
            futures_lite::future::block_on(tuner.tune(&Com(&control), hz)).unwrap();
            assert_eq!(tuner.shadow[0x1a - 5] & 0xc3, filter, "RF frequency {hz}");
            assert_eq!(tuner.shadow[0x1b - 5], corner, "RF frequency {hz}");
            // Preserve the PLL autotune bits while selecting the RF mux.
            assert_eq!(tuner.shadow[0x1a - 5] & 0x0c, 8);
        }
    }
    #[test]
    fn v4_notches_follow_rf_at_every_band_edge_and_retune() {
        let control = FakeTransport::default();
        control.state.tuner_address.store(0x74, Ordering::SeqCst);
        let mut tuner = Tuner::new(TunerKind::R828D, true);
        for (hz, notch) in [
            (1_000_000, 0),
            (2_200_000, 0),
            (2_200_001, 8),
            (83_000_000, 8),
            (84_999_999, 8),
            (85_000_000, 0),
            (110_000_000, 0),
            (112_000_000, 0),
            (112_000_001, 8),
            (170_000_000, 8),
            (171_999_999, 8),
            (172_000_000, 0),
            (240_000_000, 0),
            (242_000_000, 0),
            (242_000_001, 8),
            (100_000_000, 0),
            (83_000_000, 8),
            (1_000_000, 0),
        ] {
            futures_lite::future::block_on(tuner.tune(&Com(&control), hz)).unwrap();
            let writes = control.state.control_out_requests.lock().unwrap();
            let value = writes
                .iter()
                .rev()
                .find(|r| r.index == 0x610 && r.data.first() == Some(&0x17))
                .unwrap()
                .data[1];
            assert_eq!(value & 8, notch, "RF frequency {hz}");
        }
    }
    #[test]
    fn standard_tuner_open_drain_still_follows_mux() {
        for (kind, address) in [(TunerKind::R820T, 0x34), (TunerKind::R828D, 0x74)] {
            let control = FakeTransport::default();
            control.state.tuner_address.store(address, Ordering::SeqCst);
            let mut tuner = Tuner::new(kind, false);
            for (hz, drain) in [(50_000_000, 8), (83_000_000, 0)] {
                futures_lite::future::block_on(tuner.tune(&Com(&control), hz)).unwrap();
                assert_eq!(tuner.shadow[0x17 - 5] & 8, drain);
            }
        }
    }
    fn calibration_responses(codes: &[u8]) -> FakeTransport {
        let control = FakeTransport::default();
        for code in codes {
            control.state.responses.lock().unwrap().extend([
                Ok(vec![0, 0, 0, 0, 0x20u8.reverse_bits()]),
                Ok(vec![0, 0, 0x40u8.reverse_bits()]),
                Ok(vec![0, 0, 0, 0, code.reverse_bits()]),
            ]);
        }
        control
    }
    #[test]
    fn calibration_keeps_first_valid_result() {
        for code in 1..15 {
            let control = calibration_responses(&[code, 0]);
            let mut tuner = Tuner::new(TunerKind::R820T, false);
            assert_eq!(
                futures_lite::future::block_on(tuner.calibrate(&Com(&control))).unwrap(),
                code
            );
            assert_eq!(control.state.responses.lock().unwrap().len(), 3);
        }
    }
    #[test]
    fn calibration_retries_invalid_codes_once() {
        for invalid in [0, 15] {
            for second in [0, 7, 15] {
                let control = calibration_responses(&[invalid, second]);
                let mut tuner = Tuner::new(TunerKind::R820T, false);
                assert_eq!(
                    futures_lite::future::block_on(tuner.calibrate(&Com(&control))).unwrap(),
                    if second == 15 { 0 } else { second }
                );
                assert!(control.state.responses.lock().unwrap().is_empty());
            }
        }
    }
    #[test]
    fn r828d_probe_is_awaited_and_missing_tuners_are_rejected() {
        let control = FakeTransport::default();
        control.state.tuner_address.store(0x74, Ordering::SeqCst);
        assert_eq!(
            futures_lite::future::block_on(Tuner::detect(&Com(&control))).unwrap(),
            TunerKind::R828D
        );
        control.state.tuner_address.store(0, Ordering::SeqCst);
        assert!(matches!(
            futures_lite::future::block_on(Tuner::detect(&Com(&control))),
            Err(Error::UnsupportedTuner)
        ));
    }
    #[test]
    fn pll_reports_unlock_after_retry_instead_of_returning_a_frequency() {
        let control = FakeTransport::default();
        control.state.pll_locked.store(false, Ordering::SeqCst);
        let mut tuner = Tuner::new(TunerKind::R820T, false);
        assert!(matches!(
            futures_lite::future::block_on(tuner.set_pll(&Com(&control), 103_570_000)),
            Err(Error::PllUnlocked)
        ));
        let reads = control.state.control_in_requests.lock().unwrap();
        assert_eq!(
            reads
                .iter()
                .filter(|r| r.index == 0x600 && r.length == 3)
                .count(),
            2
        );
    }
    #[test]
    fn pll_quantization_stays_within_one_fractional_step_across_rf_range() {
        for crystal in [28_785_946, 28_800_000, 28_814_054] {
            for hz in (32_370_000..=1_769_570_000).step_by(1_000_000) {
                let plan = pll(hz, crystal, 2, 2).unwrap();
                assert!(plan.actual_hz <= hz);
                assert!(hz - plan.actual_hz < 500);
                assert!(plan.divider <= 6);
            }
        }
    }
    #[test]
    fn gain_and_v4_band_edges() {
        assert_eq!(gain_steps(0.0), (0, 0));
        assert_eq!(gain_steps(2.3), (1, 0));
        assert_eq!(gain_steps(3.5), (1, 1));
        assert_eq!(gain_steps(52.5), (15, 15));
        assert_eq!(v4_input(28_799_999), 2);
        assert_eq!(v4_input(28_800_000), 1);
        assert_eq!(v4_input(249_999_999), 1);
        assert_eq!(v4_input(250_000_000), 0);
    }
}
