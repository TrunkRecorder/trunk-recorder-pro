//! Receiver diversity: several receivers on one channel (CQPSK, CQPSK with the
//! T/2 CMA equaliser, C4FM), each with its own framer. Frames that start at the
//! same channel-sample instant (within half a TDU) form a group; a group is
//! released, oldest first, once every receiver's output has passed it, and the
//! consumer takes the best of it — per TSBK the CRC-valid copy, per IMBE
//! codeword the one the soft decoder trusted most. No receiver has to be right
//! about the whole channel, only one of them about each frame.

use std::collections::VecDeque;

use num_complex::Complex32;

use super::frame::{Frame, Framer, FramerOptions, LDU1, LDU2, TSDU};
use super::tsbk::{tsbk_last, tsbk_ok, trellis_viterbi, Tsbk};
use super::voice::{decode_ldu1_lc, decode_ldu2_es, ldu_imbe, EncryptionSync, ImbeParams, LinkControl};
use crate::dsp::c4fm::{C4fm, C4fmOptions};
use crate::dsp::cqpsk::{self, Cqpsk};
use crate::dsp::{Receiver, Symbol};

/// One frame slot as seen by every receiver that found it.
pub type Group = Vec<Frame>;

#[derive(Clone, Copy, Debug)]
pub struct BankConfig {
    pub cqpsk: bool,
    pub cqpsk_eq: bool,
    pub c4fm: bool,
    pub eq_taps: usize,
    pub eq_mu: f32,
    pub framer: FramerOptions,
    /// Pass bit reliabilities to the framer (soft Viterbi / soft FEC). Off:
    /// hard decisions only, for comparisons.
    pub soft: bool,
}

impl Default for BankConfig {
    fn default() -> Self {
        BankConfig { cqpsk: true, cqpsk_eq: true, c4fm: true, eq_taps: 9, eq_mu: 0.02, framer: FramerOptions::default(), soft: true }
    }
}

/// CQPSK receivers are kept as such so they can share a matched filter.
enum Demod {
    Cqpsk(Box<Cqpsk>),
    Other(Box<dyn Receiver + Send>),
}

struct Rx {
    demod: Demod,
    framer: Framer,
    /// Sample instant of the latest symbol.
    progress: f64,
    frames: u64,
}

struct Pending {
    sample: f64,
    frames: Group,
}

pub struct Bank {
    rx: Vec<Rx>,
    pending: VecDeque<Pending>,
    guard: f64,
    hold: f64,
    soft: bool,
    last_released: Option<f64>,
    syms: Vec<Symbol>,
    new_frames: Vec<Frame>,
    /// The lead CQPSK receiver's matched filter output for this push.
    filtered: [Vec<f32>; 2],
    pub groups: u64,
}

impl Bank {
    pub fn new(rate: f64, cfg: BankConfig) -> Self {
        Self::with_c4fm(rate, cfg, C4fmOptions::default())
    }

    /// With the C4FM receiver's variant options (weak-signal comparisons).
    pub fn with_c4fm(rate: f64, cfg: BankConfig, c4fm: C4fmOptions) -> Self {
        let mut rx = Vec::new();
        let mut add = |d: Demod| rx.push(Rx { demod: d, framer: Framer::new(cfg.framer), progress: 0.0, frames: 0 });
        if cfg.cqpsk {
            add(Demod::Cqpsk(Box::new(Cqpsk::new(rate, cqpsk::Options::default()))));
        }
        if cfg.cqpsk_eq {
            add(Demod::Cqpsk(Box::new(Cqpsk::new(rate, cqpsk::Options { eq_taps: cfg.eq_taps, eq_mu: cfg.eq_mu, ..Default::default() }))));
        }
        if cfg.c4fm {
            add(Demod::Other(Box::new(C4fm::with_options(rate, c4fm))));
        }
        Bank {
            rx,
            pending: VecDeque::new(),
            guard: 36.0 * rate / 4800.0,
            // Every receiver must be this far past a group's start: the longest
            // frame (LDU, 0.18 s) + the C4FM receiver's latency (~0.1 s) + slack.
            hold: 0.35 * rate,
            soft: cfg.soft,
            last_released: None,
            syms: Vec::new(),
            new_frames: Vec::new(),
            filtered: Default::default(),
            groups: 0,
        }
    }

