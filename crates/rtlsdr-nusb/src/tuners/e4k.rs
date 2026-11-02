// SPDX-License-Identifier: GPL-2.0-or-later
//
// Elonics E4000 tuner driver, ported to Rust from osmocom librtlsdr's
// src/tuner_e4k.c (https://gitea.osmocom.org/sdr/rtl-sdr):
//   (C) 2011-2012 by Harald Welte <laforge@gnumonks.org>
//   (C) 2012 by Sylvain Munaut <tnt@246tNt.com>
//   (C) 2012 by Hoernchen <la@tfc-server.de>
//
// This program is free software; you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation; either version 2 of the License, or (at your option)
// any later version. See LICENSE-GPL-2.0 in this crate. Unlike the rest of
// rtlsdr-nusb (Apache-2.0), this file is GPL-2.0-or-later.

//! Elonics E4000 tuner (zero IF), after osmocom librtlsdr's `tuner_e4k.c`.
//!
//! librtlsdr read-modify-writes every register; here they are kept as
//! written ([`Registers`]), as the R82xx's are, but for the ones whose bits
//! the chip changes itself — SYNTH1 (PLL lock), MASTER1 (power-on reset
//! flag) and GAIN1 (the LNA gain its AGC runs) — which are read each time.
//! The E4000 runs off the RTL2832U's 28.8 MHz crystal, so the ppm-corrected
//! crystal is its PLL reference.

use super::i2c::{Reads, Registers};
use super::{TunerKind, XTAL_HZ};
use crate::{Error, GainConfig, Result, protocol::Com, usb::ControlBackend};

const MASTER1: u8 = 0x00;
const CLK_INP: u8 = 0x05;
const REF_CLK: u8 = 0x06;
const SYNTH1: u8 = 0x07;
const SYNTH3: u8 = 0x09;
const SYNTH4: u8 = 0x0a;
const SYNTH5: u8 = 0x0b;
const SYNTH7: u8 = 0x0d;
const FILT1: u8 = 0x10;
const FILT2: u8 = 0x11;
const FILT3: u8 = 0x12;
const GAIN1: u8 = 0x14;
const GAIN2: u8 = 0x15;
const GAIN3: u8 = 0x16;
const GAIN4: u8 = 0x17;
const AGC1: u8 = 0x1a;
const AGC4: u8 = 0x1d;
const AGC5: u8 = 0x1e;
const AGC6: u8 = 0x1f;
const AGC7: u8 = 0x20;
const DC5: u8 = 0x2d;
const DCTIME1: u8 = 0x70;
const DCTIME2: u8 = 0x71;
const BIAS: u8 = 0x78;
const CLKOUT_PWDN: u8 = 0x7a;

/// MASTER1: reset, normal (not standby), clear power-on-reset flag.
const MASTER1_RESET: u8 = 1 << 0;
const MASTER1_NORM_STBY: u8 = 1 << 1;
const MASTER1_POR_DET: u8 = 1 << 2;
/// AGC1 mode field: LNA under serial (manual) control, or IF serial + LNA autonomous.
const AGC1_MOD_MASK: u8 = 0x0f;
const AGC_MOD_SERIAL: u8 = 0x0;
const AGC_MOD_IF_SERIAL_LNA_AUTON: u8 = 0x9;
const AGC7_MIX_GAIN_AUTO: u8 = 0x01;
const FILT3_DISABLE: u8 = 1 << 5;
const SYNTH1_PLL_LOCK: u8 = 0x01;

/// Undocumented values librtlsdr writes at init (`magic_init`).
const MAGIC: &[(u8, u8)] = &[
    (0x7e, 0x01),
    (0x7f, 0xfe),
    (0x82, 0x00),
    (0x86, 0x50),
    (0x87, 0x20),
    (0x88, 0x01),
    (0x9f, 0x7f),
    (0xa0, 0x07),
];

/// (LO below this, SYNTH7 value: bit 3 three-phase mixing + divider select, multiplier R).
const PLL_VARS: &[(u64, u8, u64)] = &[
    (72_400_000, (1 << 3) | 7, 48),
    (81_200_000, (1 << 3) | 6, 40),
    (108_300_000, (1 << 3) | 5, 32),
    (162_500_000, (1 << 3) | 4, 24),
    (216_600_000, (1 << 3) | 3, 16),
    (325_000_000, (1 << 3) | 2, 12),
    (350_000_000, (1 << 3) | 1, 8),
    (432_000_000, 3, 8),
    (667_000_000, 2, 6),
    (1_200_000_000, 1, 4),
];
const PLL_Y: u64 = 65536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Band {
    Vhf2 = 0,
    Vhf3 = 1,
    Uhf = 2,
    L = 3,
}

