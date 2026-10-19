//! Shared device ownership and cancellation-safe cleanup.

use crate::{
    DeviceDescriptor,
    config::Settings,
    device::DeviceInfo,
    errors::{Error, Result},
    maybe_future::{Either, MaybeFutureExt, operation, ready},
    protocol::Com,
    rtl2832u::{Applied, Hardware},
    usb::{ControlBackend, NusbTransport},
};
use nusb::MaybeFuture;
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock, PoisonError,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeviceLifecycle {
    Open,
    Closing,
    Closed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiverState {
    Stopped,
    CleanupRequired,
    Running,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClaimAvailability {
    Available,
    Held,
    CleanupRequired,
}
#[derive(Debug)]
pub(crate) struct SharedDeviceState {
    pub device: DeviceLifecycle,
    pub claim: ClaimAvailability,
}
#[derive(Debug)]
pub(crate) struct Session<C: ControlBackend + 'static = NusbTransport> {
    pub control: Arc<C>,
    pub info: OnceLock<DeviceInfo>,
    descriptor: DeviceDescriptor,
    state: Mutex<SharedDeviceState>,
    hardware: Mutex<Option<Hardware>>,
    configuration_valid: AtomicBool,
}

impl<C: ControlBackend + 'static> Session<C> {
    pub(crate) fn new(control: C, descriptor: DeviceDescriptor) -> Arc<Self> {
        Arc::new(Self {
            control: Arc::new(control),
            info: OnceLock::new(),
            descriptor,
            state: Mutex::new(SharedDeviceState {
                device: DeviceLifecycle::Open,
                claim: ClaimAvailability::Available,
            }),
            hardware: Mutex::new(Some(Hardware::default())),
            configuration_valid: AtomicBool::new(false),
        })
    }
    pub(crate) fn lock(&self) -> MutexGuard<'_, SharedDeviceState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub(crate) fn ensure_open(&self) -> Result<()> {
        if self.lock().device == DeviceLifecycle::Open {
            Ok(())
        } else {
            Err(Error::DeviceClosed)
        }
    }
    pub(crate) fn ensure_configured(&self) -> Result<()> {
        self.ensure_open()?;
        if self.configuration_valid.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(Error::ConfigurationUnknown)
        }
    }
    fn lease(self: &Arc<Self>) -> Result<HardwareLease<C>> {
        // Never hold a mutex across I/O. A simultaneous control sequence returns
        // Busy rather than interleaving I2C operations or deadlocking Drop.
        let hardware = self
            .hardware
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or(Error::Busy)?;
        Ok(HardwareLease {
            shared: Arc::clone(self),
            hardware: Some(hardware),
        })
    }
    pub(crate) fn configure(
        self: &Arc<Self>,
        settings: Settings,
    ) -> impl MaybeFuture<Output = Result<Applied>> + use<C> {
        let shared = Arc::clone(self);
        operation(async move {
            shared.ensure_open()?;
            settings.validate()?;
            let mut lease = shared.lease()?;
            lease
                .hardware
                .as_mut()
                .expect("leased hardware")
                .configure(shared.control.as_ref(), &settings, &shared.descriptor)
                .await
        })
    }
    pub(crate) fn initialize(
        control: C,
        descriptor: DeviceDescriptor,
        settings: Settings,
    ) -> impl MaybeFuture<Output = Result<(Arc<Self>, Applied)>> + use<C> {
        let shared = Self::new(control, descriptor);
        shared
            .configure(settings)
            .continue_with(move |result| match result {
                Ok(applied) => {
                    let tuner = shared
                        .hardware
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .as_ref()
                        .and_then(|h| h.tuner.as_ref())
                        .expect("initialized tuner")
                        .kind;
                    shared
                        .info
                        .set(DeviceInfo {
                            descriptor: shared.descriptor.clone(),
                            tuner,
                            rtl_sdr_blog_v4: shared.descriptor.blog_v4(),
                        })
                        .expect("metadata initialized once");
                    Either::left(ready(Ok((shared, applied))))
                }
                Err(error) => Either::right(shared.shutdown().map(move |_| {
                    drop(shared);
                    Err(error)
                })),
            })
    }
    pub(crate) fn receiver_mode(
        self: &Arc<Self>,
        enabled: bool,
    ) -> impl MaybeFuture<Output = Result<()>> + use<C> {
        let shared = Arc::clone(self);
        operation(async move {
            if enabled {
                shared.ensure_configured()?;
            }
            let mut lease = shared.lease()?;
            let hardware = lease.hardware.as_mut().expect("leased hardware");
            // Record the cleanup intent even if an operation is canceled.
            hardware.receiving = enabled;
            Com(shared.control.as_ref()).receiver(enabled).await
        })
    }
    fn begin_shutdown(&self) -> Result<bool> {
        let mut state = self.lock();
        match state.device {
            DeviceLifecycle::Closed => Ok(false),
            DeviceLifecycle::Closing => Ok(true),
            DeviceLifecycle::Open => {
                if state.claim == ClaimAvailability::Held {
                    return Err(Error::Busy);
                }
                state.device = DeviceLifecycle::Closing;
                Ok(true)
            }
        }
    }
    pub(crate) fn shutdown(self: &Arc<Self>) -> impl MaybeFuture<Output = Result<()>> + use<C> {
        let shared = Arc::clone(self);
        operation(async move {
            if !shared.begin_shutdown()? {
                return Ok(());
            }
            let mut lease = shared.lease()?;
            let result = lease
                .hardware
                .as_mut()
                .expect("leased hardware")
                .shutdown(shared.control.as_ref())
                .await;
            if result.is_ok() {
                let mut state = shared.lock();
                state.device = DeviceLifecycle::Closed;
                state.claim = ClaimAvailability::Available;
            }
            result
        })
    }
}

