//! Sub-audible tones on analog FM: which CTCSS tone or DCS code a call
//! carried, from the 8 kHz audio before the voice high-pass (Trunk
//! Recorder's `Tone` channel column; its PR #1137 names the JSON fields).
//!
//! ```text
//! audio (8 kHz) → low-pass, ÷8 → 1 kHz
//!   CTCSS: each of the 51 standard tones mixed to 0 Hz and summed over
//!          50 ms blocks. Every block: the strongest tone over the last
//!          250 ms against the median of all 51 (a real tone stands 20+ dB
//!          clear; voice, kept above 300 Hz by the radio, doesn't) and
//!          against the strongest more than 8 Hz away (DCS has many). Its
//!          frequency comes from how fast its sums turn from block to block
//!          (±10 Hz range, a fraction of a Hz after a few blocks), snapped
//!          to the nearest standard tone — so 150.0 and 151.4 Hz, or DCS's
//!          134.4 Hz turn-off and 136.5, aren't confused.
//!   DCS:   134.4 baud NRZ, DC removed, summed over each bit by NPHASE
//!          slicers on staggered bit clocks; a slicer's last 23 bits within
//!          one bit of a code's 23-bit Golay word, and the same code again
//!          23 bits later, is a frame of that code.
//! ```
//!
//! DCS words repeat continuously, so a word read from the wrong starting bit
//! can be another valid code (D023N and D047I are one signal): codes are
//! compared as classes of rotations, and a class is reported by its member
//! with normal polarity, first in the table.
//!
//! About 600 flops per 8 kHz sample (5 Mflop/s), on analog channels only,
//! while their carrier is up.

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use num_complex::Complex32;

use super::filters::lowpass;
use super::fm::AUDIO_RATE;

/// The CTCSS tones, tenths of a hertz.
pub const CTCSS: [u16; 51] = [
    670, 693, 719, 744, 770, 797, 825, 854, 885, 915, 948, 974, 1000, 1035, 1072, 1109, 1148, 1188, 1230, 1273, 1318, 1365, 1413, 1462, 1500, 1514,
    1567, 1598, 1622, 1655, 1679, 1713, 1738, 1773, 1799, 1835, 1862, 1899, 1928, 1966, 1995, 2035, 2065, 2107, 2181, 2257, 2291, 2336, 2418,
    2503, 2541,
];

/// The DCS codes (the 83 of Motorola's DPL plus scanners' extended set),
/// their octal digits read as a decimal number (D023 → 23).
pub const DCS: [u16; 112] = [
    6, 7, 15, 17, 21, 23, 25, 26, 27, 31, 32, 36, 43, 47, 50, 51, 53, 54, 65, 71, 72, 73, 74, 114, 115, 116, 122, 125, 131, 132, 134, 143, 145, 152,
    155, 156, 162, 165, 172, 174, 205, 212, 223, 225, 226, 232, 243, 244, 245, 246, 251, 252, 255, 261, 263, 265, 266, 271, 274, 306, 311, 315,
    325, 331, 332, 343, 346, 351, 356, 364, 365, 371, 411, 412, 413, 423, 431, 432, 445, 446, 452, 454, 455, 462, 464, 465, 466, 503, 506, 516,
    523, 526, 532, 546, 565, 606, 612, 624, 627, 631, 632, 654, 662, 664, 703, 712, 723, 731, 732, 734, 743, 754,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Tenths of a hertz (151.4 Hz → 1514).
    Ctcss(u16),
    /// Code (octal digits as decimal), inverted ("I").
    Dcs(u16, bool),
}

impl fmt::Display for Tone {
    /// Trunk Recorder's form: `151.4`, `D023N`.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Tone::Ctcss(t) => write!(f, "{}.{}", t / 10, t % 10),
            Tone::Dcs(c, inv) => write!(f, "D{c:03}{}", if inv { 'I' } else { 'N' }),
        }
    }
}

