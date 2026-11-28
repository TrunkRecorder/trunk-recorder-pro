//! TETRA downlink bursts (EN 300 392-2 clause 9.4.4): the training sequences, and the framer
//! that cuts the symbol stream into 255-symbol slots ([`Framer`] → [`Burst`]).
//!
//! ```text
//! normal (training 1: one channel; 2: two):
//!   tail 6 │ phase 1 │ block 1 108 │ AACH 7 │ training 11 │ AACH 8 │ block 2 108 │ phase 1 │ tail 5
//! synchronisation:
//!   tail 6 │ phase 1 │ frequency correction 40 │ BSCH 60 │ training 19 │ AACH 15 │ block 2 108 │ phase 1 │ tail 5
//! ```

use std::collections::VecDeque;

use crate::dsp::Symbol;

/// Symbols in a slot: 14.17 ms, four to a frame.
pub const SLOT_SYMBOLS: usize = 255;

/// Training sequences 1 and 2 and the synchronisation one, as dibits (high bit first).
pub const NTS1: [u8; 11] = [3, 1, 0, 0, 3, 2, 2, 1, 3, 1, 0];
pub const NTS2: [u8; 11] = [1, 3, 2, 2, 1, 0, 0, 3, 1, 3, 2];
pub const STS: [u8; 19] = [3, 0, 0, 1, 2, 1, 3, 0, 3, 2, 2, 1, 3, 0, 0, 1, 2, 1, 3];
/// Where each starts in its slot.
pub const NTS_AT: usize = 122;
pub const STS_AT: usize = 107;

/// Bits a training sequence may differ by where the grid expects it.
const NTS_ERRS: u32 = 3;
const STS_ERRS: u32 = 7;
/// Slots with no training sequence before the framer lets go.
const UNLOCK_SLOTS: u32 = 12;

/// What a burst carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// BSCH, then a half slot.
    Sync,
    /// Both blocks one channel: SCH/F, or speech.
    Full,
    /// Two half-slot channels.
    Halves,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Sync, Kind::Full, Kind::Halves];
    fn training(self) -> (&'static [u8], usize, u32) {
        match self {
            Kind::Sync => (&STS, STS_AT, STS_ERRS),
            Kind::Full => (&NTS1, NTS_AT, NTS_ERRS),
            Kind::Halves => (&NTS2, NTS_AT, NTS_ERRS),
        }
    }
}

/// A dibit with its phase step `j` quarter turns further on.
pub fn turned(d: u8, j: u8) -> u8 {
    // Steps in quarter turns from +45°: 00, 01, 11, 10 (its own inverse).
    const STEP: [u8; 4] = [0, 1, 3, 2];
    STEP[((STEP[d as usize & 3] + j) & 3) as usize]
}

/// One slot, with the receiver's carrier ambiguity undone.
#[derive(Clone, Debug)]
pub struct Burst {
    pub kind: Kind,
    /// Symbol count, and channel-sample instant, of its first symbol.
    pub start: u64,
    pub sample: f64,
    /// Bits its training sequence differed by.
    pub errs: u32,
    /// Quarter turns a symbol the receiver's steps stood on.
    pub turn: u8,
    /// Its 510 bits: + a 1, − a 0.
    soft: Vec<f32>,
}

impl Burst {
    fn symbols(&self, at: usize, n: usize) -> &[f32] {
        &self.soft[2 * at..2 * (at + n)]
    }
    /// The BSCH's 120 bits ([`Kind::Sync`]).
    pub fn bsch(&self) -> &[f32] {
        self.symbols(47, 60)
    }
    /// The first half slot's 216 bits (not [`Kind::Sync`]).
    pub fn block1(&self) -> &[f32] {
        self.symbols(7, 108)
    }
    /// The second half slot's 216 bits.
    pub fn block2(&self) -> &[f32] {
        self.symbols(141, 108)
    }
    /// The whole slot's 432 bits ([`Kind::Full`]).
    pub fn full(&self) -> Vec<f32> {
        [self.block1(), self.block2()].concat()
    }
    pub fn aach(&self) -> [f32; 30] {
        let bits = if self.kind == Kind::Sync { self.symbols(126, 15).to_vec() } else { [self.symbols(115, 7), self.symbols(133, 8)].concat() };
        bits.try_into().unwrap()
    }
}

/// Cuts decided symbols into slots, holding the 255-symbol grid.
#[derive(Default)]
pub struct Framer {
    buf: VecDeque<Symbol>,
    /// Symbol count of `buf`'s first.
    base: u64,
    /// The next slot's first symbol (locked).
    next: Option<u64>,
    missed: u32,
    /// The last burst's turn: what a training sequence that fits two ways is read with.
    turn: u8,
    /// Slots with a training sequence, and slots of the grid without one.
    pub good: u64,
    pub bad: u64,
    pub locks: u64,
}

