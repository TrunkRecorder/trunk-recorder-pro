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
use super::tsbk::{tsbk_last, tsbk_ok, trellis_viterbi, Tsbk};
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
    /// With both kinds of receiver: rest the kind that isn't helping ([`Adapt`]).
    pub adapt: bool,
}

impl Default for BankConfig {
    fn default() -> Self {
        BankConfig { cqpsk: true, cqpsk_eq: true, c4fm: true, eq_taps: 9, eq_mu: 0.02, framer: FramerOptions::default(), soft: true, adapt: true }
    }
}

/// The CQPSK receivers and the C4FM one, as kinds of receiver ([`Adapt`]).
const KINDS: usize = 2;
const CQPSK_KIND: usize = 0;
const C4FM_KIND: usize = 1;

/// Frame slots looked at before a kind is rested.
const ADAPT_GROUPS: u32 = 40;
/// A resting kind is woken this often (s of the channel), to look again.
const ADAPT_PROBE_S: f64 = 60.0;
/// Nor while frames come no further apart than this, s.
const ADAPT_GAP_S: f64 = 1.0;
/// It wakes when the kind left running needs this many NID corrections on
/// average (a frame lost counting as 11, the most a NID corrects). Not on one
/// frame's: on a strong simulcast signal CQPSK frames need 0–1, now and
/// then 4–7, and the C4FM receiver still decodes none the CQPSK ones miss.
const ADAPT_WAKE_MEAN: f32 = 1.5;
/// And a kind rests only while the other's frames are cleaner than this on
/// average (a weak signal keeps every receiver: both count then).
const ADAPT_REST_MEAN: f32 = 0.8;

/// Which kinds of receiver run. On a system of one modulation only one kind
/// ever decodes a frame the other doesn't (`tool snr`, real captures: on
/// simulcast CQPSK the C4FM receiver adds nothing at any SNR; on C4FM the
/// CQPSK receivers find no frame at all) — yet the other costs as much or
/// more (C4FM: two thirds of a P25 channel). After [`ADAPT_GROUPS`] frame
/// slots in which one kind was never needed — never alone in a slot, never
/// the copy with fewer NID corrections — while the other found nearly every
/// one, it rests. It wakes, its receivers started afresh, as soon as the
/// other kind's frames need correcting or stop for [`ADAPT_GAP_S`] (the
/// signal weakening: both kinds count again), and every [`ADAPT_PROBE_S`]
/// to look again.
#[derive(Default)]
struct Adapt {
    /// Since the last look: slots in which each kind was needed, or found the frame, and slots seen.
    needed: [u32; KINDS],
    found: [u32; KINDS],
    /// Each kind's NID corrections (its best copy; 11 when it missed the frame), summed.
    errs: [u32; KINDS],
    groups: u32,
    resting: Option<usize>,
    /// Channel samples pushed; when to wake the resting kind to look again; the last frame slot's sample.
    pos: f64,
    probe_at: f64,
    last_group: f64,
    /// While one kind rests: the other's NID corrections, smoothed.
    mean_errs: f32,
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
    /// None: every receiver runs, always (one kind configured, or `adapt` off).
    adapt: Option<Adapt>,
    /// To start a resting kind's receivers afresh.
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
        self.adapt.as_ref().is_none_or(|a| a.resting != Some(Self::kind(r)))
    }

    /// The resting kind's receivers again, started afresh, from now.
    fn wake(&mut self) {
        let Some(a) = self.adapt.as_mut() else { return };
        let Some(k) = a.resting.take() else { return };
        *a = Adapt { pos: a.pos, last_group: a.pos, ..Default::default() };
        let pos = a.pos;
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
            adapt: (cfg.adapt && both).then(Adapt::default),
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
        if let Some(a) = &self.adapt {
            let gap = ADAPT_GAP_S * self.rate;
            if a.resting.is_some() && (a.pos >= a.probe_at || a.pos - a.last_group > gap) {
                self.wake();
            }
        }
        let resting = self.adapt.as_ref().and_then(|a| a.resting);
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
        if let Some(a) = self.adapt.as_mut() {
            a.pos += iq.len() as f64;
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
        if let Some(q) = self.rx.iter().filter(|r| self.running(r)).find_map(|r| match &r.demod {
            Demod::Other(d) => d.quality(),
            Demod::Cqpsk(_) => None,
        }) {
            sink.gauge("sep", q as f64);
        }
        if let Some(e) = self.rx.iter().filter(|r| self.running(r)).find_map(|r| match &r.demod {
            Demod::Cqpsk(c) => c.phase_error_deg(),
            Demod::Other(_) => None,
        }) {
            sink.gauge("phaseErr", e as f64);
        }
        if let Some(off) = self.offset_hz() {
            sink.gauge("offset", off as f64);
        }
    }
}