const RF_FILTER_UHF_MHZ: [u64; 16] = [
    360, 380, 405, 425, 450, 475, 505, 540, 575, 615, 670, 720, 760, 840, 890, 970,
];
const RF_FILTER_L_MHZ: [u64; 16] = [
    1300, 1320, 1360, 1410, 1445, 1460, 1490, 1530, 1560, 1590, 1640, 1660, 1680, 1700, 1720, 1750,
];

const MIX_FILTER_KHZ: [u64; 16] = [
    27000, 27000, 27000, 27000, 27000, 27000, 27000, 27000, 4600, 4200, 3800, 3400, 3300, 2700,
    2300, 1900,
];
const RC_FILTER_KHZ: [u64; 16] = [
    21400, 21000, 17600, 14700, 12400, 10600, 9000, 7700, 6400, 5300, 4400, 3400, 2600, 1800, 1200,
    1000,
];
const CHAN_FILTER_KHZ: [u64; 32] = [
    5500, 5300, 5000, 4800, 4600, 4400, 4300, 4100, 3900, 3800, 3700, 3600, 3400, 3300, 3200, 3100,
    3000, 2950, 2900, 2800, 2750, 2700, 2600, 2550, 2500, 2450, 2400, 2300, 2280, 2240, 2200, 2150,
];

/// (LNA gain in tenths of a dB, GAIN1 code).
const LNA_GAIN: [(i32, u8); 13] = [
    (-50, 0),
    (-25, 1),
    (0, 4),
    (25, 5),
    (50, 6),
    (75, 7),
    (100, 8),
    (125, 9),
    (150, 10),
    (175, 11),
    (200, 12),
    (250, 13),
    (300, 14),
];

/// The mixer's gain (GAIN2 bit 0): 4 dB, or 12 dB at the top step.
const MIXER_TENTHS_DB: [i32; 2] = [40, 120];

/// An E4000's registers and the reference its PLL runs from.
#[derive(Debug)]
pub(crate) struct E4000 {
    pub crystal_hz: u64,
    regs: Registers,
}

