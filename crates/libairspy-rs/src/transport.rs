//! The USB transport seam: every wire operation the driver performs
//! goes through [`UsbTransport`], implemented by [`NusbTransport`]
//! (wrapping a claimed `nusb::Interface`) in production and by
//! [`mock::MockTransport`] in tests — giving the control surface and
//! streaming engine transport-boundary tests without hardware.
//!
//! ## Streaming model
//!
//! Unlike the previous rusb backend — which exposed only synchronous
//! bulk reads and so kept a single transfer in flight — nusb's
//! [`Endpoint`](nusb::Endpoint) lets the reader keep
//! [`TRANSFER_COUNT`](crate::stream::TRANSFER_COUNT) bulk transfers
//! queued at once, exactly like `airspy.c`'s `transfer_count = 16`
//! asynchronous URB pool. The whole streaming loop therefore lives
//! behind the transport, in [`UsbTransport::run_bulk_stream`], so the
//! reader thread in `stream.rs` just calls it and the mock can drive a
//! scripted sequence through the same seam.

use core::time::Duration;
use std::sync::atomic::Ordering;

use nusb::MaybeFuture as _;
use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, In, Recipient, TransferError};

use crate::error::Result;
use crate::stream::{EVENT_TIMEOUT, StreamShared, TRANSFER_COUNT};

/// Stop the stream after this many consecutive non-fatal transfer errors
/// with no successful transfer in between. A single transient fault still
/// resets on the next good buffer; this only fires on a *persistent*
/// Fault/short-transfer storm, so a stuck endpoint can't spin the reader
/// forever. Hardware-tunable (see issue #75).
pub(crate) const MAX_CONSECUTIVE_TRANSFER_ERRORS: u64 = 64;

/// Backoff before resubmitting after a non-fatal transfer error, so an
/// error storm doesn't busy-spin the CPU. Short enough that a genuine
/// isolated transient is imperceptible.
const TRANSFER_ERROR_BACKOFF: Duration = Duration::from_millis(2);

/// The USB operations `airspy.c` performs against an open device.
///
/// The control methods return the driver [`Result`] directly (the nusb
/// error types are mapped at the boundary); [`run_bulk_stream`] owns the
/// entire nusb transfer-pool loop.
///
/// [`run_bulk_stream`]: UsbTransport::run_bulk_stream
pub(crate) trait UsbTransport: Send + Sync + std::fmt::Debug {
    /// `libusb_control_transfer`, host-to-device. Returns the number of
    /// bytes accepted by the device.
    fn write_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &[u8],
        timeout: Duration,
    ) -> Result<usize>;

    /// `libusb_control_transfer`, device-to-host. Returns the number of
    /// bytes read into `buf`.
    fn read_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<usize>;

    /// `libusb_clear_halt` on the given endpoint (`airspy_set_samplerate`
    /// clears the bulk-IN halt before its request; C ignores the result).
    fn clear_halt(&self, endpoint: u8) -> Result<()>;

    /// Run the whole bulk-streaming loop until `!shared.running()`,
    /// keeping [`TRANSFER_COUNT`] transfers in flight and pushing each
    /// completed `buffer_size`-byte buffer into `shared.queue`. On
    /// return, `streaming` is cleared and the queue is shut down.
    fn run_bulk_stream(&self, endpoint: u8, shared: &StreamShared, buffer_size: usize);

    /// `libusb_release_interface` (the close half of `airspy_open_exit`).
    /// nusb releases a claimed interface when the `Interface` drops, so
    /// production needs no explicit call; the seam is kept for the mock.
    fn release_interface(&self, iface: u8) -> Result<()>;
}

/// Production transport: a claimed `nusb::Interface`.
///
/// `nusb::Interface` is `Send + Sync` and cheap to clone (an `Arc`
/// internally), but it is not `Debug`, so the impl below is hand-rolled.
pub(crate) struct NusbTransport {
    interface: nusb::Interface,
}

impl NusbTransport {
    /// Wrap a claimed `nusb::Interface` in the production transport.
    pub(crate) fn new(interface: nusb::Interface) -> Self {
        Self { interface }
    }
}

