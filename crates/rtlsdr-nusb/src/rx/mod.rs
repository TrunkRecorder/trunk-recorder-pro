//! Typed receive streams backed by one persistent USB queue.

pub(crate) mod processing;
mod queue;
mod receiver;

use crate::{
    Complex32,
    config::{F32Iq, RawIq, SampleMode},
    errors::Result,
    session::RxStreamClaim,
    usb::NusbTransport,
};
use nusb::MaybeFuture;
pub use queue::MAX_F32_IQ_SAMPLES_PER_TRANSFER;
use receiver::Receiver;
use std::time::Duration;

/// Counters collected during a receive stream.
///
/// These counters describe USB completions observed by the host. RTL-SDR bulk data
/// has no sequence number, so the driver cannot detect samples lost in the device
/// before a USB transfer completes (for example, when the application stops
/// polling long enough to exhaust the host transfer queue).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StreamingStats {
    /// Number of USB completions consumed by the driver.
    pub buffers_received: u64,
    /// Number of buffers successfully processed by the streaming layer.
    pub buffers_processed: u64,
    /// Number of observed completions not delivered because of an error or controlled restart.
    ///
    /// This does not include device-side loss that happened before USB completion.
    pub buffers_dropped: u64,
    /// Number of dropped buffers that belonged to the queue retained across a restart.
    ///
    /// This is a subset of [`StreamingStats::buffers_dropped`]. On backends without
    /// transfer cancellation, including WebUSB, consuming these old submissions may
    /// delay the first fresh block after restarting.
    pub buffers_discarded_on_restart: u64,
}

impl StreamingStats {
    pub(crate) fn accumulate(&mut self, other: Self) {
        self.buffers_received += other.buffers_received;
        self.buffers_processed += other.buffers_processed;
        self.buffers_dropped += other.buffers_dropped;
        self.buffers_discarded_on_restart += other.buffers_discarded_on_restart;
    }

    pub(crate) fn combined(mut self, other: Self) -> Self {
        self.accumulate(other);
        self
    }
}

/// Borrowed view of one raw receive block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleBlock<'a> {
    raw: &'a [u8],
    sample_count: usize,
    dropped_samples: u64,
}

impl<'a> SampleBlock<'a> {
    pub(crate) const fn new(raw: &'a [u8], sample_count: usize, dropped_samples: u64) -> Self {
        Self {
            raw,
            sample_count,
            dropped_samples,
        }
    }

    /// Raw USB bytes for this sample block.
    pub const fn raw_bytes(&self) -> &'a [u8] {
        self.raw
    }

    /// Sample count reported for this block.
    pub const fn sample_count(&self) -> usize {
        self.sample_count
    }

    /// Estimated sample count represented by USB buffers the driver discarded.
    ///
    /// RTL-SDR transfers have no sequence numbers, so this cannot include samples
    /// lost inside the device while the host transfer queue was exhausted.
    pub const fn dropped_samples(&self) -> u64 {
        self.dropped_samples
    }
}

/// Owned receive stream for sample mode `M`, defaulting to [`F32Iq`].
///
/// Create this stream with [`crate::Device::rx_stream`]. On native targets,
/// each operation may be waited or awaited. Both mechanisms use the same
/// persistent queue, including when used sequentially on one stream.
///
/// The stream keeps its device alive and can still start, stop, and restart
/// after the public device handle is dropped. Configured RF bias power remains
/// enabled until explicit device shutdown or the final owner is dropped.
#[must_use = "RX streams retain the device's exclusive stream claim until closed or dropped"]
pub struct RxStream<M: SampleMode = F32Iq> {
    inner: Receiver<NusbTransport, M>,
}

impl<M: SampleMode> RxStream<M> {
    pub(crate) fn new(claim: RxStreamClaim<NusbTransport>) -> Self {
        Self {
            inner: Receiver::new(claim),
        }
    }

