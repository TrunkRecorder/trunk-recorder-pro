//! Persistent receive buffers, completion validation, and restart recycling.

use super::{SampleBlock, StreamingStats};
use crate::{
    errors::{Error, Result},
    usb::{BulkInBackend, BulkInCompletion},
};
use nusb::MaybeFuture;
use std::task::{Context, Poll};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

pub(crate) const DEFAULT_BUFFER_SIZE: usize = 16_384;
/// Maximum number of unpacked complex `F32Iq` samples produced by one USB transfer
/// with the default [`RxBuffers`].
///
/// Short USB transfers may contain fewer samples, and smaller buffers produce fewer.
pub const MAX_F32_IQ_SAMPLES_PER_TRANSFER: usize = DEFAULT_BUFFER_SIZE / 2;

/// The receive stream's USB transfers: how big each is and how many are kept in flight.
///
/// The default is 128 transfers of 16384 bytes (2 MiB, ~440 ms at 2.4 MS/s).
/// The chip's FIFO holds only ~3.7 KB, and a control request served while
/// samples flow makes it end each waiting transfer early: with few transfers
/// queued, one is soon missing and the FIFO overflows (sdr-rtl findings 16.7,
/// 16.8: 16 deep lost blocks on a change, 128 none). Small transfers also let
/// the host controller interleave several receivers on one USB 2.0 bus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RxBuffers {
    /// Bytes per transfer: a nonzero multiple of 512 (the bulk packet size), at most 16 MiB.
    pub len: usize,
    /// Transfers kept submitted: 1..=256.
    pub count: usize,
}
impl Default for RxBuffers {
    fn default() -> Self {
        Self {
            len: DEFAULT_BUFFER_SIZE,
            count: 128,
        }
    }
}
impl RxBuffers {
    /// Check `len` and `count` against the limits above.
    pub fn validate(&self) -> Result<()> {
        if self.len == 0 || self.len % 512 != 0 || self.len > 16 << 20 {
            return Err(Error::invalid_config(
                "buffer length",
                "must be a nonzero multiple of 512 bytes, at most 16 MiB",
            ));
        }
        if !(1..=256).contains(&self.count) {
            return Err(Error::invalid_config("buffer count", "must be in 1..=256"));
        }
        Ok(())
    }
}

pub(crate) struct Queue<B: BulkInBackend> {
    pub(super) bulk_in: Option<B>,
    buffers: Vec<B::Buffer>,
    pub(super) initialized: bool,
    pub(super) current: Option<B::Buffer>,
    pub(super) current_len: usize,
    pub(super) stats: StreamingStats,
    discard_remaining: usize,
    /// Samples lost since the last block handed out, and before the current one.
    dropped_pending: u64,
    dropped_before: u64,
    /// Bulk stalls cleared in place lately, for giving up on a loop of them.
    #[cfg(not(target_arch = "wasm32"))]
    stalls: Vec<std::time::Instant>,
}

/// How long a stop waits for each cancelled transfer to come back.
#[cfg(not(target_arch = "wasm32"))]
const CANCEL_WAIT: Duration = Duration::from_millis(200);
/// More bulk stalls than this within [`STALL_WINDOW`] end the stream (and
/// so reopen the device) rather than be cleared again.
#[cfg(not(target_arch = "wasm32"))]
const STALLS_MAX: usize = 3;
#[cfg(not(target_arch = "wasm32"))]
const STALL_WINDOW: Duration = Duration::from_secs(10);

impl<B: BulkInBackend> Queue<B> {
    pub(crate) fn new(bulk_in: B, sizes: RxBuffers) -> Self {
        let missing = sizes.count.saturating_sub(bulk_in.pending());
        let len = sizes.len;
        let buffers = (0..missing).map(|_| bulk_in.allocate(len)).collect();
        Self {
            bulk_in: Some(bulk_in),
            buffers,
            initialized: false,
            current: None,
            current_len: 0,
            stats: StreamingStats::default(),
            discard_remaining: 0,
            dropped_pending: 0,
            dropped_before: 0,
            #[cfg(not(target_arch = "wasm32"))]
            stalls: Vec::new(),
        }
    }

    /// Queue every transfer not in flight (all of a new queue's, a stopped
    /// one's), the endpoint's halt cleared first when none is.
    pub(crate) fn initialize(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let bulk_in = self.bulk_in.as_mut().expect("queue has an endpoint");
        let clear = if bulk_in.pending() == 0 {
            crate::maybe_future::Either::left(bulk_in.clear_halt())
        } else {
            // (Not allowed with transfers pending: a WebUSB stop leaves them.)
            crate::maybe_future::Either::right(crate::maybe_future::ready(Ok(())))
        };
        clear.map(move |result| {
            result?;
            for buffer in self.buffers.drain(..) {
                self.bulk_in.as_mut().expect("queue has an endpoint").submit(buffer);
            }
            self.initialized = true;
            Ok(())
        })
    }
    /// Samples lost outside the queue (the chip's own count), told with the next block.
    pub(crate) fn add_dropped(&mut self, n: u64) {
        self.dropped_pending += n;
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.bulk_in.is_none()
    }
    pub(crate) fn release_current(&mut self) {
        if let Some(buffer) = self.current.take()
            && let Some(bulk_in) = &mut self.bulk_in
        {
            bulk_in.submit(buffer);
        }
        self.current_len = 0;
    }
    /// Every transfer cancelled. Natively each comes back at once and what it
    /// held is counted as dropped, its buffer kept for [`Queue::initialize`];
    /// WebUSB can't cancel, so those are discarded as they complete.
    pub(crate) fn pause(&mut self) {
        // (Kept for the restart, not resubmitted to a stopped device.)
        if let Some(buffer) = self.current.take() {
            self.stats.buffers_dropped += 1;
            self.dropped_pending += (self.current_len / 2) as u64;
            self.buffers.push(buffer);
            self.current_len = 0;
        }
        let Some(bulk_in) = &mut self.bulk_in else { return };
        bulk_in.cancel_all();
        #[cfg(not(target_arch = "wasm32"))]
        while bulk_in.pending() > 0 {
            let Some(c) = bulk_in.wait_next_complete(CANCEL_WAIT) else { break };
            self.stats.buffers_received += 1;
            if c.actual_len > 0 {
                self.stats.buffers_dropped += 1;
                self.stats.buffers_discarded_on_restart += 1;
                self.dropped_pending += (c.actual_len / 2) as u64;
            }
            self.buffers.push(c.buffer);
        }
        self.discard_remaining = bulk_in.pending();
    }
    pub(crate) fn close(&mut self) -> StreamingStats {
        if let Some(mut bulk_in) = self.bulk_in.take() {
            bulk_in.cancel_all();
        }
        self.current = None;
        self.buffers.clear();
        self.current_len = 0;
        self.stats
    }

