//! Scripted transport shared by protocol, lifecycle, and receive tests.
use crate::{
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
    pub(crate) pause_control_out_at: AtomicUsize,
    pub(crate) pause_clear_halt: AtomicBool,
    pub(crate) fail_bulk_completion: AtomicBool,
    pub(crate) bulk_in_count: AtomicUsize,
    pub(crate) cancel_count: AtomicUsize,
    pub(crate) events: Mutex<Vec<FakeEvent>>,
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
            pause_control_out_at: Default::default(),
            pause_clear_halt: Default::default(),
            fail_bulk_completion: Default::default(),
            bulk_in_count: Default::default(),
            cancel_count: Default::default(),
            events: Default::default(),
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
}

impl ControlBackend for FakeTransport {
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
            if self.state.fail_control_out.load(Ordering::SeqCst)
                || self.state.fail_control_out_at.load(Ordering::SeqCst) == call
            {
                Err(nusb::transfer::TransferError::Fault.into())
            } else {
                if self.request.index == 0x210
                    && let Some(value) = self.request.data.first()
                {
                    self.state
                        .sys_registers
                        .lock()
                        .unwrap()
                        .insert(self.request.value, *value);
                }
                Ok(())
            }
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
        if self.state.fail_control_out.load(Ordering::SeqCst)
            || self.state.fail_control_out_at.load(Ordering::SeqCst) == call
        {
            Err(nusb::transfer::TransferError::Fault.into())
        } else {
            Ok(())
        }
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
        let status = if std::mem::take(&mut self.fail_next)
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
            if self.request.value != u16::from(self.state.tuner_address.load(Ordering::SeqCst)) {
                return Ok(vec![0; usize::from(self.request.length)]);
            }
            let mut data = vec![0; usize::from(self.request.length)];
            data[0] = 0x69;
            if data.len() >= 3 && self.state.pll_locked.load(Ordering::SeqCst) {
                data[2] = 0x40u8.reverse_bits();
            }
            if data.len() >= 5 {
                data[4] = (if self.request.value == 0x34 {
                    0x20u8
                } else {
                    0x10u8
                })
                .reverse_bits();
            }
            return Ok(data);
        }
        if self.request.index == 0x200 {
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
