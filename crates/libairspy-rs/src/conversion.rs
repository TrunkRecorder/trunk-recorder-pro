//! Sample-format conversion, starting with the 12-bit packed-mode
//! unpacker (`unpack_samples` in airspy.c). The IQ converters land
//! here with the DSP milestone.

/// Bytes per packed 32-bit word (`uint32_t *input` in
/// `unpack_samples`, airspy.c).
const WORD_BYTES: usize = 4;
/// Bytes consumed per unpacked group — three 32-bit words (`i += 3`
/// in `unpack_samples`, airspy.c).
const BYTES_PER_GROUP: usize = 3 * WORD_BYTES;
/// Samples produced per group (`j += 8` in `unpack_samples`,
/// airspy.c).
const SAMPLES_PER_GROUP: usize = 8;

/// Unpack 12-bit packed samples from the raw USB byte stream: each
/// 12-byte group (three little-endian 32-bit words) yields eight
/// 12-bit samples, bit layout transcribed from `unpack_samples`
/// (airspy.c). C casts the byte buffer to `uint32_t*` and relies on a
/// little-endian host; loading through `u32::from_le_bytes` gives the
/// identical values portably and without alignment requirements.
///
/// Returns the number of samples written — `input_bytes * 2 / 3`,
/// matching C's packed `sample_count` (174762 for a 262144-byte
/// buffer). The final partial group's samples are decoded from its
/// in-bounds words only; C instead runs its full 8-sample loop body
/// there, reading past the input and writing past the output (an
/// upstream out-of-bounds this port does not reproduce).
// Every assembled value is at most 12 bits ((0xFF << 4) | 0xF =
// 0xFFF), but clippy cannot prove it through the shift-or pairs.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn unpack_samples(input: &[u8], output: &mut [u16]) -> usize {
    let groups = core::cmp::min(
        input.len() / BYTES_PER_GROUP,
        output.len() / SAMPLES_PER_GROUP,
    );
    let word = |chunk: &[u8], i: usize| -> u32 {
        // Infallible: chunks_exact guarantees 12 bytes per group.
        chunk
            .get(i * 4..i * 4 + 4)
            .and_then(|b| b.try_into().ok())
            .map_or(0, u32::from_le_bytes)
    };
    for (bytes, samples) in input
        .chunks_exact(BYTES_PER_GROUP)
        .zip(output.chunks_exact_mut(SAMPLES_PER_GROUP))
    {
        let (w0, w1, w2) = (word(bytes, 0), word(bytes, 1), word(bytes, 2));
        // Bit expressions transcribed 1:1 from the C loop body; the
        // literal masks/shifts keep the side-by-side parity audit
        // against airspy.c trivial.
        samples[0] = ((w0 >> 20) & 0xFFF) as u16;
        samples[1] = ((w0 >> 8) & 0xFFF) as u16;
        samples[2] = (((w0 & 0xFF) << 4) | ((w1 >> 28) & 0xF)) as u16;
        samples[3] = ((w1 & 0x0FFF_0000) >> 16) as u16;
        samples[4] = ((w1 & 0xFFF0) >> 4) as u16;
        samples[5] = (((w1 & 0xF) << 8) | ((w2 & 0xFF00_0000) >> 24)) as u16;
        samples[6] = ((w2 >> 12) & 0xFFF) as u16;
        samples[7] = (w2 & 0xFFF) as u16;
    }

    // Partial tail: decode the samples fully contained in the
    // remaining in-bounds words (a 4-byte tail carries s0/s1, an
    // 8-byte tail s0..s4 of the C layout above). This is how the port
    // reaches C's bytes*2/3 count without C's overrun.
    let mut written = groups * SAMPLES_PER_GROUP;
    // Same bit expressions as the group loop above (s0..s4 of the C
    // layout), applied to however many complete tail words exist.
    let tail = &input[groups * BYTES_PER_GROUP..];
    if tail.len() >= WORD_BYTES {
        let w0 = word(tail, 0);
        let mut push = |v: u32| {
            if let Some(slot) = output.get_mut(written) {
                *slot = v as u16;
                written += 1;
            }
        };
        push((w0 >> 20) & 0xFFF);
        push((w0 >> 8) & 0xFFF);
        if tail.len() >= 2 * WORD_BYTES {
            let w1 = word(tail, 1);
            push(((w0 & 0xFF) << 4) | ((w1 >> 28) & 0xF));
            push((w1 & 0x0FFF_0000) >> 16);
            push((w1 & 0xFFF0) >> 4);
            if tail.len() >= BYTES_PER_GROUP {
                // A full group landed in the tail because the output
                // bound (not the input) limited the group loop.
                let w2 = word(tail, 2);
                push(((w1 & 0xF) << 8) | ((w2 & 0xFF00_0000) >> 24));
                push((w2 >> 12) & 0xFFF);
                push(w2 & 0xFFF);
            }
        }
    }
    written
}

