//! A call's vocoder frames as an `.sdr` file: MimoSDR's DigitalStream
//! (github.com/MimoCAD/sdr-stream), the binary record container its server,
//! player and decoder read. One file is the whole call: a CallHeader, the
//! voice records, a CallTrailer.
//!
//! The voice records are rebuilt from the frames the call kept (the ones
//! its audio was made from), FEC-encoded again, so they hold corrected
//! codewords as the format asks:
//!
//! ```text
//! P25 Phase 1  Ldu (typ 2)     9 IMBE frames per 180 ms, LDU1 / LDU2 alternating
//! P25 Phase 2  P2Vch (typ 5)   18 AMBE+2 frames per 360 ms superframe, descrambled
//! DMR          DmrVoice (typ 6) 18 AMBE+2 frames per 360 ms superframe
//! ```
//!
//! NXDN has no layout in the format: [`call_sdr`] gives None.
//!
//! What the format doesn't carry rides in a tail after each voice record
//! (readers skip bytes past a layout they know): `TRP`, the frames the
//! record holds, then a byte per frame: what our vocoder made of it (bits
//! 7..5, [`Kind`]) and its FEC errors (bits 4..0: IMBE u0's, which the
//! repeat / mute rules use; AMBE the frame's, which the format has no
//! field for). Records are rebuilt, not what the air sent: an LDU's link
//! control / encryption sync are zero (flag RS_OK clear: untrusted), a
//! Phase 2 frame's "measured phase" is its dibit, and time runs on the
//! call's audio clock.

use sdr_stream::dmr::{self as sdr_dmr, DmrVoice};
use sdr_stream::p25::{LduFrame, P2VchFrame};
use sdr_stream::{CallHeader, CallTrailer, Mode, MsgHead, Raw, Record};

use super::frames::{Codec, VoiceFrame};
use crate::ambe::{decode_vcw, encode_vcw};
use crate::mbe::Kind;
use crate::p25::fec::{golay23_encode, hamming15_encode};
use crate::p25::voice::imbe_header_decode;
use crate::tables::VOICE_CODEWORD_BITS;

/// The call-level facts the header and trailer carry.
#[derive(Clone, Debug, Default)]
pub struct SdrInfo {
    /// Wall clock of the call's first audio, Unix µs.
    pub start_us: u64,
    pub duration_ms: u32,
    pub hz: u32,
    /// P25 NAC, or the DMR colour code.
    pub nac: u16,
    pub wacn: u32,
    pub sysid: u16,
    /// The archive directory label: WACN + System ID in hex for P25, else the system's short name.
    pub system: String,
    pub conventional: bool,
    /// The TDMA slot (Phase 2 / DMR).
    pub slot: u8,
    pub tg: u32,
    /// The first and last talkers.
    pub first_src: u32,
    pub src: u32,
    pub encrypted: bool,
    pub emergency: bool,
    pub signal_db: Option<f32>,
    pub floor_db: Option<f32>,
    pub offset_hz: Option<i32>,
    /// The last talker's alias, the talkgroup's alpha tag.
    pub alias: String,
    pub alphatag: String,
}

/// Which voice layout a call's frames go in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdrMode {
    P25Fdma,
    P25Tdma,
    Dmr,
}

impl SdrMode {
    fn mode(self) -> Mode {
        match self {
            SdrMode::P25Fdma => Mode::P25Fdma,
            SdrMode::P25Tdma => Mode::P25Tdma,
            SdrMode::Dmr => Mode::Dmr,
        }
    }
    /// Frames per voice record.
    fn unit(self) -> usize {
        if self == SdrMode::P25Fdma { 9 } else { 18 }
    }
    fn unit_us(self) -> u64 {
        if self == SdrMode::P25Fdma { 180_000 } else { 360_000 }
    }
}

const TAIL_MAGIC: &[u8; 3] = b"TRP";

fn kind_code(k: Kind) -> u8 {
    match k {
        Kind::Voice => 0,
        Kind::Repeat => 1,
        Kind::Muted => 2,
        Kind::Erasure => 3,
        Kind::Tone => 4,
    }
}

fn code_kind(c: u8) -> Kind {
    match c {
        1 => Kind::Repeat,
        2 => Kind::Muted,
        3 => Kind::Erasure,
        4 => Kind::Tone,
        _ => Kind::Voice,
    }
}

/// The tail byte of a frame.
fn tail_byte(f: &VoiceFrame) -> u8 {
    let errs = if f.codec == Codec::Imbe { f.e0 } else { f.errs };
    kind_code(f.kind) << 5 | errs.min(31) as u8
}

