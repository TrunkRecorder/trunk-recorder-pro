//! Persistent receive buffers, completion validation, and restart recycling.

use super::{SampleBlock, StreamingStats};
use crate::{
    errors::{Error, Result},
    protocol::TRANSFER_COUNT,
    usb::{BulkInBackend, BulkInCompletion},
};
use nusb::MaybeFuture;
use std::task::{Context, Poll};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

pub(crate) const DEFAULT_BUFFER_SIZE: usize = 262_144;
/// Maximum number of unpacked complex `F32Iq` samples produced by one USB transfer.
///
/// Short USB transfers may contain fewer samples.
pub const MAX_F32_IQ_SAMPLES_PER_TRANSFER: usize = DEFAULT_BUFFER_SIZE / 2;

pub(crate) struct Queue<B: BulkInBackend> {
    pub(super) bulk_in: Option<B>,
    buffers: Vec<B::Buffer>,
    pub(super) initialized: bool,
    pub(super) current: Option<B::Buffer>,
    pub(super) current_len: usize,
    pub(super) stats: StreamingStats,
    discard_remaining: usize,
    dropped_samples: u64,
}

impl<B: BulkInBackend> Queue<B> {
    pub(crate) fn new(bulk_in: B) -> Self {
        let missing = TRANSFER_COUNT.saturating_sub(bulk_in.pending());
        let len = DEFAULT_BUFFER_SIZE;
        let buffers = (0..missing).map(|_| bulk_in.allocate(len)).collect();
        Self {
            bulk_in: Some(bulk_in),
            buffers,
            initialized: false,
            current: None,
            current_len: 0,
            stats: StreamingStats::default(),
            discard_remaining: 0,
            dropped_samples: 0,
        }
    }

    pub(crate) fn initialize(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.bulk_in
            .as_mut()
            .expect("new queue has an endpoint")
            .clear_halt()
            .map(move |result| {
                result?;
                for buffer in self.buffers.drain(..) {
                    self.bulk_in.as_mut().expect("new endpoint").submit(buffer);
                }
                self.initialized = true;
                Ok(())
            })
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
    pub(crate) fn discard_current(&mut self) {
        if self.current.is_some() {
            self.stats.buffers_dropped += 1;
            self.dropped_samples += (self.current_len / 2) as u64;
        }
        self.release_current();
    }
    pub(crate) fn pause(&mut self) {
        self.discard_current();
        if let Some(bulk_in) = &mut self.bulk_in {
            self.discard_remaining = bulk_in.pending();
            bulk_in.cancel_all();
        }
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
            self.dropped_samples += (completion.actual_len / 2) as u64;
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
            if self.accept(completion)? {
                return Ok(true);
            }
        }
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
            self.dropped_samples,
        ))
    }
}
impl<B: BulkInBackend> Drop for Queue<B> {
    fn drop(&mut self) {
        self.close();
    }
}
