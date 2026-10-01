//! Analog signalling: the unit ID some analog radios send as a short data
//! burst when they key up or unkey, decoded from the 8 kHz voice audio
//! (Trunk Recorder's `decodeMDC` / `decodeFSync`).
//!
//! - MDC1200 (Motorola): 1200 baud, 1200/1800 Hz; a 40-bit sync, then 112
//!   bits interleaved 16×7 — op, arg, 16-bit unit, CRC-16, a status byte
//!   and 7 bytes of rate-½ convolutional parity.
//! - FleetSync (Kenwood): 1200 baud on 1200/1800 Hz or 2400 baud on
//!   1200/2400 Hz; a 32-bit sync, then 64-bit blocks of 48 data bits, a
//!   15-bit CRC and a parity bit. Only the first block is read (it holds the
//!   sender); FleetSync II's interleaved ECC framing isn't decoded.
//!
//! All three are MSK (the tones half a bit rate apart), so one front end
//! serves them: mixed down by 1200 Hz, a 1200 Hz bit holds its phase and
//! the other tone turns it half a cycle.
//!
//! ```text
//! audio → × e^(−j2π·1200t) → summed over each bit, by NPHASE slicers on
//!   staggered bit clocks (no timing loop: one is within ⅛ bit of the best
//!   instant) → a complex value per bit per slicer
//!   MDC1200:   phase against a reference from the values squared (coherent;
//!              MDC sends the other tone for a bit unlike the one before,
//!              so the phase is the bit itself)
//!   FleetSync: phase against the bit before (did the tone turn it?)
//! → each slicer's bits hunt the sync word, then take a block; a block that
//!   passes its CRC is a unit ID
//! ```
//!
//! Phase, not tone level, decides every bit, so de-emphasis tilting one tone
//! below the other doesn't matter; a one-tap pre-emphasis first undoes the
//! de-emphasis, which would otherwise turn data a radio sends flat into a
//! wandering baseline (fatal at 2400 baud). Coherent MDC is ~5 dB better
//! than deciding each bit's tone, where one wrong tone inverts every later
//! bit. On the FM demodulator's audio it decodes more than Trunk Recorder's
//! decoders at every carrier-to-noise tried (research/trunk-recorder-gaps.md).
//! About 30 flops per audio sample, on analog channels only.

use num_complex::Complex32;

use super::fm::{AUDIO_RATE, DEEMPH_TAU};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mdc1200,
    FleetSync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitId {
    pub kind: Kind,
    /// MDC1200: the 16-bit ID (TR's `unitID`); FleetSync: fleet × 10000 + unit.
    pub unit: u32,
    pub emergency: bool,
}

/// Bit clocks per front end, staggered by 1/NPHASE of a bit.
const NPHASE: usize = 4;

/// Audio in; per slicer, the bit's samples mixed down by 1200 Hz and summed.
struct Msk {
    lo: Complex32,
    lo_step: Complex32,
    n: u32,
    /// Bits per sample.
    step: f32,
    clock: [f32; NPHASE],
    acc: [Complex32; NPHASE],
}

impl Msk {
    fn new(baud: f64) -> Self {
        let w = -2.0 * std::f64::consts::PI * 1200.0 / AUDIO_RATE;
        Msk {
            lo: Complex32::new(1.0, 0.0),
            lo_step: Complex32::new(w.cos() as f32, w.sin() as f32),
            n: 0,
            step: (baud / AUDIO_RATE) as f32,
            clock: std::array::from_fn(|k| k as f32 / NPHASE as f32),
            acc: [Complex32::default(); NPHASE],
        }
    }

    /// Take one audio sample; `bit(slicer, value)` for each slicer whose bit ended.
    #[inline]
    fn push(&mut self, x: f32, mut bit: impl FnMut(usize, Complex32)) {
        let m = self.lo * x;
        self.lo *= self.lo_step;
        self.n += 1;
        if self.n.is_multiple_of(1024) {
            self.lo /= self.lo.norm();
        }
        for k in 0..NPHASE {
            self.clock[k] += self.step;
            if self.clock[k] >= 1.0 {
                // The sample straddles the bit's end: split it (at 2400 baud
                // a bit is only 3⅓ samples).
                self.clock[k] -= 1.0;
                let over = self.clock[k] / self.step;
                bit(k, self.acc[k] + m * (1.0 - over));
                self.acc[k] = m * over;
            } else {
                self.acc[k] += m;
            }
        }
    }
}

