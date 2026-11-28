//! LTR (E.F. Johnson's Logic Trunked Radio): LTR Standard, LTR-Net and
//! Passport. No control channel: each repeater sends 300 baud status words
//! under its own voice (and alone, in an idle burst every few seconds), so a
//! site is watched whole ([`site::Site`]) and a call is found on the repeater
//! that carries it.
//!
//! ```text
//! repeater IQ → FM (no de-emphasis) → 8 kHz audio before the voice high-pass
//!   → Decimator (400 Hz, ÷4 → 2 kHz) → Slicers (300 baud NRZ, 8 phases)
//!   → each slicer's last 40 bits (LTR) and 68 (Passport) after a sync,
//!     either polarity → checksum → Word
//! ```
//!
//! Every word starts with the sync `101011000` (a radio's inbound words are
//! the same bits inverted). Fields are numbered from that first sync bit:
//!
//! ```text
//! LTR, LTR-Net (40): sync 0–8 · area 9 · channel 10–14 · home 15–19 · group 20–27 · free 28–32 · checksum 33–39
//! Passport (68):     sync 0–8 · DCC 9–10 · LCN 11–21 · site 22–28 · group 29–44 · type 45–48 · free 49–59 · checksum 60–67
//! ```
//!
//! Both checksums are linear: one value per field bit, XORed together for
//! the bits set (LTR's 7 bits over 9–32; Passport's 8 over 9–59, which with
//! the sent checksum XORs to 0). An LTR talkgroup is written A-HH-GGG (area,
//! home repeater, group): [`ltr_talkgroup`] numbers it AHHGGG as scanners
//! do; a Passport talkgroup is its 16-bit group.
//!
//! The field layouts, checksum values and LTR-Net message numbers are
//! those documented by the RadioReference wiki and sdrtrunk's decoders; no
//! LTR system has been heard by this decoder yet (issue #267).

pub mod site;

use num_complex::Complex32;

use crate::dsp::fm::{Nbfm, AUDIO_RATE};
use crate::dsp::nrz::{Decimator, Slicers, NPHASE};
use crate::dsp::AnalogTuning;

pub use site::{LtrConfig, Site, SiteStatus};

/// Bits a second.
pub const BAUD: f32 = 300.0;
/// One-sided channel filter cutoff of a watched repeater, Hz (its voice's, as an FM recorder's).
pub const CHANNEL_CUTOFF_HZ: f64 = 7000.0;
/// The sync every outbound word starts with.
const SYNC: u128 = 0b1_0101_1000;
const SYNC_BITS: u32 = 9;
const LTR_BITS: u32 = 40;
const PASSPORT_BITS: u32 = 68;
/// The 8 kHz audio's decimation, and the rate the slicers run at.
const DECIM: usize = 4;
const RATE: f32 = (AUDIO_RATE / DECIM as f64) as f32;
/// The highest LCN an LTR site has.
pub const LAST_LCN: u8 = 20;

/// Which of the three a site is, as its words show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Flavor {
    Standard,
    /// LTR Standard's words, and messages of its own (unique IDs, site ID, frequencies, neighbours).
    Net,
    Passport,
}

impl Flavor {
    pub fn name(self) -> &'static str {
        match self {
            Flavor::Standard => "LTR Standard",
            Flavor::Net => "LTR-Net",
            Flavor::Passport => "Passport",
        }
    }
}

/// An LTR talkgroup's number: A-HH-GGG as AHHGGG (1-05-123 → 105123), as scanners write it.
pub fn ltr_talkgroup(area: bool, home: u8, group: u8) -> u32 {
    area as u32 * 100_000 + home as u32 * 1000 + group as u32
}

/// [`ltr_talkgroup`]'s number as RadioReference writes it: `1-05-123`.
pub fn ltr_label(tg: u32) -> String {
    format!("{}-{:02}-{:03}", tg / 100_000, tg / 1000 % 100, tg % 1000)
}

/// Bits `a..=b` of a `len`-bit word (bit 0 sent first), the first sent highest.
fn field(w: u128, len: u32, a: u32, b: u32) -> u32 {
    ((w >> (len - 1 - b)) & ((1u128 << (b - a + 1)) - 1)) as u32
}

