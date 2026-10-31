//! The radio runtime: what every radio's driver ([`crate::radio`]) streams
//! through, and what the threads that take its samples receive.
//!
//! A source runs on a thread of its own ([`run_with`]): opened (with a
//! deadline, [`crate::radio::open`]), streamed into a [`Sink`] until an error,
//! then reopened after its back-off at the centre and gain last set. The
//! sink never holds a live radio up: a full queue costs that block, counted,
//! not the driver's own buffer (which an RTL-SDR would overflow unseen).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use num_complex::Complex32;

use crate::radio::priority;

/// What a source thread sends.
pub enum SourceMsg {
    /// Samples, the samples lost just before them (the driver's count, and
    /// blocks the queue had no room for), and when the driver handed them
    /// over (the source's clock is measured by it).
    Samples { source: usize, block: Block, dropped: u64, at: Instant },
    /// A [`Control`] request was applied: the samples after this are at
    /// `center_hz`, and the radio made `gap` samples' worth of quiet on purpose
    /// for it (an RTL-SDR's halt), not lost.
    Tuned { center_hz: f64, gap: u64 },
    Error { source: usize, error: String },
    /// A finite source (a capture file) has no more data.
    End { source: usize },
}

/// One block of samples, in a buffer its driver's thread reuses.
pub enum Block {
    /// 8-bit offset I/Q (RTL-SDR; a HackRF's signed bytes, offset as sent).
    U8(Pooled<u8>),
    /// Float I/Q.
    Iq(Pooled<Complex32>),
    /// Real samples at twice the I/Q rate (an Airspy's ADC, libairspy's
    /// INT16_REAL): turned into I/Q by the [`Inbox`], on the thread that takes
    /// them, not the driver's.
    Real16(Pooled<i16>),
}

impl Block {
    /// Its I/Q samples.
    pub fn len(&self) -> usize {
        match self {
            Block::U8(b) => b.len() / 2,
            Block::Iq(b) => b.len(),
            Block::Real16(b) => b.len() / 2,
        }
    }
}

/// A buffer that goes back to its pool when dropped: a driver's thread
/// (which must keep up with its device) reuses buffers instead of
/// allocating one per transfer.
pub struct Pooled<T> {
    v: Vec<T>,
    pool: Arc<Mutex<Vec<Vec<T>>>>,
}

/// Buffers a pool keeps (more are freed): a few in flight plus a queue's worth beyond the first.
const POOL_KEEP: usize = 64;

/// Buffers for a driver to fill ([`Pooled`]).
#[derive(Clone, Default)]
pub struct Pool<T> {
    free: Arc<Mutex<Vec<Vec<T>>>>,
}

impl<T> Pool<T> {
    /// An empty buffer (one returned earlier when there is one) for at least `n`.
    pub fn take(&self, n: usize) -> Pooled<T> {
        let mut v = self.free.lock().unwrap().pop().unwrap_or_default();
        v.clear();
        v.reserve(n);
        Pooled { v, pool: self.free.clone() }
    }
}

impl<T: Copy> Pool<T> {
    /// A buffer holding a copy of `from`.
    pub fn copy(&self, from: &[T]) -> Pooled<T> {
        let mut p = self.take(from.len());
        p.v.extend_from_slice(from);
        p
    }
}

impl<T> Pooled<T> {
    /// To fill.
    pub fn vec(&mut self) -> &mut Vec<T> {
        &mut self.v
    }
}

impl<T> std::ops::Deref for Pooled<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.v
    }
}

impl<T> Drop for Pooled<T> {
    fn drop(&mut self) {
        let mut free = self.pool.lock().unwrap();
        if free.len() < POOL_KEEP {
            free.push(std::mem::take(&mut self.v));
        }
    }
}

/// How a radio sends its 8-bit samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wire {
    /// Offset (0–255, centred on 127.5): RTL-SDR.
    Offset,
    /// Two's complement: HackRF (made offset as they're copied).
    Signed,
}