/// A record with our tail after its layout, padded to the record alignment
/// (and `len` set to cover it).
fn with_tail(mut rec: Vec<u8>, frames: &[VoiceFrame]) -> Vec<u8> {
    rec.extend_from_slice(TAIL_MAGIC);
    rec.push(frames.len() as u8);
    rec.extend(frames.iter().map(tail_byte));
    let total = sdr_stream::pad8(rec.len());
    rec.resize(total, 0);
    rec[6..8].copy_from_slice(&(total as u16).to_le_bytes());
    rec
}

/// Our tail on a voice record of `layout` bytes: the frames it holds, a byte each.
fn tail(r: &Raw, layout: usize) -> Option<&[u8]> {
    let t = r.bytes.get(layout..)?;
    if t.len() < 4 || &t[..3] != TAIL_MAGIC {
        return None;
    }
    t.get(4..4 + t[3] as usize)
}

/// Body bit (status symbols out, after the sync and NID) of an LDU's raw
/// bit `k` (op25's layout: from the sync, a status dibit after every 35).
fn body_bit(k: usize) -> usize {
    k - 2 * (k / 2 / 36) - 112
}

/// u0..u7 (u7 stored <<1) → the 144-bit IMBE codeword (the inverse of
/// [`imbe_header_decode`]: Golay / Hamming coded, PN-whitened).
fn imbe_encode(u: &[u32; 8]) -> [u8; 144] {
    let mut cw = [0u8; 144];
    let mut put = |v: u32, at: usize, n: usize| {
        for b in 0..n {
            cw[at + b] = (v >> (n - 1 - b) & 1) as u8;
        }
    };
    put(golay23_encode(u[0]), 0, 23);
    let mut pr = u[0] << 4;
    let mut pn = |n: u32| {
        let mut m = 0;
        for i in (0..n).rev() {
            pr = (173 * pr + 13849) & 0xffff;
            if pr & 32768 != 0 {
                m |= 1 << i;
            }
        }
        m
    };
    for k in 1..=3 {
        put(golay23_encode(u[k]) ^ pn(23), 23 * k, 23);
    }
    for k in 4..=6 {
        put(hamming15_encode(u[k]) ^ pn(15), 92 + (k - 4) * 15, 15);
    }
    put(u[7] >> 1, 137, 7);
    cw
}

/// mbelib's imbe_d[88] → u0..u7 (u7 <<1).
fn imbe_bits_to_params(d: &[u8]) -> [u32; 8] {
    const W: [usize; 8] = [12, 12, 12, 12, 11, 11, 11, 7];
    let mut u = [0u32; 8];
    let mut p = 0;
    for k in 0..8 {
        u[k] = d[p..p + W[k]].iter().fold(0, |v, &b| v << 1 | (b & 1) as u32);
        p += W[k];
    }
    u[7] <<= 1;
    u
}

fn ambe_bits(f: &VoiceFrame) -> [u8; 49] {
    std::array::from_fn(|i| f.bits.get(i).copied().unwrap_or(0) & 1)
}

fn ldu(frames: &[VoiceFrame], head: MsgHead) -> Vec<u8> {
    let mut body = [0u8; LduFrame::BODY_OCTETS];
    let (mut slot_valid, mut errors) = (0u16, [LduFrame::SLOT_INVALID; 9]);
    for (s, f) in frames.iter().enumerate() {
        let d: [u8; 88] = std::array::from_fn(|i| f.bits.get(i).copied().unwrap_or(0));
        let cw = imbe_encode(&imbe_bits_to_params(&d));
        for (j, &b) in cw.iter().enumerate() {
            let k = body_bit(VOICE_CODEWORD_BITS[s * 144 + j] as usize);
            body[k / 8] |= b << (7 - k % 8);
        }
        if !f.erased {
            slot_valid |= 1 << s;
            errors[s] = f.errs.min(254) as u8;
        }
    }
    let mut rec = Vec::with_capacity(LduFrame::BYTES + 16);
    LduFrame { head, slot_valid, errors, body }.encode_into(&mut rec);
    with_tail(rec, frames)
}

fn p2vch(frames: &[VoiceFrame], head: MsgHead, slot: u8) -> Vec<u8> {
    let mut phase = [[0u8; 36]; 18];
    let mut frame_valid = 0u32;
    for (k, f) in frames.iter().enumerate() {
        // The dibit in the top two bits, mid-bucket.
        phase[k] = encode_vcw(&ambe_bits(f)).map(|d| d << 6 | 0x20);
        frame_valid |= 1 << k;
    }
    let mut rec = Vec::with_capacity(P2VchFrame::BYTES + 24);
    P2VchFrame { head, slot, frame_valid, phase, flush_cause: 0, gap_fragments: 0 }.encode_into(&mut rec);
    with_tail(rec, frames)
}