const MDC_SYNC: u64 = 0x07_092a_446f;
/// Reference phase smoothing per bit (about 8 bits).
const REF_ALPHA: f32 = 0.125;
/// Sync bits that may be wrong.
const MDC_SYNC_ERRS: u32 = 5;

#[derive(Clone, Copy, Default)]
struct MdcSlicer {
    /// The bit values squared, smoothed: twice the reference phase.
    sq: Complex32,
    reference: Complex32,
    sync: u64,
    invert: bool,
    /// Bits of the block still to come (0: hunting the sync).
    left: u8,
    block: u128,
}

#[derive(Default)]
struct Mdc {
    s: [MdcSlicer; NPHASE],
}

impl Mdc {
    fn bit(&mut self, k: usize, v: Complex32, out: &mut Vec<UnitId>) {
        let s = &mut self.s[k];
        let a = v.norm();
        if a > 0.0 {
            s.sq += (v * v / a - s.sq) * REF_ALPHA;
        }
        // The square root's sign is free: keep the one nearer the last.
        let r = s.sq.sqrt();
        s.reference = if (r * s.reference.conj()).re < 0.0 { -r } else { r };
        let b = ((v * s.reference.conj()).re > 0.0) != s.invert;
        if s.left == 0 {
            s.sync = (s.sync << 1 | b as u64) & 0xff_ffff_ffff;
            let errs = (s.sync ^ MDC_SYNC).count_ones();
            if errs <= MDC_SYNC_ERRS || errs >= 40 - MDC_SYNC_ERRS {
                s.invert ^= errs > MDC_SYNC_ERRS;
                s.left = 112;
                s.block = 0;
            }
            return;
        }
        s.block = s.block << 1 | b as u128;
        s.left -= 1;
        if s.left > 0 {
            return;
        }
        if let Some(id) = mdc_block(s.block) {
            out.push(id);
            // The other slicers have the same packet.
            self.s.iter_mut().for_each(|s| s.left = 0);
        }
    }
}

/// The block's bytes (bit 0 of byte 0 first) from its 112 received bits,
/// sent 16 × 7 interleaved: received bit j·16 + i is block bit i·7 + j.
fn mdc_deinterleave(block: u128) -> [u8; 14] {
    let mut d = [0u8; 14];
    for i in 0..16 {
        for j in 0..7 {
            let r = j * 16 + i;
            if block >> (111 - r) & 1 == 1 {
                let k = i * 7 + j;
                d[k / 8] |= 1 << (k % 8);
            }
        }
    }
    d
}

/// Rate-½ convolutional code over the first 7 bytes: parity bit k (in bytes
/// 7–13) is data bits k, k−2, k−5 and k−6 summed. A data bit three of whose
/// four checks fail is flipped (majority logic).
fn mdc_correct(d: &mut [u8; 14]) {
    let bit = |d: &[u8; 14], k: usize| d[k / 8] >> (k % 8) & 1;
    let mut syn: [u8; 56] = std::array::from_fn(|k| [0, 2, 5, 6].iter().filter(|&&t| k >= t).fold(bit(d, 56 + k), |p, &t| p ^ bit(d, k - t)));
    for m in 0..50 {
        let checks = [m, m + 2, m + 5, m + 6];
        if checks.iter().map(|&c| syn[c]).sum::<u8>() >= 3 {
            d[m / 8] ^= 1 << (m % 8);
            checks.iter().for_each(|&c| syn[c] ^= 1);
        }
    }
}

/// CRC-16 (0x1021 reflected, init 0, inverted) of the first four bytes.
fn mdc_crc(d: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in d {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 == 1 { crc >> 1 ^ 0x8408 } else { crc >> 1 };
        }
    }
    !crc
}

fn mdc_block(block: u128) -> Option<UnitId> {
    let mut d = mdc_deinterleave(block);
    mdc_correct(&mut d);
    if mdc_crc(&d[..4]) != u16::from_le_bytes([d[4], d[5]]) {
        return None;
    }
    // Only PTT IDs (op 0x01) and emergencies (0x00) name the talker; other
    // ops (radio check, stun, status…) may address someone else.
    match d[0] {
        0x00 | 0x01 => Some(UnitId { kind: Kind::Mdc1200, unit: u16::from_be_bytes([d[2], d[3]]) as u32, emergency: d[0] == 0x00 }),
        _ => None,
    }
}

