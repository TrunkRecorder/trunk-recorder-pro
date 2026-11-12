//! Rafael Micro R820T / R820T2 / R860 and R828D tuners (low IF, 3.57 MHz),
//! the RTL-SDR Blog V4's HF upconverter, notches and input switching included.

use super::i2c::{Reads, Registers};
use super::registers::*;
use super::{TunerKind, XTAL_HZ};
use crate::{Error, GainConfig, Result, protocol::Com, usb::ControlBackend};

/// The IF the tuner hands the demodulator.
pub(crate) const IF_HZ: u64 = 3_570_000;

#[derive(Debug)]
pub(crate) struct R82xx {
    pub kind: TunerKind,
    pub blog_v4: bool,
    pub crystal_hz: u64,
    /// R5–R31 as written (the chip's status registers R0–R4 are read).
    pub(super) regs: Registers,
    input: Option<u8>,
}

impl R82xx {
    pub fn new(kind: TunerKind, blog_v4: bool) -> Self {
        Self {
            kind,
            blog_v4,
            crystal_hz: XTAL_HZ,
            regs: Registers::new(kind.address(), Reads::FromZeroReversed, &[]).holding(5, &INITIAL),
            input: None,
        }
    }
    pub async fn open<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<()> {
        self.input = None;
        for (i, value) in INITIAL.iter().copied().enumerate() {
            self.regs.put(com, i as u8 + 5, value).await?;
        }
        self.regs.write_many(com, INIT_BEFORE_CAL).await?;
        let cap = self.calibrate(com).await?;
        self.regs.write(com, 0x0a, 0x10 | cap, 0x1f).await?;
        self.regs.write_many(com, INIT_AFTER_CAL).await
    }
    pub async fn standby<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<()> {
        // Try every independent power-down write even if one transfer fails.
        let mut result = Ok(());
        for &(reg, value, mask) in STANDBY {
            let next = self.regs.write(com, reg, value, mask).await;
            result = result.and(next);
        }
        result
    }
    pub async fn set_gain<C: ControlBackend>(&mut self, com: &Com<'_, C>, gain: GainConfig) -> Result<()> {
        match gain {
            GainConfig::Auto => self.regs.write_many(com, &[(0x05, 0, 0x10), (0x07, 0x10, 0x10), (0x0c, 0x0b, 0x9f)]).await,
            GainConfig::Manual(db) => {
                let (lna, mixer) = gain_steps(db);
                self.regs.write_many(com, &[(0x05, 0x10, 0x10), (0x07, 0, 0x10), (0x0c, 8, 0x9f), (0x05, lna, 0x0f), (0x07, mixer, 0x0f)]).await
            }
        }
    }
    pub async fn tune<C: ControlBackend>(&mut self, com: &Com<'_, C>, hz: u64) -> Result<u64> {
        let upconvert = if self.blog_v4 && hz < XTAL_HZ { XTAL_HZ } else { 0 };
        let lo = hz + upconvert + IF_HZ;
        let &(_, drain, filter, corner) = STANDARD_MUX.iter().rev().find(|row| row.0 <= lo).expect("table starts at zero");
        // V4 notch filters follow antenna RF, not the LO used by the mux.
        let drain = if self.blog_v4 {
            match hz {
                0..=2_200_000 | 85_000_000..=112_000_000 | 172_000_000..=242_000_000 => 0,
                _ => 8,
            }
        } else {
            drain
        };
        self.regs.write_many(com, &[(0x17, drain, 8), (0x1a, filter, 0xc3), (0x1b, corner, 0xff), (0x10, 0, 0x0b), (0x08, 0, 0x3f), (0x09, 0, 0x3f)]).await?;
        let actual_lo = self.set_pll(com, lo).await?;
        if self.kind == TunerKind::R828D {
            let input = if self.blog_v4 { v4_input(hz) } else { u8::from(hz <= 345_000_000) };
            // The HF upconverter path does not benefit from the tracking filter.
            // Reapply the bypass on every tune because the mux resets it above.
            if self.blog_v4 && input == 2 {
                self.regs.write_many(com, &[(0x1a, 0x40, 0xc3), (0x1b, 0, 0xff)]).await?;
            }
            if self.input != Some(input) {
                if self.blog_v4 {
                    self.regs.write(com, 0x06, if input == 2 { 8 } else { 0 }, 8).await?;
                    let mux = match input {
                        0 => 0,
                        1 => 0x60,
                        _ => 0x20,
                    };
                    self.regs.write(com, 0x05, mux, 0x60).await?;
                    com.gpio(5, input != 2).await?;
                } else {
                    self.regs.write(com, 0x05, if input == 0 { 0 } else { 0x60 }, 0x60).await?;
                }
                self.input = Some(input);
            }
        }
        Ok(actual_lo.saturating_sub(IF_HZ + upconvert))
    }
    pub(super) async fn calibrate<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<u8> {
        for attempt in 0..2 {
            self.regs.write_many(com, &[(0x0b, 0x60, 0x60), (0x0f, 4, 4), (0x10, 0, 3)]).await?;
            self.set_pll(com, 56_000_000).await?;
            self.regs.write_many(com, &[(0x0b, 0x10, 0x10), (0x0b, 0, 0x10), (0x0f, 0, 4)]).await?;
            let code = self.regs.read_from_zero(com, 5).await?[4] & 15;
            let code = if code == 15 { 0 } else { code };
            // Codes 0 and 15 are invalid; keep the first valid calibration.
            if code != 0 || attempt == 1 {
                return Ok(code);
            }
        }
        unreachable!()
    }
    /// Program the PLL for `hz` and wait for lock. Unlocked even with more
    /// charge-pump current, the neighbouring divider is tried where it keeps
    /// the VCO in range: near the VCO's edges one divider may not lock where
    /// the other does (sdr-rtl findings 20, 21: 851–858 MHz on two dongles).
    pub(super) async fn set_pll<C: ControlBackend>(&mut self, com: &Com<'_, C>, hz: u64) -> Result<u64> {
        self.regs.write_many(com, &[(0x10, 0, 0x10), (0x1a, 0, 0x0c), (0x12, 0x80, 0xe0)]).await?;
        let fine = (self.regs.read_from_zero(com, 5).await?[4] >> 4) & 3;
        let power_ref = if self.kind == TunerKind::R820T { 2 } else { 1 };
        for shift in [0, -1, 1] {
            let Ok(plan) = pll(hz, self.crystal_hz, fine, power_ref, shift) else { continue };
            let sdm_zero = if plan.sdm == 0 { 8 } else { 0 };
            self.regs
                .write_many(com, &[(0x12, 0x80, 0xe0), (0x10, plan.divider << 5, 0xe0), (0x14, plan.integer, 0xff), (0x12, sdm_zero, 8), (0x16, (plan.sdm >> 8) as u8, 0xff), (0x15, plan.sdm as u8, 0xff)])
                .await?;
            for attempt in 0..2 {
                if self.regs.read_from_zero(com, 3).await?[2] & 0x40 != 0 {
                    self.regs.write(com, 0x1a, 8, 8).await?;
                    return Ok(plan.actual_hz);
                }
                if attempt == 0 {
                    self.regs.write(com, 0x12, 0x60, 0xe0).await?;
                }
            }
        }
        Err(Error::PllUnlocked)
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
/// librtlsdr's measured gain of each LNA and mixer step, tenths of a dB
/// (`r82xx_lna_gain_steps`, `r82xx_mixer_gain_steps`): uneven, so a dB
/// figure is a pair of steps only by this table.
const LNA_STEPS: [i32; 16] = [0, 9, 13, 40, 38, 13, 31, 22, 26, 31, 26, 14, 19, 5, 35, 13];
const MIXER_STEPS: [i32; 16] = [0, 5, 10, 10, 19, 9, 10, 25, 17, 10, 8, 16, 13, 6, 3, -8];
/// The gains librtlsdr offers, tenths of a dB (`r82xx_gains`): what Trunk
/// Recorder (through gr-osmosdr) takes the nearest of.
const GAINS: [i32; 29] = [0, 9, 14, 27, 37, 77, 87, 125, 144, 157, 166, 197, 207, 229, 254, 280, 297, 328, 338, 364, 372, 386, 402, 421, 434, 439, 445, 480, 496];

/// The offered gain nearest `db` (first of equals), tenths of a dB.
pub(super) fn nearest_gain(db: f32) -> i32 {
    let tenths = (db * 10.0).round() as i32;
    GAINS.into_iter().min_by_key(|&g| (g - tenths).abs()).expect("a gain at least")
}

/// The LNA and mixer steps for the gain nearest `db`, as librtlsdr sets
/// them: raising the LNA and then the mixer a step in turn until there.
fn gain_steps(db: f32) -> (u8, u8) {
    let want = nearest_gain(db);
    let (mut lna, mut mixer, mut total) = (0, 0, 0);
    while total < want && mixer < 15 {
        lna += 1;
        total += LNA_STEPS[lna];
        if total >= want {
            break;
        }
        mixer += 1;
        total += MIXER_STEPS[mixer];
    }
    (lna as u8, mixer as u8)
}
#[derive(Debug)]
struct Pll {
    divider: u8,
    integer: u8,
    sdm: u16,
    actual_hz: u64,
}
/// The VCO's range for a neighbouring divider (sdr-rtl's measured 1.71–3.65 GHz).
const VCO_RANGE_HZ: std::ops::RangeInclusive<u64> = 1_710_000_000..=3_650_000_000;

/// The words for `hz`: the divider that puts the VCO at 1.77 GHz or above
/// (librtlsdr's rule), or with `shift` ±1 its neighbour, if that keeps the VCO in range.
fn pll(hz: u64, crystal: u64, fine: u8, power_ref: u8, shift: i32) -> Result<Pll> {
    if hz == 0 || hz > 1_770_000_000 || crystal == 0 {
        return Err(Error::PllUnlocked);
    }
    let div = (1_770_000_000 / hz).ilog2().min(6) as i32 + shift;
    if !(0..=6).contains(&div) || (shift != 0 && !VCO_RANGE_HZ.contains(&(hz << (div + 1)))) {
        return Err(Error::PllUnlocked);
    }
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
        let mut tuner = R82xx::new(TunerKind::R828D, true);
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
            let reg = |r| tuner.regs.get(r).unwrap();
            assert_eq!(reg(0x1a) & 0xc3, filter, "RF frequency {hz}");
            assert_eq!(reg(0x1b), corner, "RF frequency {hz}");
            // Preserve the PLL autotune bits while selecting the RF mux.
            assert_eq!(reg(0x1a) & 0x0c, 8);
        }
    }
    #[test]
    fn v4_notches_follow_rf_at_every_band_edge_and_retune() {
        let control = FakeTransport::default();
        control.state.tuner_address.store(0x74, Ordering::SeqCst);
        let mut tuner = R82xx::new(TunerKind::R828D, true);
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
            // (The copy is what was written: an unchanged register isn't written again.)
            assert_eq!(tuner.regs.get(0x17).unwrap() & 8, notch, "RF frequency {hz}");
        }
    }
    #[test]
    fn standard_tuner_open_drain_still_follows_mux() {
        for kind in [TunerKind::R820T, TunerKind::R828D] {
            let control = FakeTransport::default();
            control.state.tuner_address.store(kind.address(), Ordering::SeqCst);
            let mut tuner = R82xx::new(kind, false);
            for (hz, drain) in [(50_000_000, 8), (83_000_000, 0)] {
                futures_lite::future::block_on(tuner.tune(&Com(&control), hz)).unwrap();
                assert_eq!(tuner.regs.get(0x17).unwrap() & 8, drain);
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
            let mut tuner = R82xx::new(TunerKind::R820T, false);
            assert_eq!(futures_lite::future::block_on(tuner.calibrate(&Com(&control))).unwrap(), code);
            assert_eq!(control.state.responses.lock().unwrap().len(), 3);
        }
    }
    #[test]
    fn calibration_retries_invalid_codes_once() {
        for invalid in [0, 15] {
            for second in [0, 7, 15] {
                let control = calibration_responses(&[invalid, second]);
                let mut tuner = R82xx::new(TunerKind::R820T, false);
                assert_eq!(futures_lite::future::block_on(tuner.calibrate(&Com(&control))).unwrap(), if second == 15 { 0 } else { second });
                assert!(control.state.responses.lock().unwrap().is_empty());
            }
        }
    }
    #[test]
    fn pll_reports_unlock_after_retry_instead_of_returning_a_frequency() {
        let control = FakeTransport::default();
        control.state.pll_locked.store(false, Ordering::SeqCst);
        let mut tuner = R82xx::new(TunerKind::R820T, false);
        assert!(matches!(futures_lite::future::block_on(tuner.set_pll(&Com(&control), 103_570_000)), Err(Error::PllUnlocked)));
        let reads = control.state.control_in_requests.lock().unwrap();
        assert_eq!(reads.iter().filter(|r| r.index == 0x600 && r.length == 3).count(), 2);
    }
    #[test]
    fn pll_quantization_stays_within_one_fractional_step_across_rf_range() {
        for crystal in [28_785_946, 28_800_000, 28_814_054] {
            for hz in (32_370_000..=1_769_570_000).step_by(1_000_000) {
                let plan = pll(hz, crystal, 2, 2, 0).unwrap();
                assert!(plan.actual_hz <= hz);
                assert!(hz - plan.actual_hz < 500);
                assert!(plan.divider <= 6);
            }
        }
    }
    #[test]
    fn gain_and_v4_band_edges() {
        assert_eq!(gain_steps(0.0), (0, 0));
        // (The top, 49.6 dB, is the mixer's 14th step: its 15th takes 0.8 dB off.)
        assert_eq!(gain_steps(52.5), (15, 14));
        // Each gain offered is its own pair of steps, adding up to it as librtlsdr measured them.
        let measured = |(lna, mixer): (u8, u8)| LNA_STEPS[..=lna as usize].iter().sum::<i32>() + MIXER_STEPS[..=mixer as usize].iter().sum::<i32>();
        for g in GAINS {
            assert_eq!(measured(gain_steps(g as f32 / 10.0)), g, "{g}");
        }
        // Issue 163: what isn't offered goes to the nearest that is.
        assert_eq!((nearest_gain(34.0), gain_steps(34.0)), (338, gain_steps(33.8)));
        assert_eq!(nearest_gain(35.2), 364);
        assert_eq!(nearest_gain(-3.0), 0);
        assert_eq!(v4_input(28_799_999), 2);
        assert_eq!(v4_input(28_800_000), 1);
        assert_eq!(v4_input(249_999_999), 1);
        assert_eq!(v4_input(250_000_000), 0);
    }
}