fn dmr_voice(frames: &[VoiceFrame], head: MsgHead, slot: u8) -> Vec<u8> {
    let mut out = [[0u8; sdr_dmr::FRAME_OCTETS]; sdr_dmr::VOICE_FRAMES];
    for (k, f) in frames.iter().enumerate() {
        let d = encode_vcw(&ambe_bits(f));
        let bits: [u8; 72] = std::array::from_fn(|i| d[i / 2] >> (1 - i % 2) & 1);
        out[k] = DmrVoice::pack_frame(&bits);
    }
    // A burst is three frames; the last record may cover fewer bursts.
    let slots = frames.len().div_ceil(3) as u8;
    let burst_valid = (0..6).filter(|b| (b + 1) * 3 <= frames.len()).fold(0u8, |m, b| m | 1 << b);
    let mut rec = Vec::with_capacity(DmrVoice::BYTES + 24);
    DmrVoice { head, slot, burst_valid, slots, frames: out }.encode_into(&mut rec);
    with_tail(rec, frames)
}

/// A call's frames (the kept ones, in order) as an `.sdr` file; None when
/// they aren't all of one codec, or the mode has no layout.
pub fn call_sdr(mode: SdrMode, frames: &[VoiceFrame], info: &SdrInfo) -> Option<Vec<u8>> {
    let codec = if mode == SdrMode::P25Fdma { Codec::Imbe } else { Codec::Ambe };
    if frames.is_empty() || frames.iter().any(|f| f.codec != codec) {
        return None;
    }
    let head = |seq: u32, epoch_us: u64, flags: u16| MsgHead { seq, epoch_us, hz: info.hz, nac: info.nac, flags, site: 0 };
    let slot = if mode == SdrMode::P25Fdma { 0xFF } else { info.slot };
    let mut flags = sdr_stream::HDR_FLAG_SEEDED;
    if info.conventional {
        flags |= sdr_stream::HDR_FLAG_CONVENTIONAL;
    }
    let mut out = Vec::with_capacity(frames.len() * 40 + 256);
    CallHeader {
        head: head(0, info.start_us, flags),
        mode: mode.mode() as u8,
        slot,
        tg: info.tg,
        src: info.first_src,
        wacn: info.wacn,
        sysid: info.sysid,
        system: info.system.clone(),
    }
    .encode_into(&mut out);
    let mut seq = 0;
    for (k, chunk) in frames.chunks(mode.unit()).enumerate() {
        seq = k as u32 + 1;
        let t = info.start_us + k as u64 * mode.unit_us();
        let rec = match mode {
            SdrMode::P25Fdma => ldu(chunk, head(seq, t, if k % 2 == 1 { sdr_stream::LDU_FLAG_LDU2 } else { 0 })),
            SdrMode::P25Tdma => p2vch(chunk, head(seq, t, sdr_stream::P2V_FLAG_DESCRAMBLED), info.slot),
            SdrMode::Dmr => dmr_voice(chunk, head(seq, t, 0), info.slot),
        };
        out.extend_from_slice(&rec);
    }
    let mut tflags = 0;
    if info.encrypted {
        tflags |= sdr_stream::TRL_FLAG_ENCRYPTED;
    }
    if info.emergency {
        tflags |= sdr_stream::TRL_FLAG_EMERGENCY;
    }
    let delta = match (info.signal_db, info.floor_db) {
        (Some(s), Some(n)) => s - n,
        _ => 0.0,
    };
    CallTrailer {
        head: head(seq + 1, info.start_us + info.duration_ms as u64 * 1000, tflags),
        duration_ms: info.duration_ms,
        tg: info.tg,
        src: info.src,
        delta_db: delta,
        signal_db: info.signal_db.unwrap_or(f32::NAN),
        floor_db: info.floor_db.unwrap_or(f32::NAN),
        offset_hz: info.offset_hz.unwrap_or(sdr_stream::OFFSET_UNKNOWN),
        alias: info.alias.clone(),
        alphatag: info.alphatag.clone(),
    }
    .encode_into(&mut out);
    Some(out)
}

