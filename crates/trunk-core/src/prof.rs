//! Where the time goes: each thread's time by [`Stage`] and by source.
//!
//! Off until the platform gives a clock ([`set_clock`]): a [`span`] is then
//! one atomic load, so the browser build (no clock) and tests pay nothing.
//! With a clock a span costs two clock reads.
//!
//! Time is exclusive: a span opened inside another stops the outer one's
//! clock until it closes, so the stages of a thread add up to its busy time
//! and nothing is counted twice (the vocoder's time is not also the voice
//! receiver's). Each span is also counted, and its longest single run
//! (inclusive) kept.
//!
//! Each thread keeps its own totals and folds them into a shared copy every
//! [`FLUSH_NS`] when its outermost span closes; [`snapshot`] reads the shared
//! copies of every thread that has recorded anything (totals since start).

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex, OnceLock};

/// What a span of time was spent on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Stage {
    /// Wideband samples into the channelizer's block (u8 → float).
    ChzInput,
    /// The forward FFT of each wideband block, and its copy into the pre-roll history.
    ChzFft,
    /// One head's window × filter, inverse FFT and rotation (every channel: voice, control, conventional).
    ChzHead,
    /// A new head replaying the pre-roll history.
    ChzPreroll,
    /// Trunked voice channels' receivers and framing (not their vocoder).
    VoiceP25p1,
    VoiceP25p2,
    VoiceDmr,
    VoiceNxdn,
    VoiceAnalog,
    /// IMBE (P25 Phase 1) and AMBE+2 (Phase 2, DMR, NXDN) synthesis.
    VocoderImbe,
    VocoderAmbe,
    /// Control channels' receivers and decoding.
    ControlP25,
    ControlSmartnet,
    ControlDmr,
    ControlNxdn,
    /// Following the control channel: grants, calls, recorders, events (all that isn't DSP).
    TrunkCalls,
    /// Conventional channels: squelch over every channel of the source.
    ConvScan,
    /// Open conventional channels' receivers (not their vocoder).
    ConvFm,
    ConvAm,
    ConvP25,
    ConvDmr,
    ConvNxdn,
    /// Silence fed for samples a source dropped or never sent.
    Gap,
    /// The platform's per-block bookkeeping around the engine (signal meter, clocks).
    SessionPush,
    /// Turning engine events into outputs (calls, status, logs).
    SessionPoll,
    /// Waterfall spectra.
    SessionSpectrum,
    /// Status and stats reports for the dashboard.
    SessionStatus,
    /// Handing outputs to the dashboard, the plugins and the finish thread.
    Deliver,
    /// The engine loop's own housekeeping (commands, saves).
    Housekeeping,
    /// Writing a finished call (WAV, JSON) and its plugin uploads.
    FinishWrite,
    /// Encoding a finished call (M4A).
    FinishEncode,
}

impl Stage {
    pub const ALL: [Stage; 31] = [
        Stage::ChzInput,
        Stage::ChzFft,
        Stage::ChzHead,
        Stage::ChzPreroll,
        Stage::VoiceP25p1,
        Stage::VoiceP25p2,
        Stage::VoiceDmr,
        Stage::VoiceNxdn,
        Stage::VoiceAnalog,
        Stage::VocoderImbe,
        Stage::VocoderAmbe,
        Stage::ControlP25,
        Stage::ControlSmartnet,
        Stage::ControlDmr,
        Stage::ControlNxdn,
        Stage::TrunkCalls,
        Stage::ConvScan,
        Stage::ConvFm,
        Stage::ConvAm,
        Stage::ConvP25,
        Stage::ConvDmr,
        Stage::ConvNxdn,
        Stage::Gap,
        Stage::SessionPush,
        Stage::SessionPoll,
        Stage::SessionSpectrum,
        Stage::SessionStatus,
        Stage::Deliver,
        Stage::Housekeeping,
        Stage::FinishWrite,
        Stage::FinishEncode,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Stage::ChzInput => "chz.input",
            Stage::ChzFft => "chz.fft",
            Stage::ChzHead => "chz.head",
            Stage::ChzPreroll => "chz.preroll",
            Stage::VoiceP25p1 => "voice.p25p1",
            Stage::VoiceP25p2 => "voice.p25p2",
            Stage::VoiceDmr => "voice.dmr",
            Stage::VoiceNxdn => "voice.nxdn",
            Stage::VoiceAnalog => "voice.analog",
            Stage::VocoderImbe => "vocoder.imbe",
            Stage::VocoderAmbe => "vocoder.ambe",
            Stage::ControlP25 => "control.p25",
            Stage::ControlSmartnet => "control.smartnet",
            Stage::ControlDmr => "control.dmr",
            Stage::ControlNxdn => "control.nxdn",
            Stage::TrunkCalls => "trunk.calls",
            Stage::ConvScan => "conv.scan",
            Stage::ConvFm => "conv.fm",
            Stage::ConvAm => "conv.am",
            Stage::ConvP25 => "conv.p25",
            Stage::ConvDmr => "conv.dmr",
            Stage::ConvNxdn => "conv.nxdn",
            Stage::Gap => "gap",
            Stage::SessionPush => "session.push",
            Stage::SessionPoll => "session.poll",
            Stage::SessionSpectrum => "session.spectrum",
            Stage::SessionStatus => "session.status",
            Stage::Deliver => "deliver",
            Stage::Housekeeping => "housekeeping",
            Stage::FinishWrite => "finish.write",
            Stage::FinishEncode => "finish.encode",
        }
    }

    /// A control channel's stage, by [`crate::trunk::control::ControlChannel::name`].
    pub fn control(name: &str) -> Stage {
        match name {
            "SmartNet" => Stage::ControlSmartnet,
            "DMR" => Stage::ControlDmr,
            "NXDN" => Stage::ControlNxdn,
            _ => Stage::ControlP25,
        }
    }
}

