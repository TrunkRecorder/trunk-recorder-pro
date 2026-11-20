//! Receiver diversity: several receivers on one channel (CQPSK, CQPSK with the
//! T/2 CMA equaliser, C4FM), each with its own framer. Frames (Phase 1) or slot
//! packets (Phase 2, [`Framing`]) that start at the same channel-sample instant
//! form a group; a group is released, oldest first, once every receiver's
//! output has passed it, and the consumer takes the best of it — per TSBK the
//! CRC-valid copy, per IMBE or AMBE codeword the one that decoded best. No
//! receiver has to be right about the whole channel, only one of them about
//! each frame.

use std::borrow::Cow;

use std::collections::VecDeque;

use num_complex::Complex32;

use super::frame::{blocks_dibits, frame_dibits, Frame, Framer, FramerOptions, HDU, LDU1, LDU2, PDU, TDU, TSDU};
use super::phase2::{self, Packet, BURST_DIBITS, VCW_AT_2V, VCW_AT_4V, VCW_DIBITS};
use crate::ambe::decode_vcw;
use super::mbt::{self, Pdu};
use super::osp::{self, Osp};
use super::tsbk::{trellis_list, tsbk_last, tsbk_ok, trellis_viterbi, Tsbk};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};
use super::lc::LinkControl;
use super::voice::{decode_hdu, decode_ldu1_lc, decode_ldu2_es, ldu_imbe, EncryptionSync, HduInfo, ImbeParams};
use crate::dsp::c4fm::{C4fm, C4fmOptions, SYMBOL_RATE};
use crate::dsp::cqpsk::{self, Cqpsk};
use crate::dsp::{Receiver, Symbol};
use crate::metrics::{Instrumented, Sink};

/// One frame slot as seen by every receiver that found it, in the order of
/// the bank's receivers (the plain CQPSK receiver's first).
pub type Group<U = Frame> = Vec<U>;

/// What a bank's receivers frame: Phase 1's frames ([`Framer`]) or Phase 2's
/// slot packets ([`phase2::Framer`]).
pub trait Framing {
    /// A frame as one receiver framed it.
    type Unit;
    /// Copies that start within this of each other (s) are one group (the
    /// receivers' timing differs by a few symbols).
    const GUARD_S: f64;
    /// Every receiver must be this far (s) past a group's start before the
    /// group is released.
    const HOLD_S: f64;
    fn make(cfg: &BankConfig) -> Self;
    fn push(&mut self, s: &Symbol, out: &mut Vec<Self::Unit>);
    fn expect_nac(&mut self, nac: Option<u16>);
    /// The channel-sample instant it starts at.
    fn sample(u: &Self::Unit) -> f64;
    /// Corrections its header needed, for [`Detect`].
    fn errors(u: &Self::Unit) -> u8;
}

impl Framing for Framer {
    type Unit = Frame;
    /// Half a TDU.
    const GUARD_S: f64 = 36.0 / SYMBOL_RATE;
    /// The longest frame (LDU, 0.18 s) + the C4FM receiver's latency (~0.1 s) + slack.
    const HOLD_S: f64 = 0.35;
    fn make(cfg: &BankConfig) -> Self {
        Framer::new(cfg.framer)
    }
    fn push(&mut self, s: &Symbol, out: &mut Vec<Frame>) {
        Framer::push(self, s, out)
    }
    fn expect_nac(&mut self, nac: Option<u16>) {
        Framer::expect_nac(self, nac)
    }
    fn sample(f: &Frame) -> f64 {
        f.sample
    }
    fn errors(f: &Frame) -> u8 {
        f.nid.errors
    }
}