/// LTR's checksum: one value per bit of 9–32 (area, channel, home, group, free), XORed for the bits set.
const LTR_CHECK: [u8; 24] = [
    0x38, // area
    0x1C, 0x0E, 0x46, 0x23, 0x51, // channel, high bit first
    0x68, 0x75, 0x7A, 0x3D, 0x1F, // home
    0x4F, 0x26, 0x52, 0x29, 0x15, 0x0B, 0x45, 0x62, // group
    0x31, 0x19, 0x0D, 0x07, 0x43, // free
];

/// Passport's: one value per bit of 9–59 (DCC, LCN, site, group, type, free).
const PASSPORT_CHECK: [u8; 51] = [
    0x6E, 0xBF, // DCC
    0xD6, 0xE3, 0xF8, 0x7C, 0x3E, 0x97, 0xC2, 0xE9, 0x75, 0x3B, 0x94, // LCN
    0x4A, 0xAD, 0x57, 0xA2, 0xD9, 0x6D, 0x37, // site
    0x92, 0xC1, 0x61, 0x31, 0x19, 0x0D, 0x07, 0x8A, 0xCD, 0x67, 0xBA, 0xD5, 0x6B, 0xBC, 0x5E, 0xA7, // group
    0xDA, 0xE5, 0x73, 0xB0, // type
    0x58, 0x2C, 0x16, 0x83, 0xC8, 0x64, 0x32, 0x91, 0x49, 0x25, 0x13, // free
];

/// The checksum of a `len`-bit word's bits from 9, by `table`.
fn check(w: u128, len: u32, table: &[u8]) -> u8 {
    table.iter().enumerate().filter(|&(i, _)| field(w, len, 9 + i as u32, 9 + i as u32) == 1).fold(0, |c, (_, &v)| c ^ v)
}

/// An LTR (Standard or Net) outbound status word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct LtrWord {
    pub area: bool,
    /// The LCN the call is on (LTR-Net: above 20, the kind of message).
    pub channel: u8,
    /// The group's home repeater (31: the end of a call).
    pub home: u8,
    pub group: u8,
    /// A free repeater for the next call (0: none).
    pub free: u8,
}

/// A Passport word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PassportWord {
    pub dcc: u8,
    pub lcn: u16,
    pub site: u8,
    pub group: u16,
    pub kind: u8,
    pub free: u16,
}

/// A word read off a repeater.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Word {
    Ltr(LtrWord),
    Passport(PassportWord),
}

impl LtrWord {
    /// From 40 bits (the first sent highest): sync and checksum checked.
    pub fn decode(w: u64) -> Option<LtrWord> {
        let w = w as u128;
        if field(w, LTR_BITS, 0, 8) as u128 != SYNC || check(w, LTR_BITS, &LTR_CHECK) != field(w, LTR_BITS, 33, 39) as u8 {
            return None;
        }
        let f = |a, b| field(w, LTR_BITS, a, b) as u8;
        Some(LtrWord { area: f(9, 9) == 1, channel: f(10, 14), home: f(15, 19), group: f(20, 27), free: f(28, 32) })
    }

    /// Its 40 bits, as sent.
    pub fn encode(&self) -> u64 {
        let body = (self.area as u128) << 23 | (self.channel as u128 & 31) << 18 | (self.home as u128 & 31) << 13 | (self.group as u128) << 5 | (self.free as u128 & 31);
        let w = SYNC << 31 | body << 7;
        (w | check(w, LTR_BITS, &LTR_CHECK) as u128) as u64
    }

    /// Bits 17–32 (the low 3 of home, group, free): an LTR-Net unique ID or ESN half.
    fn sixteen(&self) -> u32 {
        (self.home as u32 & 7) << 13 | (self.group as u32) << 5 | self.free as u32
    }

    /// Bits 23–32 (the low 5 of group, free): an LTR-Net site ID.
    fn site_id(&self) -> u32 {
        (self.group as u32 & 31) << 5 | self.free as u32
    }