impl std::fmt::Debug for NusbTransport {
    /// Stable, address-free debug output (the `nusb::Interface` Debug
    /// would print internal pointers).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NusbTransport").finish_non_exhaustive()
    }
}

impl UsbTransport for NusbTransport {
    /// `libusb_control_transfer`, host-to-device. Returns the number of
    /// bytes accepted by the device.
    fn write_control(
        &self,
        _request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &[u8],
        timeout: Duration,
    ) -> Result<usize> {
        // The driver only ever issues vendor requests to the device
        // recipient; direction is carried by control_out vs control_in,
        // so the `request_type` byte (0x40) is redundant here.
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    data: buf,
                },
                timeout,
            )
            .wait()?;
        // libusb/rusb report the byte count moved; a successful nusb
        // control_out transferred the whole payload.
        Ok(buf.len())
    }

    /// `libusb_control_transfer`, device-to-host. Returns the number of
    /// bytes read into `buf`.
    fn read_control(
        &self,
        _request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<usize> {
        let length = u16::try_from(buf.len()).unwrap_or(u16::MAX);
        let data = self
            .interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    length,
                },
                timeout,
            )
            .wait()?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    fn clear_halt(&self, endpoint: u8) -> Result<()> {
        // libusb_clear_halt does TWO things: the standard
        // CLEAR_FEATURE(ENDPOINT_HALT) control request AND a reset of the
        // host-side data toggle. nusb's Endpoint::clear_halt does both; a
        // bare control_out would send the request but leave the host
        // toggle out of sync, so after a halt reset following bulk traffic
        // the host/device toggles diverge and subsequent bulk transfers
        // fail (issue #75, finding #4). We claim the endpoint transiently
        // and drop it immediately — callers ensure the streaming reader is
        // NOT holding it: `set_samplerate` runs while stopped, and
        // `start_rx` clears between the receiver-mode OFF/RX commands,
        // before the reader thread is spawned.
        let mut ep = self.interface.endpoint::<Bulk, In>(endpoint)?;
        ep.clear_halt().wait()?;
        Ok(())
    }

    fn run_bulk_stream(&self, endpoint: u8, shared: &StreamShared, buffer_size: usize) {
        run_bulk_stream_impl(&self.interface, endpoint, shared, buffer_size);
    }

    fn release_interface(&self, _iface: u8) -> Result<()> {
        // nusb releases the claimed interface when the `Interface` (held
        // by this transport) drops. No explicit release call exists.
        Ok(())
    }
}

/// Running tally of non-fatal transfer errors for one streaming session.
#[derive(Default)]
struct ErrorRun {
    /// Every tolerated error, for the end-of-stream summary log.
    total: u64,
    /// Errors since the last successful full transfer. Resets on any good
    /// delivery; bounds a persistent storm via
    /// [`MAX_CONSECUTIVE_TRANSFER_ERRORS`].
    consecutive: u64,
}

/// Record one non-fatal transfer error and decide whether to keep going.
/// Returns `false` (stop the stream) once the consecutive run exceeds
/// [`MAX_CONSECUTIVE_TRANSFER_ERRORS`]; otherwise backs off briefly and
/// returns `true` so the caller resubmits. A single fault is invisible —
/// the run resets on the next successful transfer.
fn record_transient(run: &mut ErrorRun) -> bool {
    run.total = run.total.saturating_add(1);
    run.consecutive = run.consecutive.saturating_add(1);
    if run.consecutive > MAX_CONSECUTIVE_TRANSFER_ERRORS {
        log::warn!(
            "airspy: {} consecutive bulk transfer errors; stopping stream",
            run.consecutive
        );
        return false;
    }
    std::thread::sleep(TRANSFER_ERROR_BACKOFF);
    true
}

/// Resubmit a transient failure's buffer and keep streaming — unless the
/// consecutive-error run has hit the cap, in which case stop (return
/// `false`) and let the buffer drop with the outstanding pool.
fn resubmit_or_stop(
    ep: &mut nusb::Endpoint<Bulk, In>,
    buffer: nusb::transfer::Buffer,
    run: &mut ErrorRun,
) -> bool {
    if record_transient(run) {
        ep.submit(buffer);
        true
    } else {
        false
    }
}