impl Tone {
    /// A channel's Tone as people write it — Trunk Recorder's `151.4` /
    /// `D023N`, RadioReference's `151.4 PL` / `023 DPL`, and `PL 151.4`,
    /// `151.4 Hz`, `D023`, `023`, `D023I`. Empty, `0`, `S` (Trunk Recorder's
    /// search) and `CSQ`: none. A bare number is a CTCSS tone when it is one
    /// (`100`), else a DCS code (`023`).
    pub fn parse(text: &str) -> Result<Option<Tone>, String> {
        let up = text.trim().to_uppercase();
        if ["", "0", "0.0", "S", "SEARCH", "CSQ", "NONE", "OFF", "ANY"].contains(&up.as_str()) {
            return Ok(None);
        }
        let words: Vec<&str> = up.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
        let dcs_word = words.iter().any(|w| ["DPL", "DCS", "CDCSS", "DTCS"].contains(w));
        let body: String = words.iter().filter(|w| !["PL", "TPL", "CTCSS", "CTC", "HZ", "TONE", "DPL", "DCS", "CDCSS", "DTCS"].contains(w)).copied().collect();
        let body = body.strip_suffix("HZ").unwrap_or(&body);
        let bad = || format!("\"{}\" isn't a CTCSS tone (67.0–254.1 Hz) or a DCS code (like D023N)", text.trim());
        // DCS: [D]ddd[N|I|R].
        let d = body.strip_prefix('D');
        let (digits, inverted) = {
            let b = d.unwrap_or(body);
            match b.chars().last() {
                Some('N') => (&b[..b.len() - 1], false),
                Some('I' | 'R') => (&b[..b.len() - 1], true),
                _ => (b, false),
            }
        };
        let octal = !digits.is_empty() && digits.len() <= 3 && digits.chars().all(|c| ('0'..='7').contains(&c));
        let marked = d.is_some() || dcs_word || digits.len() != body.len();
        if marked || (octal && !body.contains('.') && Self::ctcss_near(body).is_none()) {
            if !octal {
                return Err(bad());
            }
            let code: u16 = digits.parse().map_err(|_| bad())?;
            return if DCS.contains(&code) { Ok(Some(Tone::Dcs(code, inverted))) } else { Err(format!("D{code:03} isn't a DCS code")) };
        }
        let hz: f32 = body.parse().map_err(|_| bad())?;
        if let Some(t) = Self::ctcss_near(body) {
            return Ok(Some(Tone::Ctcss(t)));
        }
        let nearest = *CTCSS.iter().min_by(|&&a, &&b| (a as f32 / 10.0 - hz).abs().total_cmp(&(b as f32 / 10.0 - hz).abs())).unwrap();
        if (nearest as f32 / 10.0 - hz).abs() <= 3.0 {
            Err(format!("{hz} Hz isn't a standard CTCSS tone — {}?", Tone::Ctcss(nearest)))
        } else {
            Err(bad())
        }
    }

    fn ctcss_near(body: &str) -> Option<u16> {
        let hz: f32 = body.parse().ok()?;
        CTCSS.iter().copied().find(|&t| (t as f32 / 10.0 - hz).abs() < 0.05)
    }

    /// Whether `heard` on the air satisfies a channel set to `self` (a DCS
    /// code is satisfied by any code that is the same signal).
    pub fn matches(self, heard: Tone) -> bool {
        match (self, heard) {
            (Tone::Dcs(c, i), Tone::Dcs(..)) => dcs_aliases(c, i).contains(&heard),
            _ => self == heard,
        }
    }
}

/// The tone a call carried, and the share of it that carried the tone (0–1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneHeard {
    pub tone: Tone,
    pub confidence: f32,
}

const DECIM: usize = 8;
const RATE: f32 = (AUDIO_RATE / DECIM as f64) as f32;
const DECIM_TAPS: usize = 129;
/// CTCSS block, samples at 1 kHz (50 ms), and blocks per decision window.
const BLOCK: usize = 50;
const WINDOW: usize = 5;
/// The strongest tone's power over the median of all of them, to count (a
/// power ratio: 20× is 13 dB, well under the 20+ dB a real tone stands clear).
const CTCSS_OVER_MEDIAN: f32 = 20.0;
/// … and over the strongest tone more than 8 Hz away.
const CTCSS_OVER_OTHERS: f32 = 10.0;
/// Snapped only this close to a standard tone, Hz.
const CTCSS_SNAP_HZ: f32 = 1.0;
const DCS_BAUD: f32 = 134.4;
const NPHASE: usize = 8;
/// A DCS frame is 23 bits (171 ms); one counts per code at most this often, samples.
const DCS_FRAME_GAP: u32 = 150;
/// A verdict needs at least this share of the call, and this many frames (DCS) or windows (CTCSS).
const MIN_CONFIDENCE: f32 = 0.3;
const MIN_COUNT: u32 = 2;

