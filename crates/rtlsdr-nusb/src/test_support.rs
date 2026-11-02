//! Scripted transport shared by protocol, lifecycle, and receive tests.
use crate::{
    TunerKind,
    errors::Result,
    protocol::{InRequest, OutRequest},
    usb::{BulkInBackend, BulkInCompletion, ControlBackend, Transport},
};
use nusb::MaybeFuture;
use std::{
    collections::VecDeque,
    future::IntoFuture,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
#[derive(Debug)]
pub(crate) struct FakeState {
    pub(crate) tuner_address: AtomicU8,
    pub(crate) pll_locked: AtomicBool,
    pub(crate) sys_registers: Mutex<std::collections::HashMap<u16, u8>>,
    pub(crate) pause_control_in_at: AtomicUsize,
    pub(crate) control_in_count: AtomicUsize,
    pub(crate) responses: Mutex<VecDeque<Result<Vec<u8>>>>,
    pub(crate) control_in_requests: Mutex<Vec<InRequest>>,
    pub(crate) pause_bulk: AtomicBool,
    pub(crate) control_out_count: AtomicUsize,
    pub(crate) control_out_requests: Mutex<Vec<OutRequest>>,
    pub(crate) fail_control_out: AtomicBool,
    pub(crate) fail_control_out_at: AtomicUsize,
    /// Control writes refused (STALL) from this call number on, this many times.
    pub(crate) stall_control_out_at: AtomicUsize,
    pub(crate) stall_control_out_times: AtomicUsize,
    pub(crate) pause_control_out_at: AtomicUsize,
    pub(crate) pause_clear_halt: AtomicBool,
    pub(crate) fail_bulk_completion: AtomicBool,
    pub(crate) bulk_in_count: AtomicUsize,
    pub(crate) cancel_count: AtomicUsize,
    pub(crate) events: Mutex<Vec<FakeEvent>>,
    /// The tuner at `tuner_address` (none if no tuner kind lives there):
    /// its registers as written, and the register its next read starts at.
    pub(crate) tuner_registers: Mutex<std::collections::HashMap<u8, u8>>,
    pub(crate) tuner_pointer: AtomicU8,
    /// I2C writes to an address with no tuner fail as WebUSB on Windows
    /// reports a NAK: "disconnected".
    pub(crate) disconnect_absent_i2c: AtomicBool,
}
impl FakeState {
    /// The kind of tuner answering at `tuner_address`, if any.
    fn tuner(&self) -> Option<TunerKind> {
        let address = self.tuner_address.load(Ordering::SeqCst);
        [TunerKind::R820T, TunerKind::R828D, TunerKind::E4000].into_iter().find(|k| k.address() == address)
    }
    /// An I2C write to the tuner: its register pointer set, or a register written.
    fn tuner_write(&self, request: &OutRequest) {
        if request.index != 0x610 || self.tuner().is_none_or(|k| u16::from(k.address()) != request.value) {
            return;
        }
        match request.data[..] {
            [reg] => self.tuner_pointer.store(reg, Ordering::SeqCst),
            [reg, value] => {
                self.tuner_registers.lock().unwrap().insert(reg, value);
            }
            _ => {}
        }
    }
    /// `length` bytes from the tuner at `address` (zeros where none answers):
    /// its registers as written from the pointer on, but for what the chip
    /// itself sets — the ID register, the PLL's lock (`pll_locked`), an R82xx's
    /// VCO fine-tune bits — and an R82xx's bits reversed on the wire.
    fn tuner_read(&self, address: u16, length: u16) -> Vec<u8> {
        let Some(kind) = self.tuner().filter(|k| u16::from(k.address()) == address) else {
            return vec![0; usize::from(length)];
        };
        let locked = self.pll_locked.load(Ordering::SeqCst);
        let pointer = self.tuner_pointer.load(Ordering::SeqCst);
        let registers = self.tuner_registers.lock().unwrap();
        (0..length)
            .map(|i| {
                let reg = pointer.wrapping_add(i as u8);
                let value = registers.get(&reg).copied().unwrap_or(0);
                match kind {
                    TunerKind::E4000 => match reg {
                        0x02 => 0x40,
                        0x07 => (value & !1) | u8::from(locked),
                        _ => value,
                    },
                    _ => match reg {
                        0 => 0x96,
                        2 => if locked { 0x40 } else { 0 },
                        4 => if kind == TunerKind::R820T { 0x20 } else { 0x10 },
                        _ => value,
                    }
                    .reverse_bits(),
                }
            })
            .collect()
    }
    /// A control write's outcome (the same awaited or waited for), and its
    /// effect on the registers the fake keeps.
    fn complete_out(&self, request: &OutRequest, call: usize) -> Result<()> {
        if self.disconnect_absent_i2c.load(Ordering::SeqCst) && request.index == 0x610 && request.value != u16::from(self.tuner_address.load(Ordering::SeqCst)) {
            return Err(nusb::transfer::TransferError::Disconnected.into());
        }
        let stall_at = self.stall_control_out_at.load(Ordering::SeqCst);
        if self.fail_control_out.load(Ordering::SeqCst) || self.fail_control_out_at.load(Ordering::SeqCst) == call {
            return Err(nusb::transfer::TransferError::Fault.into());
        }
        if stall_at != 0 && call >= stall_at && self.stall_control_out_times.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
            return Err(nusb::transfer::TransferError::Stall.into());
        }
        self.tuner_write(request);
        if (request.index == 0x210 || request.index == 0x110)
            && let Some(value) = request.data.first()
        {
            self.sys_registers.lock().unwrap().insert(request.value, *value);
        }
        Ok(())
    }
}
impl Default for FakeState {
    fn default() -> Self {
        Self {
            tuner_address: AtomicU8::new(0x34),
            pll_locked: AtomicBool::new(true),
            sys_registers: Default::default(),
            pause_control_in_at: Default::default(),
            control_in_count: Default::default(),
            responses: Default::default(),
            control_in_requests: Default::default(),
            pause_bulk: Default::default(),
            control_out_count: Default::default(),
            control_out_requests: Default::default(),
            fail_control_out: Default::default(),
            fail_control_out_at: Default::default(),
            stall_control_out_at: Default::default(),
            stall_control_out_times: Default::default(),
            pause_control_out_at: Default::default(),
            pause_clear_halt: Default::default(),
            fail_bulk_completion: Default::default(),
            bulk_in_count: Default::default(),
            cancel_count: Default::default(),
            events: Default::default(),
            tuner_registers: Default::default(),
            tuner_pointer: Default::default(),
            disconnect_absent_i2c: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FakeEvent {
    Control(OutRequest),
    QueueDropped,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FakeTransport {
    pub(crate) state: Arc<FakeState>,
    pub(crate) link: Arc<crate::usb::Link>,
}

impl ControlBackend for FakeTransport {
    fn link(&self) -> &crate::usb::Link {
        &self.link
    }
    fn control_in(&self, request: InRequest) -> impl MaybeFuture<Output = Result<Vec<u8>>> + use<> {
        FakeTransportIn {
            state: Arc::clone(&self.state),
            request,
        }
    }

    fn control_out(&self, request: OutRequest) -> impl MaybeFuture<Output = Result<()>> + use<> {
        FakeTransportOut {
            state: Arc::clone(&self.state),
            request,
        }
    }
}

struct FakeTransportOut {
    pub(crate) state: Arc<FakeState>,
    pub(crate) request: OutRequest,
}

impl std::future::IntoFuture for FakeTransportOut {
    type Output = Result<()>;
    type IntoFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            self.state
                .events
                .lock()
                .unwrap()
                .push(FakeEvent::Control(self.request.clone()));
            self.state
                .control_out_requests
                .lock()
                .expect("control request lock")
                .push(self.request.clone());
            let call = self.state.control_out_count.fetch_add(1, Ordering::SeqCst) + 1;
            // Tests explicitly poll again after clearing the pause.
            std::future::poll_fn(|_| {
                if self.state.pause_control_out_at.load(Ordering::SeqCst) == call {
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            })
            .await;
            self.state.complete_out(&self.request, call)
        })
    }
}

impl MaybeFuture for FakeTransportOut {
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Result<()> {
        self.state
            .events
            .lock()
            .unwrap()
            .push(FakeEvent::Control(self.request.clone()));
        self.state
            .control_out_requests
            .lock()
            .expect("control request lock")
            .push(self.request.clone());
        let call = self.state.control_out_count.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.complete_out(&self.request, call)
    }
}

impl Transport for FakeTransport {
    type BulkIn = FakeBulkIn;
    fn bulk_in(&self, _endpoint: u8) -> Result<FakeBulkIn> {
        self.state.bulk_in_count.fetch_add(1, Ordering::SeqCst);
        let mut bulk = FakeBulkIn::default();
        bulk.state = Arc::clone(&self.state);
        Ok(bulk)
    }
}
#[derive(Debug, Default)]
pub(crate) struct FakeBulkIn {
    pub(crate) state: Arc<FakeState>,
    pub(crate) submitted: VecDeque<Vec<u8>>,
    pub(crate) submit_count: usize,
    pub(crate) pending_once: bool,
    pub(crate) fail_next: bool,
    pub(crate) stall_next: bool,
    pub(crate) short_next: bool,
    pub(crate) timeout_next: bool,
    pub(crate) last_timeout: Option<Duration>,
    pub(crate) cancelled: bool,
}
impl Drop for FakeBulkIn {
    fn drop(&mut self) {
        self.state
            .events
            .lock()
            .unwrap()
            .push(FakeEvent::QueueDropped);
    }
}
impl FakeBulkIn {
    fn complete(&mut self) -> BulkInCompletion<Vec<u8>> {
        let buffer = self
            .submitted
            .pop_front()
            .expect("fake endpoint has submitted buffers");
        let status = if std::mem::take(&mut self.stall_next) {
            Err(nusb::transfer::TransferError::Stall.into())
        } else if std::mem::take(&mut self.fail_next)
            || self
                .state
                .fail_bulk_completion
                .swap(false, Ordering::SeqCst)
        {
            Err(nusb::transfer::TransferError::Fault.into())
        } else {
            Ok(())
        };
        let actual_len = buffer.len()
            - if std::mem::take(&mut self.short_next) {
                2
            } else {
                0
            };
        BulkInCompletion {
            buffer,
            actual_len,
            status,
        }
    }
}
impl BulkInBackend for FakeBulkIn {
    type Buffer = Vec<u8>;
    fn clear_halt(&mut self) -> impl MaybeFuture<Output = Result<()>> + use<> {
        ClearHalt(Arc::clone(&self.state))
    }
    fn allocate(&self, len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 256) as u8).collect()
    }
    fn submit(&mut self, buffer: Vec<u8>) {
        self.submit_count += 1;
        self.submitted.push_back(buffer);
    }
    fn pending(&self) -> usize {
        self.submitted.len()
    }
    fn poll_next_complete(&mut self, cx: &mut Context<'_>) -> Poll<BulkInCompletion<Vec<u8>>> {
        if self.state.pause_bulk.load(Ordering::SeqCst) {
            return Poll::Pending;
        }
        if std::mem::take(&mut self.pending_once) {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(self.complete())
    }
    fn wait_next_complete(&mut self, timeout: Duration) -> Option<BulkInCompletion<Vec<u8>>> {
        self.last_timeout = Some(timeout);
        if std::mem::take(&mut self.timeout_next) {
            None
        } else {
            Some(self.complete())
        }
    }
    fn cancel_all(&mut self) {
        self.cancelled = true;
        self.state.cancel_count.fetch_add(1, Ordering::SeqCst);
    }
}
struct ClearHalt(Arc<FakeState>);
impl IntoFuture for ClearHalt {
    type Output = Result<()>;
    type IntoFuture = crate::maybe_future::OperationFuture<Self::Output>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            std::future::poll_fn(|_| {
                if self.0.pause_clear_halt.load(Ordering::SeqCst) {
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(()))
                }
            })
            .await
        })
    }
}
impl MaybeFuture for ClearHalt {
    fn wait(self) -> Result<()> {
        Ok(())
    }
}