/// About this long of samples per 8-bit message: a radio's USB transfers
/// (an RTL-SDR's 16 KiB) are gathered so the queue's 256 messages still hold
/// seconds.
const GATHER: Duration = Duration::from_millis(25);

/// Where a source's driver sends what it receives.
pub struct Sink {
    source: usize,
    tx: SyncSender<SourceMsg>,
    /// A radio: never waits for room (a capture file played as fast as it can be does).
    live: bool,
    /// Samples in blocks the queue had no room for, told with the next one.
    lost: u64,
    closed: bool,
    bytes: Pool<u8>,
    iq: Pool<Complex32>,
    real: Pool<i16>,
    /// Bytes per 8-bit message, and the one being gathered (its dropped, when the last part came).
    gather: usize,
    pending: Option<(Pooled<u8>, u64, Instant)>,
    /// The threads before the radio opened: its driver's are raised on the first samples.
    raise: Option<priority::Before>,
}

impl Sink {
    /// For source `source` at `rate_hz`; `live`: a radio (see [`Sink::live`]).
    pub fn new(source: usize, tx: SyncSender<SourceMsg>, rate_hz: f64, live: bool) -> Sink {
        let gather = ((rate_hz * 2.0 * GATHER.as_secs_f64()) as usize).clamp(16 << 10, 1 << 20);
        Sink { source, tx, live, lost: 0, closed: false, bytes: Pool::default(), iq: Pool::default(), real: Pool::default(), gather, pending: None, raise: None }
    }

    /// An empty buffer for `n` I/Q samples, to fill and [`Sink::send`].
    pub fn iq_buffer(&self, n: usize) -> Pooled<Complex32> {
        self.iq.take(n)
    }

    /// Real samples, copied ([`Block::Real16`]).
    pub fn real16(&mut self, s: &[i16], dropped: u64, at: Instant) -> bool {
        let b = Block::Real16(self.real.copy(s));
        self.send(b, dropped, at)
    }

    /// Float I/Q, copied.
    pub fn iq(&mut self, s: &[Complex32], dropped: u64, at: Instant) -> bool {
        let b = Block::Iq(self.iq.copy(s));
        self.send(b, dropped, at)
    }

    /// 8-bit I/Q straight from the radio, gathered into messages of about
    /// [`GATHER`]; `dropped` samples were lost just before `raw`.
    pub fn bytes(&mut self, wire: Wire, raw: &[u8], dropped: u64, at: Instant) -> bool {
        if dropped > 0 {
            // (The gap starts a message: it comes before these samples.)
            self.flush();
        }
        let (buf, _, last) = self.pending.get_or_insert_with(|| (self.bytes.take(self.gather), dropped, at));
        match wire {
            Wire::Offset => buf.vec().extend_from_slice(raw),
            Wire::Signed => buf.vec().extend(raw.iter().map(|b| b ^ 0x80)),
        }
        *last = at;
        if buf.len() >= self.gather {
            self.flush();
        }
        !self.closed
    }

    /// The 8-bit samples gathered so far, sent.
    pub fn flush(&mut self) {
        if let Some((buf, dropped, at)) = self.pending.take() {
            self.put(Block::U8(buf), dropped, at);
        }
    }

    /// A block; false once nobody takes them.
    pub fn send(&mut self, block: Block, dropped: u64, at: Instant) -> bool {
        self.flush();
        self.put(block, dropped, at)
    }

    fn put(&mut self, block: Block, dropped: u64, at: Instant) -> bool {
        if let Some(b) = self.raise.take() {
            // The driver's threads are all running by now.
            priority::raise_new(&b);
        }
        let dropped = dropped + std::mem::take(&mut self.lost);
        let msg = SourceMsg::Samples { source: self.source, block, dropped, at };
        let sent = if self.live {
            match self.tx.try_send(msg) {
                Err(TrySendError::Full(SourceMsg::Samples { block, dropped, .. })) => {
                    // Dropped here, where it's counted, not in the driver's buffer, where it isn't.
                    self.lost = dropped + block.len() as u64;
                    Ok(())
                }
                Err(TrySendError::Full(_)) => Ok(()),
                Err(TrySendError::Disconnected(_)) => Err(()),
                Ok(()) => Ok(()),
            }
        } else {
            self.tx.send(msg).map_err(drop)
        };
        if sent.is_err() {
            self.closed = true;
        }
        !self.closed
    }