const FS_SYNCS: [u32; 2] = [0xaaaa_23eb, 0xaaaa_052b];
const FS_SYNC_ERRS: u32 = 3;

#[derive(Clone, Copy, Default)]
struct FsSlicer {
    last: Complex32,
    sync: u32,
    left: u8,
    block: u64,
}

#[derive(Default)]
struct FleetSync {
    s: [FsSlicer; NPHASE],
}

impl FleetSync {
    fn bit(&mut self, k: usize, v: Complex32, out: &mut Vec<UnitId>) {
        let s = &mut self.s[k];
        // 1: 1200 Hz, the phase held.
        let b = (v * s.last.conj()).re > 0.0;
        s.last = v;
        if s.left == 0 {
            s.sync = s.sync << 1 | b as u32;
            if FS_SYNCS.iter().any(|&w| (s.sync ^ w).count_ones() <= FS_SYNC_ERRS) {
                s.left = 64;
            }
            return;
        }
        s.block = s.block << 1 | b as u64;
        s.left -= 1;
        if s.left > 0 {
            return;
        }
        if let Some(id) = fs_block(s.block) {
            out.push(id);
            self.s.iter_mut().for_each(|s| s.left = 0);
        }
    }
}

/// The check field (low 16 bits) for a block's 48 data bits (high 48):
/// a 15-bit CRC (0x6815) with bit 1 inverted, then even parity over the
/// data and CRC.
fn fs_check(block: u64) -> u16 {
    let data = block >> 16;
    let mut crc = 0u32;
    for i in (0..48).rev() {
        let b = (data >> i) as u32 & 1;
        if b ^ (crc >> 15 & 1) == 1 {
            crc ^= 0x6815;
        }
        crc = crc << 1 & 0xffff;
    }
    let crc = (crc ^ 2) as u16;
    let parity = (data.count_ones() + (crc >> 1).count_ones()) as u16 & 1;
    crc | parity
}

fn fs_block(block: u64) -> Option<UnitId> {
    if fs_check(block) != block as u16 {
        return None;
    }
    let m = block.to_be_bytes();
    // Bytes: command, subcommand, sender fleet − 99, sender unit − 999 (12
    // bits), receiver unit (12 bits). 0 is "none".
    let fleet = m[2] as u32 + 99;
    let unit = ((m[3] as u32) << 4 | (m[4] as u32) >> 4) + 999;
    if fleet == 99 || unit == 999 {
        return None;
    }
    Some(UnitId { kind: Kind::FleetSync, unit: fleet * 10_000 + unit, emergency: false })
}

/// The decoders for one analog channel.
pub struct Signalling {
    /// The last audio sample (for the pre-emphasis).
    last: f32,
    pre: f32,
    msk1200: Msk,
    msk2400: Msk,
    mdc: Mdc,
    fs1200: FleetSync,
    fs2400: FleetSync,
}

impl Default for Signalling {
    fn default() -> Self {
        Signalling {
            last: 0.0,
            pre: (-1.0 / (AUDIO_RATE * DEEMPH_TAU)).exp() as f32,
            msk1200: Msk::new(1200.0),
            msk2400: Msk::new(2400.0),
            mdc: Mdc::default(),
            fs1200: FleetSync::default(),
            fs2400: FleetSync::default(),
        }
    }
}