impl Framing for phase2::Framer {
    type Unit = Packet;
    /// Half a slot.
    const GUARD_S: f64 = 0.5 * phase2::SLOT_DIBITS as f64 / phase2::SYMBOL_RATE;
    /// A receiver that missed a packet doesn't hold the others up longer.
    const HOLD_S: f64 = 0.25;
    fn make(_: &BankConfig) -> Self {
        phase2::Framer::new()
    }
    fn push(&mut self, s: &Symbol, out: &mut Vec<Packet>) {
        phase2::Framer::push(self, s, out)
    }
    fn expect_nac(&mut self, _: Option<u16>) {}
    fn sample(p: &Packet) -> f64 {
        p.sample
    }
    /// (Phase 2 runs CQPSK receivers only, so never detects.)
    fn errors(_: &Packet) -> u8 {
        0
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct BankConfig {
    pub cqpsk: bool,
    pub cqpsk_eq: bool,
    pub c4fm: bool,
    pub eq_taps: usize,
    pub eq_mu: f32,
    #[serde(flatten)]
    pub framer: FramerOptions,
    /// Pass bit reliabilities to the framer (soft Viterbi / soft FEC). Off:
    /// hard decisions only, for comparisons.
    pub soft: bool,
    /// With both kinds of receiver (`modulation: auto`): find which the
    /// channel needs, then run only that ([`Detect`]). Off: both, always.
    pub detect: bool,
}

impl Default for BankConfig {
    fn default() -> Self {
        BankConfig { cqpsk: true, cqpsk_eq: true, c4fm: true, eq_taps: 9, eq_mu: 0.02, framer: FramerOptions::default(), soft: true, detect: true }
    }
}

/// The CQPSK receivers and the C4FM one, as kinds of receiver ([`Detect`]).
const KINDS: usize = 2;
const CQPSK_KIND: usize = 0;
const C4FM_KIND: usize = 1;

/// Frame slots seen before a kind is chosen, at the least; with no clear
/// winner by the most, the better one (C4FM on a tie) is taken.
const DETECT_SLOTS: u32 = 40;
const DETECT_MAX_SLOTS: u32 = 200;
/// A clear winner: at least this many NID corrections fewer (two lost
/// frames' worth), and no more than this share of the other's.
const DETECT_MARGIN: u32 = 22;
const DETECT_SHARE: f32 = 0.75;
/// No frame slot for this long, s: the signal's gone; detect afresh when it's back.
const DETECT_LOST_S: f64 = 10.0;

/// `modulation: auto`: which kind of receiver the channel needs, found
/// once. On real air one kind does all the work (`tool snr`: on simulcast
/// CQPSK the C4FM receiver adds nothing at any SNR, and decodes less than
/// half of it at 7 dB; on C4FM the CQPSK receivers find no frame at all),
/// while running both costs as much again or more. Both run until one
/// has clearly needed fewer NID corrections over [`DETECT_SLOTS`] frame
/// slots or more — a frame it missed counting as 11, the most a NID
/// corrects — and then only that one. (One window isn't enough: on a
/// strong simulcast signal C4FM can find every frame of 40.) Detected
/// again from scratch when the channel is reopened (another control
/// channel) or after [`DETECT_LOST_S`] without a frame.
#[derive(Default)]
struct Detect {
    /// Each kind's NID corrections summed over the slots seen (a slot it missed: 11).
    errs: [u32; KINDS],
    slots: u32,
    /// The kind chosen; the other's receivers no longer run.
    locked: Option<usize>,
    /// Channel samples pushed, and the last frame slot's sample.
    pos: f64,
    last_slot: f64,
}

/// CQPSK receivers are kept as such so they can share a matched filter.
enum Demod {
    Cqpsk(Box<Cqpsk>),
    Other(Box<dyn Receiver + Send>),
}

struct Rx<F> {
    demod: Demod,
    framer: F,
    /// Sample instant of the latest symbol.
    progress: f64,
    frames: u64,
    /// The channel sample its first sample was (a kind woken from rest starts afresh, later).
    offset: f64,
}

struct Pending<U> {
    sample: f64,
    frames: Group<U>,
    /// Each frame's receiver.
    rx: Vec<usize>,
}

pub struct Bank<F: Framing = Framer> {
    rx: Vec<Rx<F>>,
    pending: VecDeque<Pending<F::Unit>>,
    guard: f64,
    hold: f64,
    soft: bool,
    last_released: Option<f64>,
    syms: Vec<Symbol>,
    new_frames: Vec<F::Unit>,
    /// The lead CQPSK receiver's matched filter output for this push.
    filtered: [Vec<f32>; 2],
    pub groups: u64,
    /// None: every receiver runs, always (one kind configured, or `detect` off).
    detect: Option<Detect>,
    /// To start the other kind's receivers afresh when detecting again.
    rate: f64,
    cfg: BankConfig,
    /// The plain CQPSK receiver's options (the equalised one's: these with the equaliser, [`Bank::receivers`]).
    cqpsk: cqpsk::Options,
    c4fm: C4fmOptions,
    nac: Option<u16>,
}

impl<F: Framing> Bank<F> {
    /// The site's NAC, for the framers' NID recovery ([`Framer::expect_nac`]).
    pub fn expect_nac(&mut self, nac: Option<u16>) {
        self.nac = nac;
        for r in &mut self.rx {
            r.framer.expect_nac(nac);
        }
    }

    /// Its receivers, by kind, as configured. The equalised CQPSK receiver
    /// goes without decision feedback (Phase 2: on 21 MetroC simulcast calls,
    /// bad AMBE frames 12.3 % without it, 14.0 % with; research/p25-phase2-diversity.md).
    fn receivers(rate: f64, cfg: BankConfig, cq: cqpsk::Options, c4fm: C4fmOptions) -> Vec<Rx<F>> {
        let mut rx = Vec::new();
        let mut add = |d: Demod| rx.push(Rx { demod: d, framer: F::make(&cfg), progress: 0.0, frames: 0, offset: 0.0 });
        if cfg.cqpsk {
            add(Demod::Cqpsk(Box::new(Cqpsk::new(rate, cq))));
        }
        if cfg.cqpsk_eq {
            add(Demod::Cqpsk(Box::new(Cqpsk::new(rate, cqpsk::Options { eq_taps: cfg.eq_taps, eq_mu: cfg.eq_mu, df_beta: 0.0, ..cq }))));
        }
        if cfg.c4fm {
            add(Demod::Other(Box::new(C4fm::with_options(rate, c4fm))));
        }
        rx
    }

    fn kind(r: &Rx<F>) -> usize {
        match r.demod {
            Demod::Cqpsk(_) => CQPSK_KIND,
            Demod::Other(_) => C4FM_KIND,
        }
    }

    /// Whether receiver `r` runs now.
    fn running(&self, r: &Rx<F>) -> bool {
        self.detect.as_ref().is_none_or(|d| d.locked.is_none_or(|k| k == Self::kind(r)))
    }

    /// The kind detected, once one is: "CQPSK" or "C4FM".
    pub fn detected(&self) -> Option<&'static str> {
        self.detect.as_ref()?.locked.map(|k| if k == CQPSK_KIND { "CQPSK" } else { "C4FM" })
    }

    /// Detect afresh: the kind not chosen started again, from now.
    fn redetect(&mut self) {
        let Some(d) = self.detect.as_mut() else { return };
        let Some(chosen) = d.locked.take() else { return };
        *d = Detect { pos: d.pos, last_slot: d.pos, ..Default::default() };
        let (k, pos) = (1 - chosen, d.pos);
        let fresh = Self::receivers(self.rate, self.cfg, self.cqpsk, self.c4fm);
        for (r, f) in self.rx.iter_mut().zip(fresh) {
            if Self::kind(r) == k {
                let frames = r.frames;
                *r = f;
                // (It counts its samples from 0: from now, on the channel's count.)
                (r.frames, r.offset, r.progress) = (frames, pos, pos);
                r.framer.expect_nac(self.nac);
            }
        }
    }

    fn build(rate: f64, cfg: BankConfig, cq: cqpsk::Options, c4fm: C4fmOptions) -> Self {
        let rx = Self::receivers(rate, cfg, cq, c4fm);
        let both = rx.iter().any(|r| Self::kind(r) == CQPSK_KIND) && rx.iter().any(|r| Self::kind(r) == C4FM_KIND);
        Bank {
            detect: (cfg.detect && both).then(Detect::default),
            rate,
            cfg,
            cqpsk: cq,
            c4fm,
            nac: None,
            rx,
            pending: VecDeque::new(),
            guard: F::GUARD_S * rate,
            hold: F::HOLD_S * rate,
            soft: cfg.soft,
            last_released: None,
            syms: Vec::new(),
            new_frames: Vec::new(),
            filtered: Default::default(),
            groups: 0,
        }
    }

    /// Feed channel IQ; groups that are complete are appended to `out`, in time order.
    pub fn push(&mut self, iq: &[Complex32], out: &mut Vec<Group<F::Unit>>) {
        if self.detect.as_ref().is_some_and(|d| d.locked.is_some() && d.pos - d.last_slot > DETECT_LOST_S * self.rate) {
            self.redetect();
        }
        let resting = self.detect.as_ref().and_then(|d| d.locked).map(|k| 1 - k);
        // The first CQPSK receiver's matched filter output serves the others
        // that filter the same way (it is most of their work).
        let mut lead: Option<usize> = None;
        for i in 0..self.rx.len() {
            let (before, rest) = self.rx.split_at_mut(i);
            let r = &mut rest[0];
            let kind = Self::kind(r);
            if resting == Some(kind) {
                continue;
            }
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
            if r.offset != 0.0 {
                for s in self.syms.iter_mut() {
                    s.sample += r.offset;
                }
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
                let sample = F::sample(&f);
                if let Some(g) = self.pending.iter_mut().find(|g| (g.sample - sample).abs() < self.guard) {
                    g.frames.push(f);
                    g.rx.push(i);
                } else {
                    let at = self.pending.iter().position(|g| g.sample > sample).unwrap_or(self.pending.len());
                    self.pending.insert(at, Pending { sample, frames: vec![f], rx: vec![i] });
                }
            }
        }
        if let Some(d) = self.detect.as_mut() {
            d.pos += iq.len() as f64;
        }
        self.release(false, out);
    }

    /// Release everything still held (end of input).
    pub fn flush(&mut self, out: &mut Vec<Group<F::Unit>>) {
        self.release(true, out);
    }

    /// How far above the channel the carrier is, Hz, from whichever receiver measures it.
    pub fn offset_hz(&self) -> Option<f32> {
        self.rx.iter().filter(|r| self.running(r)).find_map(|r| match &r.demod {
            Demod::Cqpsk(c) => c.offset_hz(),
            Demod::Other(d) => d.offset_hz(),
        })
    }

    /// Frames found per receiver (CQPSK, CQPSK + EQ, C4FM, as configured).
    pub fn frames_per_rx(&self) -> Vec<u64> {
        self.rx.iter().map(|r| r.frames).collect()
    }
}

/// Phase 1: frames.
impl Bank {
    pub fn new(rate: f64, cfg: BankConfig) -> Self {
        Self::with_c4fm(rate, cfg, C4fmOptions::default())
    }