impl E4000 {
    pub fn new() -> Self {
        Self {
            crystal_hz: XTAL_HZ,
            regs: Registers::new(TunerKind::E4000.address(), Reads::Each, &[MASTER1, SYNTH1, GAIN1]),
        }
    }
    /// librtlsdr's `e4k_init`: reset, clock setup, magic values, moderate IF
    /// gains, narrow IF filters (widened by [`E4000::set_bandwidth`]), auto gain.
    pub async fn open<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<()> {
        // The first transaction after power-up is not acknowledged (librtlsdr's dummy read).
        let _ = self.regs.read(com, 0).await;
        self.regs.put(com, MASTER1, MASTER1_RESET | MASTER1_NORM_STBY | MASTER1_POR_DET).await?;
        self.regs.put(com, CLK_INP, 0x00).await?;
        // No clock output.
        self.regs.put(com, REF_CLK, 0x00).await?;
        self.regs.put(com, CLKOUT_PWDN, 0x96).await?;
        for &(reg, value) in MAGIC {
            self.regs.put(com, reg, value).await?;
        }
        // LNA AGC thresholds and loop rate.
        self.regs.put(com, AGC4, 0x10).await?;
        self.regs.put(com, AGC5, 0x04).await?;
        self.regs.put(com, AGC6, 0x1a).await?;
        self.regs.write_many(com, &[(AGC1, AGC_MOD_SERIAL, AGC1_MOD_MASK), (AGC7, 0, AGC7_MIX_GAIN_AUTO)]).await?;
        self.set_gain(com, GainConfig::Auto).await?;
        // IF stages: 1 = +6 dB, 2-4 = 0 dB, 5-6 = +9 dB (librtlsdr's defaults).
        self.regs.write_many(com, &[(GAIN3, 1, 0x01), (GAIN3, 0, 0x06), (GAIN3, 0, 0x18), (GAIN3, 0, 0x60), (GAIN4, 2, 0x07), (GAIN4, 2 << 3, 0x38)]).await?;
        self.set_filters(com, 1_900_000, 1_000_000, 2_150_000).await?;
        // The channel filter on; time-variant DC correction and its LUT off.
        self.regs.write_many(com, &[(FILT3, 0, FILT3_DISABLE), (DC5, 0, 0x03), (DCTIME1, 0, 0x03), (DCTIME2, 0, 0x03)]).await
    }
    pub async fn standby<C: ControlBackend>(&mut self, com: &Com<'_, C>) -> Result<()> {
        self.regs.write(com, MASTER1, 0, MASTER1_NORM_STBY).await
    }
    pub async fn set_gain<C: ControlBackend>(&mut self, com: &Com<'_, C>, gain: GainConfig) -> Result<()> {
        match gain {
            GainConfig::Auto => {
                let rows = [(AGC1, AGC_MOD_IF_SERIAL_LNA_AUTON, AGC1_MOD_MASK), (AGC7, AGC7_MIX_GAIN_AUTO, AGC7_MIX_GAIN_AUTO), (GAIN1, 0, 0x0f)];
                self.regs.write_many(com, &rows).await
            }
            GainConfig::Manual(db) => {
                let (_, lna, mixer) = nearest_gain(db);
                let rows = [(AGC1, AGC_MOD_SERIAL, AGC1_MOD_MASK), (AGC7, 0, AGC7_MIX_GAIN_AUTO), (GAIN1, lna, 0x0f), (GAIN2, mixer, 0x01)];
                self.regs.write_many(com, &rows).await
            }
        }
    }
    /// IF filters for `hz` of bandwidth (librtlsdr sets them to the sample rate).
    pub async fn set_bandwidth<C: ControlBackend>(&mut self, com: &Com<'_, C>, hz: u64) -> Result<()> {
        self.set_filters(com, hz, hz, hz).await
    }
    async fn set_filters<C: ControlBackend>(&mut self, com: &Com<'_, C>, mix: u64, rc: u64, chan: u64) -> Result<()> {
        let rows = [
            (FILT2, closest(&MIX_FILTER_KHZ, 1000, mix) << 4, 0xf0),
            (FILT2, closest(&RC_FILTER_KHZ, 1000, rc), 0x0f),
            (FILT3, closest(&CHAN_FILTER_KHZ, 1000, chan), 0x1f),
        ];
        self.regs.write_many(com, &rows).await
    }
    /// Tune the LO to `hz` (zero IF: the LO is the center) and check lock.
    /// Returns the LO actually programmed.
    pub async fn tune<C: ControlBackend>(&mut self, com: &Com<'_, C>, hz: u64) -> Result<u64> {
        let p = pll(hz, self.crystal_hz)?;
        let band = band(p.flo);
        let rows = [
            (SYNTH7, p.synth7, 0xff),
            (SYNTH3, p.z, 0xff),
            (SYNTH4, p.x as u8, 0xff),
            (SYNTH5, (p.x >> 8) as u8, 0xff),
            (BIAS, if band == Band::L { 0 } else { 3 }, 0xff),
            // librtlsdr's workaround: clear the band before setting it, or 325-350 MHz won't lock.
            (SYNTH1, 0, 0x06),
            (SYNTH1, (band as u8) << 1, 0x06),
            (FILT1, rf_filter(band, p.flo), 0x0f),
        ];
        self.regs.write_many(com, &rows).await?;
        // The VCO calibrates itself; a second look gives it a moment more.
        for _ in 0..2 {
            if self.regs.read(com, SYNTH1).await? & SYNTH1_PLL_LOCK != 0 {
                return Ok(p.flo);
            }
        }
        Err(Error::PllUnlocked)
    }
}

/// librtlsdr's E4000 gain steps, in tenths of a dB, with the GAIN1 LNA code
/// and GAIN2 mixer bit each is: every LNA step with the mixer at 4 dB, then
/// the top LNA step with it at 12 dB (-1 to 42 dB in all).
fn gain_steps() -> impl Iterator<Item = (i32, u8, u8)> {
    let (top, top_code) = LNA_GAIN[LNA_GAIN.len() - 1];
    LNA_GAIN.iter().map(|&(lna, code)| (lna + MIXER_TENTHS_DB[0], code, 0)).chain([(top + MIXER_TENTHS_DB[1], top_code, 1)])
}

