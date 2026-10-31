//! USB transport interfaces and the nusb implementation.

use crate::{
    errors::{Error, Result},
    maybe_future::{Either, MaybeFutureExt, NonWasmSend, defer, ready},
    protocol::{self, InRequest, OutRequest},
};
use nusb::{
    Endpoint, MaybeFuture,
    transfer::{Buffer, Bulk, In},
};
use std::time::Duration;
use std::{
    fmt::Debug,
    ops::Deref,
    task::{Context, Poll},
};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) trait BackendSafe: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync> BackendSafe for T {}
#[cfg(target_arch = "wasm32")]
pub(crate) trait BackendSafe {}
#[cfg(target_arch = "wasm32")]
impl<T> BackendSafe for T {}

/// The control pipe's own state: when a read was last answered (a
/// demodulator write needs one shortly before it on some hosts, see
/// [`crate::protocol`]) and what the chip refused.
#[derive(Debug, Default)]
pub(crate) struct Link {
    #[cfg(not(target_arch = "wasm32"))]
    last_read: std::sync::Mutex<Option<std::time::Instant>>,
    pub stall_retries: std::sync::atomic::AtomicU64,
    pub wake_reads: std::sync::atomic::AtomicU64,
}

impl Link {
    pub(crate) fn read_answered(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            *self.last_read.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        }
    }
    /// Whether a read was answered within `d` (always, where time can't be told).
    pub(crate) fn read_within(&self, d: Duration) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.last_read.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed() < d);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = d;
            true
        }
    }
}

/// Control operations must submit only when waited or polled, including after IntoFuture.
pub(crate) trait ControlBackend: Debug + BackendSafe {
    /// The pipe's state ([`Link`]).
    fn link(&self) -> &Link;
    fn control_in(
        &self,
        request: InRequest,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>>> + use<Self>;
    fn control_out(&self, request: OutRequest)
    -> impl MaybeFuture<Output = Result<()>> + use<Self>;
}

pub(crate) trait Transport: ControlBackend + 'static {
    type BulkIn: BulkInBackend;
    fn bulk_in(&self, endpoint: u8) -> Result<Self::BulkIn>;
}

#[derive(Debug)]
pub(crate) struct BulkInCompletion<B> {
    pub(crate) buffer: B,
    pub(crate) actual_len: usize,
    pub(crate) status: Result<()>,
}