    /// What it says. LTR-Net's own messages carry an LCN above 20 (or a home
    /// above 20) and name what they are by the channel field.
    pub fn status(&self) -> Status {
        let (ch, home) = (self.channel, self.home);
        let tg = || ltr_talkgroup(self.area, home, self.group);
        if home == 31 || ch == 31 {
            // Turn off: the call (or idle burst) is over.
            return Status::End { tg: (home <= LAST_LCN && home > 0).then(tg) };
        }
        if ch > LAST_LCN || home > LAST_LCN {
            return match ch {
                17 => Status::Registered { unit: self.sixteen() },
                18 => Status::Site { site: self.site_id() },
                // (24 and 25, each LCN's frequencies, are left out: the layout documented,
                // 150 MHz + 16 bits of 1.25 kHz, can't say an 800 MHz one.)
                26 => Status::Neighbour { rank: (home >> 1) + 1, site: self.site_id() },
                _ => Status::Other,
            };
        }
        match self.group {
            255 => Status::Idle { lcn: ch as u16, site: None },
            // A unique ID being assigned (the "do nothing" group), a CW ID.
            253 | 254 => Status::Other,
            _ if ch == 0 || home == 0 => Status::Other,
            _ => Status::Call { lcn: ch as u16, tg: tg(), data: false },
        }
    }

    /// One of LTR-Net's own (not a word LTR Standard has).
    pub fn net(&self) -> bool {
        self.home != 31 && self.channel != 31 && (self.channel > LAST_LCN || self.home > LAST_LCN)
    }
}

impl PassportWord {
    /// From 68 bits (the first sent highest): sync and checksum checked.
    pub fn decode(w: u128) -> Option<PassportWord> {
        if field(w, PASSPORT_BITS, 0, 8) as u128 != SYNC || check(w, PASSPORT_BITS, &PASSPORT_CHECK) != field(w, PASSPORT_BITS, 60, 67) as u8 {
            return None;
        }
        let f = |a, b| field(w, PASSPORT_BITS, a, b);
        Some(PassportWord { dcc: f(9, 10) as u8, lcn: f(11, 21) as u16, site: f(22, 28) as u8, group: f(29, 44) as u16, kind: f(45, 48) as u8, free: f(49, 59) as u16 })
    }

    /// Its 68 bits, as sent.
    pub fn encode(&self) -> u128 {
        let body = (self.dcc as u128 & 3) << 49 | (self.lcn as u128 & 0x7ff) << 38 | (self.site as u128 & 0x7f) << 31 | (self.group as u128) << 15 | (self.kind as u128 & 15) << 11 | (self.free as u128 & 0x7ff);
        let w = SYNC << 59 | body << 8;
        w | check(w, PASSPORT_BITS, &PASSPORT_CHECK) as u128
    }

    /// Bits 22–44: a mobile's ID (MIN).
    fn min(&self) -> u32 {
        (self.site as u32) << 16 | self.group as u32
    }

    pub fn status(&self) -> Status {
        match self.kind {
            1 if self.free == 2042 => Status::Other,
            1 if self.lcn == 1792 || self.lcn == 1793 => Status::Idle { lcn: 0, site: Some(self.site as u32) },
            1 if self.lcn == 2047 => Status::End { tg: Some(self.group as u32) },
            0..=2 if self.lcn < 1792 && self.group != 0 => Status::Call { lcn: self.lcn, tg: self.group as u32, data: false },
            6 => Status::Talker { unit: self.min() },
            9 if self.lcn < 1792 && self.group != 0 => Status::Call { lcn: self.lcn, tg: self.group as u32, data: true },
            11 => Status::Registered { unit: 0 },
            _ => Status::Other,
        }
    }
}

impl Word {
    pub fn status(&self) -> Status {
        match self {
            Word::Ltr(w) => w.status(),
            Word::Passport(w) => w.status(),
        }
    }

    /// The kind of site that sends it.
    pub fn flavor(&self) -> Flavor {
        match self {
            Word::Ltr(w) if w.net() => Flavor::Net,
            Word::Ltr(_) => Flavor::Standard,
            Word::Passport(_) => Flavor::Passport,
        }
    }
}

/// What a word says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// A call (`data`: Passport's data call) for talkgroup `tg` on `lcn`.
    Call { lcn: u16, tg: u32, data: bool },
    /// A call is over (talkgroup `tg`, when it says), or an idle burst.
    End { tg: Option<u32> },
    /// The repeater is idle (LTR: it names itself, LCN `lcn`; Passport: 0, and names its site).
    Idle { lcn: u16, site: Option<u32> },
    /// A radio was registered (LTR-Net: given unique ID `unit`; Passport: 0, unnamed).
    Registered { unit: u32 },
    /// The radio talking on this repeater (Passport's MIN).
    Talker { unit: u32 },
    /// LTR-Net: the site's ID.
    Site { site: u32 },
    /// LTR-Net: a neighbouring site, by rank.
    Neighbour { rank: u8, site: u32 },
    Other,
}