    /// A change the radio made ([`SourceMsg::Tuned`]): after the samples before it.
    pub fn tuned(&mut self, center_hz: f64, gap: u64) -> bool {
        self.flush();
        // (Waits for room if it must: the survey can't miss one.)
        if self.tx.send(SourceMsg::Tuned { center_hz, gap }).is_err() {
            self.closed = true;
        }
        !self.closed
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.flush();
    }
}

/// A source's samples as its consumer takes them: an Airspy's real samples
/// already turned into I/Q ([`Block::Iq`]), on this (the consumer's) thread.
pub struct Inbox {
    rx: Receiver<SourceMsg>,
    real: Option<trunk_core::dsp::real_iq::RealToIq>,
    iq: Pool<Complex32>,
}

impl Inbox {
    /// One source's queue.
    pub fn new(rx: Receiver<SourceMsg>) -> Inbox {
        Inbox { rx, real: None, iq: Pool::default() }
    }

    pub fn recv_timeout(&mut self, d: Duration) -> Result<SourceMsg, RecvTimeoutError> {
        self.rx.recv_timeout(d).map(|m| self.iq_of(m))
    }

    pub fn try_recv(&mut self) -> Result<SourceMsg, TryRecvError> {
        self.rx.try_recv().map(|m| self.iq_of(m))
    }

    fn iq_of(&mut self, m: SourceMsg) -> SourceMsg {
        let SourceMsg::Samples { source, block: Block::Real16(s), dropped, at } = m else { return m };
        let mut iq = self.iq.take(s.len() / 2 + 64);
        self.real.get_or_insert_with(trunk_core::dsp::real_iq::RealToIq::new).push(&s, iq.vec());
        SourceMsg::Samples { source, block: Block::Iq(iq), dropped, at }
    }
}

/// Retune / gain requests for a running source: the survey's, and a gain
/// changed in Setup while recording. The source applies them between blocks
/// and answers each with [`SourceMsg::Tuned`] (only the survey listens).
#[derive(Default)]
pub struct Control {
    pub freq_hz: Mutex<Option<f64>>,
    pub gain: Mutex<Option<Gain>>,
    /// The centre and gain last applied, so a reopened device comes back so.
    pub current_hz: Mutex<Option<f64>>,
    pub current_gain: Mutex<Option<Gain>>,
}

pub use sdr_core::gain::Stage;

/// A gain for a running radio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gain {
    /// Its AGC (an RTL-SDR's tuner AGC; an Airspy's runs its LNA and mixer, see [`Stages::agc`]).
    Agc,
    /// One overall gain, dB: the radio spreads it over its stages.
    Db(f32),
    /// Stage by stage (an Airspy's, a HackRF's).
    Stages(Stages),
    /// A step of one of the radio's own gain tables (libairspy's).
    Preset(Preset, u8),
}

/// A radio's own table of gains, one step setting every stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Preset {
    Linearity,
    Sensitivity,
}

/// Gain stage by stage ([`Stage`]: sdr-core's names), each in the radio's
/// own count — an Airspy's steps (LNA 0–14, mixer and VGA 0–15), a HackRF's
/// dB (its amp 0 or 14) — and whether its AGC runs the stages left out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stages {
    levels: [Option<f32>; 5],
    pub agc: bool,
}

impl Stages {
    /// These, with `stage` at `level`.
    pub fn with(mut self, stage: Stage, level: f32) -> Stages {
        self.levels[stage.index()] = Some(level);
        self
    }
    /// These, the radio's AGC running the stages left out.
    pub fn agc(mut self) -> Stages {
        self.agc = true;
        self
    }
    /// The level set for `stage`, if any.
    pub fn get(&self, stage: Stage) -> Option<f32> {
        self.levels[stage.index()]
    }
}