/// Both completion mechanisms operate on the same endpoint and submitted buffers.
pub(crate) trait BulkInBackend: Debug + NonWasmSend {
    type Buffer: Deref<Target = [u8]> + NonWasmSend;
    fn clear_halt(&mut self) -> impl MaybeFuture<Output = Result<()>> + use<Self>;
    fn allocate(&self, len: usize) -> Self::Buffer;
    fn submit(&mut self, buffer: Self::Buffer);
    fn pending(&self) -> usize;
    fn poll_next_complete(&mut self, cx: &mut Context<'_>) -> Poll<BulkInCompletion<Self::Buffer>>;
    #[cfg(not(target_arch = "wasm32"))]
    fn wait_next_complete(&mut self, timeout: Duration) -> Option<BulkInCompletion<Self::Buffer>>;
    fn cancel_all(&mut self);
}

#[derive(Debug)]
pub(crate) struct NusbTransport {
    _device: nusb::Device,
    interface: nusb::Interface,
    link: Link,
}

impl NusbTransport {
    pub(crate) fn open(
        selector: crate::discovery::Selector,
    ) -> impl MaybeFuture<Output = Result<(Self, crate::DeviceDescriptor)>> {
        crate::discovery::select(selector)
            .and_then(|(descriptor, info)| {
                info.open()
                    .map_err(|e| Error::from(e).at("opening USB device"))
                    .map_ok(move |device| (device, descriptor))
            })
            .and_then(|(device, descriptor)| {
                let configuration = if device
                    .active_configuration()
                    .is_ok_and(|c| c.configuration_value() == 1)
                {
                    Either::left(ready(Ok(())))
                } else {
                    Either::right(
                        device
                            .set_configuration(1)
                            .map_err(|e| Error::from(e).at("selecting USB configuration 1")),
                    )
                };
                configuration.map_ok(move |()| (device, descriptor))
            })
            .and_then(|(device, descriptor)| {
                device
                    .detach_and_claim_interface(0)
                    .map_err(|e| Error::from(e).at("claiming RTL-SDR USB interface 0"))
                    .map_ok(move |interface| {
                        (
                            Self {
                                _device: device,
                                interface,
                                link: Link::default(),
                            },
                            descriptor,
                        )
                    })
            })
            .and_then(|(transport, descriptor)| {
                // Windows enumeration never includes iManufacturer. Read missing
                // branding after claim, before selecting any V4 hardware behavior.
                let strings = transport._device.device_descriptor();
                let missing = (descriptor.manufacturer.is_none()
                    && strings.manufacturer_string_index().is_some())
                    || (descriptor.product.is_none() && strings.product_string_index().is_some());
                let language = if missing {
                    let device = transport._device.clone();
                    Either::left(
                        defer(move || {
                            device.get_string_descriptor_supported_languages(protocol::timeout())
                        })
                        .map(|result| result.ok().and_then(|mut languages| languages.next())),
                    )
                } else {
                    Either::right(ready(None))
                };
                language.continue_with(move |language| {
                    let language = language.unwrap_or(nusb::descriptors::language_id::US_ENGLISH);
                    let device = transport._device.clone();
                    descriptor
                        .read_missing_strings(
                            strings.manufacturer_string_index(),
                            strings.product_string_index(),
                            move |index| {
                                device
                                    .get_string_descriptor(index, language, protocol::timeout())
                                    .map(|r| r.ok())
                            },
                        )
                        .map(move |descriptor| Ok((transport, descriptor)))
                })
            })
    }
}

impl ControlBackend for NusbTransport {
    fn link(&self) -> &Link {
        &self.link
    }
    fn control_in(&self, request: InRequest) -> impl MaybeFuture<Output = Result<Vec<u8>>> + use<> {
        let interface = self.interface.clone();
        // Native nusb control transfers submit on construction, not on their first poll.
        defer(move || {
            interface
                .control_in(request.encode(), protocol::timeout())
                .map_err(Error::from)
        })
    }
    fn control_out(&self, request: OutRequest) -> impl MaybeFuture<Output = Result<()>> + use<> {
        let interface = self.interface.clone();
        defer(move || {
            interface
                .control_out(request.encode(), protocol::timeout())
                .map_err(Error::from)
        })
    }
}

impl Transport for NusbTransport {
    type BulkIn = NusbBulkIn;
    fn bulk_in(&self, endpoint: u8) -> Result<Self::BulkIn> {
        Ok(NusbBulkIn {
            endpoint: self
                .interface
                .endpoint::<Bulk, In>(endpoint)
                .map_err(Error::from)?,
        })
    }
}

#[derive(Debug)]
pub(crate) struct NusbBulkIn {
    endpoint: Endpoint<Bulk, In>,
}
impl BulkInBackend for NusbBulkIn {
    type Buffer = Buffer;
    fn clear_halt(&mut self) -> impl MaybeFuture<Output = Result<()>> + use<> {
        self.endpoint.clear_halt().map_err(Error::from)
    }
    fn allocate(&self, len: usize) -> Buffer {
        self.endpoint.allocate(len)
    }
    fn submit(&mut self, buffer: Buffer) {
        self.endpoint.submit(buffer);
    }
    fn pending(&self) -> usize {
        self.endpoint.pending()
    }
    fn poll_next_complete(&mut self, cx: &mut Context<'_>) -> Poll<BulkInCompletion<Buffer>> {
        self.endpoint
            .poll_next_complete(cx)
            .map(|c| BulkInCompletion {
                buffer: c.buffer,
                actual_len: c.actual_len,
                status: c.status.map_err(Error::from),
            })
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn wait_next_complete(&mut self, timeout: Duration) -> Option<BulkInCompletion<Buffer>> {
        self.endpoint
            .wait_next_complete(timeout)
            .map(|c| BulkInCompletion {
                buffer: c.buffer,
                actual_len: c.actual_len,
                status: c.status.map_err(Error::from),
            })
    }
    fn cancel_all(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.endpoint.cancel_all();
    }
}
