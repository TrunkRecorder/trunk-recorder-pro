//! DMR bursts (TS 102 361-1 §4–§9): the sync search, the framer that cuts
//! the symbol stream into 30 ms bursts ([`Framer`] → [`Burst`]), and the
//! burst's fixed fields.
//!
//! ```text
//! a 30 ms slot, 144 dibits (outbound from a repeater):
//!   CACH 12 │ payload 54 │ sync or EMB+embedded 24 │ payload 54
//! voice burst: AMBE 36 │ AMBE 18 ┊ … ┊ AMBE 18 │ AMBE 36
//! data burst:  info 49, slot type 5 │ sync 24 │ slot type 5, info 49
//! ```
//!
//! A repeater transmits the two slots alternately without a gap; the CACH
//! before each burst names its slot. Mobiles (and direct mode) send one
//! slot only, with 12 dibits of guard time where the CACH would be. Sync
//! comes on voice burst A and on every data burst; voice bursts B–F carry
//! the EMB instead, so the framer keeps the 144-dibit grid between syncs.

use std::collections::VecDeque;

use super::fec::{golay20_decode, hamming7_decode, qr16_decode};
use crate::dsp::Symbol;

pub const CACH_DIBITS: usize = 12;
pub const BURST_DIBITS: usize = 132;
/// CACH + burst: one slot, 30 ms at 4800 baud.
pub const SLOT_DIBITS: usize = CACH_DIBITS + BURST_DIBITS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKind {
    /// Repeater (base station) outbound.
    BsVoice,
    BsData,
    /// Mobile inbound (to a repeater).
    MsVoice,
    MsData,
    /// Mobile reverse channel.
    MsRc,
    /// Direct mode (simplex TDMA), with its slot 0 / 1.
    DirectVoice(u8),
    DirectData(u8),
}

impl SyncKind {
    pub fn is_voice(self) -> bool {
        matches!(self, SyncKind::BsVoice | SyncKind::MsVoice | SyncKind::DirectVoice(_))
    }
    pub fn is_data(self) -> bool {
        matches!(self, SyncKind::BsData | SyncKind::MsData | SyncKind::DirectData(_))
    }
    /// From a repeater: the CACH names the slot.
    pub fn is_bs(self) -> bool {
        matches!(self, SyncKind::BsVoice | SyncKind::BsData)
    }
    /// Repeater (0), mobile (1) or direct mode (2): a channel's syncs are all one family.
    pub fn family(self) -> u8 {
        match self {
            SyncKind::BsVoice | SyncKind::BsData => 0,
            SyncKind::MsVoice | SyncKind::MsData | SyncKind::MsRc => 1,
            _ => 2,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            SyncKind::BsVoice => "bs_voice",
            SyncKind::BsData => "bs_data",
            SyncKind::MsVoice => "ms_voice",
            SyncKind::MsData => "ms_data",
            SyncKind::MsRc => "ms_rc",
            SyncKind::DirectVoice(0) => "dm1_voice",
            SyncKind::DirectVoice(_) => "dm2_voice",
            SyncKind::DirectData(0) => "dm1_data",
            SyncKind::DirectData(_) => "dm2_data",
        }
    }
}

/// The 48-bit sync patterns (TS 102 361-1 §9.1.1), dibits MSB first.
pub const SYNCS: [(u64, SyncKind); 9] = [
    (0x755f_d7df_75f7, SyncKind::BsVoice),
    (0xdff5_7d75_df5d, SyncKind::BsData),
    (0x7f7d_5dd5_7dfd, SyncKind::MsVoice),
    (0xd5d7_f77f_d757, SyncKind::MsData),
    (0x77d5_5f7d_fd77, SyncKind::MsRc),
    (0x5d57_7f77_57ff, SyncKind::DirectVoice(0)),
    (0xf7fd_d5dd_fd55, SyncKind::DirectData(0)),
    (0x7dff_d5f5_5d5f, SyncKind::DirectVoice(1)),
    (0xd755_7f5f_f7f5, SyncKind::DirectData(1)),
];