impl Framer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn locked(&self) -> bool {
        self.next.is_some()
    }

    /// Bits `word` differs by from the symbols at `at`, taken `turn` quarters back.
    fn errs(&self, word: &[u8], at: u64, turn: u8) -> u32 {
        let i = (at - self.base) as usize;
        word.iter().enumerate().map(|(k, &w)| (self.buf[i + k].dibit ^ turned(w, turn)).count_ones()).sum()
    }

    /// The training sequence of the slot starting at `start`: the kind, turn and bits it differs by.
    fn training(&self, start: u64) -> Option<(Kind, u8, u32)> {
        let mut best: Option<(Kind, u8, u32, u32)> = None;
        for kind in Kind::ALL {
            let (word, at, max) = kind.training();
            for k in 0..4 {
                let turn = (self.turn + k) & 3;
                let e = self.errs(word, start + at as u64, turn);
                // Compared as parts of the sequence's length.
                let part = e * 1000 / word.len() as u32;
                if e <= max && best.is_none_or(|b| part < b.3) {
                    best = Some((kind, turn, e, part));
                }
            }
        }
        best.map(|(k, t, e, _)| (k, t, e))
    }

    fn burst(&self, start: u64, (kind, turn, errs): (Kind, u8, u32)) -> Burst {
        let i = (start - self.base) as usize;
        let mut soft = Vec::with_capacity(2 * SLOT_SYMBOLS);
        for s in self.buf.range(i..i + SLOT_SYMBOLS) {
            let d = turned(s.dibit, 4 - turn);
            // An odd number of quarters swaps the axes the two bits were decided on.
            let (hi, lo) = if turn & 1 != 0 { (s.rel_lo, s.rel_hi) } else { (s.rel_hi, s.rel_lo) };
            soft.push(if d & 2 != 0 { hi } else { -hi });
            soft.push(if d & 1 != 0 { lo } else { -lo });
        }
        Burst { kind, start, sample: self.buf[i].sample, errs, turn, soft }
    }

    /// The slot a training sequence that just ended says is under way. The synchronisation one is
    /// believed alone; a normal one, 22 bits, with another in the same place a slot before.
    fn acquire(&self) -> Option<u64> {
        let end = self.base + self.buf.len() as u64;
        let begin = |len: usize, at: usize| end.checked_sub((len + at) as u64);
        for turn in 0..4 {
            if let Some(s) = begin(STS.len(), STS_AT)
                && s >= self.base
                && self.errs(&STS, s + STS_AT as u64, turn) <= 2
            {
                return Some(s);
            }
            for word in [&NTS1, &NTS2] {
                let Some(s) = begin(word.len(), NTS_AT) else { continue };
                let Some(before) = s.checked_sub(SLOT_SYMBOLS as u64) else { continue };
                if before >= self.base && self.errs(word, s + NTS_AT as u64, turn) == 0 && [&NTS1, &NTS2].iter().any(|w| self.errs(&w[..], before + NTS_AT as u64, turn) <= 1) {
                    return Some(before);
                }
            }
        }
        None
    }

    pub fn push(&mut self, s: &Symbol, out: &mut Vec<Burst>) {
        self.buf.push_back(*s);
        let end = self.base + self.buf.len() as u64;
        if self.next.is_none() {
            self.next = self.acquire();
            match self.next {
                Some(_) => {
                    self.locks += 1;
                    self.missed = 0;
                }
                None => self.trim(end.saturating_sub(2 * SLOT_SYMBOLS as u64)),
            }
        }
        // A slot is cut once it and a symbol more are in.
        while let Some(next) = self.next {
            if end < next + SLOT_SYMBOLS as u64 + 1 {
                break;
            }
            // The timing may have slipped a symbol.
            let found = [next, next.saturating_sub(1), next + 1].into_iter().filter(|&at| at >= self.base).find_map(|at| self.training(at).map(|t| (at, t)));
            let at = found.map_or(next, |f| f.0);
            match found {
                Some((at, t)) => {
                    self.good += 1;
                    self.missed = 0;
                    self.turn = t.1;
                    out.push(self.burst(at, t));
                }
                None => {
                    self.bad += 1;
                    self.missed += 1;
                }
            }
            self.next = (self.missed < UNLOCK_SLOTS).then_some(at + SLOT_SYMBOLS as u64);
            self.trim((at + SLOT_SYMBOLS as u64).saturating_sub(1));
        }
    }

    /// Let go of the symbols before `to`.
    fn trim(&mut self, to: u64) {
        while self.base < to && !self.buf.is_empty() {
            self.buf.pop_front();
            self.base += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turning_is_undone() {
        for d in 0..4 {
            for j in 0..4 {
                assert_eq!(turned(turned(d, j), 4 - j), d);
            }
        }
        assert_eq!([0, 1, 3, 2].map(|d| turned(d, 1)), [1, 3, 2, 0]);
    }

    /// Training sequence 1 turned isn't 2 (which is it mirrored), nor either another turn of itself.
    #[test]
    fn normal_training_sequences_stand_apart_at_every_turn() {
        for a in [NTS1, NTS2] {
            for b in [NTS1, NTS2] {
                for j in 0..4 {
                    if a == b && j == 0 {
                        continue;
                    }
                    let e: u32 = a.iter().zip(b).map(|(&x, y)| (x ^ turned(y, j)).count_ones()).sum();
                    assert!(e > 2 * NTS_ERRS, "{a:?} {b:?} {j}: {e}");
                }
            }
        }
    }
}