    /// Counters accumulated across reads, stops and restarts.
    pub fn stats(&self) -> StreamingStats {
        self.inner.current_stats()
    }

    /// Start reception and the persistent USB transfer queue.
    ///
    /// A stream requiring cleanup must be stopped before restarting.
    ///
    /// Call [`MaybeFuture::wait`] for blocking operation on native targets, or
    /// await the returned operation for asynchronous operation.
    pub fn start(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.inner.start()
    }

    /// Stop reception and return accumulated streaming counters.
    ///
    /// This preserves the stream for restart, retains its exclusive claim, and
    /// leaves RF bias power unchanged.
    /// If this operation fails or is canceled after it starts, retry `stop`
    /// before reading or restarting.
    pub fn stop(&mut self) -> impl MaybeFuture<Output = Result<StreamingStats>> + '_ {
        self.inner.stop()
    }

    /// Consume the stream, stop reception, and return accumulated statistics.
    ///
    /// The returned operation owns the stream immediately, but retires its USB
    /// queue and attempts receiver-off only when polled or waited. Success
    /// releases the exclusive claim and the stream's device reference.
    ///
    /// Failure returns the original error; the stream cannot be retried. Failure
    /// or cancellation invokes best-effort drop cleanup, as does dropping an
    /// unpolled operation. WebUSB runs that fallback in the background and keeps
    /// the claim reserved until its attempt finishes.
    ///
    /// Native callers may wait or await regardless of earlier operations.
    /// WebUSB supports awaiting only.
    ///
    /// Close moves the stream even before the operation is polled:
    ///
    /// ```compile_fail,E0382
    /// # use rtlsdr_nusb::RxStream;
    /// fn example(mut rx: RxStream) {
    ///     let close = rx.close();
    ///     let _ = rx.start(); // rx was moved into close
    /// }
    /// ```
    ///
    /// A completed close cannot be called again:
    ///
    /// ```compile_fail,E0382
    /// # use rtlsdr_nusb::{RawIq, RxStream};
    /// async fn example(rx: RxStream<RawIq>) {
    ///     let _ = rx.close().await;
    ///     let _ = rx.close().await; // rx was consumed
    /// }
    /// ```
    pub fn close(self) -> impl MaybeFuture<Output = Result<StreamingStats>> + 'static {
        self.inner.close()
    }
}

impl RxStream<RawIq> {
    /// Read the next zero-copy raw I/Q USB block.
    ///
    /// The returned block borrows one buffer from the fixed transfer pool. Its
    /// buffer is resubmitted on the next call. For blocking operation, `timeout`
    /// bounds the wait and [`None`] waits indefinitely. The timeout is ignored
    /// when this operation is awaited, which waits for the next USB completion.
    /// The steady-state asynchronous path uses a concrete future without a
    /// per-call future allocation; restarting a failed stream may rebuild its USB queue.
    pub fn next_block(
        &mut self,
        timeout: Option<Duration>,
    ) -> impl MaybeFuture<Output = Result<Option<SampleBlock<'_>>>> + '_ {
        self.inner.next_block(timeout)
    }
}

impl RxStream<F32Iq> {
    /// Convert samples directly into the caller-provided complex output slice.
    ///
    /// Both blocking and asynchronous reads drain buffered samples or process at
    /// most one new usable USB completion. The returned count may be smaller
    /// than `out.len()`; loop when a full slice is required. An empty slice
    /// returns zero without waiting.
    ///
    /// `timeout` bounds blocking waits; [`None`] waits indefinitely. Awaited
    /// reads ignore this argument and can be canceled by an external timeout.
    /// Steady-state reads do not allocate a future or an output scratch buffer.
    pub fn read<'a>(
        &'a mut self,
        out: &'a mut [Complex32],
        timeout: Option<Duration>,
    ) -> impl MaybeFuture<Output = Result<usize>> + 'a {
        self.inner.read(out, timeout)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