    /// Return true for a usable completion, false for a pre-restart submission.
    fn accept(&mut self, completion: BulkInCompletion<B::Buffer>) -> Result<bool> {
        self.stats.buffers_received += 1;
        if self.discard_remaining != 0 {
            self.discard_remaining -= 1;
            self.stats.buffers_dropped += 1;
            self.stats.buffers_discarded_on_restart += 1;
            self.dropped_pending += (completion.actual_len / 2) as u64;
            self.bulk_in
                .as_mut()
                .expect("live endpoint")
                .submit(completion.buffer);
            return Ok(false);
        }
        let result = completion.status.and_then(|_| {
            if completion.actual_len == 0
                || completion.actual_len % 2 != 0
                || completion.actual_len > completion.buffer.len()
            {
                Err(Error::protocol(
                    "receive transfer",
                    "completed with an unexpected length",
                ))
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            self.stats.buffers_dropped += 1;
            self.close();
            return Err(error);
        }
        self.current = Some(completion.buffer);
        self.current_len = completion.actual_len;
        self.dropped_before = std::mem::take(&mut self.dropped_pending);
        self.stats.buffers_processed += 1;
        Ok(true)
    }

    pub(crate) fn poll_fill(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        if self.current.is_some() {
            return Poll::Ready(Ok(()));
        }
        loop {
            let bulk_in = self
                .bulk_in
                .as_mut()
                .ok_or(Error::stream_closed("RX queue is closed"))?;
            let completion = match bulk_in.poll_next_complete(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(c) => c,
            };
            if self.accept(completion)? {
                return Poll::Ready(Ok(()));
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn wait_fill(&mut self, timeout: Duration) -> Result<bool> {
        if self.current.is_some() {
            return Ok(true);
        }
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let bulk_in = self
                .bulk_in
                .as_mut()
                .ok_or(Error::stream_closed("RX queue is closed"))?;
            let remaining =
                deadline.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
            let Some(completion) = bulk_in.wait_next_complete(remaining) else {
                return Ok(false);
            };
            if self.discard_remaining == 0
                && completion.status.as_ref().is_err_and(crate::protocol::stalled)
            {
                self.clear_stall(completion)?;
                continue;
            }
            if self.accept(completion)? {
                return Ok(true);
            }
        }
    }

    /// A bulk transfer the endpoint refused (STALL): every transfer
    /// cancelled and what they held counted as dropped, the halt cleared and
    /// the queue refilled — the stream goes on (sdr-core's recovery). A loop
    /// of them ends it.
    #[cfg(not(target_arch = "wasm32"))]
    fn clear_stall(&mut self, first: BulkInCompletion<B::Buffer>) -> Result<()> {
        let now = std::time::Instant::now();
        self.stalls.retain(|t| now.duration_since(*t) < STALL_WINDOW);
        self.stalls.push(now);
        if self.stalls.len() > STALLS_MAX {
            let status = first.status;
            self.stats.buffers_dropped += 1;
            self.close();
            return status;
        }
        self.stats.stall_recoveries += 1;
        self.stats.buffers_received += 1;
        self.buffers.push(first.buffer);
        let bulk_in = self.bulk_in.as_mut().expect("live endpoint");
        bulk_in.cancel_all();
        while bulk_in.pending() > 0 {
            let Some(c) = bulk_in.wait_next_complete(CANCEL_WAIT) else { break };
            self.stats.buffers_received += 1;
            if c.status.is_ok() || c.actual_len > 0 {
                self.stats.buffers_dropped += 1;
                self.dropped_pending += (c.actual_len / 2) as u64;
            }
            self.buffers.push(c.buffer);
        }
        if bulk_in.pending() > 0 {
            self.close();
            return Err(Error::stream_closed("a stalled USB transfer didn't come back"));
        }
        bulk_in.clear_halt().wait()?;
        for buffer in self.buffers.drain(..) {
            bulk_in.submit(buffer);
        }
        Ok(())
    }

    pub(crate) fn block(&self) -> Result<SampleBlock<'_>> {
        let raw = self
            .current
            .as_ref()
            .ok_or(Error::stream_closed("RX queue has no current block"))?;
        let count = self.current_len / 2;
        Ok(SampleBlock::new(
            &raw[..self.current_len],
            count,
            self.dropped_before,
        ))
    }
}
impl<B: BulkInBackend> Drop for Queue<B> {
    fn drop(&mut self) {
        self.close();
    }
}