/// A repeater's receiver: IQ in, words out.
pub struct Receiver {
    fm: Nbfm,
    decim: Decimator,
    slicers: Slicers,
    /// Each slicer's last bits, the latest lowest.
    regs: [u128; NPHASE],
    audio: Vec<f32>,
    low: Vec<f32>,
}

impl Receiver {
    pub fn new(rate: f64) -> Self {
        // No de-emphasis: NRZ data keeps its shape. (The squelch is left open: a word's checksum tells.)
        let tuning = AnalogTuning { deemph_tau_s: 0.0, ..AnalogTuning::default() };
        Receiver {
            fm: Nbfm::with(rate, &tuning),
            decim: Decimator::new(129, 400.0 / AUDIO_RATE, DECIM),
            // DC (the carrier's offset) followed over ~0.5 s: LTR's words aren't DC-free.
            slicers: Slicers::new(BAUD, RATE, 1.0 / (0.5 * RATE)),
            regs: [0; NPHASE],
            audio: Vec::new(),
            low: Vec::new(),
        }
    }

    /// IQ whose first sample was at air time `t0`: each word read, with when it ended.
    /// Several slicers read one word: each is given.
    pub fn push(&mut self, iq: &[Complex32], t0: f64, out: &mut Vec<(f64, Word)>) {
        self.audio.clear();
        self.low.clear();
        self.fm.push_low(iq, 0.0, &mut self.audio, Some(&mut self.low));
        let Receiver { decim, slicers, regs, low, .. } = self;
        let mut n = 0usize;
        decim.push(low, |y| {
            n += 1;
            let t = t0 + n as f64 / RATE as f64;
            slicers.push(y, |k, b| {
                let r = &mut regs[k];
                *r = *r << 1 | b as u128;
                if let Some(w) = read(*r) {
                    out.push((t, w));
                }
            });
        });
    }
}

