//! One DMR channel above the framer: each burst to its slot (the CACH
//! names it on a repeater's outbound), and each slot's bursts to what they
//! carry — voice codewords, link control (who is talking to whom), CSBKs.
//!
//! ```text
//! Burst → slot (TACT, else alternate) → SlotDecoder
//!   voice sync (A) / EMB (B–F) → 3 AMBE+2 codewords per burst
//!                              → embedded LC fragments (B–E) → LC
//!   data sync → slot type → BPTC(196,96) → voice LC header / terminator (RS 12,9)
//!                                         → CSBK / MBC / PI header (CRC-CCITT)
//! ```

use super::burst::{cach, Burst, SyncKind};
use super::fec::{self, bptc196_decode, embedded_lc_decode, pack, rs129_decode};
use crate::p25::phase2::{decode_vcw, AmbeFrame};

/// Data types of a data burst's slot type (TS 102 361-1 §9.3.6).
pub const DT_PI_HEADER: u8 = 0;
pub const DT_VOICE_LC_HEADER: u8 = 1;
pub const DT_TERMINATOR_LC: u8 = 2;
pub const DT_CSBK: u8 = 3;
pub const DT_MBC_HEADER: u8 = 4;
pub const DT_MBC_CONTINUATION: u8 = 5;
pub const DT_DATA_HEADER: u8 = 6;
pub const DT_IDLE: u8 = 9;

pub fn data_type_name(dt: u8) -> &'static str {
    match dt {
        DT_PI_HEADER => "pi_header",
        DT_VOICE_LC_HEADER => "voice_lc_header",
        DT_TERMINATOR_LC => "terminator_lc",
        DT_CSBK => "csbk",
        DT_MBC_HEADER => "mbc_header",
        DT_MBC_CONTINUATION => "mbc_continuation",
        DT_DATA_HEADER => "data_header",
        7 => "rate_1/2_data",
        8 => "rate_3/4_data",
        DT_IDLE => "idle",
        10 => "rate_1_data",
        11 => "usbd",
        _ => "reserved",
    }
}

/// Feature set IDs.
pub const FID_STANDARD: u8 = 0x00;
pub const FID_MOTOROLA: u8 = 0x10;
pub const FID_HYTERA: u8 = 0x68;

/// Full link control: 9 bytes (FLCO, FID, service options, target, source).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lc(pub [u8; 9]);

