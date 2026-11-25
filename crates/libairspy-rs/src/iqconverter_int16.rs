//! Fixed-point IQ converter, a faithful port of
//! `iqconverter_int16.c` from `airspyone_host` at the all-MIT
//! reference revision `bd15be38` (see NOTICE):
//!
//! ```text
//! Copyright (C) 2014, Youssef Touil <youssef@airspy.com>
//!
//! Permission is hereby granted, free of charge, to any person
//! obtaining a copy of this software and associated documentation
//! files (the "Software"), to deal in the Software without
//! restriction, including without limitation the rights to use, copy,
//! modify, merge, publish, distribute, sublicense, and/or sell copies
//! of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be
//! included in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//! EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
//! HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
//! WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! DEALINGS IN THE SOFTWARE.
//! ```
//!
//! Pipeline (interleaved in place, `len` real samples → `len/2` IQ
//! pairs): DC removal → fs/4 translation → half-band FIR on the I
//! branch + compensating delay on the Q branch. Arithmetic uses
//! wrapping ops wherever C relies on implicit `int` truncation, so
//! outputs are bit-exact against the golden vectors.

// Truncating casts below are the C `int → int16_t` conversions; each
// site is a transcription of the corresponding C expression.
#![allow(clippy::cast_possible_truncation)]

/// `SIZE_FACTOR` (`iqconverter_int16.c`) — FIR queue length multiplier.
const SIZE_FACTOR: usize = 16;

/// The DC-blocker pole coefficient in Q15 — the literal `32100` in
/// `remove_dc` (`iqconverter_int16.c`).
const DC_POLE_Q15: i32 = 32100;

/// The Q15 fixed-point shift used by both the DC blocker and the FIR
/// accumulator (`>> 15` in `remove_dc` / `fir_interleaved`,
/// `iqconverter_int16.c`).
const Q15_SHIFT: i32 = 15;

/// `iqconverter_int16_t` — the converter's persistent state.
#[derive(Debug)]
pub(crate) struct IqConverterInt16 {
    /// Non-zero half-band taps (`hb_kernel[i * 2]`), `len/2 + 1` of them.
    fir_kernel: Vec<i32>,
    /// Sliding FIR history, `len * SIZE_FACTOR` entries.
    fir_queue: Vec<i32>,
    /// Q-branch compensating delay, `len / 2` entries.
    delay_line: Vec<i16>,
    fir_index: usize,
    delay_index: usize,
    old_x: i16,
    old_y: i16,
    old_e: i32,
}

impl IqConverterInt16 {
    /// `iqconverter_int16_create`: keep every other kernel tap
    /// (the half-band zeros drop out).
    pub(crate) fn new(hb_kernel: &[i16]) -> Self {
        let len = hb_kernel.len() / 2 + 1;
        Self {
            fir_kernel: hb_kernel.iter().step_by(2).map(|&t| i32::from(t)).collect(),
            fir_queue: vec![0; len * SIZE_FACTOR],
            // C allocates len/2 entries and, in practice, gets zeroed
            // pages; Rust zeroes explicitly (see `reset`).
            delay_line: vec![0; len / 2],
            fir_index: 0,
            delay_index: 0,
            old_x: 0,
            old_y: 0,
            old_e: 0,
        }
    }

    /// `iqconverter_int16_reset`.
    // Part of the C surface (airspy_start_rx resets converters); the
    // streaming engine currently resets by constructing fresh Scratch
    // state, and the custom-kernel path (M4) calls this directly.
    #[cfg_attr(not(test), allow(dead_code))]
    ///
    /// Deviation: C's memsets use `sizeof(int16_t)` for the
    /// `int32_t` FIR queue and quarter the delay-line size, clearing
    /// only half of each buffer — a reset there leaves stale filter
    /// state behind (masked at create time by fresh-zeroed heap
    /// pages). This port clears the full state so `reset` genuinely
    /// restores fresh-converter behavior.
    pub(crate) fn reset(&mut self) {
        self.fir_index = 0;
        self.delay_index = 0;
        self.old_x = 0;
        self.old_y = 0;
        self.old_e = 0;
        self.delay_line.fill(0);
        self.fir_queue.fill(0);
    }

    /// `fir_interleaved`: half-band FIR over the I (even) samples via
    /// a backward-sliding queue with block copy-back on wrap.
    fn fir_interleaved(&mut self, samples: &mut [i16]) {
        let fir_len = self.fir_kernel.len();
        let mut fir_index = self.fir_index;
        for i in (0..samples.len()).step_by(2) {
            self.fir_queue[fir_index] = i32::from(samples[i]);

            let mut acc: i32 = 0;
            for j in 0..fir_len {
                acc = acc
                    .wrapping_add(self.fir_kernel[j].wrapping_mul(self.fir_queue[fir_index + j]));
            }

            if fir_index == 0 {
                fir_index = fir_len * (SIZE_FACTOR - 1);
                // memcpy(queue + fir_index + 1, queue, (len - 1) * 4)
                self.fir_queue.copy_within(0..fir_len - 1, fir_index + 1);
            } else {
                fir_index -= 1;
            }

            samples[i] = (acc >> Q15_SHIFT) as i16;
        }
        self.fir_index = fir_index;
    }

