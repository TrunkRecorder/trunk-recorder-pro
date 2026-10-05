//! NXDN frames (TS 1-A §4.3–4.6, §5.2–5.3): the sync search, the framer
//! that cuts the symbol stream into 192-symbol frames ([`Framer`] →
//! [`Frame`]), the scrambler and the LICH.
//!
//! ```text
//! a frame, 192 symbols (80 ms at 2400 baud, 40 ms at 4800):
//!   FSW 10 │ LICH 8 │ 174 symbols: by the LICH, one of
//!     traffic, voice:  SACCH 30 │ VCH 36 │ VCH 36 │ VCH 36 │ VCH 36   (a FACCH1 72 may take two VCHs' place)
//!     traffic, data:   UDCH / FACCH2 174
//!     control (out):   CAC 150 │ post / collision fields 24
//! ```
//!
//! Everything after the FSW is scrambled: a PN9 sequence, restarted every
//! frame, inverts the sign of the symbols it marks.

use std::collections::VecDeque;

use crate::dsp::Symbol;

pub const FRAME_SYMBOLS: usize = 192;
pub const FSW_SYMBOLS: usize = 10;
/// Symbols after the FSW.
pub const BODY_SYMBOLS: usize = FRAME_SYMBOLS - FSW_SYMBOLS;
pub const BODY_BITS: usize = 2 * BODY_SYMBOLS;

/// Frame Sync Word: −3 +1 −3 +3 −3 −3 +3 +3 −1 +3 (dibits 11 00 11 01 11 11 01 01 10 01).
pub const FSW: u32 = 0xcdf59;
/// The FSW with every symbol's sign flipped: what spectrally inverted IQ
/// (I and Q swapped somewhere upstream) delivers. The sigidwiki NXDN
/// recordings are like that.
const FSW_INVERTED: u32 = FSW ^ 0xaaaaa;

/// Bits a FSW may differ by to lock the framer (anywhere), and to count as
/// the sync where the grid expects one.
const LOCK_ERRS: u32 = 1;
const GRID_ERRS: u32 = 6;
/// Frames with neither a sync nor a valid LICH before the framer lets go.
const UNLOCK_FRAMES: u32 = 4;

/// The scrambler (§4.6): PN9 x⁹ + x⁴ + 1 from 0x0E4, one bit per symbol;
/// a 1 inverts the symbol.
pub const SCRAMBLE: [u8; BODY_SYMBOLS] = scramble_table();

const fn scramble_table() -> [u8; BODY_SYMBOLS] {
    let mut t = [0u8; BODY_SYMBOLS];
    let mut l: u32 = 0x0e4;
    let mut i = 0;
    while i < BODY_SYMBOLS {
        t[i] = (l & 1) as u8;
        let fb = (l ^ l >> 4) & 1;
        l = l >> 1 | fb << 8;
        i += 1;
    }
    t
}

/// The RF channel a LICH names (§5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RfChannel {
    /// Trunked control channel.
    Control,
    /// Trunked traffic channel.
    Traffic,
    /// Conventional (repeater or direct) channel.
    Direct,
    /// Composite control channel (a control channel also carrying traffic);
    /// on a Type-D system, its traffic channel (RTCH2).
    Composite,
}

/// The LICH: what the rest of the frame carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lich {
    /// The 7 bits (RF channel 2, functional channel 2, option 2, direction 1).
    pub raw: u8,
}

/// What a traffic frame's body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// SACCH, then two halves each either two voice frames or a FACCH1.
    /// `superframe`: the SACCH is a quarter of a 72-bit message (else a
    /// whole 18-bit one, at a call's start and end). `idle`: no voice
    /// (the halves carry FACCH1 or nothing).
    Voice { superframe: bool, idle: bool, facch: [bool; 2] },
    /// UDCH (user data), or FACCH2 (control).
    Udch,
    Facch2,
}

impl Lich {
    pub fn rf(self) -> RfChannel {
        match self.raw >> 5 {
            0 => RfChannel::Control,
            1 => RfChannel::Traffic,
            2 => RfChannel::Direct,
            _ => RfChannel::Composite,
        }
    }
    /// Functional channel type (2 bits).
    pub fn fct(self) -> u8 {
        self.raw >> 3 & 3
    }
    /// Option field (2 bits): on traffic, which halves carry FACCH1.
    pub fn option(self) -> u8 {
        self.raw >> 1 & 3
    }
    /// From the repeater / base station.
    pub fn outbound(self) -> bool {
        self.raw & 1 != 0
    }
    /// A control channel frame carrying a CAC (outbound) / long / short CAC (inbound).
    pub fn is_cac(self) -> bool {
        self.rf() == RfChannel::Control || (self.rf() == RfChannel::Composite && self.fct() == 0 && self.outbound())
    }