    /// With the C4FM receiver's variant options (weak-signal comparisons).
    pub fn with_c4fm(rate: f64, cfg: BankConfig, c4fm: C4fmOptions) -> Self {
        Self::build(rate, cfg, cqpsk::Options::default(), c4fm)
    }
}

/// Phase 2: slot packets, from CQPSK receivers only (the bank's `cqpsk`
/// receiver, always, and the equalised one as configured). Simulcast's
/// frequency-selective channel is what limits Phase 2 (research/weak-signal.md):
/// on 21 MetroC simulcast calls the equalised receiver beside the plain one
/// took bad AMBE frames from 17.7 % to 12.3 %; a third receiver gained half
/// a point more for half as much work again (research/p25-phase2-diversity.md).
impl Bank<phase2::Framer> {
    pub fn phase2(rate: f64, cfg: BankConfig) -> Self {
        Self::phase2_with(rate, cfg, Self::phase2_cqpsk())
    }

    /// The plain receiver's options: decision-feedback differential
    /// detection, ~1 dB in noise on Phase 2 voice (`tool snr`; not used on
    /// Phase 1, where simulcast didn't like it).
    pub fn phase2_cqpsk() -> cqpsk::Options {
        cqpsk::Options { baud: phase2::SYMBOL_RATE, df_beta: 0.5, ..Default::default() }
    }

