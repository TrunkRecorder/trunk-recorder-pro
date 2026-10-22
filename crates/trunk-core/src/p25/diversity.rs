//! Receiver diversity: several receivers on one channel (CQPSK, CQPSK with the
//! T/2 CMA equaliser, C4FM), each with its own framer. Frames that start at the
//! same channel-sample instant (within half a TDU) form a group; a group is
//! released, oldest first, once every receiver's output has passed it, and the
//! consumer takes the best of it — per TSBK the CRC-valid copy, per IMBE
//! codeword the one the soft decoder trusted most. No receiver has to be right
//! about the whole channel, only one of them about each frame.

use std::collections::VecDeque;

use num_complex::Complex32;

use super::frame::{blocks_dibits, frame_dibits, Frame, Framer, FramerOptions, HDU, LDU1, LDU2, PDU, TDU, TSDU};
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

struct Rx {
    demod: Demod,
    framer: Framer,
    /// Sample instant of the latest symbol.
    progress: f64,
    frames: u64,
    /// The channel sample its first sample was (a kind woken from rest starts afresh, later).
    offset: f64,
}

struct Pending {
    sample: f64,
    frames: Group,
    /// Each frame's kind of receiver.
    kinds: Vec<usize>,
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
    /// None: every receiver runs, always (one kind configured, or `detect` off).
    detect: Option<Detect>,
    /// To start the other kind's receivers afresh when detecting again.
    rate: f64,
    cfg: BankConfig,
    c4fm: C4fmOptions,
    nac: Option<u16>,
}

impl Bank {
    /// The site's NAC, for the framers' NID recovery ([`Framer::expect_nac`]).
    pub fn expect_nac(&mut self, nac: Option<u16>) {
        self.nac = nac;
        for r in &mut self.rx {
            r.framer.expect_nac(nac);
        }
    }

    /// Its receivers, by kind, as configured.
    fn receivers(rate: f64, cfg: BankConfig, c4fm: C4fmOptions) -> Vec<Rx> {
        let mut rx = Vec::new();
        let mut add = |d: Demod| rx.push(Rx { demod: d, framer: Framer::new(cfg.framer), progress: 0.0, frames: 0, offset: 0.0 });
        if cfg.cqpsk {
            add(Demod::Cqpsk(Box::new(Cqpsk::new(rate, cqpsk::Options::default()))));
        }
        if cfg.cqpsk_eq {
            add(Demod::Cqpsk(Box::new(Cqpsk::new(rate, cqpsk::Options { eq_taps: cfg.eq_taps, eq_mu: cfg.eq_mu, ..Default::default() }))));
        }
        if cfg.c4fm {
            add(Demod::Other(Box::new(C4fm::with_options(rate, c4fm))));
        }
        rx
    }

    fn kind(r: &Rx) -> usize {
        match r.demod {
            Demod::Cqpsk(_) => CQPSK_KIND,
            Demod::Other(_) => C4FM_KIND,
        }
    }

    /// Whether receiver `r` runs now.
    fn running(&self, r: &Rx) -> bool {
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
        let fresh = Self::receivers(self.rate, self.cfg, self.c4fm);
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

    pub fn new(rate: f64, cfg: BankConfig) -> Self {
        Self::with_c4fm(rate, cfg, C4fmOptions::default())
    }

    /// With the C4FM receiver's variant options (weak-signal comparisons).
    pub fn with_c4fm(rate: f64, cfg: BankConfig, c4fm: C4fmOptions) -> Self {
        let rx = Self::receivers(rate, cfg, c4fm);
        let both = rx.iter().any(|r| Self::kind(r) == CQPSK_KIND) && rx.iter().any(|r| Self::kind(r) == C4FM_KIND);
        Bank {
            detect: (cfg.detect && both).then(Detect::default),
            rate,
            cfg,
            c4fm,
            nac: None,
            rx,
            pending: VecDeque::new(),
            guard: 36.0 * rate / SYMBOL_RATE,
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
                let sample = f.sample;
                if let Some(g) = self.pending.iter_mut().find(|g| (g.sample - sample).abs() < self.guard) {
                    g.frames.push(f);
                    g.kinds.push(kind);
                } else {
                    let at = self.pending.iter().position(|g| g.sample > sample).unwrap_or(self.pending.len());
                    self.pending.insert(at, Pending { sample, frames: vec![f], kinds: vec![kind] });
                }
            }
        }
        if let Some(d) = self.detect.as_mut() {
            d.pos += iq.len() as f64;
        }
        self.release(false, out);
    }

    /// Release everything still held (end of input).
    pub fn flush(&mut self, out: &mut Vec<Group>) {
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

/// The receivers' framing and demodulation, summed over the bank: frame
/// syncs found, NIDs that failed, frames carried over a missed sync
/// (flywheel), CQPSK equaliser resets — the C4FM receiver's eye opening and
/// the lead CQPSK receiver's phase error.
impl Bank {
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

impl Bank {
    fn release(&mut self, all: bool, out: &mut Vec<Group>) {
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
            out.push(g.frames);
        }
    }

    /// One more frame slot for [`Detect`]: each kind's NID corrections; a kind chosen once one is clearly better.
    fn detect_on(&mut self, g: &Pending) {
        let Some(d) = self.detect.as_mut() else { return };
        d.last_slot = d.last_slot.max(g.sample);
        if d.locked.is_some() {
            return;
        }
        let mut best = [None::<u8>; KINDS];
        for (f, &k) in g.frames.iter().zip(&g.kinds) {
            best[k] = Some(best[k].map_or(f.nid.errors, |e| e.min(f.nid.errors)));
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
}

/// A control channel frame group's messages (its frame's DUID `duid`).
pub fn control_frame(g: &Group, duid: u8) -> ControlFrame {
    match duid {
        TSDU => {
            let (blocks, lost) = best_tsbks(g);
            ControlFrame { air: air_symbols(blocks.len() + lost, blocks.len()), lost, osps: blocks.iter().map(osp::from_tsbk).collect() }
        }
        PDU => match best_pdu(g) {
            Pdu::Mbt(m) => ControlFrame { air: air_symbols(m.blocks(), m.blocks()), lost: 0, osps: vec![osp::from_mbt(&m)] },
            Pdu::Other(_) => ControlFrame::default(),
            Pdu::Lost => ControlFrame { lost: 1, ..Default::default() },
        },
        // A TDU between frames is filler: decoded air all the same.
        TDU => ControlFrame { air: frame_dibits(TDU) as u64, ..Default::default() },
        _ => ControlFrame::default(),
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