/// A DCS code's 23 bits as a slicer holds them: the first bit sent in bit
/// 22. Sent: the 9 code bits low first, then 0 0 1, then the 11 parity bits
/// of the Golay (23,12) code (generator 0xC75) over those 12, low first.
pub fn dcs_pattern(code: u16, inverted: bool) -> u32 {
    let data = (code / 100 % 10) << 6 | (code / 10 % 10) << 3 | (code % 10);
    let msg = 0b100 << 9 | data as u32;
    let mut reg = msg << 11;
    for i in (11..23).rev() {
        if reg >> i & 1 == 1 {
            reg ^= 0xc75 << (i - 11);
        }
    }
    let sent = (0..12).map(|i| msg >> i & 1).chain((0..11).map(|i| reg >> i & 1));
    let p = sent.enumerate().fold(0u32, |p, (i, b)| p | b << (22 - i));
    if inverted { !p & 0x7f_ffff } else { p }
}

fn rotations(p: u32) -> impl Iterator<Item = u32> {
    (0..23).map(move |r| (p << r | p >> (23 - r)) & 0x7f_ffff)
}

struct DcsTable {
    /// Every code word and each one-bit error of it → index (code × 2 + inverted).
    lookup: HashMap<u32, u16>,
    /// Each index's class: the lowest index whose word is a rotation of its own.
    class: Vec<u16>,
}

fn dcs_table() -> &'static DcsTable {
    static T: OnceLock<DcsTable> = OnceLock::new();
    T.get_or_init(|| {
        let words: Vec<u32> = (0..2 * DCS.len()).map(|i| dcs_pattern(DCS[i / 2], i % 2 == 1)).collect();
        let mut lookup = HashMap::new();
        for (i, &w) in words.iter().enumerate() {
            lookup.insert(w, i as u16);
            for b in 0..23 {
                lookup.insert(w ^ 1 << b, i as u16);
            }
        }
        let class = words.iter().map(|&w| words.iter().position(|&v| rotations(w).any(|r| r == v)).unwrap() as u16).collect();
        DcsTable { lookup, class }
    })
}

/// The DCS codes that are the same signal as `code` (itself included).
pub fn dcs_aliases(code: u16, inverted: bool) -> Vec<Tone> {
    let t = dcs_table();
    let Some(i) = DCS.iter().position(|&c| c == code) else { return vec![Tone::Dcs(code, inverted)] };
    let k = t.class[2 * i + inverted as usize];
    (0..2 * DCS.len()).filter(|&j| t.class[j] == k).map(|j| Tone::Dcs(DCS[j / 2], j % 2 == 1)).collect()
}

#[derive(Clone, Copy, Default)]
struct Slicer {
    clock: f32,
    acc: f32,
    reg: u32,
    bits: u32,
    /// Index + 1 of the code read when `bits % 23` was last this.
    seen: [u16; 23],
}

pub struct ToneDetector {
    taps: Vec<f32>,
    ring: Vec<f32>,
    pos: usize,
    phase: usize,
    /// 1 kHz samples taken.
    n: u32,
    lo: Vec<Complex32>,
    lo_step: Vec<Complex32>,
    acc: Vec<Complex32>,
    /// The last WINDOW blocks' sums per tone (block-major).
    blocks: Vec<Complex32>,
    nblocks: usize,
    windows: u32,
    wins: Vec<u32>,
    /// Per tone, Σ block × conj(block before) while it was the winner.
    turn: Vec<Complex32>,
    power: Vec<f32>,
    dc: f32,
    slicers: [Slicer; NPHASE],
    /// Per code index: frames counted, and the sample of the last.
    frames: Vec<(u32, u32)>,
}

