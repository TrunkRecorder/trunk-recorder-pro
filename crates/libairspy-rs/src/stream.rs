//! The bulk-transfer streaming engine, ported from `airspy.c`'s
//! transfer machinery: `airspy_start_rx` / `airspy_stop_rx` /
//! `airspy_is_streaming`, the libusb transfer callback's 8-slot swap
//! ring, and the producer/consumer thread pair.
//!
//! ## The transfer pool
//!
//! C queues `TRANSFER_COUNT` asynchronous 256 KiB URBs and swaps
//! completed buffers into the ring from libusb's event loop. The reader
//! side of that model lives behind the transport
//! (`UsbTransport::run_bulk_stream`): the nusb backend keeps
//! `TRANSFER_COUNT` bulk transfers in flight and resubmits each on
//! completion, handing every full buffer into `SampleQueue`
//! **non-blocking** (drop-and-count when the consumer is behind) so the
//! pipe never starves. The ring, drop accounting, and
//! stop semantics below are shared by the production and mock readers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::commands::{Command, ReceiverMode, SampleType};
use crate::conversion::{Samples, Scratch, convert_block};
use crate::device::{Device, Family};
use crate::error::{Error, Result};
use crate::transfer::NO_WINDEX;

/// `RAW_BUFFER_COUNT` in airspy.c — slots in the received-samples ring.
pub(crate) const RAW_BUFFER_COUNT: usize = 8;

/// `device->buffer_size` in `airspy_open_init` (airspy.c) — bytes per
/// bulk transfer, for the unpacked (default) geometry.
pub(crate) const BUFFER_SIZE: usize = 262_144;

/// `device->buffer_size` while packing is enabled — `6144 * 24`
/// (`airspy_set_packing`, airspy.c) — the smaller geometry the 12-bit
/// packed stream uses.
pub(crate) const PACKED_BUFFER_SIZE: usize = 6144 * 24;

/// The bulk-transfer byte size for a stream's packing mode: unpacked
/// transfers carry [`BUFFER_SIZE`] bytes of 16-bit words, packed
/// transfers carry [`PACKED_BUFFER_SIZE`] bytes of 12-bit words.
fn buffer_size_for(packing_enabled: bool) -> usize {
    if packing_enabled {
        PACKED_BUFFER_SIZE
    } else {
        BUFFER_SIZE
    }
}

/// `transfer_count` in `airspy_open_init` (airspy.c) — the number of
/// bulk transfers the reader keeps in flight simultaneously so the USB
/// pipe never starves between completions.
pub(crate) const TRANSFER_COUNT: usize = 16;

/// `LIBUSB_ENDPOINT_IN | 1` — the bulk sample endpoint
/// (`create_io_threads` / `airspy_start_rx` in airspy.c).
pub(crate) const BULK_ENDPOINT: u8 = 0x81;

/// The 500 ms event/read timeout from `transfer_threadproc`'s
/// `struct timeval timeout = { 0, 500000 }` (airspy.c).
pub(crate) const EVENT_TIMEOUT: Duration = Duration::from_millis(500);

/// One delivered block of samples — the `airspy_transfer` view handed
/// to the `airspy_start_rx` callback.
#[derive(Debug)]
pub struct Transfer<'a> {
    /// Samples in the format selected before `start_rx`
    /// (`airspy_transfer.samples` + `sample_type`).
    pub samples: Samples<'a>,
    /// The latched sample type for this stream.
    pub sample_type: SampleType,
    /// `dropped_buffers * sample_count` — samples lost to a full ring
    /// since the previous delivery (`airspy_transfer.dropped_samples`).
    pub dropped_samples: u64,
}

/// The 8-slot ring from `airspy_libusb_transfer_callback` +
/// `consumer_threadproc`, with C's buffer swap expressed as a bounded
/// recycling pool: producers take free buffers, push them filled, and
/// consumers recycle them after delivery — zero steady-state
/// allocation, exactly one owner per buffer.
pub(crate) struct SampleQueue {
    state: Mutex<QueueState>,
    cv: Condvar,
    capacity: usize,
}

struct QueueState {
    /// Filled buffers awaiting the consumer, each carrying the
    /// dropped-buffer count accumulated before it was queued
    /// (`dropped_buffers_queue` in airspy.c).
    filled: VecDeque<(Vec<u8>, u32)>,
    /// Recycled buffers available to the producer.
    free: Vec<Vec<u8>>,
    /// `device->dropped_buffers` — buffers discarded since the last
    /// successful queue insert.
    dropped: u32,
    /// Set by [`SampleQueue::shutdown`]; wakes and finishes waiters.
    shutdown: bool,
}

impl SampleQueue {
    /// A queue with `slots` ring positions and a pool of `pool`
    /// preallocated buffers.
    pub(crate) fn with_pool(slots: usize, pool: usize, buffer_size: usize) -> Self {
        Self {
            state: Mutex::new(QueueState {
                filled: VecDeque::with_capacity(slots),
                free: (0..pool).map(|_| vec![0u8; buffer_size]).collect(),
                dropped: 0,
                shutdown: false,
            }),
            cv: Condvar::new(),
            capacity: slots,
        }
    }

    /// The production configuration: `RAW_BUFFER_COUNT` slots plus one
    /// extra buffer so the producer always has one in flight, at the
    /// buffer size the current packing mode calls for.
    pub(crate) fn for_streaming(packing_enabled: bool) -> Self {
        Self::with_pool(
            RAW_BUFFER_COUNT,
            RAW_BUFFER_COUNT + 1,
            buffer_size_for(packing_enabled),
        )
    }