impl Signalling {
    /// Decode 8 kHz audio, appending any unit IDs it carried.
    pub fn push(&mut self, audio: &[f32], out: &mut Vec<UnitId>) {
        let Signalling { last, pre, msk1200, msk2400, mdc, fs1200, fs2400 } = self;
        for &y in audio {
            let x = y - *pre * *last;
            *last = y;
            msk1200.push(x, |k, v| {
                mdc.bit(k, v, out);
                fs1200.bit(k, v, out);
            });
            msk2400.push(x, |k, v| fs2400.bit(k, v, out));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Continuous-phase FSK at 8 kHz: bit 1 the lower tone.
    pub fn ffsk(bits: &[bool], f_lo: f64, f_hi: f64, baud: f64, amp: f32) -> Vec<f32> {
        let n = (bits.len() as f64 * AUDIO_RATE / baud) as usize;
        let mut ph = 0.0f64;
        (0..n)
            .map(|i| {
                let b = bits[(i as f64 * baud / AUDIO_RATE) as usize];
                ph += 2.0 * PI * if b { f_lo } else { f_hi } / AUDIO_RATE;
                amp * ph.sin() as f32
            })
            .collect()
    }

    fn msb_bits(v: u128, n: usize) -> impl Iterator<Item = bool> {
        (0..n).rev().map(move |i| v >> i & 1 == 1)
    }

    /// An MDC1200 packet's data bits: preamble, sync, block.
    pub fn mdc_bits(op: u8, arg: u8, unit: u16) -> Vec<bool> {
        let mut d = [0u8; 14];
        d[0] = op;
        d[1] = arg;
        d[2..4].copy_from_slice(&unit.to_be_bytes());
        let crc = mdc_crc(&d[..4]);
        d[4..6].copy_from_slice(&crc.to_le_bytes());
        let bit = |d: &[u8; 14], k: usize| d[k / 8] >> (k % 8) & 1;
        for k in 0..56 {
            let p = [0, 2, 5, 6].iter().filter(|&&t| k >= t).fold(0, |p, &t| p ^ bit(&d, k - t));
            d[7 + k / 8] |= p << (k % 8);
        }
        let mut block = [false; 112];
        for i in 0..16 {
            for j in 0..7 {
                let k = i * 7 + j;
                block[j * 16 + i] = bit(&d, k) == 1;
            }
        }
        let mut bits: Vec<bool> = (0..48).map(|i| i % 2 == 0).collect();
        bits.extend(msb_bits(MDC_SYNC as u128, 40));
        bits.extend(block);
        bits.extend([false; 16]);
        bits
    }

    /// Data bits → tones (true: 1200 Hz, a bit the same as the one before).
    pub fn mdc_tones(bits: &[bool]) -> Vec<bool> {
        let mut last = false;
        bits.iter().map(|&b| !std::mem::replace(&mut last, b) ^ b).collect()
    }

    /// A FleetSync packet: preamble, sync, one block.
    pub fn fs_bits(fleet: u32, unit: u32) -> Vec<bool> {
        let (f, u) = (fleet - 99, unit - 999);
        let data = (0x01u64 << 40) | (0x40u64 << 32) | (f as u64) << 24 | (u as u64) << 12 | 0xfff;
        let block = data << 16 | fs_check(data << 16) as u64;
        let mut bits: Vec<bool> = (0..32).map(|i| i % 2 == 0).collect();
        bits.extend(msb_bits(FS_SYNCS[0] as u128, 32));
        bits.extend(msb_bits(block as u128, 64));
        bits.extend([false; 16]);
        bits
    }

    pub fn decode(audio: &[f32]) -> Vec<UnitId> {
        let mut s = Signalling::default();
        let mut out = Vec::new();
        s.push(audio, &mut out);
        out
    }

    /// Deterministic noise, roughly Gaussian (sum of uniforms), unit variance.
    pub fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut x = seed.wrapping_mul(2654435761) | 1;
        let mut u = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as f32 / u32::MAX as f32 - 0.5
        };
        (0..n).map(|_| (0..12).map(|_| u()).sum()).collect()
    }

    #[test]
    fn mdc1200_ptt_id_and_emergency() {
        let a = ffsk(&mdc_tones(&mdc_bits(0x01, 0x80, 0x1234)), 1200.0, 1800.0, 1200.0, 0.3);
        assert_eq!(decode(&a), vec![UnitId { kind: Kind::Mdc1200, unit: 0x1234, emergency: false }]);
        let a = ffsk(&mdc_tones(&mdc_bits(0x00, 0x80, 0x0042)), 1200.0, 1800.0, 1200.0, 0.3);
        assert_eq!(decode(&a), vec![UnitId { kind: Kind::Mdc1200, unit: 0x42, emergency: true }]);
    }

    #[test]
    fn mdc1200_corrects_bit_errors() {
        let mut bits = mdc_bits(0x01, 0x00, 0x5678);
        // Two scattered errors inside the block (data bits, past the sync).
        let start = 48 + 40;
        bits[start + 3] ^= true;
        bits[start + 60] ^= true;
        let a = ffsk(&mdc_tones(&bits), 1200.0, 1800.0, 1200.0, 0.3);
        assert_eq!(decode(&a).first().map(|u| u.unit), Some(0x5678));
    }