    /// A traffic channel's (or conventional channel's) body, by the
    /// functional channel and option.
    pub fn body(self) -> Body {
        match self.fct() {
            1 if self.option() == 0 => Body::Facch2,
            1 => Body::Udch,
            fct => {
                // Option: 3 voice both halves, 2 FACCH1 in the second, 1 in the first, 0 both.
                let o = self.option();
                Body::Voice { superframe: fct >= 2, idle: fct == 3, facch: [o & 2 == 0, o & 1 == 0] }
            }
        }
    }

    /// Encode as 8 dibits (bit 1 → −3, 0 → +3), parity last.
    pub fn dibits(self) -> [u8; 8] {
        let p = (self.raw >> 6 ^ self.raw >> 5 ^ self.raw >> 4 ^ self.raw >> 3) & 1;
        let bits = self.raw << 1 | p;
        std::array::from_fn(|i| if bits >> (7 - i) & 1 != 0 { 0b11 } else { 0b01 })
    }
}

/// One frame, descrambled: the 182 symbols after the FSW.
#[derive(Clone, Debug)]
pub struct Frame {
    pub dibits: [u8; BODY_SYMBOLS],
    /// Each body bit's reliability (≥ 0), air order (2 per dibit).
    pub rel: [f32; BODY_BITS],
    /// Bits the FSW differed by, when there was one where expected.
    pub sync_errs: Option<u32>,
    /// Channel-sample instant of the frame's first (FSW) symbol.
    pub sample: f64,
}

impl Frame {
    /// Body bit `i` (air order: dibit `i / 2`, high bit first).
    pub fn bit(&self, i: usize) -> u8 {
        self.dibits[i / 2] >> (1 - i % 2) & 1
    }

    /// Bit `i` as a soft value: its reliability, + for a 1, − for a 0.
    pub fn soft(&self, i: usize) -> f32 {
        let r = self.rel[i];
        if self.bit(i) != 0 { r } else { -r }
    }

    /// Soft bits `a..b` of the body.
    pub fn soft_range(&self, a: usize, b: usize) -> Vec<f32> {
        (a..b).map(|i| self.soft(i)).collect()
    }

    /// The LICH, when its parity holds and its symbols are outer ones (at
    /// most one isn't), and the number of symbols that weren't.
    pub fn lich(&self) -> Option<(Lich, u32)> {
        let mut bits = 0u8;
        let mut inner = 0;
        for d in &self.dibits[..8] {
            bits = bits << 1 | d >> 1;
            inner += (d & 1 == 0) as u32;
        }
        let raw = bits >> 1;
        let p = (raw >> 6 ^ raw >> 5 ^ raw >> 4 ^ raw >> 3) & 1;
        (p == bits & 1 && inner <= 1).then_some((Lich { raw }, inner))
    }

    /// Voice frame `k` (0..4) of a voice body: its 36 dibits and 72 bit reliabilities.
    pub fn voice_frame(&self, k: usize) -> ([u8; 36], [f32; 72]) {
        let at = 38 + 36 * k;
        let d = std::array::from_fn(|j| self.dibits[at + j]);
        let r = std::array::from_fn(|b| self.rel[2 * at + b]);
        (d, r)
    }
}

