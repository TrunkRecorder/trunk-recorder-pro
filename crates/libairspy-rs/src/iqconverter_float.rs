//! Floating-point IQ converter, a faithful port of
//! `iqconverter_float.c` from `airspyone_host` at the all-MIT
//! reference revision `bd15be38` (MIT License, Copyright (C) 2014,
//! Youssef Touil — full license text in `iqconverter_int16.rs` and
//! NOTICE; both converters share it).
//!
//! Pipeline (interleaved in place, like the int16 twin): scalar DC
//! removal → fs/4 translation (center-tap scaling on the Q phases) →
//! half-band FIR on I + compensating delay on Q. The reference builds
//! on Linux/GCC compile the scalar paths (neither `USE_SSE2` nor
//! `FIR_STANDARD` is defined there), and with the stock 47-tap kernel
//! (`len` = 24) the FIR dispatch selects `fir_interleaved_24`, whose
//! symmetric-pair summation order this port reproduces term for term
//! so results stay bit-exact against the golden vectors.

use crate::filters::HB_KERNEL_FLOAT_LEN;

/// `SIZE_FACTOR` (`iqconverter_float.c`) — FIR queue length multiplier
/// (note: 32 here, unlike the int16 converter's 16).
const SIZE_FACTOR: usize = 32;

/// `SCALE` (`iqconverter_float.c`) — the DC-removal averaging factor.
const DC_SCALE: f32 = 0.01;

/// The kernel lengths with dedicated symmetric-pair FIR
/// specializations — the `case 4/8/12/24` labels of
/// `fir_interleaved`'s switch (`iqconverter_float.c`); other lengths
/// take the generic path.
const SYMMETRIC_FIR_LENGTHS: [usize; 4] = [4, 8, 12, 24];

/// `iqconverter_float_t` — the converter's persistent state.
#[derive(Debug)]
pub(crate) struct IqConverterFloat {
    /// Non-zero half-band taps (`hb_kernel[j]`, even `j`), `len/2 + 1`.
    fir_kernel: Vec<f32>,
    /// Sliding FIR history, `len * SIZE_FACTOR` entries.
    fir_queue: Vec<f32>,
    /// Q-branch compensating delay, `len / 2` entries.
    delay_line: Vec<f32>,
    /// `hbc` — the kernel's center tap (`hb_kernel[len / 2]`), used to
    /// scale the Q phases in `translate_fs_4`.
    hbc: f32,
    fir_index: usize,
    delay_index: usize,
    /// `avg` — the DC-removal running average.
    avg: f32,
}

impl IqConverterFloat {
    /// `iqconverter_float_create`.
    pub(crate) fn new(hb_kernel: &[f32]) -> Self {
        let len = hb_kernel.len() / 2 + 1;
        Self {
            fir_kernel: hb_kernel.iter().step_by(2).copied().collect(),
            fir_queue: vec![0.0; len * SIZE_FACTOR],
            delay_line: vec![0.0; len / 2],
            hbc: hb_kernel[hb_kernel.len() / 2],
            fir_index: 0,
            delay_index: 0,
            avg: 0.0,
        }
    }