/// The step nearest `db` (above 42 dB: 42 dB; first of equals, as librtlsdr):
/// (tenths of a dB, GAIN1 LNA code, GAIN2 mixer bit).
fn nearest_gain(db: f32) -> (i32, u8, u8) {
    let tenths = (db * 10.0).round() as i32;
    gain_steps().min_by_key(|&(g, ..)| (g - tenths).abs()).expect("a step at least")
}

fn closest(table: &[u64], scale: u64, hz: u64) -> u8 {
    // First of equals wins, as librtlsdr's closest_arr_idx.
    let mut best = 0;
    for (i, &value) in table.iter().enumerate() {
        if (value * scale).abs_diff(hz) < (table[best] * scale).abs_diff(hz) {
            best = i;
        }
    }
    best as u8
}

#[derive(Debug, Eq, PartialEq)]
struct Pll {
    synth7: u8,
    z: u8,
    x: u16,
    /// The LO these words give.
    flo: u64,
}

/// librtlsdr's `e4k_compute_pll_params`: Fvco = Fosc · (Z + X/65536) = Flo · R.
fn pll(hz: u64, fosc: u64) -> Result<Pll> {
    if hz == 0 || !(16_000_000..=30_000_000).contains(&fosc) {
        return Err(Error::PllUnlocked);
    }
    let (synth7, r) = PLL_VARS.iter().find(|row| hz < row.0).map_or((0, 2), |&(_, synth7, r)| (synth7, r));
    let fvco = hz * r;
    let z = fvco / fosc;
    if !(1..=255).contains(&z) {
        return Err(Error::PllUnlocked);
    }
    let x = (fvco - fosc * z) * PLL_Y / fosc;
    let flo = (fosc * z + fosc * x / PLL_Y) / r;
    Ok(Pll { synth7, z: z as u8, x: x as u16, flo })
}

fn band(flo: u64) -> Band {
    if flo < 140_000_000 {
        Band::Vhf2
    } else if flo < 350_000_000 {
        Band::Vhf3
    } else if flo < 1_135_000_000 {
        Band::Uhf
    } else {
        Band::L
    }
}