/// Cuts decided symbols into frames, holding the 192-symbol grid between syncs.
pub struct Framer {
    buf: VecDeque<Symbol>,
    win: u32,
    n: u64,
    /// Symbol count at which the next frame's FSW / last symbol arrives (locked).
    next_fsw: Option<u64>,
    frame_end: u64,
    pending: Option<u32>,
    /// The frame being completed is the first since locking.
    fresh: bool,
    /// Symbols arrive sign-flipped (locked on the inverted FSW).
    pub inverted: bool,
    bad: u32,
    pub frames: u64,
    pub syncs: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Framer { buf: VecDeque::new(), win: 0, n: 0, next_fsw: None, frame_end: 0, pending: None, fresh: false, inverted: false, bad: 0, frames: 0, syncs: 0 }
    }

    pub fn locked(&self) -> bool {
        self.next_fsw.is_some()
    }

    fn lock(&mut self, i: u64, errs: u32, inverted: bool) {
        self.inverted = inverted;
        self.pending = Some(errs);
        self.frame_end = i + BODY_SYMBOLS as u64;
        self.next_fsw = Some(i + FRAME_SYMBOLS as u64);
        self.fresh = true;
        self.bad = 0;
    }

    pub fn push(&mut self, s: &Symbol, out: &mut Vec<Frame>) {
        self.buf.push_back(*s);
        if self.buf.len() > FRAME_SYMBOLS {
            self.buf.pop_front();
        }
        self.win = (self.win << 2 | (s.dibit & 3) as u32) & 0xf_ffff;
        let i = self.n;
        self.n += 1;
        let errs = (self.win ^ FSW).count_ones();
        let errs_inv = (self.win ^ FSW_INVERTED).count_ones();
        // The cleaner of the two polarities, for a lock anywhere.
        let (best, best_inv) = if errs_inv < errs { (errs_inv, true) } else { (errs, false) };
        let full = i + 1 >= FSW_SYMBOLS as u64;
        match self.next_fsw {
            Some(at) if at == i => {
                let e = if self.inverted { errs_inv } else { errs };
                self.pending = (e <= GRID_ERRS).then_some(e);
                self.frame_end = i + BODY_SYMBOLS as u64;
                self.next_fsw = Some(i + FRAME_SYMBOLS as u64);
            }
            // A clean FSW off the grid, when the grid's last one was missed: the grid moved.
            Some(_) if best <= LOCK_ERRS && self.bad > 0 && full => self.lock(i, best, best_inv),
            None if best <= LOCK_ERRS && full => self.lock(i, best, best_inv),
            _ => {}
        }
        if self.next_fsw.is_some() && i == self.frame_end {
            self.emit(out);
        }
    }

    fn emit(&mut self, out: &mut Vec<Frame>) {
        let sync = self.pending.take();
        let fresh = std::mem::take(&mut self.fresh);
        if self.buf.len() < FRAME_SYMBOLS {
            return;
        }
        let mut f = Frame { dibits: [0; BODY_SYMBOLS], rel: [0.0; BODY_BITS], sync_errs: sync, sample: self.buf[0].sample };
        let flip = if self.inverted { 2 } else { 0 };
        for (k, s) in self.buf.iter().skip(FSW_SYMBOLS).enumerate() {
            f.dibits[k] = s.dibit ^ flip ^ SCRAMBLE[k] << 1;
            f.rel[2 * k] = s.rel_hi;
            f.rel[2 * k + 1] = s.rel_lo;
        }
        let lich_ok = f.lich().is_some();
        if fresh && !lich_ok {
            // A 20-bit FSW is short: a lock off noise doesn't survive its LICH.
            self.next_fsw = None;
            return;
        }
        if sync.is_some() || lich_ok {
            self.bad = 0;
        } else {
            self.bad += 1;
            if self.bad >= UNLOCK_FRAMES {
                self.next_fsw = None;
                return;
            }
        }
        if sync.is_some() {
            self.syncs += 1;
        }
        self.frames += 1;
        out.push(f);
    }
}