/// The sample count [`unpack_samples`] yields for `len` packed bytes —
/// C's `((buffer_size / 2) * 4) / 3` generalized: `len * 2 / 3`.
pub(crate) const fn unpacked_sample_count(len: usize) -> usize {
    len * 2 / 3
}

use crate::commands::SampleType;
use crate::filters::{HB_KERNEL_FLOAT, HB_KERNEL_INT16};

/// Values per complex sample — IQ slices interleave I and Q, and C
/// halves `sample_count` for the IQ types (`consumer_threadproc`,
/// airspy.c).
const IQ_COMPONENTS: usize = 2;
use crate::iqconverter_float::IqConverterFloat;
use crate::iqconverter_int16::IqConverterInt16;

/// `SAMPLE_SHIFT` (airspy.c): `SAMPLE_ENCAPSULATION (16) -
/// SAMPLE_RESOLUTION (12)`.
const SAMPLE_SHIFT: i32 = 4;

/// `SAMPLE_SCALE` (airspy.c): `1.0f / (1 << (15 - SAMPLE_SHIFT))`.
const SAMPLE_SCALE: f32 = 1.0 / 2048.0;

/// `convert_samples_int16` (airspy.c): `(src - 2048) << SAMPLE_SHIFT`,
/// computed in `int` and stored as `int16_t`.
// Truncation is the C cast: the shifted value spans -32768..32752.
#[allow(clippy::cast_possible_truncation)]
fn convert_samples_int16(src: &[u16], dest: &mut [i16]) {
    for (s, d) in src.iter().zip(dest.iter_mut()) {
        *d = ((i32::from(*s) - 2048) << SAMPLE_SHIFT) as i16;
    }
}

/// `convert_samples_float` (airspy.c): `(src - 2048) * SAMPLE_SCALE`.
#[allow(clippy::cast_precision_loss)]
fn convert_samples_float(src: &[u16], dest: &mut [f32]) {
    for (s, d) in src.iter().zip(dest.iter_mut()) {
        *d = (i32::from(*s) - 2048) as f32 * SAMPLE_SCALE;
    }
}