/// The closest sync pattern to a 48-bit window (of `family` only, when
/// given) → (kind, bits different). Patterns of one family are 48 bits
/// apart; of different families, as few as 10.
pub fn match_sync(win: u64, family: Option<u8>) -> (SyncKind, u32) {
    SYNCS
        .iter()
        .filter(|s| family.is_none_or(|f| s.1.family() == f))
        .map(|&(p, k)| (k, (p ^ win).count_ones()))
        .min_by_key(|&(_, e)| e)
        .unwrap()
}

/// Bits a sync may differ by to lock the framer (anywhere), and to count as
/// the sync where the grid expects one.
const LOCK_ERRS: u32 = 3;
const GRID_ERRS: u32 = 7;
/// A repeater's voice and data syncs are 24 bits apart: on the grid, up to 11
/// errors still say which (worth ~0.3 dB near threshold, `tool snr`).
const GRID_ERRS_BS: u32 = 11;
/// Bursts with no sync before the framer lets go of the grid (a slot's voice
/// superframe has one per 12 bursts).
const UNLOCK_BURSTS: u32 = 24;

#[derive(Clone, Debug)]
pub struct Burst {
    /// CACH (or, from a mobile, guard time) dibits.
    pub cach: [u8; CACH_DIBITS],
    pub dibits: [u8; BURST_DIBITS],
    /// Each burst bit's reliability (≥ 0), air order (2 per dibit); [`Burst::soft`]
    /// gives a bit and its reliability as one signed value.
    pub rel: [f32; 2 * BURST_DIBITS],
    pub sync: Option<SyncKind>,
    pub sync_errs: u32,
    /// Channel-sample instant of the burst's first dibit.
    pub sample: f64,
}

impl Burst {
    /// Bit `i` of the burst (air order).
    pub fn bit(&self, i: usize) -> u8 {
        self.dibits[i / 2] >> (1 - i % 2) & 1
    }

    fn bits_word(&self, ranges: &[(usize, usize)]) -> (u32, Vec<f32>) {
        let (mut w, mut rel) = (0u32, Vec::new());
        for &(a, b) in ranges {
            for i in a..b {
                w = w << 1 | self.bit(i) as u32;
                rel.push(self.rel[i]);
            }
        }
        (w, rel)
    }

    /// Data burst slot type → (colour code, data type, bits corrected).
    pub fn slot_type(&self) -> (u8, u8, u32) {
        let (w, rel) = self.bits_word(&[(98, 108), (156, 166)]);
        let (d, e) = golay20_decode(w, Some(&rel));
        ((d >> 4) as u8, (d & 15) as u8, e)
    }

    /// Voice burst B–F's EMB → (colour code, PI, LCSS, bits corrected).
    pub fn emb(&self) -> (u8, bool, u8, u32) {
        let (w, rel) = self.bits_word(&[(108, 116), (148, 156)]);
        let (d, e) = qr16_decode(w, Some(&rel));
        ((d >> 3) as u8, d >> 2 & 1 != 0, (d & 3) as u8, e)
    }

    /// The 32 embedded signalling bits of voice bursts B–F.
    pub fn embedded(&self) -> [u8; 32] {
        std::array::from_fn(|i| self.bit(116 + i))
    }

    /// The 196 BPTC bits of a data burst (either side of slot type and sync).
    pub fn info196(&self) -> [u8; 196] {
        std::array::from_fn(|i| self.bit(if i < 98 { i } else { i - 98 + 166 }))
    }

    /// Bit `i` as a soft value: its reliability, + for a 1, − for a 0.
    pub fn soft(&self, i: usize) -> f32 {
        if self.bit(i) != 0 { self.rel[i].max(1e-3) } else { -self.rel[i].max(1e-3) }
    }
    pub fn embedded_soft(&self) -> [f32; 32] {
        std::array::from_fn(|i| self.soft(116 + i))
    }
    pub fn info196_soft(&self) -> [f32; 196] {
        std::array::from_fn(|i| self.soft(if i < 98 { i } else { i - 98 + 166 }))
    }