    /// Feed channel IQ; groups that are complete are appended to `out`, in time order.
    pub fn push(&mut self, iq: &[Complex32], out: &mut Vec<Group>) {
        // The first CQPSK receiver's matched filter output serves the others
        // that filter the same way (it is most of their work).
        let mut lead: Option<usize> = None;
        for i in 0..self.rx.len() {
            let (before, rest) = self.rx.split_at_mut(i);
            let r = &mut rest[0];
            self.syms.clear();
            match &mut r.demod {
                Demod::Cqpsk(c) => match lead.map(|j| &before[j].demod) {
                    Some(Demod::Cqpsk(l)) if l.same_front(c) => c.push_filtered(&self.filtered[0], &self.filtered[1], &mut self.syms),
                    Some(_) => c.push(iq, &mut self.syms),
                    None => {
                        c.push_sharing(iq, &mut self.filtered, &mut self.syms);
                        lead = Some(i);
                    }
                },
                Demod::Other(d) => d.push(iq, &mut self.syms),
            }
            if let Some(s) = self.syms.last() {
                r.progress = s.sample;
            }
            for s in self.syms.iter_mut() {
                if !self.soft {
                    s.rel_hi = -1.0;
                    s.rel_lo = -1.0;
                }
                r.framer.push(s, &mut self.new_frames);
            }
            r.frames += self.new_frames.len() as u64;
            for f in self.new_frames.drain(..) {
                let sample = f.sample;
                if let Some(g) = self.pending.iter_mut().find(|g| (g.sample - sample).abs() < self.guard) {
                    g.frames.push(f);
                } else {
                    let at = self.pending.iter().position(|g| g.sample > sample).unwrap_or(self.pending.len());
                    self.pending.insert(at, Pending { sample, frames: vec![f] });
                }
            }
        }
        self.release(false, out);
    }

    /// Release everything still held (end of input).
    pub fn flush(&mut self, out: &mut Vec<Group>) {
        self.release(true, out);
    }

    /// How far above the channel the carrier is, Hz, from whichever receiver measures it.
    pub fn offset_hz(&self) -> Option<f32> {
        self.rx.iter().find_map(|r| match &r.demod {
            Demod::Cqpsk(c) => c.offset_hz(),
            Demod::Other(d) => d.offset_hz(),
        })
    }

    /// Frames found per receiver (CQPSK, CQPSK + EQ, C4FM, as configured).
    pub fn frames_per_rx(&self) -> Vec<u64> {
        self.rx.iter().map(|r| r.frames).collect()
    }

    fn release(&mut self, all: bool, out: &mut Vec<Group>) {
        let min_progress = self.rx.iter().map(|r| r.progress).fold(f64::INFINITY, f64::min);
        while let Some(front) = self.pending.front() {
            if !all && front.sample + self.hold >= min_progress {
                break;
            }
            let g = self.pending.pop_front().unwrap();
            // Never emit out of order (a frame older than one already released).
            if self.last_released.is_some_and(|t| g.sample < t + self.guard) {
                continue;
            }
            self.last_released = Some(g.sample);
            self.groups += 1;
            out.push(g.frames);
        }
    }
}

// ── Choosing from a group ────────────────────────────────────────────────────

/// The frame to take header fields from: the one whose NID needed the fewest corrections.
pub fn best_frame(g: &Group) -> &Frame {
    g.iter().min_by_key(|f| f.nid.errors).expect("groups are never empty")
}

/// Every CRC-valid TSBK of the group's TSDUs, one per block position, up to
/// the last-block flag; `missing` counts blocks no receiver decoded.
pub fn best_tsbks(g: &Group) -> (Vec<Tsbk>, usize) {
    let mut slot: [Option<Tsbk>; 3] = [None; 3];
    let mut max_blocks = 0;
    for f in g.iter().filter(|f| f.nid.duid == TSDU && f.bits.len() >= 112) {
        let avail = ((f.bits.len() - 112) / 196).min(3);
        max_blocks = max_blocks.max(avail);
        for (b, s) in slot.iter_mut().enumerate().take(avail) {
            if s.is_some() {
                continue;
            }
            let at = 112 + b * 196;
            let t = trellis_viterbi(&f.bits[at..at + 196], (!f.soft.is_empty()).then(|| &f.soft[at..at + 196]));
            if tsbk_ok(&t) {
                *s = Some(t);
            }
        }
    }
    let last = slot.iter().position(|s| s.is_some_and(|t| tsbk_last(&t))).map_or(3, |i| i + 1);
    let mut out = Vec::new();
    let mut missing = 0;
    for (b, s) in slot.iter().enumerate().take(last) {
        match s {
            Some(t) => out.push(*t),
            None if b < max_blocks => missing += 1,
            None => {}
        }
    }
    (out, missing)
}

/// Per codeword, the candidate the soft decoder overrode least (cost per
/// unit of reliability), among the group's LDUs.
pub fn best_imbe(g: &Group) -> [ImbeParams; 9] {
    let mut best = [ImbeParams::default(); 9];
    let mut score = [f32::INFINITY; 9];
    for f in g.iter().filter(|f| f.nid.duid == LDU1 || f.nid.duid == LDU2) {
        for (k, p) in ldu_imbe(f, true).into_iter().enumerate() {
            let s = if p.mean_rel > 0.0 { p.cost / p.mean_rel } else { p.errs as f32 };
            if s < score[k] {
                score[k] = s;
                best[k] = p;
            }
        }
    }
    best
}

pub fn best_lc(g: &Group) -> Option<LinkControl> {
    g.iter().filter(|f| f.nid.duid == LDU1).find_map(|f| decode_ldu1_lc(&f.raw))
}
pub fn best_es(g: &Group) -> Option<EncryptionSync> {
    g.iter().filter(|f| f.nid.duid == LDU2).find_map(|f| decode_ldu2_es(&f.raw))
}