/// The nusb transfer-pool reader — `airspy.c`'s `transfer_threadproc` +
/// `airspy_libusb_transfer_callback`, keeping [`TRANSFER_COUNT`]
/// transfers in flight (sized by the caller from the packing mode) and
/// resubmitting each on completion so the pipe never starves.
fn run_bulk_stream_impl(
    interface: &nusb::Interface,
    endpoint: u8,
    shared: &StreamShared,
    buffer_size: usize,
) {
    // Open the bulk-IN endpoint. A failure here can't stream anything,
    // so clear streaming and shut the queue down before returning.
    let mut ep = match interface.endpoint::<Bulk, In>(endpoint) {
        Ok(ep) => ep,
        Err(err) => {
            log::warn!("airspy: failed to open bulk endpoint ({err}); stream not started");
            shared.streaming.store(false, Ordering::SeqCst);
            shared.queue.shutdown();
            return;
        }
    };

    // The endpoint halt is cleared synchronously in `start_rx`, between
    // the receiver-mode OFF and RX commands — matching airspy.c's
    // `airspy_start_rx` order (OFF → clear_halt → RX → threads) and
    // resetting the host data toggle via nusb's Endpoint::clear_halt
    // before the reader claims the endpoint (issue #75, finding #3).

    // Prime the pool: TRANSFER_COUNT outstanding transfers, as in
    // airspy.c's create_transfers loop.
    for _ in 0..TRANSFER_COUNT {
        let buf = ep.allocate(buffer_size);
        ep.submit(buf);
    }

    // With 16 transfers in flight the pipe stays fed, but a momentary
    // scheduling gap under heavy CPU/GPU load can still fault a single
    // transfer. A robust SDR reader TOLERATES such transient transfer
    // errors — dropping only that buffer's data and resubmitting it —
    // and stops only on a real device disconnect or an unrecoverable
    // endpoint error (Stall/InvalidArgument). (This is a deliberate
    // improvement over airspyone_host, which stops on any non-completed
    // transfer.)
    let mut run = ErrorRun::default();

    while shared.running() {
        // `None` is a client-side timeout: the transfer stays pending,
        // and the C event loop tolerates it and keeps polling.
        let Some(c) = ep.wait_next_complete(EVENT_TIMEOUT) else {
            continue;
        };
        if !handle_bulk_completion(&mut ep, shared, c, &mut run, buffer_size) {
            break;
        }
    }

    if run.total > 0 {
        log::info!("airspy: stream tolerated {} transient transfer errors", run.total);
    }

    // Teardown: cancel outstanding transfers and drain their completions
    // so the endpoint and its buffers release cleanly before the
    // Endpoint drops. Cancelled completions in this drain phase are
    // expected and deliberately NOT counted as transient errors.
    ep.cancel_all();
    while ep.pending() > 0 {
        if ep.wait_next_complete(EVENT_TIMEOUT).is_none() {
            break;
        }
    }
    shared.streaming.store(false, Ordering::SeqCst);
    shared.queue.shutdown();
}