    /// With the plain receiver's variant options (weak-signal comparisons).
    pub fn phase2_with(rate: f64, cfg: BankConfig, cq: cqpsk::Options) -> Self {
        Self::build(rate, BankConfig { cqpsk: true, c4fm: false, ..cfg }, cq, C4fmOptions::default())
    }
}

/// The receivers' framing and demodulation, summed over the bank: frame
/// syncs found, NIDs that failed, frames carried over a missed sync
/// (flywheel), CQPSK equaliser resets — the C4FM receiver's eye opening and
/// the lead CQPSK receiver's phase error.
impl<F: Framing> Bank<F> {
    /// The running C4FM receiver's eye opening, and the lead CQPSK receiver's phase error.
    pub fn quality(&self) -> crate::trunk::voice::ChannelQuality {
        crate::trunk::voice::ChannelQuality {
            sep: self.rx.iter().filter(|r| self.running(r)).find_map(|r| match &r.demod {
                Demod::Other(d) => d.quality(),
                Demod::Cqpsk(_) => None,
            }),
            phase_err_deg: self.rx.iter().filter(|r| self.running(r)).find_map(|r| match &r.demod {
                Demod::Cqpsk(c) => c.phase_error_deg(),
                Demod::Other(_) => None,
            }),
        }
    }
}

impl Instrumented for Bank {
    fn report(&self, sink: &mut dyn Sink) {
        let (mut syncs, mut nid_fails, mut flywheels, mut eq_resets) = (0, 0, 0, 0);
        for r in &self.rx {
            syncs += r.framer.stats.syncs;
            nid_fails += r.framer.stats.nid_fails;
            flywheels += r.framer.stats.flywheels;
            if let Demod::Cqpsk(c) = &r.demod {
                eq_resets += c.eq_resets;
            }
        }
        sink.counter("syncs", syncs);
        sink.counter("nidFails", nid_fails);
        sink.counter("flywheels", flywheels);
        sink.counter("eqResets", eq_resets);
        sink.counter("frames", self.groups);
        let q = self.quality();
        if let Some(q) = q.sep {
            sink.gauge("sep", q as f64);
        }
        if let Some(e) = q.phase_err_deg {
            sink.gauge("phaseErr", e as f64);
        }
        if let Some(off) = self.offset_hz() {
            sink.gauge("offset", off as f64);
        }
    }
}

impl<F: Framing> Bank<F> {
    fn release(&mut self, all: bool, out: &mut Vec<Group<F::Unit>>) {
        // (The kind not chosen doesn't hold the other up.)
        let min_progress = self.rx.iter().filter(|r| self.running(r)).map(|r| r.progress).fold(f64::INFINITY, f64::min);
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
            self.detect_on(&g);
            // In the receivers' order, not the order they framed it in.
            let mut frames: Vec<(usize, F::Unit)> = g.rx.into_iter().zip(g.frames).collect();
            frames.sort_by_key(|(i, _)| *i);
            out.push(frames.into_iter().map(|(_, f)| f).collect());
        }
    }