struct FakeTransportIn {
    state: Arc<FakeState>,
    request: InRequest,
}
impl FakeTransportIn {
    fn record(&self) -> usize {
        self.state
            .control_in_requests
            .lock()
            .unwrap()
            .push(self.request.clone());
        self.state.control_in_count.fetch_add(1, Ordering::SeqCst) + 1
    }
    fn response(self) -> Result<Vec<u8>> {
        if let Some(response) = self.state.responses.lock().unwrap().pop_front() {
            return response;
        }
        if self.request.index == 0x600 {
            return Ok(self.state.tuner_read(self.request.value, self.request.length));
        }
        if self.request.index == 0x200 || self.request.index == 0x100 {
            return Ok(vec![
                *self
                    .state
                    .sys_registers
                    .lock()
                    .unwrap()
                    .get(&self.request.value)
                    .unwrap_or(&0),
            ]);
        }
        Ok(vec![0; usize::from(self.request.length)])
    }
}
impl IntoFuture for FakeTransportIn {
    type Output = Result<Vec<u8>>;
    type IntoFuture = crate::maybe_future::OperationFuture<Self::Output>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let call = self.record();
            std::future::poll_fn(|_| {
                if self.state.pause_control_in_at.load(Ordering::SeqCst) == call {
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            })
            .await;
            self.response()
        })
    }
}
impl MaybeFuture for FakeTransportIn {
    fn wait(self) -> Result<Vec<u8>> {
        self.record();
        self.response()
    }
}