/// Handle one completed bulk transfer: hand a full `buffer_size` buffer to
/// the consumer without blocking (drop-and-count if it's behind) and
/// resubmit; tolerate transient errors and short transfers (resubmit and
/// count); stop on a real device disconnect or an unrecoverable endpoint
/// error (Stall/InvalidArgument). Returns `false` when the stream should
/// stop.
fn handle_bulk_completion(
    ep: &mut nusb::Endpoint<Bulk, In>,
    shared: &StreamShared,
    c: nusb::transfer::Completion,
    run: &mut ErrorRun,
    buffer_size: usize,
) -> bool {
    match c.status {
        // A complete `buffer_size` transfer: hand it to the consumer WITHOUT
        // blocking — if the consumer is behind, drop it (SampleQueue counts
        // the drop) rather than stall the pool. Then resubmit the buffer.
        // A good transfer clears the consecutive-error run.
        Ok(()) if c.buffer.len() == buffer_size => {
            run.consecutive = 0;
            if let Some(mut qbuf) = shared.queue.try_acquire_free() {
                qbuf.copy_from_slice(&c.buffer[..]);
                shared.queue.push_filled(qbuf);
            }
            ep.submit(c.buffer);
            true
        }
        // The device is really gone: fatal.
        Err(TransferError::Disconnected) => {
            log::warn!("airspy: bulk endpoint disconnected; stopping stream");
            false
        }
        // A halted endpoint (Stall) or a bad argument (InvalidArgument)
        // fails identically on every resubmit — clear_halt cannot run with
        // other transfers in flight — so retrying would spin the reader
        // with no samples forever. Stop cleanly and let the caller restart
        // the source.
        Err(err @ (TransferError::Stall | TransferError::InvalidArgument)) => {
            log::warn!("airspy: unrecoverable bulk transfer error ({err}); stopping stream");
            false
        }
        // Everything else — a Fault, a spurious Cancelled while streaming,
        // or an Unknown OS error — is transient. Drop that buffer's data,
        // do NOT push it; resubmit and keep going, unless the consecutive
        // run says the endpoint is stuck.
        Err(err) => {
            log::debug!("airspy: transient bulk transfer error ({err}); resubmitting");
            resubmit_or_stop(ep, c.buffer, run)
        }
        Ok(()) => {
            log::debug!(
                "airspy: transient short bulk transfer ({} of {buffer_size} bytes); resubmitting",
                c.buffer.len()
            );
            resubmit_or_stop(ep, c.buffer, run)
        }
    }
}

#[cfg(test)]
pub(crate) mod mock {
    use super::UsbTransport;
    use crate::error::{Error, Result};
    use crate::stream::{EVENT_TIMEOUT, StreamShared};
    use nusb::transfer::TransferError;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    /// C-defined wire expectations for boundary tests — transcribed
    /// independently from the Rust enums (`commands.rs`) so a
    /// Rust-to-wire mismatch cannot hide. Sources: the
    /// `airspy_vendor_request` enum (`airspy_commands.h`), the
    /// `receiver_mode_t` enum, `LIBUSB_CTRL_TIMEOUT_MS` (airspy.c),
    /// and the vendor `bmRequestType` compositions.
    pub(crate) mod wire {
        use std::time::Duration;