impl std::fmt::Display for Stages {
    /// "AGC · VGA 10", "LNA 16 · VGA 20 · AMP on".
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mut parts: Vec<String> = self.agc.then(|| "AGC".to_string()).into_iter().collect();
        for s in Stage::ALL {
            if let Some(v) = self.get(s) {
                parts.push(match s {
                    Stage::Amp => format!("AMP {}", if v > 0.0 { "on" } else { "off" }),
                    _ => format!("{} {v}", s.label()),
                });
            }
        }
        write!(f, "{}", parts.join(" · "))
    }
}

impl std::fmt::Display for Gain {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Gain::Agc => write!(f, "AGC"),
            Gain::Db(db) => write!(f, "{db} dB"),
            Gain::Stages(s) => write!(f, "{s}"),
            Gain::Preset(Preset::Linearity, step) => write!(f, "linearity {step}"),
            Gain::Preset(Preset::Sensitivity, step) => write!(f, "sensitivity {step}"),
        }
    }
}

impl Control {
    /// Pending requests (taken).
    pub fn take(&self) -> (Option<f64>, Option<Gain>) {
        (self.freq_hz.lock().unwrap().take(), self.gain.lock().unwrap().take())
    }

    /// Between blocks: the pending requests applied to `radio` (opened at
    /// `center_hz`) and answered with [`SourceMsg::Tuned`].
    pub fn apply(&self, radio: &mut impl Tunable, center_hz: f64, sink: &mut Sink) -> Result<Applied, String> {
        let (freq, gain) = self.take();
        if freq.is_none() && gain.is_none() {
            return Ok(Applied::Nothing);
        }
        let mut gap = 0;
        if let Some(f) = freq {
            let r = radio.tune(f)?;
            *self.current_hz.lock().unwrap() = Some(r.hz);
            gap += r.gap;
        }
        if let Some(g) = gain {
            gap += radio.set_gain(g)?;
            *self.current_gain.lock().unwrap() = Some(g);
        }
        let hz = self.current_hz.lock().unwrap().unwrap_or(center_hz);
        Ok(if sink.tuned(hz, gap) { Applied::Tuned } else { Applied::Closed })
    }
}

/// What [`Control::apply`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Applied {
    Nothing,
    /// Retuned or regained: samples queued before it are from before.
    Tuned,
    /// The receiver is gone: stop streaming.
    Closed,
}

/// A retune done: the centre now, and the samples' worth of quiet the radio
/// made for it on purpose (0 for radios that keep sampling through a change).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Retuned {
    pub hz: f64,
    pub gap: u64,
}

/// An open radio, as [`Control`]'s requests change it ([`Control::apply`]).
pub trait Tunable {
    /// Retune to `hz`.
    fn tune(&mut self, hz: f64) -> Result<Retuned, String>;
    /// Set `g` (a kind of gain the radio doesn't take is left alone); the
    /// quiet made for it, as [`Retuned::gap`].
    fn set_gain(&mut self, g: Gain) -> Result<u64, String>;
}

/// A radio's driver: [`run_with`] keeps it streaming.
pub trait Radio: Send + 'static {
    /// How long to wait before opening it again after an error.
    const BACKOFF: Duration;
    /// Its sample rate (I/Q samples per second).
    fn rate_hz(&self) -> f64;
    /// It opens at `hz` from now on (where the survey last tuned it).
    fn retune_to(&mut self, hz: f64);
    /// It opens with `g` from now on (the gain last set while running).
    fn gain_to(&mut self, g: Gain);
    /// Open it and stream into `sink` until `stop` or an error, retuning on
    /// `ctl`'s requests (each answered with [`SourceMsg::Tuned`]).
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String>;
}