    /// Take a free buffer without blocking. `None` when the pool is
    /// exhausted (bounded by construction). This is the reader's handoff
    /// primitive: the nusb (and mock) `run_bulk_stream` acquires a free
    /// buffer, copies a completed transfer into it, and pushes it — never
    /// blocking, so a slow consumer causes drops rather than pool
    /// starvation. On exhaustion the reader drops the transfer's data, so
    /// we count that here (holding the lock) — otherwise pool-exhaustion
    /// losses would escape `Transfer::dropped_samples`, which only the
    /// full-ring path (`push_filled`) would otherwise record.
    pub(crate) fn try_acquire_free(&self) -> Option<Vec<u8>> {
        match self.state.lock() {
            Ok(mut s) => {
                let buf = s.free.pop();
                if buf.is_none() {
                    s.dropped = s.dropped.saturating_add(1);
                }
                buf
            }
            Err(_) => None,
        }
    }

    /// Take a free buffer, blocking until one is recycled or the queue
    /// shuts down (`None`). Retained for the queue's own unit tests; the
    /// streaming readers use the non-blocking
    /// [`SampleQueue::try_acquire_free`].
    #[cfg(test)]
    pub(crate) fn acquire_free(&self) -> Option<Vec<u8>> {
        let mut s = self.state.lock().ok()?;
        loop {
            // Shutdown wins over an available buffer so a stopping
            // stream never starts another read.
            if s.shutdown {
                return None;
            }
            if let Some(buf) = s.free.pop() {
                return Some(buf);
            }
            s = self.cv.wait(s).ok()?;
        }
    }

    /// Queue a filled buffer. On a full ring the buffer is recycled
    /// and the drop counter increments (C: `dropped_buffers++`, the
    /// transfer resubmits).
    pub(crate) fn push_filled(&self, buf: Vec<u8>) {
        let Ok(mut s) = self.state.lock() else { return };
        if s.filled.len() >= self.capacity {
            s.dropped = s.dropped.saturating_add(1);
            s.free.push(buf);
            drop(s);
            // The recycled buffer may unblock a waiting reader.
            self.cv.notify_all();
            return;
        }
        let dropped = std::mem::take(&mut s.dropped);
        s.filled.push_back((buf, dropped));
        drop(s);
        // One condvar serves both directions (filled and free);
        // notify_all so a waiting consumer AND a buffer-starved
        // reader can each re-check their predicate.
        self.cv.notify_all();
    }

    /// Blocking pop: waits until a filled buffer arrives or
    /// [`SampleQueue::shutdown`] runs. `None` means shut down.
    pub(crate) fn pop_filled(&self) -> Option<(Vec<u8>, u32)> {
        let mut s = self.state.lock().ok()?;
        loop {
            // Shutdown wins over queued data: C's consumer discards
            // undelivered buffers once streaming clears (the
            // !streaming break in consumer_threadproc).
            if s.shutdown {
                return None;
            }
            if let Some(entry) = s.filled.pop_front() {
                return Some(entry);
            }
            s = self.cv.wait(s).ok()?;
        }
    }

    /// [`SampleQueue::pop_filled`] that gives up after `timeout`.
    pub(crate) fn pop_filled_timeout(&self, timeout: Duration) -> Popped {
        let deadline = Instant::now() + timeout;
        let Ok(mut s) = self.state.lock() else {
            return Popped::Shutdown;
        };
        loop {
            if s.shutdown {
                return Popped::Shutdown;
            }
            if let Some((buf, dropped)) = s.filled.pop_front() {
                return Popped::Filled(buf, dropped);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Popped::TimedOut;
            }
            let Ok((guard, _)) = self.cv.wait_timeout(s, left) else {
                return Popped::Shutdown;
            };
            s = guard;
        }
    }

    /// Return a consumed buffer to the free pool, waking a
    /// buffer-starved reader.
    pub(crate) fn recycle(&self, buf: Vec<u8>) {
        if let Ok(mut s) = self.state.lock() {
            s.free.push(buf);
            drop(s);
            self.cv.notify_all();
        }
    }

    /// Wake all waiters and make further pops return `None`
    /// (`kill_io_threads`' condvar signal in airspy.c).
    pub(crate) fn shutdown(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.shutdown = true;
        }
        self.cv.notify_all();
    }
}

/// What [`SampleQueue::pop_filled_timeout`] came back with.
pub(crate) enum Popped {
    /// A filled buffer and the buffers dropped before it.
    Filled(Vec<u8>, u32),
    /// Nothing arrived in time; the stream is still up.
    TimedOut,
    /// The queue shut down: the stream has ended.
    Shutdown,
}

/// State shared between the reader thread, the consumer thread, and
/// the owning [`Device`] — the streaming fields of `airspy_device_t`.
pub(crate) struct StreamShared {
    /// `device->streaming`.
    pub(crate) streaming: AtomicBool,
    /// `device->stop_requested`.
    pub(crate) stop_requested: AtomicBool,
    pub(crate) queue: SampleQueue,
}

impl StreamShared {
    /// Create the streaming state: a live flag plus a fresh sample
    /// queue, over the reader's [`SampleQueue`].
    pub(crate) fn new(queue: SampleQueue) -> Self {
        Self {
            streaming: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            queue,
        }
    }

    /// True while the stream is live — `streaming` latched on and no
    /// stop requested (`airspy_is_streaming`).
    pub(crate) fn running(&self) -> bool {
        self.streaming.load(Ordering::SeqCst) && !self.stop_requested.load(Ordering::SeqCst)
    }
}