    /// `delay_interleaved` over the Q (odd) samples: a circular
    /// `len/2`-sample delay.
    fn delay_interleaved(&mut self, samples: &mut [i16]) {
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

    /// `remove_dc`: DC-blocking IIR with error feedback, all in the
    /// exact C integer widths.
    fn remove_dc(&mut self, samples: &mut [i16]) {
        let mut old_x = self.old_x;
        let mut old_y = self.old_y;
        let mut old_e = self.old_e;
        for s in samples.iter_mut() {
            let x = *s;
            let w = (i32::from(x) - i32::from(old_x)) as i16;
            let u = old_e.wrapping_add(i32::from(old_y).wrapping_mul(DC_POLE_Q15));
            let sh = (u >> Q15_SHIFT) as i16;
            let y = (i32::from(w) + i32::from(sh)) as i16;
            old_e = u.wrapping_sub(i32::from(sh) << Q15_SHIFT);
            old_x = x;
            old_y = y;
            *s = y;
        }
        self.old_x = old_x;
        self.old_y = old_y;
        self.old_e = old_e;
    }

    /// `translate_fs_4`: the (-1, -j, +1, +j) multiply expressed as
    /// sign/shift per 4 samples, then FIR (I) + delay (Q). Like C, the
    /// phase restarts at zero on every call (the C struct keeps no
    /// phase state).
    fn translate_fs_4(&mut self, samples: &mut [i16]) {
        apply_fs_4_phases(samples);
        self.fir_interleaved(samples);
        self.delay_interleaved(samples);
    }

    /// `iqconverter_int16_process`: in-place conversion of `samples`
    /// (interleaved output: I at even, Q at odd indices).
    pub(crate) fn process(&mut self, samples: &mut [i16]) {
        self.remove_dc(samples);
        self.translate_fs_4(samples);
    }
}

/// The per-4-sample sign/shift pattern from `translate_fs_4`
/// (`iqconverter_int16.c`). Deviation from C on a partial tail
/// (packed-mode counts are ≡ 2 mod 4): C's `i += 4` loop body writes
/// the two out-of-bounds slots past such a buffer — another upstream
/// overrun this port does not reproduce; the in-bounds tail samples
/// get their phase-0/phase-1 treatment exactly as C applies it.
fn apply_fs_4_phases(samples: &mut [i16]) {
    let mut chunks = samples.chunks_exact_mut(4);
    for chunk in chunks.by_ref() {
        chunk[0] = (-i32::from(chunk[0])) as i16;
        chunk[1] = (-i32::from(chunk[1]) >> 1) as i16;
        // chunk[2] unchanged
        chunk[3] = (i32::from(chunk[3]) >> 1) as i16;
    }
    let tail = chunks.into_remainder();
    if let Some(s) = tail.first_mut() {
        *s = (-i32::from(*s)) as i16;
    }
    if let Some(s) = tail.get_mut(1) {
        *s = (-i32::from(*s) >> 1) as i16;
    }
    // A 3-sample tail's phase 2 is unchanged, like the full pattern.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::HB_KERNEL_INT16;
    use crate::test_vectors::{SCENARIOS, load_scenario};

    /// `(word - 2048) << SAMPLE_SHIFT` — the consumer's int16 input
    /// conversion, mirroring the harness.
    fn to_i16(words: &[u16]) -> Vec<i16> {
        words
            .iter()
            .map(|&w| ((i32::from(w) - 2048) << 4) as i16)
            .collect()
    }

    #[test]
    fn matches_c_golden_vectors_bit_for_bit() {
        for name in SCENARIOS {
            let v = load_scenario(name);
            // Guard against silently-truncated fixtures: both
            // sequences must be full and block-aligned before zip.
            assert_eq!(v.input.len(), v.int16.len(), "{name}: fixture lengths");
            assert_eq!(v.input.len() % 2048, 0, "{name}: block alignment");
            let mut cnv = IqConverterInt16::new(&HB_KERNEL_INT16);
            let mut samples = to_i16(&v.input);
            // Three sequential 2048-sample blocks, exactly like the
            // harness — state must persist across calls.
            for (block, expected) in samples.chunks_mut(2048).zip(v.int16.chunks(2048)) {
                cnv.process(block);
                assert_eq!(block, expected, "{name}: block mismatch");
            }
        }
    }

    #[test]
    fn fs4_phase_pattern_covers_partial_tails() {
        // Full pattern: [-x0, -x1>>1, x2, x3>>1]; a 2-sample tail gets
        // phases 0 and 1 (C applies the same but overruns the buffer).
        let mut six = [1000i16, 1000, 1000, 1000, 500, 600];
        apply_fs_4_phases(&mut six);
        assert_eq!(six, [-1000, -500, 1000, 500, -500, -300]);

        let mut seven = [8i16, 8, 8, 8, 8, 8, 8];
        apply_fs_4_phases(&mut seven);
        assert_eq!(
            &seven[4..],
            &[-8, -4, 8],
            "3-tail: phases 0, 1, and unchanged 2"
        );
    }

    #[test]
    fn reset_restores_fresh_converter_behavior() {
        let v = load_scenario("noise");
        let mut samples_a = to_i16(&v.input[..2048]);
        let mut samples_b = samples_a.clone();

        let mut cnv = IqConverterInt16::new(&HB_KERNEL_INT16);
        cnv.process(&mut samples_a);

        // Pollute state with different data, then reset.
        let mut scratch = to_i16(&v.input[2048..4096]);
        cnv.process(&mut scratch);
        cnv.reset();
        cnv.process(&mut samples_b);

        assert_eq!(samples_a, samples_b);
    }

    #[test]
    fn kernel_decimation_keeps_nonzero_taps() {
        // create() keeps every other tap: 47 → 24, ending at the
        // center value 16384 in slot 23/… the layout matters only to
        // the FIR loop, but the derived length is load-bearing.
        let cnv = IqConverterInt16::new(&HB_KERNEL_INT16);
        assert_eq!(cnv.fir_kernel.len(), 24);
        assert_eq!(cnv.delay_line.len(), 12);
    }
}