/// The word that ends with a slicer's latest bit, if one does: LTR's 40
/// bits or Passport's 68 after a sync, sent either way up (a receiver's
/// discriminator, or a radio's inbound word, has it inverted).
fn read(r: u128) -> Option<Word> {
    let at = |len: u32| (r >> (len - SYNC_BITS)) & ((1 << SYNC_BITS) - 1);
    let whole = |len: u32| r & ((1u128 << len) - 1);
    let upright = |len: u32| match at(len) {
        s if s == SYNC => Some(whole(len)),
        s if s == !SYNC & ((1 << SYNC_BITS) - 1) => Some(!whole(len) & ((1u128 << len) - 1)),
        _ => None,
    };
    if let Some(w) = upright(LTR_BITS).and_then(|w| LtrWord::decode(w as u64)) {
        return Some(Word::Ltr(w));
    }
    upright(PASSPORT_BITS).and_then(PassportWord::decode).map(Word::Passport)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Words as bits sent back to back, first bit first.
    pub fn bits(words: &[Word]) -> Vec<bool> {
        let mut out = Vec::new();
        for w in words {
            let (v, len) = match w {
                Word::Ltr(l) => (l.encode() as u128, LTR_BITS),
                Word::Passport(p) => (p.encode(), PASSPORT_BITS),
            };
            out.extend((0..len).rev().map(|i| v >> i & 1 == 1));
        }
        out
    }

    /// FM at `rate` carrying `bits` at 300 baud (±`dev` Hz), under a voice-like tone, `carrier` Hz
    /// off the channel; `bits` None: no carrier.
    pub fn modulate(bits: Option<&[bool]>, secs: f64, rate: f64, dev: f64, carrier: f64, ph: &mut f64, t0: f64) -> Vec<Complex32> {
        (0..(secs * rate) as usize)
            .map(|i| {
                let t = t0 + i as f64 / rate;
                let Some(bits) = bits else { return Complex32::default() };
                let b = bits[(t * BAUD as f64) as usize % bits.len()];
                let voice = 2000.0 * (2.0 * PI * 900.0 * t).sin();
                *ph = (*ph + 2.0 * PI * (carrier + voice + if b { dev } else { -dev }) / rate) % (2.0 * PI);
                Complex32::from_polar(0.5, *ph as f32)
            })
            .collect()
    }

    #[test]
    fn ltr_words_check_and_read_back() {
        let w = LtrWord { area: false, channel: 5, home: 5, group: 4, free: 3 };
        let bits = w.encode();
        assert_eq!(bits >> 31, SYNC as u64);
        assert_eq!(LtrWord::decode(bits), Some(w));
        for b in 0..40 {
            assert_eq!(LtrWord::decode(bits ^ 1 << b), None, "bit {b} flipped");
        }
        assert_eq!(w.status(), Status::Call { lcn: 5, tg: 5004, data: false });
        assert_eq!(ltr_label(105_123), "1-05-123");
    }

    /// sigidwiki's example decodes: `0 05 05 004 03 3D` (a call on LCN 5,
    /// 0-05-004, free 3) and `0 31 05 004 05 06` (its end): their sent checksums.
    #[test]
    fn the_wikis_examples_check() {
        let sum = |w: LtrWord| w.encode() & 0x7f;
        assert_eq!(sum(LtrWord { area: false, channel: 5, home: 5, group: 4, free: 3 }), 0x3D);
        assert_eq!(sum(LtrWord { area: false, channel: 31, home: 5, group: 4, free: 5 }), 0x06);
        assert_eq!(LtrWord { area: false, channel: 31, home: 5, group: 4, free: 5 }.status(), Status::End { tg: Some(5004) });
    }

    #[test]
    fn ltr_net_messages() {
        // The wiki's "0 17 24 003 10": unique ID 106.
        let reg = LtrWord { area: false, channel: 17, home: 24, group: 3, free: 10 };
        assert!(reg.net());
        assert_eq!(reg.status(), Status::Registered { unit: 106 });
        let site = LtrWord { area: true, channel: 18, home: 24, group: 0b0001_0011, free: 0b00101 };
        assert_eq!(site.status(), Status::Site { site: 0b10011_00101 });
        assert_eq!(LtrWord { area: true, channel: 24, home: 3, group: 0x80, free: 5 }.status(), Status::Other);
        assert_eq!(LtrWord { area: false, channel: 7, home: 2, group: 255, free: 7 }.status(), Status::Idle { lcn: 7, site: None });
    }

    #[test]
    fn passport_words_check_and_read_back() {
        let w = PassportWord { dcc: 1, lcn: 357, site: 12, group: 4321, kind: 0, free: 360 };
        let bits = w.encode();
        assert_eq!(PassportWord::decode(bits), Some(w));
        for b in 0..68 {
            assert_eq!(PassportWord::decode(bits ^ 1 << b), None, "bit {b} flipped");
        }
        assert_eq!(w.status(), Status::Call { lcn: 357, tg: 4321, data: false });
        assert_eq!(PassportWord { kind: 1, lcn: 2047, group: 4321, ..w }.status(), Status::End { tg: Some(4321) });
        assert_eq!(PassportWord { kind: 6, site: 1, group: 2, ..w }.status(), Status::Talker { unit: 1 << 16 | 2 });
    }

    /// Off the air: words under voice, at 1 kHz deviation, either way up
    /// and a little off frequency, are read — and noise alone reads none.
    #[test]
    fn the_receiver_reads_words_under_voice() {
        let rate = 25_000.0;
        let words = [Word::Ltr(LtrWord { area: true, channel: 3, home: 7, group: 42, free: 9 }), Word::Passport(PassportWord { dcc: 2, lcn: 77, site: 3, group: 900, kind: 0, free: 78 })];
        for (word, dev, carrier) in [(words[0], 1000.0, 300.0), (words[0], -1000.0, -500.0), (words[1], 800.0, 0.0)] {
            let mut ph = 0.0;
            let iq = modulate(Some(&bits(&[word])), 2.0, rate, dev, carrier, &mut ph, 0.0);
            let mut rx = Receiver::new(rate);
            let mut out = Vec::new();
            for c in iq.chunks(500) {
                rx.push(c, 0.0, &mut out);
            }
            assert!(out.len() >= 5, "{word:?} at {dev} Hz: {} read", out.len());
            assert!(out.iter().all(|(_, w)| *w == word), "{out:?}");
        }
        let mut x = 7u32;
        let noise: Vec<Complex32> = (0..(rate * 10.0) as usize)
            .map(|_| {
                let mut u = || {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as f32 / u32::MAX as f32 - 0.5
                };
                Complex32::new(u(), u())
            })
            .collect();
        let mut rx = Receiver::new(rate);
        let mut out = Vec::new();
        rx.push(&noise, 0.0, &mut out);
        // (A 7-bit checksum lets the odd one through: the site wants a word twice.)
        assert!(out.len() <= 3, "{out:?}");
    }
}