impl Lc {
    pub fn flco(&self) -> u8 {
        self.0[0] & 0x3f
    }
    pub fn fid(&self) -> u8 {
        self.0[1]
    }
    /// A voice channel user LC (who is talking to whom), and is it to a group?
    pub fn voice_user(&self) -> Option<bool> {
        match (self.fid(), self.flco()) {
            (_, 0) => Some(true),
            (_, 3) => Some(false),
            // Motorola Capacity Plus wide-area / encrypted group voice.
            (FID_MOTOROLA, 4) | (FID_MOTOROLA, 32) => Some(true),
            _ => None,
        }
    }
    pub fn service_options(&self) -> u8 {
        self.0[2]
    }
    pub fn emergency(&self) -> bool {
        self.0[2] & 0x80 != 0
    }
    /// The privacy (encryption) service option — or Motorola's encrypted voice LC.
    pub fn encrypted(&self) -> bool {
        self.0[2] & 0x40 != 0 || (self.fid() == FID_MOTOROLA && self.flco() == 32)
    }
    /// The talkgroup (or radio) called. Capacity Plus's own LCs are narrower:
    /// wide-area (linked) groups are 8 bits, encrypted voice's 16.
    pub fn target(&self) -> u32 {
        let b = &self.0;
        match (self.fid(), self.flco()) {
            (FID_MOTOROLA, 4) => b[5] as u32,
            (FID_MOTOROLA, 32) => u32::from_be_bytes([0, 0, b[4], b[5]]),
            _ => u32::from_be_bytes([0, b[3], b[4], b[5]]),
        }
    }
    /// The radio talking (Capacity Plus's LCs: 16 bits, 0 in a wide-area header).
    pub fn source(&self) -> u32 {
        let b = &self.0;
        match (self.fid(), self.flco()) {
            (FID_MOTOROLA, 4) | (FID_MOTOROLA, 32) => u32::from_be_bytes([0, 0, b[7], b[8]]),
            _ => u32::from_be_bytes([0, b[6], b[7], b[8]]),
        }
    }
    /// Linked Capacity Plus wide-area voice: the site's rest channel (logical slot number).
    pub fn rest_lsn(&self) -> Option<u8> {
        (self.fid() == FID_MOTOROLA && self.flco() == 4).then_some(self.0[6] & 0x1f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LcFrom {
    Header,
    Embedded,
    Terminator,
}

/// A control signalling block: 10 bytes (LB/PF/CSBKO, FID, 8 data bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Csbk(pub [u8; 10]);

impl Csbk {
    pub fn last_block(&self) -> bool {
        self.0[0] & 0x80 != 0
    }
    pub fn opcode(&self) -> u8 {
        self.0[0] & 0x3f
    }
    pub fn fid(&self) -> u8 {
        self.0[1]
    }
    /// Bits `a..b` of the block (0 = first bit of byte 0).
    pub fn bits(&self, a: usize, b: usize) -> u32 {
        (a..b).fold(0, |v, i| v << 1 | (self.0[i / 8] >> (7 - i % 8) & 1) as u32)
    }
}

#[derive(Clone, Debug)]
pub enum SlotEvent {
    /// A voice codeword (AMBE+2) and the burst's place in its superframe (0 = A … 5 = F; None: joined late).
    Voice { frame: AmbeFrame, pos: Option<u8> },
    Lc { lc: Lc, from: LcFrom },
    /// A CSBK, or an MBC header's first block (the rest follow as continuations).
    Csbk { csbk: Csbk, mbc: bool },
    /// An MBC continuation block's 12 bytes (no CRC of its own).
    MbcContinuation([u8; 12]),
    /// Privacy indicator header: encrypted, with this algorithm and key.
    Privacy { alg: u8, key: u8 },
    /// A data burst of another type (idle, data blocks, a bad CRC …).
    Data { data_type: u8, ok: bool },
}

/// One slot's decoding state.
#[derive(Default)]
pub struct SlotDecoder {
    /// Place of the last voice burst in its superframe; None outside voice.
    pos: Option<u8>,
    /// Embedded LC fragments gathered this superframe.
    emb: Vec<u8>,
    /// Colour code of the last good slot type / EMB.
    pub color_code: Option<u8>,
    /// Voice codewords / BPTC blocks with a bad CRC, over the slot's life.
    pub voice_frames: u64,
    pub bad_blocks: u64,
}

impl SlotDecoder {
    pub fn burst(&mut self, b: &Burst, out: &mut Vec<SlotEvent>) {
        match b.sync {
            Some(k) if k.is_voice() => {
                self.pos = Some(0);
                self.emb.clear();
                self.voice(b, out);
            }
            Some(k) if k.is_data() => {
                self.pos = None;
                self.data(b, out);
            }
            Some(_) => {}
            None => {
                // A voice burst B–F: the grid says so, or (joining late) a clean EMB.
                let (cc, _pi, lcss, errs) = b.emb();
                let fits = self.color_code.is_none_or(|c| c == cc);
                self.pos = match self.pos {
                    Some(p) if p < 5 => Some(p + 1),
                    _ => None,
                };
                if self.pos.is_none() && !(errs == 0 && fits) {
                    return;
                }
                if self.pos.is_none() {
                    // Joining late on nothing but the EMB: the voice has to look like voice too.
                    let frames: Vec<AmbeFrame> = (0..3).map(|k| self.frame(b, k)).collect();
                    if frames.iter().any(|f| f.errs > 2) {
                        return;
                    }
                }
                if errs <= 2 && fits {
                    self.color_code = Some(cc);
                    self.embedded(lcss, b, out);
                }
                self.voice(b, out);
            }
        }
    }

    fn frame(&self, b: &Burst, k: usize) -> AmbeFrame {
        let (d, rel) = b.voice_frame(k);
        decode_vcw(&d, Some(&rel))
    }

    fn voice(&mut self, b: &Burst, out: &mut Vec<SlotEvent>) {
        for k in 0..3 {
            self.voice_frames += 1;
            out.push(SlotEvent::Voice { frame: self.frame(b, k), pos: self.pos });
        }
    }

    /// LCSS 1 (first), 3, 3, 2 (last): the four fragments of bursts B–E.
    fn embedded(&mut self, lcss: u8, b: &Burst, out: &mut Vec<SlotEvent>) {
        match lcss {
            1 => {
                self.emb.clear();
                self.emb.extend(b.embedded());
            }
            3 if !self.emb.is_empty() && self.emb.len() < 96 => self.emb.extend(b.embedded()),
            2 if self.emb.len() == 96 => {
                self.emb.extend(b.embedded());
                let raw: [u8; 128] = self.emb[..].try_into().unwrap();
                self.emb.clear();
                match embedded_lc_decode(&raw) {
                    Some((bits, _)) => out.push(SlotEvent::Lc { lc: Lc(pack(&bits).try_into().unwrap()), from: LcFrom::Embedded }),
                    None => self.bad_blocks += 1,
                }
            }
            _ => self.emb.clear(),
        }
    }

    fn data(&mut self, b: &Burst, out: &mut Vec<SlotEvent>) {
        let (cc, dt, errs) = b.slot_type();
        if errs > 3 {
            return;
        }
        let bptc = matches!(dt, 0..=7 | 11);
        if !bptc {
            if errs <= 1 {
                self.color_code = Some(cc);
            }
            out.push(SlotEvent::Data { data_type: dt, ok: true });
            return;
        }
        let blk = bptc196_decode(&b.info196());
        let bytes: [u8; 12] = pack(&blk.bits).try_into().unwrap();
        let ok = match dt {
            DT_VOICE_LC_HEADER | DT_TERMINATOR_LC => {
                let mask = if dt == DT_VOICE_LC_HEADER { fec::MASK_VOICE_LC_HEADER } else { fec::MASK_TERMINATOR_LC };
                let mut cw = bytes;
                cw[9] ^= (mask >> 16) as u8;
                cw[10] ^= (mask >> 8) as u8;
                cw[11] ^= mask as u8;
                let ok = rs129_decode(&mut cw).is_some();
                if ok {
                    let from = if dt == DT_VOICE_LC_HEADER { LcFrom::Header } else { LcFrom::Terminator };
                    out.push(SlotEvent::Lc { lc: Lc(cw[..9].try_into().unwrap()), from });
                    if dt == DT_TERMINATOR_LC {
                        self.pos = None;
                    }
                }
                ok
            }
            DT_CSBK | DT_MBC_HEADER => {
                let ok = fec::crc16_ok(&blk.bits, if dt == DT_CSBK { fec::MASK_CSBK } else { fec::MASK_MBC_HEADER });
                if ok {
                    out.push(SlotEvent::Csbk { csbk: Csbk(bytes[..10].try_into().unwrap()), mbc: dt == DT_MBC_HEADER });
                }
                ok
            }
            DT_MBC_CONTINUATION => {
                out.push(SlotEvent::MbcContinuation(bytes));
                blk.errs >= 0
            }
            DT_PI_HEADER => {
                let ok = fec::crc16_ok(&blk.bits, fec::MASK_PI);
                if ok {
                    out.push(SlotEvent::Privacy { alg: bytes[0] & 7, key: bytes[2] });
                }
                ok
            }
            _ => blk.errs >= 0,
        };
        if ok {
            self.color_code = Some(cc);
        } else {
            self.bad_blocks += 1;
        }
        if !matches!(dt, DT_VOICE_LC_HEADER | DT_TERMINATOR_LC | DT_CSBK | DT_MBC_HEADER | DT_MBC_CONTINUATION | DT_PI_HEADER) || !ok {
            out.push(SlotEvent::Data { data_type: dt, ok });
        }
    }
}

/// A channel's two slots: bursts from the framer → (slot, event).
#[derive(Default)]
pub struct Channel {
    pub slots: [SlotDecoder; 2],
    last_slot: u8,
    /// Bursts whose CACH didn't decode (the slot was inferred).
    pub tact_errors: u64,
    ev: Vec<SlotEvent>,
}

impl Channel {
    /// Which slot `b` is: a repeater's CACH says; a direct-mode sync says;
    /// otherwise the other slot from the last burst's (they alternate).
    pub fn slot_of(&mut self, b: &Burst) -> u8 {
        let slot = match b.sync {
            Some(SyncKind::DirectVoice(s)) | Some(SyncKind::DirectData(s)) => s,
            Some(SyncKind::MsVoice) | Some(SyncKind::MsData) | Some(SyncKind::MsRc) => self.last_slot,
            _ => match cach(&b.cach).0 {
                Some(t) => t.slot,
                None => {
                    self.tact_errors += 1;
                    1 - self.last_slot
                }
            },
        };
        self.last_slot = slot;
        slot
    }

    pub fn burst(&mut self, b: &Burst, out: &mut Vec<(u8, SlotEvent)>) {
        let slot = self.slot_of(b);
        self.ev.clear();
        self.slots[slot as usize].burst(b, &mut self.ev);
        out.extend(self.ev.drain(..).map(|e| (slot, e)));
    }
}