/// One block of delivered samples in the format selected via
/// [`SampleType`] — the typed face of C's `airspy_transfer.samples`
/// `void*`.
#[derive(Debug)]
pub enum Samples<'a> {
    /// `AIRSPY_SAMPLE_FLOAT32_REAL` / `AIRSPY_SAMPLE_FLOAT32_IQ`
    /// (IQ slices interleave I and Q).
    Float32(&'a [f32]),
    /// `AIRSPY_SAMPLE_INT16_REAL` / `AIRSPY_SAMPLE_INT16_IQ`
    /// (IQ slices interleave I and Q).
    Int16(&'a [i16]),
    /// `AIRSPY_SAMPLE_UINT16_REAL` — raw ADC words.
    Uint16(&'a [u16]),
    /// `AIRSPY_SAMPLE_RAW` — the untouched bulk bytes.
    Raw(&'a [u8]),
}

/// Reusable consumer-thread buffers and converter state — C's
/// `unpacked_samples`, `output_buffer`, and `cnv_i` (created at open
/// with the stock kernels, reset per `airspy_start_rx`; a fresh
/// `Scratch` per stream is that reset).
#[derive(Debug)]
pub(crate) struct Scratch {
    unpacked: Vec<u16>,
    words: Vec<u16>,
    out_i16: Vec<i16>,
    out_f32: Vec<f32>,
    cnv_i: IqConverterInt16,
    cnv_f: IqConverterFloat,
}

impl Default for Scratch {
    fn default() -> Self {
        Self::with_kernels(&HB_KERNEL_FLOAT, &HB_KERNEL_INT16)
    }
}

impl Scratch {
    /// Empty buffers and fresh converters over the given half-band
    /// kernels (the stock ones unless `set_conversion_filter_*` ran).
    fn with_kernels(kernel_float: &[f32], kernel_int16: &[i16]) -> Self {
        Self {
            unpacked: Vec::new(),
            words: Vec::new(),
            out_i16: Vec::new(),
            out_f32: Vec::new(),
            cnv_i: IqConverterInt16::new(kernel_int16),
            cnv_f: IqConverterFloat::new(kernel_float),
        }
    }

    /// Preallocate for a stream delivering `buffer_len`-byte blocks in
    /// the latched format, so the consumer loop never grows a vector
    /// on the hot path (C allocates its counterparts at open).
    pub(crate) fn for_stream(
        sample_type: SampleType,
        packing_enabled: bool,
        buffer_len: usize,
        kernel_float: &[f32],
        kernel_int16: &[i16],
    ) -> Self {
        let word_count = if packing_enabled {
            unpacked_sample_count(buffer_len)
        } else {
            buffer_len / 2
        };
        let mut scratch = Self::with_kernels(kernel_float, kernel_int16);
        if packing_enabled {
            scratch.unpacked.reserve(word_count);
        } else {
            scratch.words.reserve(word_count);
        }
        match sample_type {
            SampleType::Int16Real | SampleType::Int16Iq => scratch.out_i16.reserve(word_count),
            SampleType::Float32Real | SampleType::Float32Iq => {
                scratch.out_f32.reserve(word_count);
            }
            SampleType::Uint16Real | SampleType::Raw => {}
        }
        scratch
    }
}

/// The sample-type dispatch from `consumer_threadproc` (airspy.c):
/// optional 12-bit unpack, then per-type conversion. Returns the
/// typed samples plus the sample count C would report for the block —
/// halved for IQ types, whose slices hold `count` interleaved I/Q
/// pairs (`2 × count` values).
pub(crate) fn convert_block<'a>(
    sample_type: SampleType,
    packing_enabled: bool,
    bytes: &'a [u8],
    scratch: &'a mut Scratch,
) -> (Samples<'a>, usize) {
    // RAW bypasses all preparation.
    if matches!(sample_type, SampleType::Raw) {
        // C sets the packed sample_count before the RAW branch skips
        // unpacking, so a packed RAW stream reports the unpacked
        // count (174762 per full buffer), not bytes/2.
        let count = if packing_enabled {
            unpacked_sample_count(bytes.len())
        } else {
            bytes.len() / 2
        };
        return (Samples::Raw(bytes), count);
    }
    let words_len = prepare_words(packing_enabled, bytes, scratch);
    // Field destructuring splits the borrows: the source word buffer
    // stays shared while the per-type output buffer is written.
    let Scratch {
        unpacked,
        words,
        out_i16,
        out_f32,
        cnv_i,
        cnv_f,
    } = scratch;
    let src: &[u16] = if packing_enabled {
        &unpacked[..words_len]
    } else {
        &words[..words_len]
    };
    match sample_type {
        SampleType::Raw => unreachable!("handled by the early return above"),
        SampleType::Float32Iq => {
            // C: convert_samples_float → iqconverter_float_process →
            // sample_count /= 2; complete pairs only, like Int16Iq.
            out_f32.resize(words_len, 0.0);
            convert_samples_float(src, out_f32);
            cnv_f.process(out_f32);
            let pairs = words_len / IQ_COMPONENTS;
            (Samples::Float32(&out_f32[..pairs * IQ_COMPONENTS]), pairs)
        }
        SampleType::Int16Iq => {
            // C: convert_samples_int16 → iqconverter_int16_process →
            // sample_count /= 2. The converter consumes every word
            // (state parity with C), but the delivered slice holds
            // only complete IQ pairs — an odd word count would
            // otherwise dangle an unpaired I component.
            out_i16.resize(words_len, 0);
            convert_samples_int16(src, out_i16);
            cnv_i.process(out_i16);
            let pairs = words_len / IQ_COMPONENTS;
            (Samples::Int16(&out_i16[..pairs * IQ_COMPONENTS]), pairs)
        }
        SampleType::Uint16Real => (Samples::Uint16(src), words_len),
        SampleType::Int16Real => {
            out_i16.resize(words_len, 0);
            convert_samples_int16(src, out_i16);
            (Samples::Int16(out_i16), words_len)
        }
        SampleType::Float32Real => {
            out_f32.resize(words_len, 0.0);
            convert_samples_float(src, out_f32);
            (Samples::Float32(out_f32), words_len)
        }
    }
}

/// The stream-format half of `consumer_threadproc`'s dispatch: fill
/// the scratch word buffer from the raw bytes (12-bit unpack in packed
/// mode, little-endian u16 view otherwise) and return the word count.
fn prepare_words(packing_enabled: bool, bytes: &[u8], scratch: &mut Scratch) -> usize {
    if packing_enabled {
        scratch
            .unpacked
            .resize(unpacked_sample_count(bytes.len()), 0);
        unpack_samples(bytes, &mut scratch.unpacked)
    } else {
        // C casts the buffer to uint16_t*; we copy into the reusable
        // scratch through from_le_bytes.
        scratch.words.clear();
        scratch.words.extend(
            bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]])),
        );
        scratch.words.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::SampleType;

    /// Test-only inverse of the unpacker: pack eight 12-bit samples
    /// into three words exactly as the firmware lays them out
    /// (derived by inverting the C bit expressions).
    fn pack_group(s: [u16; 8]) -> Vec<u8> {
        let s: Vec<u32> = s.iter().map(|&v| u32::from(v & 0x0FFF)).collect();
        let words = [
            (s[0] << 20) | (s[1] << 8) | (s[2] >> 4),
            ((s[2] & 0xF) << 28) | (s[3] << 16) | (s[4] << 4) | (s[5] >> 8),
            ((s[5] & 0xFF) << 24) | (s[6] << 12) | s[7],
        ];
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn unpacks_one_group_matching_c_bit_layout() {
        let samples = [0x123, 0x456, 0x789, 0xABC, 0xDEF, 0x135, 0x790, 0x246];
        let input = pack_group(samples);
        let mut output = [0u16; 8];
        assert_eq!(unpack_samples(&input, &mut output), 8);
        assert_eq!(output, samples);
    }

    #[test]
    fn unpacks_hardcoded_fixture_derived_from_c() {
        // Independent of pack_group: words 0x12345678, 0x9ABCDEF0,
        // 0x13579BDF pushed through the C expressions by hand —
        //   s0=(w0>>20)&FFF=0x123      s1=(w0>>8)&FFF=0x456
        //   s2=((w0&FF)<<4)|(w1>>28)=0x789
        //   s3=(w1&0FFF0000)>>16=0xABC s4=(w1&FFF0)>>4=0xDEF
        //   s5=((w1&F)<<8)|(w2>>24)=0x013
        //   s6=(w2>>12)&FFF=0x579      s7=w2&FFF=0xBDF
        let input: [u8; 12] = [
            0x78, 0x56, 0x34, 0x12, // w0 LE
            0xF0, 0xDE, 0xBC, 0x9A, // w1 LE
            0xDF, 0x9B, 0x57, 0x13, // w2 LE
        ];
        let mut output = [0u16; 8];
        assert_eq!(unpack_samples(&input, &mut output), 8);
        assert_eq!(
            output,
            [0x123, 0x456, 0x789, 0xABC, 0xDEF, 0x013, 0x579, 0xBDF]
        );
    }

    #[test]
    fn unpacks_multiple_groups_in_order() {
        let a = [1, 2, 3, 4, 5, 6, 7, 8];
        let b = [0xFFF, 0, 0xAAA, 0x555, 0x0F0, 0xF0F, 0x800, 0x7FF];
        let mut input = Vec::new();
        input.extend_from_slice(&pack_group(a));
        input.extend_from_slice(&pack_group(b));
        let mut output = [0u16; 16];
        assert_eq!(unpack_samples(&input, &mut output), 16);
        assert_eq!(&output[..8], &a);
        assert_eq!(&output[8..], &b);
    }

    #[test]
    fn decodes_partial_tail_without_overrun() {
        // C's packed count is bytes*2/3 (174762 for a 262144-byte
        // buffer): the final partial group's in-bounds words carry
        // real samples. A 4-byte tail yields s0/s1 of that group; an
        // 8-byte tail yields s0..s4; C's OOB reads for the rest are
        // not reproduced.
        let mut input = pack_group([9, 8, 7, 6, 5, 4, 3, 2]);
        // 4-byte tail: w0 = 0x12345678 → s0=0x123, s1=0x456.
        input.extend_from_slice(&[0x78, 0x56, 0x34, 0x12]);
        let mut output = [0u16; 16];
        assert_eq!(unpack_samples(&input, &mut output), 10);
        assert_eq!(&output[..8], &[9, 8, 7, 6, 5, 4, 3, 2]);
        assert_eq!(&output[8..10], &[0x123, 0x456]);

        // 8-byte tail: w0=0x12345678, w1=0x9ABCDEF0 → s0..s4.
        let mut input8 = pack_group([9, 8, 7, 6, 5, 4, 3, 2]);
        input8.extend_from_slice(&[0x78, 0x56, 0x34, 0x12, 0xF0, 0xDE, 0xBC, 0x9A]);
        let mut output = [0u16; 16];
        assert_eq!(unpack_samples(&input8, &mut output), 13);
        assert_eq!(&output[8..13], &[0x123, 0x456, 0x789, 0xABC, 0xDEF]);

        // Output shorter than available samples: tail fills what fits.
        let mut short_out = [0u16; 11];
        assert_eq!(unpack_samples(&input8, &mut short_out), 11);
        assert_eq!(&short_out[8..], &[0x123, 0x456, 0x789]);
    }

    #[test]
    fn output_bounded_decoding_fills_every_slot() {
        // With ample input, an output of 7 gets 7 samples (s0..s6 of
        // the first group), and an output of 15 gets a full group plus
        // s0..s6 of the next — the output bound, not the group shape,
        // limits what is written.
        let g1 = [0x111, 0x222, 0x333, 0x444, 0x555, 0x666, 0x777, 0x888];
        let g2 = [0x999, 0xAAA, 0xBBB, 0xCCC, 0xDDD, 0xEEE, 0xFFF, 0x123];
        let mut input = pack_group(g1);
        input.extend_from_slice(&pack_group(g2));

        let mut out7 = [0u16; 7];
        assert_eq!(unpack_samples(&input, &mut out7), 7);
        assert_eq!(out7, g1[..7]);

        let mut out15 = [0u16; 15];
        assert_eq!(unpack_samples(&input, &mut out15), 15);
        assert_eq!(&out15[..8], &g1);
        assert_eq!(&out15[8..], &g2[..7]);
    }

    #[test]
    fn full_stream_buffer_matches_c_sample_count() {
        // 262144-byte packed buffer → C's ((buffer_size/2)*4)/3 =
        // 174762 samples, now reached without C's overrun.
        let input = vec![0u8; 262_144];
        let mut output = vec![0u16; 175_000];
        assert_eq!(unpack_samples(&input, &mut output), 174_762);
    }

    #[test]
    fn empty_input_or_output_writes_nothing() {
        let mut output = [0u16; 8];
        assert_eq!(unpack_samples(&[], &mut output), 0);
        assert_eq!(unpack_samples(&[1u8; 12], &mut []), 0);
    }

    #[test]
    fn all_ones_and_all_zeros_survive() {
        let ones = [0xFFF_u16; 8];
        let input = pack_group(ones);
        assert_eq!(input, vec![0xFF; 12]); // fully-set bytes
        let mut output = [0u16; 8];
        unpack_samples(&input, &mut output);
        assert_eq!(output, ones);

        let mut output = [0xBEEF_u16; 8];
        unpack_samples(&[0u8; 12], &mut output);
        assert_eq!(output, [0u16; 8]);
    }

    #[test]
    fn scale_and_shift_match_c() {
        // SAMPLE_SHIFT = 16 - 12 = 4; SAMPLE_SCALE = 1/(1 << 11).
        assert_eq!(SAMPLE_SHIFT, 4);
        assert!((SAMPLE_SCALE - 1.0 / 2048.0).abs() < f32::EPSILON);
    }

    #[test]
    fn int16_conversion_matches_c_arithmetic() {
        // C: dest = (src - 2048) << 4, computed in int then stored i16.
        let src = [0u16, 2048, 4095, 2049];
        let mut dest = [0i16; 4];
        convert_samples_int16(&src, &mut dest);
        assert_eq!(dest, [-32768, 0, 32752, 16]);
    }

    #[test]
    fn float_conversion_matches_c_arithmetic() {
        // C: dest = (src - 2048) * (1/2048.0f).
        let src = [0u16, 2048, 4095];
        let mut dest = [0f32; 3];
        convert_samples_float(&src, &mut dest);
        assert!((dest[0] + 1.0).abs() < f32::EPSILON);
        assert!(dest[1].abs() < f32::EPSILON);
        assert!((dest[2] - 2047.0 / 2048.0).abs() < f32::EPSILON);
    }

    fn le_bytes(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn dispatch_raw_passes_bytes_through() {
        let bytes = [1u8, 2, 3, 4];
        let mut scratch = Scratch::default();
        let (samples, count) = convert_block(SampleType::Raw, false, &bytes, &mut scratch);
        assert!(matches!(samples, Samples::Raw(b) if b == bytes));
        assert_eq!(count, 2); // buffer_size / 2 semantics: u16 count
    }

    #[test]
    fn dispatch_uint16_real_yields_u16_words() {
        let bytes = le_bytes(&[100, 4095, 0, 2048]);
        let mut scratch = Scratch::default();
        let (samples, count) = convert_block(SampleType::Uint16Real, false, &bytes, &mut scratch);
        assert_eq!(count, 4);
        assert!(matches!(samples, Samples::Uint16(w) if w == [100, 4095, 0, 2048]));
    }

    #[test]
    fn dispatch_int16_real_converts() {
        let bytes = le_bytes(&[0, 2048, 4095, 2049]);
        let mut scratch = Scratch::default();
        let (samples, count) = convert_block(SampleType::Int16Real, false, &bytes, &mut scratch);
        assert_eq!(count, 4);
        assert!(matches!(samples, Samples::Int16(w) if w == [-32768, 0, 32752, 16]));
    }

    #[test]
    fn dispatch_float32_real_converts() {
        let bytes = le_bytes(&[2048, 0]);
        let mut scratch = Scratch::default();
        let (samples, count) = convert_block(SampleType::Float32Real, false, &bytes, &mut scratch);
        assert_eq!(count, 2);
        let Samples::Float32(w) = samples else {
            unreachable!("expected float samples");
        };
        assert!(w[0].abs() < f32::EPSILON);
        assert!((w[1] + 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dispatch_int16_iq_matches_golden_vectors() {
        // consumer_threadproc INT16_IQ: convert_samples_int16 →
        // iqconverter_int16_process → sample_count/2 (the buffer keeps
        // len interleaved I/Q values).
        use crate::test_vectors::load_scenario;
        let v = load_scenario("noise");
        let bytes: Vec<u8> = v.input[..2048]
            .iter()
            .flat_map(|&w| w.to_le_bytes())
            .collect();
        let mut scratch = Scratch::default();
        let (samples, count) = convert_block(SampleType::Int16Iq, false, &bytes, &mut scratch);
        assert_eq!(count, 1024, "IQ halves the sample count");
        let Samples::Int16(out) = samples else {
            unreachable!("Int16Iq delivers i16 samples");
        };
        assert_eq!(out, &v.int16[..2048], "golden-vector mismatch");
    }

    #[test]
    fn dispatch_int16_iq_state_persists_across_blocks() {
        use crate::test_vectors::load_scenario;
        let v = load_scenario("tone");
        let mut scratch = Scratch::default();
        for block in 0..3 {
            let range = block * 2048..(block + 1) * 2048;
            let bytes: Vec<u8> = v.input[range.clone()]
                .iter()
                .flat_map(|&w| w.to_le_bytes())
                .collect();
            let (samples, _) = convert_block(SampleType::Int16Iq, false, &bytes, &mut scratch);
            let Samples::Int16(out) = samples else {
                unreachable!("Int16Iq delivers i16 samples");
            };
            assert_eq!(out, &v.int16[range], "block {block} mismatch");
        }
    }

    #[test]
    fn dispatch_int16_iq_delivers_only_complete_pairs() {
        // Eight packed bytes decode to five words: the converter
        // consumes all five, but the callback slice holds two complete
        // IQ pairs (four values).
        let packed_tail: [u8; 8] = [0x78, 0x56, 0x34, 0x12, 0xF0, 0xDE, 0xBC, 0x9A];
        let mut scratch = Scratch::default();
        let (samples, count) = convert_block(SampleType::Int16Iq, true, &packed_tail, &mut scratch);
        assert_eq!(count, 2);
        let Samples::Int16(out) = samples else {
            unreachable!("Int16Iq delivers i16 samples");
        };
        assert_eq!(out.len(), 4, "only complete IQ pairs delivered");
    }

    #[test]
    fn dispatch_float32_iq_matches_golden_vectors() {
        use crate::test_vectors::load_scenario;
        let v = load_scenario("noise");
        let mut scratch = Scratch::default();
        for block in 0..3 {
            let range = block * 2048..(block + 1) * 2048;
            let bytes: Vec<u8> = v.input[range.clone()]
                .iter()
                .flat_map(|&w| w.to_le_bytes())
                .collect();
            let (samples, count) =
                convert_block(SampleType::Float32Iq, false, &bytes, &mut scratch);
            assert_eq!(count, 1024, "IQ halves the sample count");
            let Samples::Float32(out) = samples else {
                unreachable!("Float32Iq delivers f32 samples");
            };
            let got: Vec<u32> = out.iter().map(|f| f.to_bits()).collect();
            let want: Vec<u32> = v.float[range].iter().map(|f| f.to_bits()).collect();
            assert_eq!(got, want, "block {block} mismatch");
        }
    }

    #[test]
    fn dispatch_float32_iq_delivers_only_complete_pairs() {
        let packed_tail: [u8; 8] = [0x78, 0x56, 0x34, 0x12, 0xF0, 0xDE, 0xBC, 0x9A];
        let mut scratch = Scratch::default();
        let (samples, count) =
            convert_block(SampleType::Float32Iq, true, &packed_tail, &mut scratch);
        assert_eq!(count, 2);
        let Samples::Float32(out) = samples else {
            unreachable!("Float32Iq delivers f32 samples");
        };
        assert_eq!(out.len(), 4, "only complete IQ pairs delivered");
    }

    #[test]
    fn dispatch_unpacks_before_converting_when_packing_enabled() {
        // consumer_threadproc: packing_enabled && type != RAW → unpack
        // first. One packed group of 2048s converts to eight zeros.
        let packed = pack_group([2048u16; 8]);
        let mut scratch = Scratch::default();
        let (samples, count) = convert_block(SampleType::Int16Real, true, &packed, &mut scratch);
        assert_eq!(count, 8);
        assert!(matches!(samples, Samples::Int16(w) if w == [0i16; 8]));

        // RAW skips the unpack even in packed mode, but reports the
        // packed sample count like C (12 bytes → 8).
        let (samples, count) = convert_block(SampleType::Raw, true, &packed, &mut scratch);
        assert!(matches!(samples, Samples::Raw(b) if b == packed.as_slice()));
        assert_eq!(count, 8);
    }
}