/// Stream `radio` as source `source` until `stop` is set; blocks go to
/// `tx`. Reopens it after errors (a USB stall, a sleep/wake, an unplug, a
/// stuck stream) after its back-off, at the centre and gain `ctl` last set.
/// This thread, and the threads the radio's driver starts, run real time
/// ([`priority`]).
pub fn run_with<R: Radio>(source: usize, mut radio: R, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>, ctl: Option<Arc<Control>>) {
    priority::raise_self();
    while !stop.load(Ordering::Relaxed) {
        if let Some(c) = ctl.as_ref().and_then(|c| *c.current_hz.lock().unwrap()) {
            radio.retune_to(c);
        }
        if let Some(g) = ctl.as_ref().and_then(|c| *c.current_gain.lock().unwrap()) {
            radio.gain_to(g);
        }
        let mut sink = Sink::new(source, tx.clone(), radio.rate_hz(), true);
        sink.raise = Some(priority::before());
        let r = radio.stream_once(&mut sink, &stop, ctl.as_deref());
        drop(sink);
        if let Err(e) = r {
            let _ = tx.send(SourceMsg::Error { source, error: crate::radio::usb_memory_hint(&e) });
            std::thread::sleep(R::BACKOFF);
        }
    }
}

/// Watches a radio's raw 8-bit stream for the ways a stuck one fails while
/// still "streaming" — a frozen waterfall at the full rate, nothing dropped.
/// Live RF never repeats itself (the noise alone sees to that), so a block
/// that does — the one before it again, or one run of bytes over and over
/// (whole USB packets; the RTL2832U's 3,760-byte FIFO replay, sdr-rtl
/// findings 9) — is a radio replaying a buffer; and one that sends far
/// faster than its rate is replaying its FIFO. Either for [`STUCK_FOR`]
/// is an error (and so a reopen).
pub struct StreamCheck {
    /// Bytes per second as set.
    rate: f64,
    last: Vec<u8>,
    replay_since: Option<Instant>,
    /// The rate window: when it began, the bytes since.
    window: Option<(Instant, u64)>,
    fast_windows: u32,
}

/// How long a stream may replay itself before it counts as stuck.
const STUCK_FOR: Duration = Duration::from_secs(2);

impl StreamCheck {
    /// For a radio sending `rate_hz` I/Q samples a second, 2 bytes each.
    pub fn new(rate_hz: f64) -> StreamCheck {
        StreamCheck { rate: rate_hz * 2.0, last: Vec::new(), replay_since: None, window: None, fast_windows: 0 }
    }

    /// Each block as it comes: Err once the stream is stuck.
    pub fn block(&mut self, b: &[u8], at: Instant) -> Result<(), String> {
        let again = b == self.last.as_slice() || periodic(b);
        self.last.clear();
        self.last.extend_from_slice(b);
        if !again {
            self.replay_since = None;
        } else if self.replay_since.get_or_insert(at).elapsed() >= STUCK_FOR {
            return Err("keeps sending the same samples over and over (its USB stream is stuck, so the waterfall stands still)".into());
        }
        // Seconds-long windows, two in a row past 1.5×: a backlog the
        // transfers held while this thread waited empties faster than that.
        let (start, n) = self.window.get_or_insert((at, 0));
        *n += b.len() as u64;
        let secs = at.duration_since(*start).as_secs_f64();
        if secs >= 1.0 {
            let ratio = *n as f64 / secs / self.rate;
            self.window = Some((at, 0));
            self.fast_windows = if ratio > 1.5 { self.fast_windows + 1 } else { 0 };
            if self.fast_windows >= 2 {
                return Err(format!("sends samples at {ratio:.1}× its sample rate (it's replaying its buffer, not receiving)"));
            }
        }
        Ok(())
    }
}