fn rf_filter(band: Band, flo: u64) -> u8 {
    match band {
        Band::Vhf2 | Band::Vhf3 => 0,
        Band::Uhf => closest(&RF_FILTER_UHF_MHZ, 1_000_000, flo),
        Band::L => closest(&RF_FILTER_L_MHZ, 1_000_000, flo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeTransport;
    use futures_lite::future::block_on;
    use std::sync::atomic::Ordering;

    const XTAL: u64 = 28_800_000;

    fn e4k() -> FakeTransport {
        let control = FakeTransport::default();
        control.state.tuner_address.store(TunerKind::E4000.address(), Ordering::SeqCst);
        control
    }
    fn reg(control: &FakeTransport, reg: u8) -> u8 {
        control.state.tuner_registers.lock().unwrap().get(&reg).copied().unwrap_or(0)
    }

    #[test]
    fn pll_words_match_librtlsdr_and_stay_within_a_fractional_step() {
        // 851.0125 MHz: two-phase, R = 4, Fvco 3.404 GHz.
        let p = pll(851_012_500, XTAL).unwrap();
        assert_eq!((p.synth7, p.z), (1, 118));
        // 155 MHz: three-phase, R = 24.
        assert_eq!(pll(155_000_000, XTAL).unwrap().synth7, (1 << 3) | 4);
        for crystal in [28_785_946, XTAL, 28_814_054] {
            for hz in (55_000_000..=1_766_000_000).step_by(997_000) {
                let p = pll(hz, crystal).unwrap();
                assert!(p.flo <= hz && hz - p.flo < 500, "{hz}: {}", p.flo);
            }
        }
        assert!(pll(0, XTAL).is_err());
        assert!(pll(100_000_000, 40_000_000).is_err());
    }

    #[test]
    fn p25_bands_keep_the_vco_in_its_specified_range() {
        for hz in (136_000_000..=174_000_000)
            .chain(380_000_000..=520_000_000)
            .chain(764_000_000..=870_000_000)
            .step_by(250_000)
        {
            let r = PLL_VARS.iter().find(|row| hz < row.0).unwrap().2;
            assert!((2_590_000_000..=3_900_000_000).contains(&(hz * r)), "{hz}");
        }
    }

    #[test]
    fn tune_sets_pll_band_and_rf_filter() {
        let control = e4k();
        let com = Com(&control);
        let mut e4k = E4000::new();
        let flo = block_on(e4k.tune(&com, 851_012_500)).unwrap();
        assert!(851_012_500 - flo < 500);
        assert_eq!(reg(&control, SYNTH7), 1);
        assert_eq!(reg(&control, SYNTH3), 118);
        assert_eq!(reg(&control, BIAS), 3);
        assert_eq!(reg(&control, SYNTH1) & 0x06, (Band::Uhf as u8) << 1);
        assert_eq!(reg(&control, FILT1) & 0x0f, 13, "840 MHz RF filter");
        block_on(e4k.tune(&com, 155_000_000)).unwrap();
        assert_eq!(reg(&control, SYNTH1) & 0x06, (Band::Vhf3 as u8) << 1);
        assert_eq!(reg(&control, FILT1) & 0x0f, 0);
        block_on(e4k.tune(&com, 1_575_420_000)).unwrap();
        assert_eq!(reg(&control, BIAS), 0);
        assert_eq!(reg(&control, SYNTH1) & 0x06, (Band::L as u8) << 1);
    }

    #[test]
    fn unlocked_pll_is_an_error() {
        let control = e4k();
        control.state.pll_locked.store(false, Ordering::SeqCst);
        assert!(matches!(
            block_on(E4000::new().tune(&Com(&control), 851_012_500)),
            Err(Error::PllUnlocked)
        ));
    }

    #[test]
    fn gain_follows_librtlsdr_steps() {
        // The steps, as librtlsdr's e4k_gains[] lists them.
        let steps: Vec<i32> = gain_steps().map(|(g, ..)| g).collect();
        assert_eq!(steps, [-10, 15, 40, 65, 90, 115, 140, 165, 190, 215, 240, 290, 340, 420]);
        for (g, _, mixer) in gain_steps() {
            assert_eq!(mixer, u8::from(g > 340), "{g}");
        }
        assert_eq!(nearest_gain(0.0).0, -10);
        assert_eq!(nearest_gain(30.0).0, 290);
        assert_eq!(nearest_gain(40.0).0, 420);
        assert_eq!(nearest_gain(52.5).0, 420);
        assert_eq!(nearest_gain(-1.0), (-10, 0, 0));
        assert_eq!(nearest_gain(29.0), (290, 13, 0));
        assert_eq!(nearest_gain(34.0), (340, 14, 0));
        assert_eq!(nearest_gain(42.0), (420, 14, 1));

        let control = e4k();
        let com = Com(&control);
        let mut e4k = E4000::new();
        block_on(e4k.set_gain(&com, GainConfig::Manual(42.0))).unwrap();
        assert_eq!(reg(&control, AGC1) & 0x0f, AGC_MOD_SERIAL);
        assert_eq!(reg(&control, AGC7) & 1, 0);
        assert_eq!(
            (reg(&control, GAIN1) & 0x0f, reg(&control, GAIN2) & 1),
            (14, 1)
        );
        block_on(e4k.set_gain(&com, GainConfig::Auto)).unwrap();
        assert_eq!(reg(&control, AGC1) & 0x0f, AGC_MOD_IF_SERIAL_LNA_AUTON);
        assert_eq!(reg(&control, AGC7) & 1, 1);
    }

    #[test]
    fn open_initializes_and_bandwidth_follows_sample_rate() {
        let control = e4k();
        let com = Com(&control);
        let mut e4k = E4000::new();
        block_on(e4k.open(&com)).unwrap();
        assert_eq!(reg(&control, CLKOUT_PWDN), 0x96);
        assert_eq!(reg(&control, GAIN3), 0x01);
        assert_eq!(reg(&control, GAIN4), 0x12);
        assert_eq!(reg(&control, FILT2), 0xff, "narrowest mixer and RC filters");
        assert_eq!(reg(&control, FILT3), 31, "2.15 MHz channel filter, enabled");
        block_on(e4k.set_bandwidth(&com, 2_400_000)).unwrap();
        assert_eq!(reg(&control, FILT2), (14 << 4) | 12);
        assert_eq!(reg(&control, FILT3), 26);
        block_on(e4k.standby(&com)).unwrap();
        assert_eq!(reg(&control, MASTER1) & MASTER1_NORM_STBY, 0);
    }
}