struct HardwareLease<C: ControlBackend + 'static> {
    shared: Arc<Session<C>>,
    hardware: Option<Hardware>,
}
impl<C: ControlBackend + 'static> Drop for HardwareLease<C> {
    fn drop(&mut self) {
        let hardware = self.hardware.take().expect("hardware returned once");
        self.shared
            .configuration_valid
            .store(hardware.valid, Ordering::Release);
        *self
            .shared
            .hardware
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hardware);
    }
}
impl<C: ControlBackend + 'static> Drop for Session<C> {
    fn drop(&mut self) {
        if self.lock().device == DeviceLifecycle::Closed {
            return;
        }
        let mut hardware = self
            .hardware
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("no lease outlives final owner");
        let control = Arc::clone(&self.control);
        let cleanup = operation(async move {
            let _ = hardware.shutdown(control.as_ref()).await;
        });
        #[cfg(not(target_arch = "wasm32"))]
        cleanup.wait();
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            cleanup.await;
        });
    }
}

/// Keeps both the device and its exclusive stream claim alive through cleanup.
#[derive(Debug)]
pub(crate) struct RxStreamClaim<C: ControlBackend + 'static> {
    pub(crate) shared: Arc<Session<C>>,
    receiver: ReceiverState,
    cleanup_armed: bool,
}

impl<C: ControlBackend + 'static> RxStreamClaim<C> {
    pub(crate) fn receiver_state(&self) -> ReceiverState {
        self.receiver
    }

    pub(crate) fn set_receiver_state(&mut self, receiver: ReceiverState) {
        self.receiver = receiver;
    }

    pub(crate) fn begin_start(&mut self) -> Result<bool> {
        match self.receiver {
            ReceiverState::Running => Ok(false),
            ReceiverState::CleanupRequired => Err(Error::Busy),
            ReceiverState::Stopped => {
                self.receiver = ReceiverState::CleanupRequired;
                Ok(true)
            }
        }
    }

    pub(crate) fn begin_stop(&mut self) -> bool {
        if self.receiver == ReceiverState::Stopped {
            return false;
        }
        // Cancellation or failure must not leave a stream apparently running.
        self.receiver = ReceiverState::CleanupRequired;
        true
    }

    pub(crate) fn acquire(shared: &Arc<Session<C>>) -> Result<Self> {
        let mut state = shared.lock();
        if state.device != DeviceLifecycle::Open {
            return Err(Error::DeviceClosed);
        }
        if state.claim != ClaimAvailability::Available {
            return Err(Error::Busy);
        }
        state.claim = ClaimAvailability::Held;
        Ok(Self {
            shared: Arc::clone(shared),
            receiver: ReceiverState::Stopped,
            cleanup_armed: true,
        })
    }

    fn release(&mut self) {
        self.shared.lock().claim = ClaimAvailability::Available;
        self.cleanup_armed = false;
    }

    pub(crate) fn close(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        ready(()).continue_with(move |()| {
            let operation = if self.begin_stop() {
                Either::left(self.shared.receiver_mode(false))
            } else {
                Either::right(ready(Ok(())))
            };
            operation.map(move |result| {
                if result.is_ok() {
                    self.receiver = ReceiverState::Stopped;
                    self.release();
                }
                result
            })
        })
    }

    // The owned operation keeps the claim reserved even before a WebUSB
    // background task is polled. Drop runs it to completion best-effort.
    pub(crate) fn cleanup(&mut self) -> impl MaybeFuture<Output = ()> + use<C> {
        self.cleanup_armed = false;
        let shared = Arc::clone(&self.shared);
        let operation = if self.begin_stop() {
            Either::left(shared.receiver_mode(false))
        } else {
            Either::right(ready(Ok(())))
        };
        operation.map(move |result| {
            let mut state = shared.lock();
            state.claim = if result.is_ok() {
                ClaimAvailability::Available
            } else {
                ClaimAvailability::CleanupRequired
            };
        })
    }
}

impl<C: ControlBackend + 'static> Drop for RxStreamClaim<C> {
    fn drop(&mut self) {
        if !self.cleanup_armed {
            return;
        }
        let cleanup = self.cleanup();
        #[cfg(not(target_arch = "wasm32"))]
        cleanup.wait();
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            cleanup.await;
        });
    }
}