impl Bank {
    fn release(&mut self, all: bool, out: &mut Vec<Group>) {
        // (A resting kind's receivers don't hold the others up.)
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
            self.adapt_to(&g);
            out.push(g.frames);
        }
    }

    /// One more frame slot for [`Adapt`]: which kind it needed; time to rest one, or to wake it.
    fn adapt_to(&mut self, g: &Pending) {
        let Some(a) = self.adapt.as_mut() else { return };
        a.last_group = a.last_group.max(g.sample);
        // Each kind's best copy: its NID's corrections.
        let mut best = [None::<u8>; KINDS];
        for (f, &k) in g.frames.iter().zip(&g.kinds) {
            best[k] = Some(best[k].map_or(f.nid.errors, |e| e.min(f.nid.errors)));
        }
        if let Some(r) = a.resting {
            // The kind left running struggles with its frames (or lost one): the signal is weaker.
            let e = best[1 - r].unwrap_or(u8::MAX);
            a.mean_errs += 0.1 * (e.min(11) as f32 - a.mean_errs);
            if a.mean_errs >= ADAPT_WAKE_MEAN {
                self.wake();
            }
            return;
        }
        a.groups += 1;
        for k in 0..KINDS {
            a.errs[k] += best[k].unwrap_or(11).min(11) as u32;
            if let Some(e) = best[k] {
                a.found[k] += 1;
                if best[1 - k].is_none_or(|o| e < o) {
                    a.needed[k] += 1;
                }
            }
        }
        if a.groups < ADAPT_GROUPS {
            return;
        }
        // Rest the kind never needed while the other found nearly every frame: C4FM first (it costs the most).
        let clean = |k: usize| a.errs[k] as f32 <= ADAPT_REST_MEAN * a.groups as f32;
        let rest = [C4FM_KIND, CQPSK_KIND].into_iter().find(|&k| a.needed[k] == 0 && a.found[1 - k] * 10 >= a.groups * 9 && a.found[1 - k] >= a.found[k] && clean(1 - k));
        *a = Adapt { pos: a.pos, last_group: a.last_group, resting: rest, probe_at: a.pos + ADAPT_PROBE_S * self.rate, ..Default::default() };
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
    g.iter().filter(|f| f.nid.duid == HDU).find_map(|f| decode_hdu(&f.raw))
}
pub fn best_lc(g: &Group) -> Option<LinkControl> {
    g.iter().filter(|f| f.nid.duid == LDU1).find_map(|f| decode_ldu1_lc(&f.raw))
}
pub fn best_es(g: &Group) -> Option<EncryptionSync> {
    g.iter().filter(|f| f.nid.duid == LDU2).find_map(|f| decode_ldu2_es(&f.raw))
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
mod adapt_tests {
    use super::*;
    use crate::p25::synth::{dibits, imbe, modulate, Tx};

    /// On a clean C4FM signal the CQPSK receivers find nothing: they rest,
    /// and every frame the whole bank finds is still found.
    #[test]
    fn a_kind_that_never_helps_rests() {
        let rate = 37_500.0;
        let tx = Tx { nac: 0x293, talkgroup: 1201, source: 1_234_567, cipher: None, voice: (0..18 * 40).map(imbe).collect() };
        let mut ph = 0.0;
        let mut iq = modulate(&dibits(&tx.frames()), rate, 0.0, 0.3, &mut ph);
        // (The channel runs on after the transmission, as live: the C4FM
        // receiver's ~0.1 s latency holds its last frame till then.)
        iq.extend(vec![Complex32::default(); (0.5 * rate) as usize]);
        crate::nxdn::synth::add_noise(&mut iq, 0.002, &mut 1);
        let run = |adapt: bool| {
            let mut b = Bank::new(rate, BankConfig { adapt, ..Default::default() });
            let (mut out, mut rested) = (Vec::new(), false);
            for c in iq.chunks(192) {
                b.push(c, &mut out);
                rested |= b.adapt.as_ref().is_some_and(|a| a.resting == Some(CQPSK_KIND));
            }
            b.flush(&mut out);
            (out.len(), rested)
        };
        let (all, never) = run(false);
        let (adaptive, rested) = run(true);
        assert!(!never && rested, "the CQPSK receivers rest");
        assert_eq!(adaptive, all, "the same frames");
    }
}