impl Default for ToneDetector {
    fn default() -> Self {
        let lo_step = CTCSS
            .iter()
            .map(|&t| {
                let w = -2.0 * std::f32::consts::PI * t as f32 / 10.0 / RATE;
                Complex32::new(w.cos(), w.sin())
            })
            .collect();
        let n = CTCSS.len();
        ToneDetector {
            taps: lowpass(DECIM_TAPS, 320.0 / AUDIO_RATE),
            ring: vec![0.0; 2 * DECIM_TAPS],
            pos: 0,
            phase: 0,
            n: 0,
            lo: vec![Complex32::new(1.0, 0.0); n],
            lo_step,
            acc: vec![Complex32::default(); n],
            blocks: vec![Complex32::default(); n * WINDOW],
            nblocks: 0,
            windows: 0,
            wins: vec![0; n],
            turn: vec![Complex32::default(); n],
            power: vec![0.0; n],
            dc: 0.0,
            slicers: std::array::from_fn(|k| Slicer { clock: k as f32 / NPHASE as f32, ..Default::default() }),
            frames: vec![(0, 0); 2 * DCS.len()],
        }
    }
}

impl ToneDetector {
    /// Take 8 kHz audio (before the voice high-pass).
    pub fn push(&mut self, audio: &[f32]) {
        for &x in audio {
            self.ring[self.pos] = x;
            self.ring[self.pos + DECIM_TAPS] = x;
            self.pos = (self.pos + 1) % DECIM_TAPS;
            self.phase += 1;
            if self.phase == DECIM {
                self.phase = 0;
                let y: f32 = self.taps.iter().zip(&self.ring[self.pos..self.pos + DECIM_TAPS]).map(|(t, v)| t * v).sum();
                self.sample(y);
            }
        }
    }

    fn sample(&mut self, y: f32) {
        self.n += 1;
        for k in 0..CTCSS.len() {
            self.acc[k] += self.lo[k] * y;
            self.lo[k] *= self.lo_step[k];
        }
        if (self.n as usize).is_multiple_of(BLOCK) {
            self.block();
        }
        // DCS: DC (the carrier's offset) out with a ~1.5 Hz high-pass.
        self.dc += (y - self.dc) * 0.0094;
        let v = y - self.dc;
        let step = DCS_BAUD / RATE;
        let table = dcs_table();
        for s in &mut self.slicers {
            s.clock += step;
            if s.clock < 1.0 {
                s.acc += v;
                continue;
            }
            s.clock -= 1.0;
            let over = s.clock / step;
            let b = s.acc + v * (1.0 - over) > 0.0;
            s.acc = v * over;
            s.reg = (s.reg << 1 | b as u32) & 0x7f_ffff;
            s.bits += 1;
            let at = (s.bits % 23) as usize;
            let hit = if s.bits >= 23 { table.lookup.get(&s.reg).copied() } else { None };
            let again = hit.is_some_and(|i| s.seen[at] == i + 1);
            s.seen[at] = hit.map_or(0, |i| i + 1);
            if let Some(i) = hit.filter(|_| again) {
                let f = &mut self.frames[i as usize];
                if f.0 == 0 || self.n - f.1 >= DCS_FRAME_GAP {
                    *f = (f.0 + 1, self.n);
                }
            }
        }
    }

    fn block(&mut self) {
        let n = CTCSS.len();
        let slot = self.nblocks % WINDOW;
        let prev = (self.nblocks + WINDOW - 1) % WINDOW;
        for k in 0..n {
            self.blocks[slot * n + k] = std::mem::take(&mut self.acc[k]);
            let a = self.lo[k].norm();
            self.lo[k] /= a;
        }
        self.nblocks += 1;
        if self.nblocks < WINDOW {
            return;
        }
        self.windows += 1;
        for k in 0..n {
            let s: Complex32 = (0..WINDOW).map(|b| self.blocks[b * n + k]).sum();
            self.power[k] = s.norm_sqr();
        }
        let best = (0..n).max_by(|&a, &b| self.power[a].total_cmp(&self.power[b])).unwrap();
        let mut sorted = self.power.clone();
        sorted.sort_by(f32::total_cmp);
        // One line, not one of many (DCS's words repeat every 171 ms: lines
        // every 5.84 Hz, some within 0.1 Hz of a standard tone).
        let f = |k: usize| CTCSS[k] as i32;
        let other = (0..n).filter(|&k| (f(k) - f(best)).abs() > 80).map(|k| self.power[k]).fold(0.0, f32::max);
        if self.power[best] > CTCSS_OVER_MEDIAN * sorted[n / 2] && self.power[best] > CTCSS_OVER_OTHERS * other {
            self.wins[best] += 1;
            self.turn[best] += self.blocks[slot * n + best] * self.blocks[prev * n + best].conj();
        }
    }