    /// One more frame slot for [`Detect`]: each kind's NID corrections; a kind chosen once one is clearly better.
    fn detect_on(&mut self, g: &Pending<F::Unit>) {
        let Some(d) = self.detect.as_mut() else { return };
        d.last_slot = d.last_slot.max(g.sample);
        if d.locked.is_some() {
            return;
        }
        let mut best = [None::<u8>; KINDS];
        for (f, &i) in g.frames.iter().zip(&g.rx) {
            let (k, e) = (Self::kind(&self.rx[i]), F::errors(f));
            best[k] = Some(best[k].map_or(e, |b| b.min(e)));
        }
        d.slots += 1;
        for k in 0..KINDS {
            d.errs[k] += best[k].map_or(11, |e| e.min(11)) as u32;
        }
        if d.slots < DETECT_SLOTS {
            return;
        }
        let (q, c) = (d.errs[CQPSK_KIND], d.errs[C4FM_KIND]);
        let clear = |w: u32, l: u32| w + DETECT_MARGIN <= l && w as f32 <= DETECT_SHARE * l as f32;
        d.locked = if clear(q, c) {
            Some(CQPSK_KIND)
        } else if clear(c, q) {
            Some(C4FM_KIND)
        } else if d.slots >= DETECT_MAX_SLOTS {
            Some(if q < c { CQPSK_KIND } else { C4FM_KIND })
        } else {
            None
        };
    }
}

// ── Choosing from a group ────────────────────────────────────────────────────

/// The frame to take header fields from: the one whose NID needed the fewest corrections.
pub fn best_frame(g: &Group) -> &Frame {
    g.iter().min_by_key(|f| f.nid.errors).expect("groups are never empty")
}