    /// Voice frame `k` (0..3): its 36 dibits and 72 bit reliabilities.
    pub fn voice_frame(&self, k: usize) -> ([u8; 36], [f32; 72]) {
        let at = |j: usize| match k {
            0 => j,
            1 if j < 18 => 36 + j,
            1 => 78 + j - 18,
            _ => 96 + j,
        };
        let d = std::array::from_fn(|j| self.dibits[at(j)]);
        let r = std::array::from_fn(|b| self.rel[2 * at(b / 2) + b % 2]);
        (d, r)
    }
}

/// The CACH's TACT: which slot the burst is, and the outbound channel's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tact {
    /// Access type: the inbound channel of this slot is busy.
    pub busy: bool,
    pub slot: u8,
    /// Short LC fragment: 0 single, 1 first, 2 last, 3 continuation.
    pub lcss: u8,
}

/// TS 102 361-1 §B.3.9: the CACH's 24 bits in the order TACT (7), payload (17).
const CACH_ORDER: [usize; 24] = [0, 4, 8, 12, 14, 18, 22, 1, 2, 3, 5, 6, 7, 9, 10, 11, 13, 15, 16, 17, 19, 20, 21, 23];

/// The TACT (None when its Hamming code fails) and the 17 short-LC payload bits.
pub fn cach(c: &[u8; CACH_DIBITS]) -> (Option<Tact>, [u8; 17]) {
    let raw = |i: usize| c[i / 2] >> (1 - i % 2) & 1;
    let tact = CACH_ORDER[..7].iter().fold(0u32, |w, &i| w << 1 | raw(i) as u32);
    let (d, e) = hamming7_decode(tact);
    let t = (e <= 1).then_some(Tact { busy: d & 8 != 0, slot: (d >> 2 & 1) as u8, lcss: (d & 3) as u8 });
    (t, std::array::from_fn(|i| raw(CACH_ORDER[7 + i])))
}