    /// The tone heard so far, if any held for enough of the call.
    pub fn heard(&self) -> Option<ToneHeard> {
        let table = dcs_table();
        // DCS: the class read on the most frames, by its preferred member.
        let mut by_class: HashMap<u16, u32> = HashMap::new();
        for (i, f) in self.frames.iter().enumerate() {
            let c = by_class.entry(table.class[i]).or_default();
            *c = (*c).max(f.0);
        }
        let dcs = by_class.into_iter().max_by_key(|&(k, c)| (c, std::cmp::Reverse(k))).filter(|&(_, c)| c >= MIN_COUNT).map(|(k, c)| {
            let first = (0..2 * DCS.len()).filter(|&j| table.class[j] == k).min_by_key(|&j| (j % 2, j)).unwrap();
            let expected = (self.n as f32 / (23.0 / DCS_BAUD * RATE)).max(1.0);
            ToneHeard { tone: Tone::Dcs(DCS[first / 2], first % 2 == 1), confidence: (c as f32 / expected).min(1.0) }
        });
        let ctcss = (0..CTCSS.len()).max_by_key(|&k| self.wins[k]).filter(|&k| self.wins[k] >= MIN_COUNT).and_then(|k| {
            let block_s = BLOCK as f32 / RATE;
            let hz = CTCSS[k] as f32 / 10.0 + self.turn[k].arg() / (2.0 * std::f32::consts::PI * block_s);
            let near = *CTCSS.iter().min_by(|&&a, &&b| (a as f32 / 10.0 - hz).abs().total_cmp(&(b as f32 / 10.0 - hz).abs())).unwrap();
            ((near as f32 / 10.0 - hz).abs() <= CTCSS_SNAP_HZ)
                .then(|| ToneHeard { tone: Tone::Ctcss(near), confidence: self.wins[k] as f32 / self.windows.max(1) as f32 })
        });
        [dcs, ctcss].into_iter().flatten().filter(|h| h.confidence >= MIN_CONFIDENCE).max_by(|a, b| a.confidence.total_cmp(&b.confidence))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::dsp::fm::Nbfm;
    use std::f64::consts::PI;

    /// The deviation (Hz) of `secs` of a DCS code: NRZ at 134.4 baud, ±`dev`.
    pub fn dcs_dev(code: u16, inverted: bool, dev: f64, secs: f64, rate: f64) -> Vec<f64> {
        let p = dcs_pattern(code, inverted);
        (0..(secs * rate) as usize)
            .map(|i| {
                let bit = (i as f64 * 134.4 / rate) as u32 % 23;
                if p >> (22 - bit) & 1 == 1 { dev } else { -dev }
            })
            .collect()
    }

    /// FM at 39 kHz carrying `dev` (Hz per channel sample) plus a 1 kHz-ish
    /// "voice" at 2.5 kHz, in noise; the 8 kHz audio before the high-pass.
    pub fn over_fm(dev: &[f64], noise_sd: f32, seed: u32) -> Vec<f32> {
        let rate = 39_062.5;
        let mut x = seed.wrapping_mul(2654435761) | 1;
        let mut u = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as f32 / u32::MAX as f32 - 0.5
        };
        let mut ph = 0.0f64;
        let iq: Vec<Complex32> = dev
            .iter()
            .enumerate()
            .map(|(i, &d)| {
                let t = i as f64 / rate;
                let voice = 2500.0 * (2.0 * PI * (800.0 + 300.0 * (3.0 * t).sin()) * t).sin();
                ph += 2.0 * PI * (d + voice) / rate;
                let n = Complex32::new((0..12).map(|_| u()).sum(), (0..12).map(|_| u()).sum());
                Complex32::from_polar(0.5, ph as f32) + n * noise_sd
            })
            .collect();
        let mut fm = Nbfm::new(rate);
        let (mut out, mut low) = (Vec::new(), Vec::new());
        fm.push_low(&iq, 1e-3, &mut out, Some(&mut low));
        low
    }