/// Encode a frame's symbols (FSW, then the body scrambled), from 182 body dibits.
pub fn frame_dibits(body: &[u8; BODY_SYMBOLS]) -> [u8; FRAME_SYMBOLS] {
    let mut d = [0u8; FRAME_SYMBOLS];
    for (i, x) in d.iter_mut().take(FSW_SYMBOLS).enumerate() {
        *x = (FSW >> (18 - 2 * i) & 3) as u8;
    }
    for k in 0..BODY_SYMBOLS {
        d[FSW_SYMBOLS + k] = body[k] ^ SCRAMBLE[k] << 1;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrambler_matches_op25s_table() {
        // op25 nxdn.cc scramble_t: the first inverted symbol positions.
        let inv: Vec<usize> = (0..BODY_SYMBOLS).filter(|&i| SCRAMBLE[i] != 0).take(12).collect();
        assert_eq!(inv, [2, 5, 6, 7, 10, 12, 14, 16, 17, 22, 23, 25]);
    }

    #[test]
    fn fsw_symbols() {
        // −3 +1 −3 +3 −3 −3 +3 +3 −1 +3 with 01 → +3, 00 → +1, 10 → −1, 11 → −3.
        let level = |d: u32| match d {
            0b01 => 3,
            0b00 => 1,
            0b10 => -1,
            _ => -3,
        };
        let s: Vec<i32> = (0..10).map(|i| level(FSW >> (18 - 2 * i) & 3)).collect();
        assert_eq!(s, [-3, 1, -3, 3, -3, -3, 3, 3, -1, 3]);
    }

    #[test]
    fn lich_values() {
        // Voice both halves, superframe, outbound on a conventional channel: 0x57.
        let l = Lich { raw: 0x57 };
        assert_eq!(l.rf(), RfChannel::Direct);
        assert_eq!(l.body(), Body::Voice { superframe: true, idle: false, facch: [false, false] });
        assert!(l.outbound());
        assert_eq!(Lich { raw: 0x35 }.body(), Body::Voice { superframe: true, idle: false, facch: [false, true] });
        assert_eq!(Lich { raw: 0x33 }.body(), Body::Voice { superframe: true, idle: false, facch: [true, false] });
        assert_eq!(Lich { raw: 0x21 }.body(), Body::Voice { superframe: false, idle: false, facch: [true, true] });
        assert_eq!(Lich { raw: 0x29 }.body(), Body::Facch2);
        assert_eq!(Lich { raw: 0x2f }.body(), Body::Udch);
        assert!(Lich { raw: 0x01 }.is_cac());
    }

    fn frames_of(syms: &[u8]) -> Vec<Frame> {
        let mut f = Framer::new();
        let mut out = Vec::new();
        for (i, &d) in syms.iter().enumerate() {
            f.push(&Symbol { dibit: d, sample: i as f64, rel_hi: 1.0, rel_lo: 1.0 }, &mut out);
        }
        out
    }

    fn body_with_lich(raw: u8, seed: &mut u32) -> [u8; BODY_SYMBOLS] {
        let mut b = [0u8; BODY_SYMBOLS];
        for d in b.iter_mut() {
            *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *d = (*seed >> 30) as u8;
        }
        b[..8].copy_from_slice(&Lich { raw }.dibits());
        b
    }

    #[test]
    fn framer_descrambles_and_keeps_the_grid() {
        let mut seed = 7;
        let mut syms: Vec<u8> = (0..57).map(|i| (i * 7 % 4) as u8).collect();
        let mut bodies = Vec::new();
        for k in 0..6 {
            let b = body_with_lich(0x57, &mut seed);
            let mut f = frame_dibits(&b);
            if k == 2 || k == 3 {
                // Two frames' syncs lost in a fade: the grid holds.
                for d in f.iter_mut().take(4) {
                    *d ^= 3;
                }
            }
            syms.extend(f);
            bodies.push(b);
        }
        let out = frames_of(&syms);
        assert_eq!(out.len(), 6);
        for (k, f) in out.iter().enumerate() {
            assert_eq!(f.dibits, bodies[k], "frame {k}");
            assert_eq!(f.sample, (57 + k * FRAME_SYMBOLS) as f64);
            assert_eq!(f.lich().unwrap().0, Lich { raw: 0x57 });
            assert_eq!(f.sync_errs.is_none(), k == 2 || k == 3, "frame {k}");
        }
    }

    #[test]
    fn inverted_symbols_are_framed_too() {
        let mut seed = 11;
        let mut syms = vec![1u8; 30];
        let mut bodies = Vec::new();
        for _ in 0..3 {
            let b = body_with_lich(0x56, &mut seed);
            syms.extend(frame_dibits(&b).map(|d| d ^ 2));
            bodies.push(b);
        }
        let out = frames_of(&syms);
        assert_eq!(out.len(), 3);
        for (k, f) in out.iter().enumerate() {
            assert_eq!(f.dibits, bodies[k]);
        }
    }

    #[test]
    fn a_lock_on_noise_without_a_lich_is_dropped() {
        // An FSW followed by a body whose LICH fails parity: no frame.
        let mut seed = 3;
        let mut b = body_with_lich(0x57, &mut seed);
        b[7] ^= 2; // the parity bit
        let out = frames_of(&frame_dibits(&b));
        assert!(out.is_empty());
    }
}