/// An `.sdr` file's voice frames, FEC-decoded again; their errors and what
/// our vocoder made of them from the tail when it's there (another
/// recorder's file: the record's counts, and Voice).
pub fn sdr_frames(bytes: &[u8]) -> Vec<VoiceFrame> {
    let mut out = Vec::new();
    for rec in sdr_stream::records(bytes) {
        let Record::Raw(r) = rec else { continue };
        if let Some(l) = LduFrame::from_raw(&r) {
            let t = tail(&r, LduFrame::BYTES);
            let n = t.map_or(9, |t| t.len().min(9));
            for s in 0..n {
                let mut cw = [0u8; 144];
                for (j, b) in cw.iter_mut().enumerate() {
                    let k = body_bit(VOICE_CODEWORD_BITS[s * 144 + j] as usize);
                    *b = l.body[k / 8] >> (7 - k % 8) & 1;
                }
                let p = imbe_header_decode(&cw, None);
                let erased = l.slot_valid >> s & 1 == 0 || l.errors[s] == LduFrame::SLOT_INVALID;
                let tb = t.map(|t| t[s]);
                out.push(VoiceFrame {
                    codec: Codec::Imbe,
                    bits: crate::p25::voice::imbe_params_to_bits(&p.u).to_vec(),
                    e0: tb.map_or(p.e0, |b| (b & 31) as u32),
                    errs: if erased { 0 } else { l.errors[s] as u32 },
                    erased,
                    kind: tb.map_or(Kind::Voice, |b| code_kind(b >> 5)),
                });
            }
        } else if let Some(v) = P2VchFrame::from_raw(&r) {
            let t = tail(&r, P2VchFrame::BYTES);
            for k in (0..18).filter(|&k| v.frame_valid >> k & 1 == 1) {
                let d = v.phase[k].map(|p| p >> 6);
                out.push(ambe_frame(&d, t.and_then(|t| t.get(k).copied())));
            }
        } else if let Some(v) = DmrVoice::from_raw(&r) {
            let t = tail(&r, DmrVoice::BYTES);
            let n = t.map_or(v.slots as usize * 3, |t| t.len()).min(18);
            for k in (0..n).filter(|&k| t.is_some() || v.burst_present(k / 3)) {
                let bits = DmrVoice::unpack_frame(&v.frames[k]);
                let d: [u8; 36] = std::array::from_fn(|i| bits[2 * i] << 1 | bits[2 * i + 1]);
                out.push(ambe_frame(&d, t.and_then(|t| t.get(k).copied())));
            }
        }
    }
    out
}

fn ambe_frame(d: &[u8; 36], tail: Option<u8>) -> VoiceFrame {
    let f = decode_vcw(d, None);
    VoiceFrame {
        codec: Codec::Ambe,
        bits: f.bits.to_vec(),
        e0: 0,
        errs: tail.map_or(f.errs, |b| (b & 31) as u32),
        erased: false,
        kind: tail.map_or(Kind::Voice, |b| code_kind(b >> 5)),
    }
}

/// Whether `bytes` start with a DigitalStream record.
pub fn is_sdr(bytes: &[u8]) -> bool {
    bytes.starts_with(&sdr_stream::MAGIC)
}