    fn ctcss_dev(tenths: u16, secs: f64, rate: f64) -> Vec<f64> {
        (0..(secs * rate) as usize).map(|i| 500.0 * (2.0 * PI * tenths as f64 / 10.0 * i as f64 / rate).sin()).collect()
    }

    fn heard(low: &[f32]) -> Option<ToneHeard> {
        let mut d = ToneDetector::default();
        d.push(low);
        d.heard()
    }

    #[test]
    fn every_ctcss_tone() {
        for &t in &CTCSS {
            let h = heard(&over_fm(&ctcss_dev(t, 1.0, 39_062.5), 0.02, t as u32));
            assert_eq!(h.map(|h| h.tone), Some(Tone::Ctcss(t)), "{t}");
            assert!(h.unwrap().confidence > 0.7, "{t}: {:?}", h);
        }
    }

    #[test]
    fn every_dcs_code_as_its_class() {
        for &c in &DCS {
            for inv in [false, true] {
                let h = heard(&over_fm(&dcs_dev(c, inv, 700.0, 1.0, 39_062.5), 0.02, c as u32));
                let tone = h.map(|h| h.tone);
                assert!(tone.is_some_and(|t| dcs_aliases(c, inv).contains(&t)), "D{c:03}{}: {h:?}", if inv { 'I' } else { 'N' });
            }
        }
    }

    #[test]
    fn aliases_and_names() {
        assert!(dcs_aliases(23, false).contains(&Tone::Dcs(47, true)));
        assert!(!dcs_aliases(23, false).contains(&Tone::Dcs(23, true)));
        assert_eq!(Tone::Dcs(23, false).to_string(), "D023N");
        assert_eq!(Tone::Ctcss(1514).to_string(), "151.4");
        // A class is named by its first normal-polarity member.
        let h = heard(&over_fm(&dcs_dev(47, true, 700.0, 1.0, 39_062.5), 0.02, 1));
        assert_eq!(h.map(|h| h.tone), Some(Tone::Dcs(23, false)));
    }

    #[test]
    fn parses_what_people_write() {
        let p = |s: &str| Tone::parse(s);
        for s in ["151.4", "151.4 PL", "PL 151.4", "151.4 Hz", "151.4Hz", "CTCSS 151.4", " 151.40 "] {
            assert_eq!(p(s), Ok(Some(Tone::Ctcss(1514))), "{s}");
        }
        assert_eq!(p("100"), Ok(Some(Tone::Ctcss(1000))));
        for s in ["D023N", "d023n", "023 DPL", "D023", "023", "D23N", "DPL 023", "D023 N", "023N", "DCS 23"] {
            assert_eq!(p(s), Ok(Some(Tone::Dcs(23, false))), "{s}");
        }
        assert_eq!(p("D023I"), Ok(Some(Tone::Dcs(23, true))));
        assert_eq!(p("D754R"), Ok(Some(Tone::Dcs(754, true))));
        for s in ["", "0", "S", "s", "CSQ"] {
            assert_eq!(p(s), Ok(None), "{s}");
        }
        assert_eq!(p("151.5"), Err("151.5 Hz isn't a standard CTCSS tone — 151.4?".into()));
        assert_eq!(p("D024N"), Err("D024 isn't a DCS code".into()));
        assert!(p("D089").is_err() && p("hello").is_err() && p("1000").is_err());
        assert!(Tone::Dcs(23, false).matches(Tone::Dcs(47, true)));
        assert!(!Tone::Dcs(23, false).matches(Tone::Dcs(23, true)));
        assert!(Tone::Ctcss(1514).matches(Tone::Ctcss(1514)) && !Tone::Ctcss(1514).matches(Tone::Ctcss(1500)));
    }

    #[test]
    fn nothing_without_a_tone_and_not_dcs_turn_off() {
        let rate = 39_062.5;
        assert_eq!(heard(&over_fm(&vec![0.0; (rate * 2.0) as usize], 0.05, 9)), None);
        // DCS's turn-off: 134.4 Hz, between 131.8 and 136.5.
        let off: Vec<f64> = (0..(rate * 1.0) as usize).map(|i| 500.0 * (2.0 * PI * 134.4 * i as f64 / rate).sin()).collect();
        assert_eq!(heard(&over_fm(&off, 0.02, 5)), None);
    }
}