/// No source: the span isn't one source's work.
pub const NO_SOURCE: u16 = u16::MAX;

/// How often a thread folds its totals into the shared copy, ns.
pub const FLUSH_NS: u64 = 50_000_000;

static CLOCK: OnceLock<fn() -> u64> = OnceLock::new();
static THREADS: Mutex<Vec<(String, Arc<Mutex<Totals>>)>> = Mutex::new(Vec::new());

/// Turn timing on: `clock` is monotonic nanoseconds. Once per process; later calls are ignored.
pub fn set_clock(clock: fn() -> u64) {
    let _ = CLOCK.set(clock);
}

pub fn enabled() -> bool {
    CLOCK.get().is_some()
}

/// One (stage, source)'s totals.
#[derive(Clone, Copy, Default, Debug)]
pub struct Tally {
    /// Exclusive time, ns.
    pub ns: u64,
    pub count: u64,
    /// The longest single span (inclusive of what ran inside it), ns.
    pub max_ns: u64,
}

impl Tally {
    fn add(&mut self, o: &Tally) {
        self.ns += o.ns;
        self.count += o.count;
        self.max_ns = self.max_ns.max(o.max_ns);
    }
}

/// A thread's tallies: by stage, then by source (index `source + 1`; 0 is [`NO_SOURCE`]).
#[derive(Clone, Default, Debug)]
pub struct Totals {
    by_stage: Vec<Vec<Tally>>,
}

impl Totals {
    fn slot(&mut self, stage: Stage, source: u16) -> &mut Tally {
        let s = stage as usize;
        if self.by_stage.len() <= s {
            self.by_stage.resize(Stage::ALL.len(), Vec::new());
        }
        let k = if source == NO_SOURCE { 0 } else { source as usize + 1 };
        let v = &mut self.by_stage[s];
        if v.len() <= k {
            v.resize(k + 1, Tally::default());
        }
        &mut v[k]
    }

    fn merge(&mut self, o: &Totals) {
        for (s, v) in o.by_stage.iter().enumerate() {
            for (k, t) in v.iter().enumerate() {
                if t.count > 0 || t.ns > 0 {
                    let source = if k == 0 { NO_SOURCE } else { (k - 1) as u16 };
                    self.slot(Stage::ALL[s], source).add(t);
                }
            }
        }
    }

    /// Every (stage, source, tally) with anything in it.
    pub fn iter(&self) -> impl Iterator<Item = (Stage, Option<u16>, Tally)> + '_ {
        self.by_stage.iter().enumerate().flat_map(|(s, v)| {
            v.iter().enumerate().filter(|(_, t)| t.count > 0 || t.ns > 0).map(move |(k, t)| (Stage::ALL[s], (k > 0).then(|| (k - 1) as u16), *t))
        })
    }
}

/// A thread's totals since it first recorded, as [`snapshot`] read them.
#[derive(Clone, Debug)]
pub struct ThreadTotals {
    /// Which thread (names repeat: a recording started again has a new `engine` thread).
    pub id: usize,
    pub thread: String,
    pub totals: Totals,
}

/// Every thread's totals since start.
pub fn snapshot() -> Vec<ThreadTotals> {
    THREADS.lock().unwrap().iter().enumerate().map(|(id, (name, t))| ThreadTotals { id, thread: name.clone(), totals: t.lock().unwrap().clone() }).collect()
}