/// The file's call, as MimoSDR projects it (its sidecar JSON).
pub fn sdr_sidecar_json(bytes: &[u8]) -> Option<String> {
    let recs: Vec<Record> = sdr_stream::records(bytes).collect();
    sdr_stream::sidecar::Sidecar::from_records(&recs).map(|s| s.to_json())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::voice::imbe_params_to_bits;

    fn lcg(seed: &mut u32) -> u32 {
        *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        *seed >> 8
    }

    fn imbe_frame(seed: &mut u32, k: usize) -> VoiceFrame {
        let mut u = [0u32; 8];
        for (i, w) in [12, 12, 12, 12, 11, 11, 11, 7].into_iter().enumerate() {
            u[i] = lcg(seed) & ((1 << w) - 1);
        }
        u[7] <<= 1;
        let kind = if k % 7 == 3 { Kind::Repeat } else { Kind::Voice };
        VoiceFrame { codec: Codec::Imbe, bits: imbe_params_to_bits(&u).to_vec(), e0: (k % 3) as u32, errs: (k % 5) as u32, erased: k == 12, kind }
    }

    fn ambe_frame_rand(seed: &mut u32, k: usize) -> VoiceFrame {
        let bits = (0..49).map(|_| (lcg(seed) & 1) as u8).collect();
        VoiceFrame { codec: Codec::Ambe, bits, e0: 0, errs: (k % 4) as u32, erased: false, kind: if k == 5 { Kind::Tone } else { Kind::Voice } }
    }

    fn info() -> SdrInfo {
        SdrInfo {
            start_us: 1_790_000_000_000_000,
            duration_ms: 4_000,
            hz: 851_012_500,
            nac: 0x293,
            wacn: 0xBEE00,
            sysid: 0x3A1,
            system: "BEE003A1".into(),
            tg: 101,
            first_src: 1234,
            src: 5678,
            signal_db: Some(-30.0),
            floor_db: Some(-52.5),
            offset_hz: Some(-41),
            alias: "E12".into(),
            alphatag: "Fire Disp".into(),
            ..Default::default()
        }
    }

    #[test]
    fn imbe_codewords_round_trip() {
        let mut seed = 7;
        for k in 0..200 {
            let f = imbe_frame(&mut seed, k);
            let u = imbe_bits_to_params(&f.bits);
            let p = imbe_header_decode(&imbe_encode(&u), None);
            assert_eq!((p.u, p.errs), (u, 0));
        }
    }

    /// Every IMBE bit of an LDU lands in the 784-dibit body, once each.
    #[test]
    fn voice_bits_fill_the_ldu_body() {
        let mut seen = vec![false; LduFrame::BODY_OCTETS * 8];
        for &k in VOICE_CODEWORD_BITS.iter() {
            let b = body_bit(k as usize);
            assert!(!seen[b]);
            seen[b] = true;
            assert_ne!((k as usize / 2 + 1) % 36, 0, "a status symbol");
        }
    }

    #[test]
    fn phase1_call_round_trips() {
        let mut seed = 3;
        // 22 frames: two whole LDUs and four frames of a third.
        let frames: Vec<VoiceFrame> = (0..22).map(|k| imbe_frame(&mut seed, k)).collect();
        let bytes = call_sdr(SdrMode::P25Fdma, &frames, &info()).unwrap();
        let recs: Vec<Record> = sdr_stream::records(&bytes).collect();
        assert_eq!(recs.len(), 5);
        assert!(matches!(&recs[0], Record::Header(h) if h.mode == Mode::P25Fdma as u8 && h.tg == 101 && h.src == 1234));
        let ldus: Vec<LduFrame> = recs.iter().filter_map(|r| if let Record::Raw(r) = r { LduFrame::from_raw(r) } else { None }).collect();
        assert_eq!(ldus.len(), 3);
        assert!(ldus[0].ldu1() && !ldus[1].ldu1());
        assert_eq!(ldus[1].head.epoch_us - ldus[0].head.epoch_us, 180_000);
        // Frame 12 was erased; the third LDU's last five slots hold nothing.
        assert_eq!(ldus[1].slot_valid, 0x1FF & !(1 << 3));
        assert_eq!(ldus[2].slot_valid, 0xF);
        let back = sdr_frames(&bytes);
        assert_eq!(back.len(), 22);
        for (a, b) in frames.iter().zip(&back) {
            assert_eq!((&a.bits, a.e0, a.erased, a.kind), (&b.bits, b.e0, b.erased, b.kind));
            if !a.erased {
                assert_eq!(a.errs, b.errs);
            }
        }
        let side = sdr_sidecar_json(&bytes).unwrap();
        assert!(side.contains("\"tg\":101") && side.contains("\"alphatag\":\"Fire Disp\"") && side.contains("\"mode\":\"F\""), "{side}");
    }

    #[test]
    fn tdma_and_dmr_calls_round_trip() {
        for mode in [SdrMode::P25Tdma, SdrMode::Dmr] {
            let mut seed = 11;
            let frames: Vec<VoiceFrame> = (0..40).map(|k| ambe_frame_rand(&mut seed, k)).collect();
            let bytes = call_sdr(mode, &frames, &SdrInfo { slot: 1, ..info() }).unwrap();
            let back = sdr_frames(&bytes);
            assert_eq!(back.len(), 40, "{mode:?}");
            for (a, b) in frames.iter().zip(&back) {
                assert_eq!((&a.bits, a.errs, a.kind), (&b.bits, b.errs, b.kind), "{mode:?}");
            }
        }
        assert_eq!(call_sdr(SdrMode::Dmr, &[imbe_frame(&mut 1, 0)], &info()), None, "the wrong codec");
    }

    /// Without our tail (another recorder's file) frames still decode.
    #[test]
    fn foreign_records_decode() {
        let mut seed = 5;
        let frames: Vec<VoiceFrame> = (0..9).map(|k| imbe_frame(&mut seed, k)).collect();
        let rec = ldu(&frames, MsgHead::default());
        let mut plain = rec[..LduFrame::BYTES].to_vec();
        plain[6..8].copy_from_slice(&(LduFrame::BYTES as u16).to_le_bytes());
        let back = sdr_frames(&plain);
        assert_eq!(back.len(), 9);
        assert!(back.iter().zip(&frames).all(|(b, a)| b.bits == a.bits && b.kind == Kind::Voice));
    }
}