/// Cuts decided symbols into bursts, holding the 30 ms grid between syncs.
pub struct Framer {
    /// Bits a repeater's sync may differ by where the grid expects one ([`GRID_ERRS_BS`]).
    pub grid_errs: u32,
    buf: VecDeque<Symbol>,
    win: u64,
    n: u64,
    /// Symbol count at which the next burst's last dibit arrives (locked).
    next_end: Option<u64>,
    pending: Option<(SyncKind, u32)>,
    /// Family of the sync that set the grid.
    family: u8,
    since_sync: u32,
    pub bursts: u64,
    pub syncs: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Framer { grid_errs: GRID_ERRS_BS, buf: VecDeque::new(), win: 0, n: 0, next_end: None, pending: None, family: 0, since_sync: 0, bursts: 0, syncs: 0 }
    }

    pub fn locked(&self) -> bool {
        self.next_end.is_some()
    }

    pub fn push(&mut self, s: &Symbol, out: &mut Vec<Burst>) {
        self.buf.push_back(*s);
        if self.buf.len() > SLOT_DIBITS {
            self.buf.pop_front();
        }
        self.win = (self.win << 2 | (s.dibit & 3) as u64) & 0xffff_ffff_ffff;
        let i = self.n;
        self.n += 1;
        // The sync's last dibit is burst dibit 77; the burst ends 54 later.
        match self.next_end {
            Some(end) if end - 54 == i => {
                let (kind, errs) = match_sync(self.win, Some(self.family));
                // Direct-mode syncs of different slots are only 10 apart.
                let lim = if self.family == 0 { self.grid_errs } else { GRID_ERRS.min(self.grid_errs) };
                self.pending = (errs <= lim).then_some((kind, errs));
            }
            _ => {
                let (kind, errs) = match_sync(self.win, None);
                if errs <= LOCK_ERRS && i >= 77 {
                    // A clean sync off the grid (or with no grid): the grid is here.
                    self.next_end = Some(i + 54);
                    self.pending = Some((kind, errs));
                    self.family = kind.family();
                }
            }
        }
        if self.next_end == Some(i) {
            self.next_end = Some(i + SLOT_DIBITS as u64);
            let sync = self.pending.take();
            self.since_sync = if sync.is_some() { 0 } else { self.since_sync + 1 };
            if sync.is_some() {
                self.syncs += 1;
            }
            if self.since_sync > UNLOCK_BURSTS {
                self.next_end = None;
                return;
            }
            if self.buf.len() < SLOT_DIBITS {
                return;
            }
            self.bursts += 1;
            let mut b = Burst {
                cach: [0; CACH_DIBITS],
                dibits: [0; BURST_DIBITS],
                rel: [0.0; 2 * BURST_DIBITS],
                sync: sync.map(|s| s.0),
                sync_errs: sync.map_or(0, |s| s.1),
                sample: self.buf[CACH_DIBITS].sample,
            };
            for (j, s) in self.buf.iter().enumerate() {
                if j < CACH_DIBITS {
                    b.cach[j] = s.dibit;
                } else {
                    let k = j - CACH_DIBITS;
                    b.dibits[k] = s.dibit;
                    b.rel[2 * k] = s.rel_hi;
                    b.rel[2 * k + 1] = s.rel_lo;
                }
            }
            out.push(b);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::dmr::fec::hamming7_encode;

    /// Dibits of a 48-bit sync pattern.
    pub fn sync_dibits(k: SyncKind) -> [u8; 24] {
        let p = SYNCS.iter().find(|s| s.1 == k).unwrap().0;
        std::array::from_fn(|i| (p >> (46 - 2 * i) & 3) as u8)
    }

    /// A CACH naming `slot` (empty payload).
    pub fn cach_dibits(slot: u8) -> [u8; 12] {
        let t = hamming7_encode(slot as u32 * 4);
        let mut raw = [0u8; 24];
        for (j, &i) in CACH_ORDER[..7].iter().enumerate() {
            raw[i] = (t >> (6 - j) & 1) as u8;
        }
        std::array::from_fn(|i| raw[2 * i] << 1 | raw[2 * i + 1])
    }

    #[test]
    fn syncs_are_far_apart() {
        for (i, a) in SYNCS.iter().enumerate() {
            for b in &SYNCS[i + 1..] {
                // A repeater's voice and data syncs, the ones that matter most on the grid, are 24 apart.
                let d = (a.0 ^ b.0).count_ones();
                assert!(d >= 10 && (a.1.family() != 0 || b.1.family() != 0 || d >= 24), "{:?} {:?}", a.1, b.1);
            }
        }
    }

    #[test]
    fn cach_names_the_slot() {
        for slot in 0..2 {
            let mut c = cach_dibits(slot);
            assert_eq!(cach(&c).0, Some(Tact { busy: false, slot, lcss: 0 }));
            c[0] ^= 1; // one bit wrong: still decodes
            assert_eq!(cach(&c).0.map(|t| t.slot), Some(slot));
        }
    }

    #[test]
    fn framer_keeps_the_grid_between_syncs() {
        // Burst 0 has a data sync; bursts 1–6 have none; burst 7 has voice sync.
        let mut syms = Vec::new();
        let mut x = 0x1234_5678u32;
        for b in 0..8 {
            for d in cach_dibits(b as u8 & 1) {
                syms.push(d);
            }
            let mut burst = [0u8; BURST_DIBITS];
            for d in burst.iter_mut() {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                *d = (x >> 30) as u8;
            }
            let s = match b {
                0 => Some(SyncKind::BsData),
                7 => Some(SyncKind::BsVoice),
                _ => None,
            };
            if let Some(k) = s {
                burst[54..78].copy_from_slice(&sync_dibits(k));
            }
            syms.extend(burst);
        }
        let mut f = Framer::new();
        let mut out = Vec::new();
        for (i, &d) in syms.iter().enumerate() {
            f.push(&Symbol { dibit: d, sample: i as f64, rel_hi: 1.0, rel_lo: 1.0 }, &mut out);
        }
        assert_eq!(out.len(), 8);
        assert_eq!(out[0].sync, Some(SyncKind::BsData));
        assert!(out[1..7].iter().all(|b| b.sync.is_none()));
        assert_eq!(out[7].sync, Some(SyncKind::BsVoice));
        for (k, b) in out.iter().enumerate() {
            assert_eq!(b.sample, (k * SLOT_DIBITS + CACH_DIBITS) as f64);
            assert_eq!(cach(&b.cach).0.unwrap().slot, k as u8 & 1);
        }
    }
}