    /// `iqconverter_float_reset` — correct upstream (unlike the int16
    /// twin's), transcribed as-is.
    // Part of the C surface; the streaming engine resets via fresh
    // Scratch state, and the custom-kernel path (M4) calls this.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn reset(&mut self) {
        self.avg = 0.0;
        self.fir_index = 0;
        self.delay_index = 0;
        self.delay_line.fill(0.0);
        self.fir_queue.fill(0.0);
    }

    /// The symmetric-pair FIR shared by C's `fir_interleaved_4/8/12/24`
    /// specializations: `Σ kernel[k] * (queue[k] + queue[n-1-k])`,
    /// summed left-to-right exactly as the C expressions chain.
    fn fir_symmetric(&mut self, samples: &mut [f32]) {
        let n = self.fir_kernel.len();
        let half = n / 2;
        let mut fir_index = self.fir_index;
        for i in (0..samples.len()).step_by(2) {
            self.fir_queue[fir_index] = samples[i];
            let queue = &self.fir_queue[fir_index..fir_index + n];

            let mut acc = 0.0f32;
            for k in 0..half {
                acc += self.fir_kernel[k] * (queue[k] + queue[n - 1 - k]);
            }
            samples[i] = acc;

            if fir_index == 0 {
                fir_index = n * (SIZE_FACTOR - 1);
                self.fir_queue.copy_within(0..n - 1, fir_index + 1);
            } else {
                fir_index -= 1;
            }
        }
        self.fir_index = fir_index;
    }

    /// `process_fir_taps` (the generic path for custom kernel lengths
    /// that are not 4/8/12/24): a straight dot product in C's exact
    /// grouping — 8-tap blocks, then a 4-tap block, then a 2-tap
    /// remainder; a final odd tap is dead code upstream (commented
    /// out) and therefore ignored here too.
    fn process_fir_taps(kernel: &[f32], queue: &[f32]) -> f32 {
        let mut sum = 0.0f32;
        let mut len = kernel.len();
        let mut k = 0usize;
        while len >= 8 {
            sum += kernel[k] * queue[k]
                + kernel[k + 1] * queue[k + 1]
                + kernel[k + 2] * queue[k + 2]
                + kernel[k + 3] * queue[k + 3]
                + kernel[k + 4] * queue[k + 4]
                + kernel[k + 5] * queue[k + 5]
                + kernel[k + 6] * queue[k + 6]
                + kernel[k + 7] * queue[k + 7];
            k += 8;
            len -= 8;
        }
        if len >= 4 {
            sum += kernel[k] * queue[k]
                + kernel[k + 1] * queue[k + 1]
                + kernel[k + 2] * queue[k + 2]
                + kernel[k + 3] * queue[k + 3];
            k += 4;
            len -= 4;
        }
        if len >= 2 {
            sum += kernel[k] * queue[k] + kernel[k + 1] * queue[k + 1];
        }
        sum
    }

    /// `fir_interleaved_generic` for custom kernels.
    fn fir_generic(&mut self, samples: &mut [f32]) {
        let n = self.fir_kernel.len();
        let mut fir_index = self.fir_index;
        for i in (0..samples.len()).step_by(2) {
            self.fir_queue[fir_index] = samples[i];
            samples[i] =
                Self::process_fir_taps(&self.fir_kernel, &self.fir_queue[fir_index..fir_index + n]);
            if fir_index == 0 {
                fir_index = n * (SIZE_FACTOR - 1);
                self.fir_queue.copy_within(0..n - 1, fir_index + 1);
            } else {
                fir_index -= 1;
            }
        }
        self.fir_index = fir_index;
    }

    /// `fir_interleaved`'s dispatch: the 4/8/12/24 specializations all
    /// share the symmetric-pair form; everything else takes the
    /// generic path.
    fn fir_interleaved(&mut self, samples: &mut [f32]) {
        if SYMMETRIC_FIR_LENGTHS.contains(&self.fir_kernel.len()) {
            self.fir_symmetric(samples);
        } else {
            self.fir_generic(samples);
        }
    }

    /// `delay_interleaved` over the Q (odd) samples.
    fn delay_interleaved(&mut self, samples: &mut [f32]) {
        let half_len = self.delay_line.len();
        let mut index = self.delay_index;
        for i in (1..samples.len()).step_by(2) {
            core::mem::swap(&mut self.delay_line[index], &mut samples[i]);
            index += 1;
            if index >= half_len {
                index = 0;
            }
        }
        self.delay_index = index;
    }

    /// `remove_dc`: running-average DC removal.
    fn remove_dc(&mut self, samples: &mut [f32]) {
        let mut avg = self.avg;
        for s in samples.iter_mut() {
            *s -= avg;
            avg += DC_SCALE * *s;
        }
        self.avg = avg;
    }

    /// `translate_fs_4` (scalar path): phases `[-1, -hbc, +1, +hbc]`
    /// per 4 samples. C's `i < len / 4` loop simply skips a partial
    /// tail (no overrun here, unlike the int16 twin) — `chunks_exact`
    /// reproduces that.
    fn translate_fs_4(&mut self, samples: &mut [f32]) {
        let hbc = self.hbc;
        for chunk in samples.chunks_exact_mut(4) {
            chunk[0] = -chunk[0];
            chunk[1] = -chunk[1] * hbc;
            // chunk[2] unchanged
            chunk[3] *= hbc;
        }
        self.fir_interleaved(samples);
        self.delay_interleaved(samples);
    }

    /// `iqconverter_float_process`: in-place conversion (interleaved
    /// I/Q output, like the int16 twin).
    pub(crate) fn process(&mut self, samples: &mut [f32]) {
        self.remove_dc(samples);
        self.translate_fs_4(samples);
    }
}

/// Compile-time tie between the stock kernel length and the largest
/// FIR specialization (`len = 24`) the module docs promise.
const _: () = assert!(HB_KERNEL_FLOAT_LEN / 2 + 1 == SYMMETRIC_FIR_LENGTHS[3]);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::HB_KERNEL_FLOAT;
    use crate::test_vectors::{SCENARIOS, load_scenario};

    /// `(word - 2048) * SAMPLE_SCALE` — the consumer's float input
    /// conversion, mirroring the harness.
    #[allow(clippy::cast_precision_loss)]
    fn to_f32(words: &[u16]) -> Vec<f32> {
        words
            .iter()
            .map(|&w| (i32::from(w) - 2048) as f32 * (1.0 / 2048.0))
            .collect()
    }

    #[test]
    fn matches_c_golden_vectors_bit_for_bit() {
        for name in SCENARIOS {
            let v = load_scenario(name);
            assert_eq!(v.input.len(), v.float.len(), "{name}: fixture lengths");
            assert_eq!(v.input.len() % 2048, 0, "{name}: block alignment");
            let mut cnv = IqConverterFloat::new(&HB_KERNEL_FLOAT);
            let mut samples = to_f32(&v.input);
            for (bi, (block, expected)) in samples
                .chunks_mut(2048)
                .zip(v.float.chunks(2048))
                .enumerate()
            {
                cnv.process(block);
                for (i, (got, want)) in block.iter().zip(expected.iter()).enumerate() {
                    assert!(
                        got.to_bits() == want.to_bits(),
                        "{name} block {bi} sample {i}: got {got:e} ({:#010x}) want {want:e} ({:#010x})",
                        got.to_bits(),
                        want.to_bits()
                    );
                }
            }
        }
    }

    #[test]
    fn reset_restores_fresh_converter_behavior() {
        let v = load_scenario("noise");
        let mut a = to_f32(&v.input[..2048]);
        let mut b = a.clone();
        let mut cnv = IqConverterFloat::new(&HB_KERNEL_FLOAT);
        cnv.process(&mut a);
        let mut scratch = to_f32(&v.input[2048..4096]);
        cnv.process(&mut scratch);
        cnv.reset();
        cnv.process(&mut b);
        assert_eq!(
            a.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
            b.iter().map(|f| f.to_bits()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn kernel_shape_and_center_tap() {
        let cnv = IqConverterFloat::new(&HB_KERNEL_FLOAT);
        assert_eq!(cnv.fir_kernel.len(), 24);
        assert_eq!(cnv.delay_line.len(), 12);
        assert!((cnv.hbc - 0.5).abs() < f32::EPSILON, "hbc = center tap");
    }
}