/// `consumer_threadproc`: pop filled buffers, run the sample-type
/// dispatch, hand the typed block to the callback, recycle. A `false`
/// return stops streaming.
pub(crate) fn run_consumer(
    shared: &StreamShared,
    sample_type: SampleType,
    packing_enabled: bool,
    mut scratch: Scratch,
    callback: &mut (impl FnMut(Transfer<'_>) -> bool + ?Sized),
) {
    while shared.running() {
        let Some((buf, dropped)) = shared.queue.pop_filled() else {
            break;
        };
        // A stop that raced the pop wins: recycle and exit without a
        // post-stop callback.
        if !shared.running() {
            shared.queue.recycle(buf);
            break;
        }
        let (samples, sample_count) =
            convert_block(sample_type, packing_enabled, &buf, &mut scratch);
        // C: dropped_samples = dropped_buffers * sample_count, with
        // the block's own converted count.
        let keep_going = callback(Transfer {
            samples,
            sample_type,
            dropped_samples: u64::from(dropped) * sample_count as u64,
        });
        shared.queue.recycle(buf);
        if !keep_going {
            shared.streaming.store(false, Ordering::SeqCst);
        }
    }
    shared.streaming.store(false, Ordering::SeqCst);
}

/// Worker-thread handles held by a streaming [`Device`].
pub(crate) struct StreamWorkers {
    pub(crate) shared: Arc<StreamShared>,
    reader: Option<JoinHandle<()>>,
    /// The thread running the callback; none for a polled stream.
    consumer: Option<JoinHandle<()>>,
    /// A polled stream's converter state ([`Device::start_rx_polled`]).
    polled: Option<Polled>,
}

/// What [`Device::recv`] converts with: the sample type and packing
/// latched at start, and the scratch a consumer thread would own.
struct Polled {
    sample_type: SampleType,
    packing_enabled: bool,
    scratch: Scratch,
}

impl Device {
    /// `airspy_set_receiver_mode`: `AIRSPY_RECEIVER_MODE` with the
    /// mode in wValue.
    fn set_receiver_mode(&self, mode: ReceiverMode) -> Result<()> {
        // airspy_set_receiver_mode sends no payload (NULL, 0).
        let payload: [u8; 0] = [];
        let n = self.vendor_out(Command::ReceiverMode, mode as u16, NO_WINDEX, &payload)?;
        if n != payload.len() {
            // C: result != 0 → AIRSPY_ERROR_LIBUSB (an empty request
            // that reports transferred bytes is a protocol mismatch).
            return Err(Error::TransferLengthMismatch {
                expected: payload.len(),
                actual: n,
            });
        }
        Ok(())
    }

    /// Start streaming (`airspy_start_rx`): receiver off, clear the
    /// bulk endpoint halt, receiver on, then spawn the reader and
    /// consumer threads. Returns [`Error::Busy`] while streaming.
    ///
    /// The sample type and packing mode are latched here (see
    /// [`Device::set_sample_type`]); every sample type is supported.
    ///
    /// The callback runs on the consumer thread; return `true` to
    /// keep streaming, `false` to stop (C's nonzero-return stop).
    pub fn start_rx(
        &mut self,
        callback: impl FnMut(Transfer<'_>) -> bool + Send + 'static,
    ) -> Result<()> {
        let (shared, sample_type, packing_enabled, scratch) = self.begin_rx()?;

        let consumer_shared = Arc::clone(&shared);
        let mut cb = callback;
        let consumer_spawn = std::thread::Builder::new()
            .name("airspy-consumer".into())
            .spawn(move || {
                crate::priority::raise();
                run_consumer(
                    &consumer_shared,
                    sample_type,
                    packing_enabled,
                    scratch,
                    &mut cb,
                );
            });
        let Ok(consumer) = consumer_spawn else {
            // Hardening beyond C (which leaves the receiver running
            // until stop/close after a thread-create failure): switch
            // it off so a failed start doesn't keep the device
            // streaming into nothing.
            let _ = self.set_receiver_mode(ReceiverMode::Off);
            return Err(Error::Thread);
        };

        let Ok(reader) = self.spawn_reader(&shared, packing_enabled) else {
            // Don't orphan the already-running consumer, and switch
            // the receiver off (same hardening as above).
            shared.streaming.store(false, Ordering::SeqCst);
            shared.queue.shutdown();
            let _ = consumer.join();
            let _ = self.set_receiver_mode(ReceiverMode::Off);
            return Err(Error::Thread);
        };

        self.set_stream_workers(Some(StreamWorkers {
            shared,
            reader: Some(reader),
            consumer: Some(consumer),
            polled: None,
        }));
        Ok(())
    }

    /// Start streaming without a consumer thread: the caller takes each
    /// block itself with [`Device::recv`], on whatever thread owns the
    /// device, so what it does with the samples needn't be `Send` or
    /// `'static`. Not in C. Everything else is [`Device::start_rx`]:
    /// the same receiver-mode sequence, reader thread, ring and drop
    /// counting.
    pub fn start_rx_polled(&mut self) -> Result<()> {
        let (shared, sample_type, packing_enabled, scratch) = self.begin_rx()?;
        let Ok(reader) = self.spawn_reader(&shared, packing_enabled) else {
            shared.streaming.store(false, Ordering::SeqCst);
            let _ = self.set_receiver_mode(ReceiverMode::Off);
            return Err(Error::Thread);
        };
        self.set_stream_workers(Some(StreamWorkers {
            shared,
            reader: Some(reader),
            consumer: None,
            polled: Some(Polled {
                sample_type,
                packing_enabled,
                scratch,
            }),
        }));
        Ok(())
    }

    /// Wait up to `timeout` for the next block of a polled stream and
    /// hand it to `f`. `Ok(None)` when nothing arrived in time;
    /// [`Error::StreamingStopped`] once the stream has ended (the
    /// device went away, or it was never started with
    /// [`Device::start_rx_polled`]).
    pub fn recv<R>(
        &mut self,
        timeout: Duration,
        f: impl FnOnce(Transfer<'_>) -> R,
    ) -> Result<Option<R>> {
        let Some(workers) = self.stream_workers_mut() else {
            return Err(Error::StreamingStopped);
        };
        let Some(polled) = workers.polled.as_mut() else {
            return Err(Error::StreamingStopped);
        };
        let shared = &workers.shared;
        if !shared.running() {
            return Err(Error::StreamingStopped);
        }
        let (buf, dropped) = match shared.queue.pop_filled_timeout(timeout) {
            Popped::Filled(buf, dropped) => (buf, dropped),
            Popped::TimedOut => return Ok(None),
            Popped::Shutdown => return Err(Error::StreamingStopped),
        };
        let (samples, sample_count) = convert_block(
            polled.sample_type,
            polled.packing_enabled,
            &buf,
            &mut polled.scratch,
        );
        let out = f(Transfer {
            samples,
            sample_type: polled.sample_type,
            dropped_samples: u64::from(dropped) * sample_count as u64,
        });
        shared.queue.recycle(buf);
        Ok(Some(out))
    }

    /// The first half of `airspy_start_rx`, shared by both ways of
    /// starting: receiver off, clear the halt, receiver on, and fresh
    /// stream state (flagged streaming) with the type and packing it
    /// latched.
    fn begin_rx(&mut self) -> Result<(Arc<StreamShared>, SampleType, bool, Scratch)> {
        let sample_type = self.sample_type();
        let packing_enabled = self.packing_enabled;
        self.reap_finished_workers()?;
        // Converter resets and drop-counter zeroing from
        // airspy_start_rx happen via fresh queue/converter state here.
        self.set_receiver_mode(ReceiverMode::Off)?;
        // Clear the bulk-endpoint halt between the receiver-mode OFF and RX
        // commands, matching airspy_start_rx's order (OFF → clear_halt →
        // RX → threads) and resetting the host data toggle before the
        // reader claims the endpoint (issue #75, finding #3). C ignores
        // clear_halt's result and so do we — a halt-clear failure must not
        // block a start that would otherwise succeed.
        //
        // An RFOne's is cleared after the receiver is on, "after firmware
        // has enabled it" (hydrasdr_generic_start_rx: OFF → RX →
        // clear_halt → threads); still before the reader claims it.
        let rfone = self.family() == Family::Rfone;
        if !rfone {
            let _ = self.usb_handle().clear_halt(BULK_ENDPOINT);
        }
        self.set_receiver_mode(ReceiverMode::Rx)?;
        if rfone {
            let _ = self.usb_handle().clear_halt(BULK_ENDPOINT);
        }

        let shared = Arc::new(StreamShared::new(SampleQueue::for_streaming(
            packing_enabled,
        )));
        shared.streaming.store(true, Ordering::SeqCst);
        // Preallocated so the hot loop never grows a buffer.
        let scratch = Scratch::for_stream(
            sample_type,
            packing_enabled,
            buffer_size_for(packing_enabled),
            &self.kernel_float,
            &self.kernel_int16,
        );
        Ok((shared, sample_type, packing_enabled, scratch))
    }

    /// The thread that keeps the bulk transfers in flight
    /// (`transfer_threadproc`).
    fn spawn_reader(
        &self,
        shared: &Arc<StreamShared>,
        packing_enabled: bool,
    ) -> std::io::Result<JoinHandle<()>> {
        let reader_shared = Arc::clone(shared);
        let reader_handle = self.usb_handle_arc();
        let buffer_size = buffer_size_for(packing_enabled);
        std::thread::Builder::new()
            .name("airspy-reader".into())
            .spawn(move || {
                crate::priority::raise();
                reader_handle.run_bulk_stream(BULK_ENDPOINT, &reader_shared, buffer_size);
            })
    }

    /// C gates on `streaming || stop_requested` only, so a device
    /// whose callback stopped the stream can be restarted; reap any
    /// finished workers, or report [`Error::Busy`] for a live stream.
    fn reap_finished_workers(&mut self) -> Result<()> {
        if let Some(mut workers) = self.take_stream_workers() {
            if workers.shared.running() {
                self.set_stream_workers(Some(workers));
                return Err(Error::Busy);
            }
            workers.shared.queue.shutdown();
            if let Some(t) = workers.reader.take() {
                let _ = t.join();
            }
            if let Some(t) = workers.consumer.take() {
                let _ = t.join();
            }
        }
        Ok(())
    }

    /// Stop streaming (`airspy_stop_rx`): request stop, switch the
    /// receiver off, then join the worker threads.
    ///
    /// Like C, the receiver-off command is sent even when no stream is
    /// running — `airspy_stop_rx` does so unconditionally, which also
    /// recovers a device left in RX by a failed [`Device::start_rx`].
    pub fn stop_rx(&mut self) -> Result<()> {
        let Some(mut workers) = self.take_stream_workers() else {
            return self.set_receiver_mode(ReceiverMode::Off);
        };
        workers.shared.stop_requested.store(true, Ordering::SeqCst);
        let mode_result = self.set_receiver_mode(ReceiverMode::Off);

        // kill_io_threads: clear flags, wake the consumer, join both.
        workers.shared.streaming.store(false, Ordering::SeqCst);
        workers.shared.queue.shutdown();
        if let Some(t) = workers.reader.take() {
            let _ = t.join();
        }
        if let Some(t) = workers.consumer.take() {
            let _ = t.join();
        }
        workers.shared.stop_requested.store(false, Ordering::SeqCst);

        mode_result
    }

    /// `airspy_is_streaming`: true between a successful
    /// [`Device::start_rx`] and a stop request.
    #[must_use]
    pub fn is_streaming(&self) -> bool {
        self.stream_workers().is_some_and(|w| w.shared.running())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    #[test]
    fn constants_match_c() {
        // RAW_BUFFER_COUNT (8), device->buffer_size (262144), and the
        // bulk endpoint (LIBUSB_ENDPOINT_IN | 1) from airspy.c.
        assert_eq!(RAW_BUFFER_COUNT, 8);
        assert_eq!(BUFFER_SIZE, 262_144);
        assert_eq!(TRANSFER_COUNT, 16);
        assert_eq!(BULK_ENDPOINT, 0x81);
        assert_eq!(EVENT_TIMEOUT, core::time::Duration::from_millis(500));
        // device->buffer_size while packing is enabled (airspy_set_packing).
        assert_eq!(PACKED_BUFFER_SIZE, 6144 * 24);
    }

    fn make_queue() -> SampleQueue {
        SampleQueue::with_pool(3, 4, BUFFER_SIZE) // 3 slots, 4-buffer pool, tiny for tests
    }

    /// Phase 1: the production queue must size its pool from the
    /// packing mode — packed transfers get the 6144*24-byte geometry,
    /// unpacked get the 256 KiB one — the same reassignment
    /// `airspy_set_packing` makes to `device->buffer_size` (airspy.c).
    #[test]
    fn streaming_queue_sizes_buffers_from_packing_mode() {
        let packed = SampleQueue::for_streaming(true);
        {
            let s = packed.state.lock().expect("queue lock");
            assert_eq!(s.free.len(), RAW_BUFFER_COUNT + 1);
            for buf in &s.free {
                assert_eq!(buf.len(), PACKED_BUFFER_SIZE);
            }
        }
        let unpacked = SampleQueue::for_streaming(false);
        {
            let s = unpacked.state.lock().expect("queue lock");
            assert_eq!(s.free.len(), RAW_BUFFER_COUNT + 1);
            for buf in &s.free {
                assert_eq!(buf.len(), BUFFER_SIZE);
            }
        }
    }

    #[test]
    fn buffers_pass_through_in_fifo_order() {
        let q = make_queue();
        let mut a = q.acquire_free().expect("pool has buffers");
        a[0] = 1;
        let mut b = q.acquire_free().expect("pool has buffers");
        b[0] = 2;
        q.push_filled(a);
        q.push_filled(b);
        let (first, dropped) = q.pop_filled().expect("filled");
        assert_eq!((first[0], dropped), (1, 0));
        let (second, dropped) = q.pop_filled().expect("filled");
        assert_eq!((second[0], dropped), (2, 0));
    }

    #[test]
    fn full_queue_drops_and_next_entry_carries_the_count() {
        let q = make_queue();
        for _ in 0..3 {
            let buf = q.acquire_free().expect("pool");
            q.push_filled(buf);
        }
        // Queue full: this push is dropped, its buffer recycled
        // (C: dropped_buffers++ and the transfer resubmits).
        let buf = q.acquire_free().expect("pool");
        q.push_filled(buf);

        // Drain one slot, then push again: the new entry carries the
        // accumulated drop count and the counter resets (C's
        // dropped_buffers_queue[head] = dropped_buffers; dropped = 0).
        let (buf, dropped) = q.pop_filled().expect("filled");
        assert_eq!(dropped, 0);
        q.recycle(buf);
        let buf = q.acquire_free().expect("pool");
        q.push_filled(buf);
        let (_, d0) = q.pop_filled().expect("filled");
        assert_eq!(d0, 0); // second slot, filled before the drop
        let (_, d1) = q.pop_filled().expect("filled");
        assert_eq!(d1, 0); // third slot
        let (_, d2) = q.pop_filled().expect("filled");
        assert_eq!(d2, 1); // the post-drop push carries the count
    }

    #[test]
    fn pool_exhaustion_drop_is_counted() {
        // Two distinct drop paths must both reach dropped_samples: a full
        // ring (push_filled) AND an exhausted pool (try_acquire_free ->
        // None, when the reader can't even acquire a buffer to fill). This
        // covers the latter.
        let q = make_queue(); // 3 slots, 4-buffer pool
        let held: Vec<_> = (0..4)
            .map(|_| q.try_acquire_free().expect("pool"))
            .collect();
        // Pool empty: the reader would drop this transfer's data.
        assert!(q.try_acquire_free().is_none(), "pool exhausted");

        // Recycle one buffer and queue it: the entry carries the
        // pool-exhaustion drop accumulated while the pool was empty.
        let mut held = held;
        q.recycle(held.pop().expect("held"));
        let buf = q.acquire_free().expect("recycled");
        q.push_filled(buf);
        let (_, dropped) = q.pop_filled().expect("filled");
        assert_eq!(dropped, 1, "a pool-exhaustion loss must be counted");
        drop(held);
    }

    #[test]
    fn pool_is_bounded() {
        let q = make_queue();
        let held: Vec<_> = (0..4)
            .map(|_| q.try_acquire_free().expect("pool"))
            .collect();
        assert!(q.try_acquire_free().is_none(), "pool must be bounded");
        drop(held);
    }

    #[test]
    fn blocking_acquire_wakes_on_recycle() {
        let q = Arc::new(make_queue());
        let held: Vec<_> = (0..4)
            .map(|_| q.try_acquire_free().expect("pool"))
            .collect();
        let q2 = Arc::clone(&q);
        let waiter = std::thread::spawn(move || q2.acquire_free());
        std::thread::sleep(core::time::Duration::from_millis(50));
        let mut held = held;
        q.recycle(held.pop().expect("held"));
        assert!(waiter.join().expect("join").is_some());
    }

    #[test]
    fn blocking_acquire_wakes_none_on_shutdown() {
        let q = Arc::new(make_queue());
        let _held: Vec<_> = (0..4)
            .map(|_| q.try_acquire_free().expect("pool"))
            .collect();
        let q2 = Arc::clone(&q);
        let waiter = std::thread::spawn(move || q2.acquire_free());
        std::thread::sleep(core::time::Duration::from_millis(50));
        q.shutdown();
        assert!(waiter.join().expect("join").is_none());
    }

    #[test]
    fn shutdown_wakes_blocking_pop() {
        let q = Arc::new(make_queue());
        let q2 = Arc::clone(&q);
        let waiter = std::thread::spawn(move || q2.pop_filled());
        std::thread::sleep(core::time::Duration::from_millis(50));
        q.shutdown();
        assert!(waiter.join().expect("join").is_none());
    }

    #[test]
    fn consumer_loop_delivers_transfers_and_stops_on_false() {
        // Mirrors consumer_threadproc: pops buffers, hands the caller
        // a Transfer with the accumulated dropped-sample count, and a
        // callback returning false clears `streaming` (C: nonzero
        // return → device->streaming = false).
        let shared = Arc::new(StreamShared::new(SampleQueue::with_pool(3, 4, BUFFER_SIZE)));
        shared.streaming.store(true, Ordering::SeqCst);

        for fill in [1u8, 2, 3] {
            let mut buf = shared.queue.acquire_free().expect("pool");
            buf[0] = fill;
            shared.queue.push_filled(buf);
        }

        let mut seen = Vec::new();
        let shared2 = Arc::clone(&shared);
        let consumer = std::thread::spawn(move || {
            run_consumer(
                &shared2,
                SampleType::Raw,
                false,
                Scratch::default(),
                &mut |transfer: Transfer<'_>| {
                    let Samples::Raw(bytes) = transfer.samples else {
                        unreachable!("RAW stream delivers raw bytes");
                    };
                    seen.push((bytes[0], transfer.dropped_samples));
                    seen.len() < 2 // stop after the second delivery
                },
            );
            seen
        });

        let seen = consumer.join().expect("join");
        assert_eq!(seen, vec![(1, 0), (2, 0)]);
        assert!(!shared.streaming.load(Ordering::SeqCst));
    }

    #[test]
    fn end_to_end_stream_over_mock_transport() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport, wire};
        use std::sync::mpsc;

        let transport = Arc::new(MockTransport::default());
        // Three full buffers; the exhausted script then times out,
        // keeping the stream alive until the test stops it.
        transport.script_bulk(vec![
            BulkRead::Fill(0xAB),
            BulkRead::Fill(0xCD),
            BulkRead::Fill(0xEF),
        ]);
        let mut device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");

        let (tx, rx) = mpsc::channel();
        device
            .start_rx(move |transfer| {
                let Samples::Raw(bytes) = transfer.samples else {
                    unreachable!("RAW stream");
                };
                tx.send((bytes[0], bytes.len(), transfer.dropped_samples))
                    .is_ok()
            })
            .expect("start_rx");

        // The three scripted buffers arrive in order, full-sized.
        for expected in [0xAB, 0xCD, 0xEF] {
            let (first, len, dropped) = rx
                .recv_timeout(core::time::Duration::from_secs(5))
                .expect("block");
            assert_eq!(first, expected);
            assert_eq!(len, BUFFER_SIZE);
            assert_eq!(dropped, 0);
        }

        // The stream is still alive on timeouts; stop_rx tears it
        // down and joins cleanly.
        assert!(device.is_streaming());
        device.stop_rx().expect("stop");
        assert!(!device.is_streaming());

        // The unpacked end-to-end stream must record the unpacked
        // transfer geometry for every delivery.
        let bulk = transport.bulk_calls.lock().expect("mock lock").clone();
        assert!(!bulk.is_empty());
        for call in &bulk {
            assert_eq!(call.endpoint, BULK_ENDPOINT);
            assert_eq!(call.buf_len, BUFFER_SIZE);
        }

        let modes: Vec<(u8, u16)> = transport
            .take_recorded()
            .into_iter()
            .filter(|c| c.request == wire::RECEIVER_MODE)
            .map(|c| (c.request, c.value))
            .collect();
        // start_rx: OFF then RX; stop_rx: OFF.
        assert_eq!(
            modes,
            vec![
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_OFF),
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_RX),
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_OFF),
            ]
        );
    }

    #[test]
    fn bulk_reads_use_c_parameters() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};

        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![BulkRead::Fill(0x11)]);
        let mut device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");
        device.start_rx(|_| true).expect("start_rx");
        // Wait (bounded) for the reader's first bulk call so an
        // instant stop can't win the race.
        let deadline = std::time::Instant::now() + core::time::Duration::from_secs(5);
        while transport.bulk_calls.lock().expect("mock lock").is_empty()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(core::time::Duration::from_millis(5));
        }
        device.stop_rx().expect("stop");

        // Every bulk read routed to the C bulk-IN endpoint. (buf_len and
        // timeout are recorded by the mock from the same BUFFER_SIZE /
        // EVENT_TIMEOUT constants, so asserting them here would be
        // self-referential; `constants_match_c` pins their values.)
        let bulk = transport.bulk_calls.lock().expect("mock lock").clone();
        assert!(!bulk.is_empty());
        for call in &bulk {
            assert_eq!(call.endpoint, BULK_ENDPOINT);
        }
    }

    /// Phase 1: after `set_packing(true)` + `start_rx` the reader's bulk
    /// transfers must carry the packed geometry (6144*24), not the
    /// unpacked 256 KiB buffer — and deliveries must come back at that
    /// size too.
    #[test]
    fn packed_stream_uses_packed_buffer_size() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};
        use std::sync::mpsc;
        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![BulkRead::Fill(0x33)]);
        let mut device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        // Drain the open-time samplerate-query traffic before
        // scripting the IN-with-status status byte set_packing needs.
        transport.take_recorded();
        transport.script_reads(vec![Ok(vec![0u8])]); // IN-with-status status byte
        device.set_sample_type(SampleType::Raw).expect("set type");
        device.set_packing(true).expect("packing");
        let (tx, rx) = mpsc::channel();
        device
            .start_rx(move |transfer| {
                let Samples::Raw(bytes) = transfer.samples else {
                    unreachable!("RAW stream delivers raw bytes");
                };
                tx.send((bytes[0], bytes.len(), transfer.dropped_samples))
                    .is_ok()
            })
            .expect("start_rx");

        // The three scripted buffers arrive in order, full-sized.
        let (first, len, dropped) = rx
            .recv_timeout(core::time::Duration::from_secs(5))
            .expect("block");
        assert_eq!(first, 0x33);
        assert_eq!(len, PACKED_BUFFER_SIZE);
        assert_eq!(dropped, 0);

        // Wait (bounded) for the reader's first bulk call so an
        // instant stop can't win the race.
        let deadline = std::time::Instant::now() + core::time::Duration::from_secs(5);
        while transport.bulk_calls.lock().expect("mock lock").is_empty()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(core::time::Duration::from_millis(5));
        }
        device.stop_rx().expect("stop");

        let bulk = transport.bulk_calls.lock().expect("mock lock").clone();
        assert!(!bulk.is_empty());
        for call in &bulk {
            assert_eq!(call.endpoint, BULK_ENDPOINT);
            assert_eq!(call.buf_len, PACKED_BUFFER_SIZE);
        }
    }

    #[test]
    fn disconnect_stops_stream() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};

        // Disconnected is the ONLY fatal transfer error: the device is
        // really gone, so the stream must stop.
        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![BulkRead::Fail(
            nusb::transfer::TransferError::Disconnected,
        )]);
        let mut device = Device::from_transport(transport as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");
        device.start_rx(|_| true).expect("start_rx");

        let deadline = std::time::Instant::now() + core::time::Duration::from_secs(5);
        while device.is_streaming() && std::time::Instant::now() < deadline {
            std::thread::sleep(core::time::Duration::from_millis(10));
        }
        assert!(!device.is_streaming(), "a disconnect must stop the stream");
        device.stop_rx().expect("stop");
    }

    #[test]
    fn unrecoverable_error_stops_stream() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};

        // A halted endpoint (Stall) or a bad argument (InvalidArgument)
        // fails identically on every resubmit — clear_halt cannot run with
        // transfers in flight — so retrying would spin the reader with no
        // samples forever. These must stop the stream (unlike a transient
        // Fault), letting the caller restart the source.
        for status in [
            nusb::transfer::TransferError::Stall,
            nusb::transfer::TransferError::InvalidArgument,
        ] {
            let transport = Arc::new(MockTransport::default());
            transport.script_bulk(vec![BulkRead::Fail(status)]);
            let mut device = Device::from_transport(transport as Arc<_>);
            device.set_sample_type(SampleType::Raw).expect("set type");
            device.start_rx(|_| true).expect("start_rx");

            let deadline = std::time::Instant::now() + core::time::Duration::from_secs(5);
            while device.is_streaming() && std::time::Instant::now() < deadline {
                std::thread::sleep(core::time::Duration::from_millis(10));
            }
            assert!(
                !device.is_streaming(),
                "{status:?} must stop the stream, not spin",
            );
            device.stop_rx().expect("stop");
        }
    }

    #[test]
    fn start_rx_clears_halt_between_receiver_modes() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport, wire};

        // C airspy_start_rx order: receiver OFF -> clear_halt -> receiver
        // RX -> threads; stop_rx sends OFF. The clear must land BETWEEN the
        // OFF and RX commands (issue #75, finding #3), not deferred into
        // the reader.
        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![BulkRead::Fill(0x11)]);
        let mut device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");
        device.start_rx(|_| true).expect("start_rx");
        device.stop_rx().expect("stop");

        let seq: Vec<(u8, u16)> = transport
            .take_recorded()
            .into_iter()
            .filter(|c| c.request == wire::RECEIVER_MODE || c.request == wire::CLEAR_HALT_MARKER)
            .map(|c| (c.request, c.value))
            .collect();
        assert_eq!(
            seq,
            vec![
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_OFF),
                (wire::CLEAR_HALT_MARKER, 0),
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_RX),
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_OFF),
            ]
        );
    }

    #[test]
    fn rfone_clears_halt_after_the_receiver_is_on() {
        use crate::commands::SampleType;
        use crate::device::{Device, Family};
        use crate::transport::mock::{BulkRead, MockTransport, wire};

        // hydrasdr_generic_start_rx order: receiver OFF -> receiver RX ->
        // clear_halt -> threads.
        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![BulkRead::Fill(0x11)]);
        let mut device = Device::from_transport_as(Family::Rfone, Arc::clone(&transport) as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");
        device.start_rx(|_| true).expect("start_rx");
        device.stop_rx().expect("stop");

        let seq: Vec<(u8, u16)> = transport
            .take_recorded()
            .into_iter()
            .filter(|c| c.request == wire::RECEIVER_MODE || c.request == wire::CLEAR_HALT_MARKER)
            .map(|c| (c.request, c.value))
            .collect();
        assert_eq!(
            seq,
            vec![
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_OFF),
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_RX),
                (wire::CLEAR_HALT_MARKER, 0),
                (wire::RECEIVER_MODE, wire::RECEIVER_MODE_OFF),
            ]
        );
    }

    #[test]
    fn persistent_fault_stops_stream() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::MAX_CONSECUTIVE_TRANSFER_ERRORS;
        use crate::transport::mock::{BulkRead, MockTransport};

        // A single Fault is tolerated (see transient_error_does_not_stop_stream),
        // but a PERSISTENT storm with no good transfer in between must stop
        // the reader rather than spin forever (issue #75, finding #5).
        // Script more consecutive faults than the cap so the stream stops
        // on its own.
        let transport = Arc::new(MockTransport::default());
        let faults: Vec<BulkRead> = (0..(MAX_CONSECUTIVE_TRANSFER_ERRORS + 5))
            .map(|_| BulkRead::Fail(nusb::transfer::TransferError::Fault))
            .collect();
        transport.script_bulk(faults);
        let mut device = Device::from_transport(transport as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");
        device.start_rx(|_| true).expect("start_rx");

        let deadline = std::time::Instant::now() + core::time::Duration::from_secs(10);
        while device.is_streaming() && std::time::Instant::now() < deadline {
            std::thread::sleep(core::time::Duration::from_millis(10));
        }
        assert!(
            !device.is_streaming(),
            "a persistent fault storm must stop the stream, not spin",
        );
        device.stop_rx().expect("stop");
    }

    #[test]
    fn transient_error_does_not_stop_stream() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};
        use std::sync::mpsc;

        // A transient Fault (the ~28s-under-load case from real
        // hardware) must NOT kill the stream: the faulted buffer is
        // dropped, and subsequent transfers keep delivering.
        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![
            BulkRead::Fail(nusb::transfer::TransferError::Fault),
            BulkRead::Fill(0xAB),
            BulkRead::Fill(0xCD),
        ]);
        let mut device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");

        let (tx, rx) = mpsc::channel();
        device
            .start_rx(move |transfer| {
                let Samples::Raw(bytes) = transfer.samples else {
                    unreachable!("RAW stream");
                };
                tx.send(bytes[0]).is_ok()
            })
            .expect("start_rx");

        // The fault is dropped (no delivery); the two fills after it
        // still arrive, proving the stream tolerated the error.
        for expected in [0xAB, 0xCD] {
            let first = rx
                .recv_timeout(core::time::Duration::from_secs(5))
                .expect("delivery after a tolerated transient error");
            assert_eq!(first, expected);
        }
        assert!(
            device.is_streaming(),
            "a transient transfer error must not stop the stream"
        );
        device.stop_rx().expect("stop");
        assert!(!device.is_streaming());
    }

    #[test]
    fn slow_consumer_drops_without_stopping_the_stream() {
        // The whole point of the transfer pool: a consumer that can't
        // keep up must cause DROPS, never a blocked reader or a stopped
        // stream. Drive a burst of full buffers through the mock stream
        // against a deliberately slow consumer and prove the stream
        // keeps delivering and stays alive under the backpressure.
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};
        use std::sync::mpsc;

        let transport = Arc::new(MockTransport::default());
        // Far more full buffers than the SampleQueue pool can hold at
        // once, so try_acquire_free must return None (→ drop) for many.
        let fills: Vec<BulkRead> = (0..40).map(|_| BulkRead::Fill(0x5A)).collect();
        transport.script_bulk(fills);
        let mut device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");

        let (tx, rx) = mpsc::channel();
        device
            .start_rx(move |transfer| {
                // Hold each buffer long enough that the producer outruns
                // the consumer and the queue backs up.
                std::thread::sleep(core::time::Duration::from_millis(5));
                let Samples::Raw(bytes) = transfer.samples else {
                    unreachable!("RAW stream");
                };
                tx.send(bytes.len()).is_ok()
            })
            .expect("start_rx");

        // Deliveries must keep arriving (a stopped stream would drop the
        // sender and end recv); receive several under backpressure.
        for _ in 0..3 {
            let len = rx
                .recv_timeout(core::time::Duration::from_secs(5))
                .expect("delivery under backpressure");
            assert_eq!(len, BUFFER_SIZE);
        }
        assert!(
            device.is_streaming(),
            "backpressure must drop buffers, never stop the stream"
        );
        device.stop_rx().expect("stop");
        assert!(!device.is_streaming());
    }

    #[test]
    fn short_bulk_read_delivers_nothing_but_keeps_streaming() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};
        use std::sync::mpsc;

        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![BulkRead::Short(100)]);
        let mut device = Device::from_transport(transport as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");

        let (tx, rx) = mpsc::channel::<()>();
        device
            .start_rx(move |_| tx.send(()).is_ok())
            .expect("start_rx");

        // A short transfer (actual_length != length) delivers nothing,
        // but is now treated as transient — the stream stays alive
        // instead of stopping.
        assert!(
            rx.recv_timeout(core::time::Duration::from_millis(500))
                .is_err(),
            "no callback for a short transfer"
        );
        assert!(
            device.is_streaming(),
            "a short transfer must not stop the stream"
        );
        device.stop_rx().expect("stop");
        assert!(!device.is_streaming());
    }

    #[test]
    fn polled_stream_delivers_on_the_callers_thread() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};

        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![BulkRead::Fill(0xAB), BulkRead::Fill(0xCD)]);
        let mut device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");
        // Not started: nothing to receive from.
        assert!(matches!(
            device.recv(Duration::from_millis(1), |_| ()),
            Err(Error::StreamingStopped)
        ));
        device.start_rx_polled().expect("start");
        assert!(matches!(device.start_rx_polled(), Err(Error::Busy)));

        // A borrowed, non-Send sink: what start_rx's callback can't take.
        let seen = std::cell::RefCell::new(Vec::new());
        for _ in 0..2 {
            let got = device
                .recv(Duration::from_secs(5), |t| {
                    let Samples::Raw(bytes) = t.samples else {
                        unreachable!("RAW stream");
                    };
                    seen.borrow_mut().push((bytes[0], bytes.len(), t.dropped_samples));
                })
                .expect("recv");
            assert!(got.is_some());
        }
        assert_eq!(
            *seen.borrow(),
            vec![(0xAB, BUFFER_SIZE, 0), (0xCD, BUFFER_SIZE, 0)]
        );
        // The script has run dry: a timeout, and the stream is still up.
        assert!(matches!(
            device.recv(Duration::from_millis(20), |_| ()),
            Ok(None)
        ));
        assert!(device.is_streaming());
        device.stop_rx().expect("stop");
        assert!(matches!(
            device.recv(Duration::from_millis(1), |_| ()),
            Err(Error::StreamingStopped)
        ));
    }

    #[test]
    fn polled_stream_reports_a_disconnect() {
        use crate::commands::SampleType;
        use crate::device::Device;
        use crate::transport::mock::{BulkRead, MockTransport};

        let transport = Arc::new(MockTransport::default());
        transport.script_bulk(vec![BulkRead::Fail(
            nusb::transfer::TransferError::Disconnected,
        )]);
        let mut device = Device::from_transport(transport as Arc<_>);
        device.set_sample_type(SampleType::Raw).expect("set type");
        device.start_rx_polled().expect("start");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match device.recv(Duration::from_millis(50), |_| ()) {
                Err(Error::StreamingStopped) => break,
                Ok(None) => assert!(Instant::now() < deadline, "disconnect never surfaced"),
                other => unreachable!("unexpected {other:?}"),
            }
        }
        // And it can be started again, like a callback stream.
        device.stop_rx().expect("stop");
    }

    #[test]
    fn pop_with_timeout_times_out_then_sees_shutdown() {
        let q = make_queue();
        assert!(matches!(
            q.pop_filled_timeout(Duration::from_millis(10)),
            Popped::TimedOut
        ));
        let buf = q.acquire_free().expect("pool");
        q.push_filled(buf);
        assert!(matches!(
            q.pop_filled_timeout(Duration::ZERO),
            Popped::Filled(_, 0)
        ));
        q.shutdown();
        assert!(matches!(
            q.pop_filled_timeout(Duration::from_secs(5)),
            Popped::Shutdown
        ));
    }
}