    #[test]
    fn fleetsync_1200_and_2400() {
        let id = UnitId { kind: Kind::FleetSync, unit: 101 * 10_000 + 1234, emergency: false };
        let a = ffsk(&fs_bits(101, 1234), 1200.0, 1800.0, 1200.0, 0.3);
        assert_eq!(decode(&a), vec![id]);
        let a = ffsk(&fs_bits(101, 1234), 1200.0, 2400.0, 2400.0, 0.3);
        assert_eq!(decode(&a), vec![id]);
    }

    /// `audio` (8 kHz, ±1 = 5 kHz deviation) over FM, with a 100 Hz CTCSS
    /// tone and complex noise of `noise_sd` per component on a 0.5 carrier,
    /// through [`Nbfm`] (de-emphasis, 300 Hz high-pass, squelch).
    fn over_fm(audio: &[f32], noise_sd: f32, seed: u32) -> Vec<f32> {
        use crate::dsp::fm::Nbfm;
        let rate = 39_062.5;
        let n = (audio.len() as f64 * rate / AUDIO_RATE) as usize;
        let noise = noise(2 * n, seed);
        let mut ph = 0.0f64;
        let iq: Vec<Complex32> = (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                let dev = 5000.0 * audio[(t * AUDIO_RATE) as usize] as f64 + 500.0 * (2.0 * PI * 100.0 * t).sin();
                ph += 2.0 * PI * dev / rate;
                Complex32::from_polar(0.5, ph as f32) + Complex32::new(noise[2 * i], noise[2 * i + 1]) * noise_sd
            })
            .collect();
        let mut fm = Nbfm::new(rate);
        let mut out = Vec::new();
        fm.push(&iq, 1e-3, &mut out);
        out
    }

    /// The radio's own 750 µs pre-emphasis (some put data through it).
    fn pre_emphasised(a: &[f32]) -> Vec<f32> {
        let p = (-1.0 / (AUDIO_RATE * DEEMPH_TAU)).exp() as f32;
        (0..a.len()).map(|i| (a[i] - p * if i > 0 { a[i - 1] } else { 0.0 }) * 0.25).collect()
    }

    #[test]
    fn through_the_fm_demodulator() {
        // Key-up: 100 ms of carrier, then each packet at 2 kHz deviation.
        // Sent flat, de-emphasis turns the steps between tones into a
        // wandering baseline, which the decoder's pre-emphasis takes out.
        let mut a = vec![0.0f32; 800];
        a.extend(ffsk(&mdc_tones(&mdc_bits(0x01, 0x80, 0x0815)), 1200.0, 1800.0, 1200.0, 0.4));
        a.extend(ffsk(&fs_bits(150, 2000), 1200.0, 2400.0, 2400.0, 0.4));
        a.extend(ffsk(&fs_bits(150, 2001), 1200.0, 1800.0, 1200.0, 0.4));
        a.extend(pre_emphasised(&ffsk(&fs_bits(150, 2002), 1200.0, 2400.0, 2400.0, 0.4)));
        // Then "voice".
        a.extend((0..8000).map(|i| (2.0 * PI * 700.0 * i as f64 / AUDIO_RATE).sin() as f32 * 0.4));
        let ids: Vec<u32> = decode(&over_fm(&a, 0.05, 3)).iter().map(|u| u.unit).collect();
        assert_eq!(ids, vec![0x0815, 1502000, 1502001, 1502002]);
    }

    #[test]
    fn decodes_in_noise_and_nothing_from_noise() {
        // ~10 dB carrier-to-noise in 11 kHz, data at 2.5 kHz deviation.
        let mut hits = 0;
        for seed in 0..20 {
            let mut a = vec![0.0f32; 800];
            a.extend(ffsk(&mdc_tones(&mdc_bits(0x01, 0x80, 0x1234)), 1200.0, 1800.0, 1200.0, 0.5));
            a.extend(ffsk(&fs_bits(200, 4000), 1200.0, 2400.0, 2400.0, 0.5));
            a.extend(vec![0.0f32; 800]);
            hits += decode(&over_fm(&a, 0.18, seed)).len();
        }
        assert!(hits >= 38, "{hits}/40 decoded at 10 dB");
        // A minute of noise, then a minute of 1 kHz-ish "voice" tones: nothing.
        let mut a = noise(480_000, 7);
        a.extend((0..480_000).map(|i| (2.0 * PI * (900.0 + 300.0 * (i as f64 / 4000.0).sin()) * i as f64 / AUDIO_RATE).sin() as f32 * 0.3));
        assert!(decode(&a).is_empty());
    }
}