        /// `AIRSPY_RECEIVER_MODE = 1` (`airspy_commands.h`).
        pub(crate) const RECEIVER_MODE: u8 = 1;
        /// `RECEIVER_MODE_OFF = 0` (`airspy_commands.h`).
        pub(crate) const RECEIVER_MODE_OFF: u16 = 0;
        /// `RECEIVER_MODE_RX = 1` (`airspy_commands.h`).
        pub(crate) const RECEIVER_MODE_RX: u16 = 1;
        /// `AIRSPY_SET_FREQ = 13` (`airspy_commands.h`).
        pub(crate) const SET_FREQ: u8 = 13;
        /// `AIRSPY_BOARD_ID_READ = 9` (`airspy_commands.h`).
        pub(crate) const BOARD_ID_READ: u8 = 9;
        /// `AIRSPY_GET_SAMPLERATES = 25` (`airspy_commands.h`).
        pub(crate) const GET_SAMPLERATES: u8 = 25;
        /// `AIRSPY_SET_SAMPLERATE = 12` (`airspy_commands.h`).
        pub(crate) const SET_SAMPLERATE: u8 = 12;
        /// `AIRSPY_SET_LNA_GAIN = 14` (`airspy_commands.h`).
        pub(crate) const SET_LNA_GAIN: u8 = 14;
        /// `AIRSPY_SET_MIXER_GAIN = 15` (`airspy_commands.h`).
        pub(crate) const SET_MIXER_GAIN: u8 = 15;
        /// `AIRSPY_SET_VGA_GAIN = 16` (`airspy_commands.h`).
        pub(crate) const SET_VGA_GAIN: u8 = 16;
        /// `AIRSPY_SET_LNA_AGC = 17` (`airspy_commands.h`).
        pub(crate) const SET_LNA_AGC: u8 = 17;
        /// `AIRSPY_SET_MIXER_AGC = 18` (`airspy_commands.h`).
        pub(crate) const SET_MIXER_AGC: u8 = 18;
        /// `AIRSPY_GPIO_WRITE = 21` (`airspy_commands.h`).
        pub(crate) const GPIO_WRITE: u8 = 21;
        /// `AIRSPY_SET_PACKING = 26` (`airspy_commands.h`).
        pub(crate) const SET_PACKING: u8 = 26;
        /// `AIRSPY_SI5351C_WRITE = 2` (`airspy_commands.h`).
        pub(crate) const SI5351C_WRITE: u8 = 2;
        /// `AIRSPY_SI5351C_READ = 3` (`airspy_commands.h`).
        pub(crate) const SI5351C_READ: u8 = 3;
        /// `AIRSPY_R820T_WRITE = 4` (`airspy_commands.h`).
        pub(crate) const R820T_WRITE: u8 = 4;
        /// `AIRSPY_R820T_READ = 5` (`airspy_commands.h`).
        pub(crate) const R820T_READ: u8 = 5;
        /// `AIRSPY_SPIFLASH_ERASE = 6` (`airspy_commands.h`).
        pub(crate) const SPIFLASH_ERASE: u8 = 6;
        /// `AIRSPY_SPIFLASH_WRITE = 7` (`airspy_commands.h`).
        pub(crate) const SPIFLASH_WRITE: u8 = 7;
        /// `AIRSPY_SPIFLASH_READ = 8` (`airspy_commands.h`).
        pub(crate) const SPIFLASH_READ: u8 = 8;
        /// `AIRSPY_GPIO_READ = 22` (`airspy_commands.h`).
        pub(crate) const GPIO_READ: u8 = 22;
        /// `AIRSPY_GPIODIR_WRITE = 23` (`airspy_commands.h`).
        pub(crate) const GPIODIR_WRITE: u8 = 23;
        /// `AIRSPY_GPIODIR_READ = 24` (`airspy_commands.h`).
        pub(crate) const GPIODIR_READ: u8 = 24;
        /// `AIRSPY_SPIFLASH_ERASE_SECTOR = 27` (`airspy_commands.h`).
        pub(crate) const SPIFLASH_ERASE_SECTOR: u8 = 27;
        /// OUT|VENDOR|DEVICE (airspy.c's host-to-device transfers).
        pub(crate) const VENDOR_OUT: u8 = 0x40;
        /// IN|VENDOR|DEVICE (airspy.c's device-to-host transfers).
        pub(crate) const VENDOR_IN: u8 = 0xC0;
        /// `LIBUSB_CTRL_TIMEOUT_MS = 500` (airspy.c).
        pub(crate) const CTRL_TIMEOUT: Duration = Duration::from_millis(500);
        /// The samplerate fallback pair `airspy_open_init` installs
        /// when the firmware query fails: `{10000000, 2500000}`
        /// (airspy.c).
        pub(crate) const FALLBACK_SAMPLERATES: [u32; 2] = [10_000_000, 2_500_000];
        /// Test-only sentinel recorded by the mock's `clear_halt` so its
        /// ordering relative to the receiver-mode control calls is
        /// observable. Not a real vendor request (0xFE is unused by
        /// `airspy_commands.h`).
        pub(crate) const CLEAR_HALT_MARKER: u8 = 0xFE;
    }

    /// Poll delay served while the bulk script is exhausted — a
    /// mock-only pacing value with no C equivalent (the real device
    /// blocks in nusb's `wait_next_complete` instead).
    const EXHAUSTED_BULK_POLL: Duration = Duration::from_millis(5);