/// Every CRC-valid TSBK of the group's TSDUs, one per block position, up to
/// the last-block flag; `missing` counts blocks no receiver decoded.
/// RESEARCH: list Viterbi's list length (≤ 1: off) and its metric limit (f32 bits).
pub static TSBK_LIST_K: AtomicUsize = AtomicUsize::new(0);
pub static TSBK_LIST_DELTA: AtomicU32 = AtomicU32::new(0x7f80_0000);

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
    // RESEARCH: list Viterbi for the blocks no receiver's best path got.
    let k = TSBK_LIST_K.load(Relaxed);
    let delta = f32::from_bits(TSBK_LIST_DELTA.load(Relaxed));
    if k > 1 {
        for (b, s) in slot.iter_mut().enumerate().take(max_blocks) {
            if s.is_some() {
                continue;
            }
            let at = 112 + b * 196;
            let mut cands: Vec<(f32, Tsbk)> = g
                .iter()
                .filter(|f| f.nid.duid == TSDU && f.bits.len() >= at + 196)
                .flat_map(|f| {
                    let l = trellis_list(&f.bits[at..at + 196], (!f.soft.is_empty()).then(|| &f.soft[at..at + 196]), k);
                    let best = l[0].1;
                    l.into_iter().skip(1).map(move |(t, m)| (m - best, t))
                })
                .filter(|c| c.0 <= delta)
                .collect();
            cands.sort_by(|a, b| a.0.total_cmp(&b.0));
            *s = cands.into_iter().map(|c| c.1).find(tsbk_ok);
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

/// Symbols of air that `good` of a frame's `blocks` blocks account for: the
/// frame's length shared out over its blocks. (Summed, over the symbol rate
/// times the time it took: the share of the control channel decoded, which
/// counting messages can't give where an MBT takes several TSBKs' air.)
pub fn air_symbols(blocks: usize, good: usize) -> u64 {
    if blocks == 0 {
        return 0;
    }
    (blocks_dibits(blocks) * good.min(blocks) / blocks) as u64
}

/// A PDU group's message, from whichever receivers' copies it takes.
pub fn best_pdu(g: &Group) -> Pdu {
    mbt::decode(g.iter().filter(|f| f.nid.duid == PDU).map(|f| (&f.bits[..], (!f.soft.is_empty()).then_some(&f.soft[..]))))
}

/// What a control channel frame (a group of copies) held.
#[derive(Default)]
pub struct ControlFrame {
    /// Its trunking messages: each TSBK's, or the MBT's.
    pub osps: Vec<Osp>,
    /// Messages no receiver decoded.
    pub lost: usize,
    /// Symbols of air the decoded ones account for ([`air_symbols`]).
    pub air: u64,
    /// Symbols of air the frame took, decoded or not: its length (a PDU
    /// whose header said nothing, at 3 blocks).
    pub framed: u64,
}

/// A control channel frame group's messages (its frame's DUID `duid`).
pub fn control_frame(g: &Group, duid: u8) -> ControlFrame {
    match duid {
        TSDU => {
            let (blocks, lost) = best_tsbks(g);
            let n = blocks.len() + lost;
            ControlFrame { air: air_symbols(n, blocks.len()), lost, osps: blocks.iter().map(osp::from_tsbk).collect(), framed: air_symbols(n, n) }
        }
        PDU => match best_pdu(g) {
            Pdu::Mbt(m) => ControlFrame { air: air_symbols(m.blocks(), m.blocks()), lost: 0, osps: vec![osp::from_mbt(&m)], framed: air_symbols(m.blocks(), m.blocks()) },
            Pdu::Other(_) => ControlFrame { framed: frame_dibits(PDU) as u64, ..Default::default() },
            Pdu::Lost => ControlFrame { lost: 1, framed: frame_dibits(PDU) as u64, ..Default::default() },
        },
        // A TDU between frames is filler: decoded air all the same.
        TDU => ControlFrame { air: frame_dibits(TDU) as u64, framed: frame_dibits(TDU) as u64, ..Default::default() },
        d => ControlFrame { framed: frame_dibits(d) as u64, ..Default::default() },
    }
}

/// Per codeword, the candidate the soft decoder overrode least (cost per
/// unit of reliability), among the group's LDUs.
pub fn best_imbe(g: &Group) -> [ImbeParams; 9] {
    let mut best = [ImbeParams::default(); 9];
    let mut score = [f32::INFINITY; 9];
    for f in g.iter().filter(|f| f.nid.duid == LDU1 || f.nid.duid == LDU2) {
        for (k, p) in ldu_imbe(f, true).into_iter().enumerate() {
            let s = imbe_score(&p);
            if s < score[k] {
                score[k] = s;
                best[k] = p;
            }
        }
    }
    best
}

/// A candidate's cost per unit of reliability. An erased bit (no
/// reliability) is flipped for free, so a copy heard through a fade would
/// always win; each counts its expected cost instead, half the mean
/// reliability of the codeword's other bits (it is wrong half the time).
/// Without erased bits: `cost / mean_rel`.
fn imbe_score(p: &ImbeParams) -> f32 {
    if p.mean_rel <= 0.0 {
        return p.errs as f32;
    }
    if p.erased == 0 {
        return p.cost / p.mean_rel;
    }
    let mean = p.mean_rel * 144.0 / (144 - p.erased.min(143)) as f32;
    (p.cost + p.erased as f32 * mean / 2.0) / mean
}

/// A Phase 2 slot's burst type: the one most copies decoded (the first
/// copy's among equals); none when no copy decoded one.
pub fn best_burst_kind(kinds: &[i8]) -> Option<i8> {
    let valid = || kinds.iter().copied().filter(|k| *k >= 0);
    valid().max_by_key(|k| (valid().filter(|j| j == k).count(), std::cmp::Reverse(kinds.iter().position(|j| j == k))))
}

/// A Phase 2 voice burst (a 4V's four AMBE codewords, a 2V's two) from
/// descrambled copies and their bit reliabilities: the first copy's, with
/// each codeword taken from the copy that decodes it with the fewest
/// errors, then the highest mean reliability. (Not [`imbe_score`]: an AMBE
/// codeword's decoder counts its corrections only.)
pub fn best_ambe<'a>(copies: &[([u8; BURST_DIBITS], Option<&'a [f32]>)], four: bool) -> ([u8; BURST_DIBITS], Option<Cow<'a, [f32]>>) {
    let (mut x, rel) = copies[0];
    let mut rel = rel.map(Cow::Borrowed);
    if copies.len() < 2 {
        return (x, rel);
    }
    let at: &[usize] = if four { &VCW_AT_4V } else { &VCW_AT_2V };
    for &a in at {
        let (d, b) = (a..a + VCW_DIBITS, 2 * a..2 * (a + VCW_DIBITS));
        let cost = |(cx, cr): &([u8; BURST_DIBITS], Option<&[f32]>)| {
            let r = cr.map(|r| &r[b.clone()]);
            let errs = decode_vcw(&cx[d.clone()], r).errs;
            let conf = r.map_or(0.0, |r| r.iter().sum::<f32>() / r.len() as f32);
            (errs, -conf)
        };
        let best = (0..copies.len()).min_by(|&i, &j| cost(&copies[i]).partial_cmp(&cost(&copies[j])).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
        if best != 0 {
            let (cx, cr) = &copies[best];
            x[d.clone()].copy_from_slice(&cx[d.clone()]);
            if let (Some(r), Some(cr)) = (rel.as_mut(), cr) {
                r.to_mut()[b.clone()].copy_from_slice(&cr[b.clone()]);
            }
        }
    }
    (x, rel)
}

/// The HDU, from whichever receivers' copy decodes.
pub fn best_hdu(g: &Group) -> Option<HduInfo> {
    g.iter().filter(|f| f.nid.duid == HDU).find_map(|f| decode_hdu(f, true))
}
pub fn best_lc(g: &Group) -> Option<LinkControl> {
    g.iter().filter(|f| f.nid.duid == LDU1).find_map(|f| decode_ldu1_lc(f, true))
}
pub fn best_es(g: &Group) -> Option<EncryptionSync> {
    g.iter().filter(|f| f.nid.duid == LDU2).find_map(|f| decode_ldu2_es(f, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p25::phase2::{BURST_2V, BURST_4V};

    #[test]
    fn a_phase2_burst_type_is_the_copies_majority_is_the_copies_best_burst_kind() {
        assert_eq!(best_burst_kind(&[BURST_4V, BURST_4V]), Some(BURST_4V));
        assert_eq!(best_burst_kind(&[-1, BURST_2V]), Some(BURST_2V), "a copy that decoded none doesn't count");
        assert_eq!(best_burst_kind(&[BURST_2V, BURST_4V, BURST_4V]), Some(BURST_4V));
        assert_eq!(best_burst_kind(&[BURST_2V, BURST_4V]), Some(BURST_2V), "the first copy's among equals");
        assert_eq!(best_burst_kind(&[-1, -1]), None);
    }

    #[test]
    fn each_codeword_comes_from_the_copy_that_decodes_it_best() {
        // A 4V burst of four known codewords, and two copies of it each damaged in a different one.
        let words: Vec<[u8; 49]> = (0..4u8).map(|k| std::array::from_fn(|i| ((i * 7 + k as usize * 3) % 5 == 0) as u8)).collect();
        let mut clean = [0u8; BURST_DIBITS];
        for (w, &a) in words.iter().zip(&VCW_AT_4V) {
            clean[a..a + VCW_DIBITS].copy_from_slice(&crate::ambe::encode_vcw(w));
        }
        let damage = |x: &mut [u8; BURST_DIBITS], a: usize| {
            // Three dibits of c0 (Golay 24): correctable, but counted as errors.
            for d in [a, a + 1, a + 2] {
                x[d] ^= 1;
            }
        };
        let (mut a, mut b) = (clean, clean);
        damage(&mut a, VCW_AT_4V[1]);
        damage(&mut b, VCW_AT_4V[2]);
        let (x, _) = best_ambe(&[(a, None), (b, None)], true);
        for (w, &at) in words.iter().zip(&VCW_AT_4V) {
            let v = decode_vcw(&x[at..at + VCW_DIBITS], None);
            assert_eq!((v.bits, v.errs), (*w, 0), "codeword at {at}: from the copy without errors there");
        }
        // One copy: as it is.
        assert_eq!(best_ambe(&[(a, None)], true).0, a);
        // A 2V's two codewords only: what is past them stays the first copy's.
        let mut c = a;
        c[VCW_AT_4V[2]] ^= 3;
        assert_eq!(best_ambe(&[(c, None), (clean, None)], false).0[VCW_AT_4V[2]], c[VCW_AT_4V[2]]);
    }


    #[test]
    fn airtime_is_the_frames_length_shared_over_its_blocks() {
        // At 4800 symbols/s: three TSBKs fill 75 ms (40 a second at most); an
        // MBT of header and one data block, 60 ms (2.4 TSBKs' worth).
        assert_eq!(air_symbols(3, 3), 360);
        assert_eq!(air_symbols(3, 2), 240);
        assert_eq!(air_symbols(2, 2), 288);
        assert_eq!(air_symbols(3, 0), 0);
        assert_eq!(air_symbols(0, 0), 0);
    }

    #[test]
    fn a_copy_heard_through_a_fade_does_not_win_on_free_flips() {
        // (From a DCFD call: CQPSK had the codeword with one marginal flip;
        // C4FM, in a fade, made 24 flips, all on bits with no reliability.)
        let clear = ImbeParams { errs: 1, cost: 0.012, mean_rel: 1.357, ..Default::default() };
        let faded = ImbeParams { errs: 24, cost: 0.0, mean_rel: 0.410, erased: 42, ..Default::default() };
        assert!(imbe_score(&clear) < imbe_score(&faded));
        // Without erased bits the score is as it was.
        assert_eq!(imbe_score(&clear), 0.012 / 1.357);
    }
}

#[cfg(test)]
mod detect_tests {
    use super::*;
    use crate::p25::synth::{dibits, imbe, modulate, Tx};

    /// On a C4FM signal the bank settles on C4FM, and finds every frame
    /// the bank with both kinds always on finds.
    #[test]
    fn auto_locks_to_the_kind_that_decodes() {
        let rate = 37_500.0;
        let tx = Tx { nac: 0x293, talkgroup: 1201, source: 1_234_567, cipher: None, voice: (0..18 * 40).map(imbe).collect() };
        let mut ph = 0.0;
        let mut iq = modulate(&dibits(&tx.frames()), rate, 0.0, 0.3, &mut ph);
        // (The channel runs on after the transmission, as live: the C4FM
        // receiver's ~0.1 s latency holds its last frame till then.)
        iq.extend(vec![Complex32::default(); (0.5 * rate) as usize]);
        crate::nxdn::synth::add_noise(&mut iq, 0.01, &mut 1);
        let run = |detect: bool| {
            let mut b = Bank::new(rate, BankConfig { detect, ..Default::default() });
            let mut out = Vec::new();
            for c in iq.chunks(192) {
                b.push(c, &mut out);
            }
            b.flush(&mut out);
            (out.len(), b.detected())
        };
        let (all, none) = run(false);
        let (detected, kind) = run(true);
        assert_eq!((none, kind), (None, Some("C4FM")));
        assert_eq!(detected, all, "the same frames");
    }
}