/// [`snapshot`], but each longest span ([`Tally::max_ns`]) is the longest
/// since the last `take`: they start again from 0 here (for one sampler).
pub fn take() -> Vec<ThreadTotals> {
    THREADS
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(id, (name, t))| {
            let mut t = t.lock().unwrap();
            let totals = t.clone();
            for v in t.by_stage.iter_mut() {
                for x in v.iter_mut() {
                    x.max_ns = 0;
                }
            }
            ThreadTotals { id, thread: name.clone(), totals }
        })
        .collect()
}

struct Open {
    stage: Stage,
    source: u16,
    start: u64,
}

struct Local {
    stack: Vec<Open>,
    /// When the innermost open span's clock last (re)started.
    mark: u64,
    acc: Totals,
    shared: Arc<Mutex<Totals>>,
    flushed: u64,
}

impl Local {
    fn new() -> Local {
        let shared = Arc::new(Mutex::new(Totals::default()));
        let name = std::thread::current().name().map_or_else(|| format!("{:?}", std::thread::current().id()), str::to_string);
        THREADS.lock().unwrap().push((name, shared.clone()));
        Local { stack: Vec::new(), mark: 0, acc: Totals::default(), shared, flushed: 0 }
    }
}

thread_local! {
    static LOCAL: RefCell<Option<Local>> = const { RefCell::new(None) };
    static SOURCE: Cell<u16> = const { Cell::new(NO_SOURCE) };
}

/// Time `stage` until the guard drops (on this thread's current [`source`]).
#[must_use]
pub fn span(stage: Stage) -> Span {
    let Some(clock) = CLOCK.get() else { return Span { on: false } };
    let now = clock();
    let source = SOURCE.with(Cell::get);
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        let l = l.get_or_insert_with(Local::new);
        if let Some(top) = l.stack.last() {
            let (st, src) = (top.stage, top.source);
            let d = now.saturating_sub(l.mark);
            l.acc.slot(st, src).ns += d;
        }
        l.stack.push(Open { stage, source, start: now });
        l.mark = now;
    });
    Span { on: true }
}

pub struct Span {
    on: bool,
}

impl Drop for Span {
    fn drop(&mut self) {
        if !self.on {
            return;
        }
        let now = CLOCK.get().map_or(0, |c| c());
        LOCAL.with(|l| {
            let mut l = l.borrow_mut();
            let Some(l) = l.as_mut() else { return };
            let Some(top) = l.stack.pop() else { return };
            let d = now.saturating_sub(l.mark);
            let t = l.acc.slot(top.stage, top.source);
            t.ns += d;
            t.count += 1;
            t.max_ns = t.max_ns.max(now.saturating_sub(top.start));
            l.mark = now;
            if l.stack.is_empty() && now.saturating_sub(l.flushed) >= FLUSH_NS {
                l.shared.lock().unwrap().merge(&l.acc);
                l.acc = Totals::default();
                l.flushed = now;
            }
        });
    }
}

/// Spans opened until the guard drops are source `source`'s work.
#[must_use]
pub fn source(source: usize) -> SourceGuard {
    let prev = SOURCE.with(|s| s.replace(source.min(NO_SOURCE as usize - 1) as u16));
    SourceGuard { prev }
}

pub struct SourceGuard {
    prev: u16,
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        SOURCE.with(|s| s.set(self.prev));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static T: AtomicU64 = AtomicU64::new(0);

    fn fake() -> u64 {
        T.load(Ordering::Relaxed)
    }

    #[test]
    fn nested_spans_are_exclusive() {
        set_clock(fake);
        std::thread::Builder::new()
            .name("prof-test".into())
            .spawn(|| {
                let _src = source(2);
                T.store(1_000_000_000, Ordering::Relaxed);
                {
                    let _a = span(Stage::VoiceP25p1);
                    T.fetch_add(10, Ordering::Relaxed);
                    {
                        let _b = span(Stage::VocoderImbe);
                        T.fetch_add(5, Ordering::Relaxed);
                    }
                    T.fetch_add(1, Ordering::Relaxed);
                }
            })
            .unwrap()
            .join()
            .unwrap();
        let snap = snapshot();
        let t = &snap.iter().find(|t| t.thread == "prof-test").unwrap().totals;
        let get = |s: Stage| t.iter().find(|(st, _, _)| *st == s).unwrap();
        let (_, src, voice) = get(Stage::VoiceP25p1);
        let (_, _, voc) = get(Stage::VocoderImbe);
        assert_eq!(src, Some(2));
        assert_eq!((voice.ns, voice.count, voice.max_ns), (11, 1, 16));
        assert_eq!((voc.ns, voc.count, voc.max_ns), (5, 1, 5));
    }
}