/// Whether `b` is one run of bytes over and over (any period up to half of it).
fn periodic(b: &[u8]) -> bool {
    const HEAD: usize = 64;
    let n = b.len();
    n >= 2 * HEAD && (1..=n / 2).filter(|&p| b[p] == b[0]).any(|p| b[p..p + HEAD] == b[..HEAD] && b[p..] == b[..n - p])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, mut x: u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    /// Fresh samples never count; a block sent again, or a run of bytes
    /// repeated within one (the chip's 3,760-byte replay too), count once
    /// they've gone on for STUCK_FOR; so does a stream at 10× its rate.
    #[test]
    fn stuck_streams_are_found() {
        assert!(!periodic(&noise(1 << 18, 1)));
        assert!(periodic(&noise(1536, 2).repeat(64)), "whole packets over and over");
        assert!(periodic(&noise(3760, 3).repeat(8)[..16384]), "the FIFO replay");
        assert!(periodic(&[127; 4096]), "nothing at all");
        let t0 = Instant::now();
        let mut c = StreamCheck::new(2.4e6);
        // 16 KiB at 2.4 MS/s: one every 3.4 ms.
        let at = |k: u32| t0 + Duration::from_micros(3413) * k;
        for k in 0..2000 {
            assert!(c.block(&noise(16384, k + 1), at(k)).is_ok());
        }
        let same = noise(16384, 99);
        assert!(c.block(&same, t0).is_ok() && c.block(&same, t0).is_ok(), "not at once");
        c.replay_since = Some(t0 - STUCK_FOR);
        assert!(c.block(&same, t0).unwrap_err().contains("same samples"));
        let mut c = StreamCheck::new(2.4e6);
        let fast = |k: u32| t0 + Duration::from_micros(341) * k;
        let r = (0..20_000).map(|k| c.block(&noise(16384, k + 1), fast(k))).find(Result::is_err);
        assert!(r.unwrap().unwrap_err().contains("10.0×"));
    }

    /// A buffer goes back to its pool when dropped, and the next copy reuses it.
    #[test]
    fn buffers_are_reused() {
        let pool = Pool::<u8>::default();
        let a = pool.copy(&[1, 2, 3]);
        let ptr = a.as_ptr();
        drop(a);
        let b = pool.copy(&[4, 5]);
        assert_eq!(b.as_ptr(), ptr);
        assert_eq!(&*b, &[4, 5]);
    }

    /// A full queue never holds a live radio up: the block is dropped and its
    /// samples told with the next one; small transfers are gathered; signed
    /// bytes arrive offset.
    #[test]
    fn a_live_sink_never_waits_and_counts_what_it_drops() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut s = Sink::new(0, tx, 2.4e6, true);
        let t = Instant::now();
        let a = s.iq_buffer(4);
        s.send(Block::Iq(a), 3, t);
        let mut b = s.iq_buffer(10);
        b.vec().resize(10, Complex32::default());
        assert!(s.send(Block::Iq(b), 0, t), "full: dropped, not waited for");
        let take = |rx: &Receiver<SourceMsg>| match rx.try_recv() {
            Ok(SourceMsg::Samples { block, dropped, .. }) => (block.len(), dropped),
            _ => panic!("samples"),
        };
        assert_eq!(take(&rx), (0, 3));
        s.iq(&[Complex32::default(); 2], 1, t);
        assert_eq!(take(&rx), (2, 1 + 10), "the dropped block's 10 told with the next");
        // 2.4 MS/s: gathered into ~120 KB messages.
        s.bytes(Wire::Signed, &[0, 1, 0x80, 0xff], 0, t);
        s.bytes(Wire::Signed, &[5; 16384], 0, t);
        assert!(rx.try_recv().is_err(), "still gathering");
        s.flush();
        match rx.try_recv() {
            Ok(SourceMsg::Samples { block: Block::U8(b), .. }) => assert_eq!((&b[..4], b.len()), (&[0x80, 0x81, 0, 0x7f][..], 16388)),
            _ => panic!("bytes"),
        }
        drop(rx);
        assert!(!s.iq(&[], 0, t), "nobody takes them: stop");
    }

    #[test]
    fn real_samples_become_iq() {
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        let mut s = Sink::new(0, tx, 10e6, true);
        s.real16(&[1000i16; 4096], 0, Instant::now());
        let mut inbox = Inbox::new(rx);
        match inbox.try_recv() {
            Ok(SourceMsg::Samples { block: Block::Iq(iq), .. }) => assert!(!iq.is_empty()),
            _ => panic!("I/Q"),
        }
    }
}