    /// One recorded (simulated) bulk transfer's parameters.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct BulkCall {
        pub(crate) endpoint: u8,
        pub(crate) buf_len: usize,
        pub(crate) timeout: Duration,
    }

    /// One recorded control transfer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct ControlCall {
        pub(crate) request_type: u8,
        pub(crate) request: u8,
        pub(crate) value: u16,
        pub(crate) index: u16,
        /// Payload written (OUT) or buffer length requested (IN).
        pub(crate) data: Vec<u8>,
        pub(crate) timeout: Duration,
    }

    /// A scripted response for one control transfer, consumed in
    /// order. `Ok` carries bytes to return: for IN requests they fill
    /// the caller's buffer; for OUT requests the value is the
    /// transferred-byte count. `Err` is mapped to [`Error::Transfer`].
    type Scripted = core::result::Result<Vec<u8>, TransferError>;

    /// One scripted bulk outcome delivered by the mock streaming loop.
    #[derive(Debug, Clone)]
    pub(crate) enum BulkRead {
        /// Deliver a complete buffer filled with this byte.
        Fill(u8),
        /// A short transfer of this many bytes — treated as a *transient*
        /// error by the resilient reader: the buffer is dropped and the
        /// stream keeps going (it does not stop).
        Short(usize),
        /// Fail with this transfer error. The reader stops **only** on
        /// [`TransferError::Disconnected`]; every other error is transient
        /// (dropped and resubmitted, the stream continues). Script
        /// `Fail(TransferError::Disconnected)` to drive a stop.
        Fail(TransferError),
    }

    /// Recording, scriptable [`UsbTransport`] for boundary tests.
    /// Read and write responses queue separately so a control write
    /// can never consume a response scripted for a read (or vice
    /// versa), keeping tests independent of unrelated call ordering.
    #[derive(Debug, Default)]
    pub(crate) struct MockTransport {
        pub(crate) calls: Mutex<Vec<ControlCall>>,
        pub(crate) bulk_calls: Mutex<Vec<BulkCall>>,
        write_responses: Mutex<VecDeque<Scripted>>,
        read_responses: Mutex<VecDeque<Scripted>>,
        bulk: Mutex<VecDeque<BulkRead>>,
    }

    impl MockTransport {
        /// Queue scripted responses for control WRITES (consumed
        /// FIFO). Unscripted writes succeed in full, so receiver-mode
        /// chatter needn't be scripted everywhere; `Ok` values are
        /// transferred-byte counts.
        pub(crate) fn script_writes(&self, responses: Vec<Scripted>) {
            *self.write_responses.lock().expect("mock lock") = responses.into();
        }

        /// Queue scripted responses for control READS (consumed FIFO).
        /// Unscripted reads fail with `Disconnected` so missing
        /// expectations surface loudly; `Ok` bytes fill the caller's
        /// buffer.
        pub(crate) fn script_reads(&self, responses: Vec<Scripted>) {
            *self.read_responses.lock().expect("mock lock") = responses.into();
        }

        /// Queue bulk outcomes (consumed FIFO); once exhausted, the mock
        /// streaming loop emulates a tolerated client timeout — which
        /// keeps the stream alive for the consumer to drain — so
        /// terminal outcomes must be scripted explicitly.
        pub(crate) fn script_bulk(&self, reads: Vec<BulkRead>) {
            *self.bulk.lock().expect("mock lock") = reads.into();
        }

        /// Drain the recorded calls (e.g. to discard construction-time
        /// traffic before the assertion window).
        pub(crate) fn take_recorded(&self) -> Vec<ControlCall> {
            std::mem::take(&mut *self.calls.lock().expect("mock lock"))
        }

        fn next_write_response(&self) -> Option<Scripted> {
            self.write_responses.lock().expect("mock lock").pop_front()
        }

        fn next_read_response(&self) -> Option<Scripted> {
            self.read_responses.lock().expect("mock lock").pop_front()
        }
    }

    impl UsbTransport for MockTransport {
        fn write_control(
            &self,
            request_type: u8,
            request: u8,
            value: u16,
            index: u16,
            buf: &[u8],
            timeout: Duration,
        ) -> Result<usize> {
            self.calls.lock().expect("mock lock").push(ControlCall {
                request_type,
                request,
                value,
                index,
                data: buf.to_vec(),
                timeout,
            });
            match self.next_write_response() {
                None => Ok(buf.len()),
                Some(Ok(bytes)) => Ok(bytes.len()),
                Some(Err(e)) => Err(Error::from(e)),
            }
        }

        fn read_control(
            &self,
            request_type: u8,
            request: u8,
            value: u16,
            index: u16,
            buf: &mut [u8],
            timeout: Duration,
        ) -> Result<usize> {
            self.calls.lock().expect("mock lock").push(ControlCall {
                request_type,
                request,
                value,
                index,
                data: vec![0; buf.len()],
                timeout,
            });
            match self.next_read_response() {
                // Unscripted reads fail loudly: silently returning
                // zeroed data would hide missing test expectations.
                None => Err(Error::from(TransferError::Disconnected)),
                Some(Ok(bytes)) => {
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    Ok(n)
                }
                Some(Err(e)) => Err(Error::from(e)),
            }
        }

        fn clear_halt(&self, endpoint: u8) -> Result<()> {
            // Record the call (as a sentinel ControlCall) so tests can
            // assert it lands between the receiver-mode OFF/RX commands.
            // Propagate a poisoned-lock error through the Result rather
            // than panicking (no expect/unwrap/panic in the crate).
            self.calls
                .lock()
                .map_err(|_| Error::Other)?
                .push(ControlCall {
                    request_type: 0,
                    request: wire::CLEAR_HALT_MARKER,
                    value: 0,
                    index: u16::from(endpoint),
                    data: Vec::new(),
                    timeout: Duration::ZERO,
                });
            Ok(())
        }

        fn run_bulk_stream(&self, endpoint: u8, shared: &StreamShared, buffer_size: usize) {
            // Walk the scripted sequence through the same SampleQueue
            // handoff the nusb reader uses: NON-BLOCKING acquire → drop
            // when the consumer is behind (never block, never stall). The
            // error policy mirrors the production loop: `Disconnected` and
            // the unrecoverable endpoint errors (Stall/InvalidArgument)
            // stop; every other error/short transfer is transient (dropped
            // + kept going).
            let mut run = super::ErrorRun::default();
            while shared.running() {
                // Record the simulated transfer's C parameters for the
                // wire-contract test.
                self.bulk_calls.lock().expect("mock lock").push(BulkCall {
                    endpoint,
                    buf_len: buffer_size,
                    timeout: EVENT_TIMEOUT,
                });
                let next = self.bulk.lock().expect("mock lock").pop_front();
                if !handle_mock_bulk(next.as_ref(), shared, &mut run, buffer_size) {
                    break;
                }
            }
            if run.total > 0 {
                log::info!("airspy: stream tolerated {} transient transfer errors", run.total);
            }
            shared.streaming.store(false, Ordering::SeqCst);
            shared.queue.shutdown();
        }

        fn release_interface(&self, _iface: u8) -> Result<()> {
            Ok(())
        }
    }

    /// Mirror of the production [`super::handle_bulk_completion`] for the
    /// scripted mock loop: process one step and return `false` when the
    /// stream should stop. Keeping the two in lockstep is what lets the
    /// unit tests assert the real reader's error policy.
    fn handle_mock_bulk(
        next: Option<&BulkRead>,
        shared: &StreamShared,
        run: &mut super::ErrorRun,
        buffer_size: usize,
    ) -> bool {
        match next {
            Some(BulkRead::Fill(byte)) => {
                run.consecutive = 0;
                if let Some(mut qbuf) = shared.queue.try_acquire_free() {
                    qbuf.fill(*byte);
                    shared.queue.push_filled(qbuf);
                }
                // else: consumer behind → drop (queue counts it).
                true
            }
            // The device is really gone: fatal.
            Some(BulkRead::Fail(TransferError::Disconnected)) => {
                log::warn!("airspy: bulk endpoint disconnected; stopping stream");
                false
            }
            // Unrecoverable endpoint errors: mirror the production loop and
            // stop rather than spin on futile resubmits.
            Some(BulkRead::Fail(err @ (TransferError::Stall | TransferError::InvalidArgument))) => {
                log::warn!("airspy: unrecoverable bulk transfer error ({err}); stopping stream");
                false
            }
            // A short transfer or any other non-fatal error is transient:
            // drop it and keep streaming, unless the consecutive run says
            // the endpoint is stuck (mirrors the production cap).
            Some(BulkRead::Short(n)) => {
                log::debug!("airspy: transient short bulk transfer ({n} of {buffer_size} bytes); resubmitting");
                super::record_transient(run)
            }
            Some(BulkRead::Fail(err)) => {
                log::debug!("airspy: transient bulk transfer error ({err}); resubmitting");
                super::record_transient(run)
            }
            // Exhausted script: emulate a tolerated client timeout (nusb's
            // wait_next_complete → None) and keep going.
            None => {
                std::thread::sleep(EXHAUSTED_BULK_POLL);
                true
            }
        }
    }
}
