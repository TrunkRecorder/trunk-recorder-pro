//! Device discovery, configuration and shared hardware ownership.
use crate::{
    DeviceDescriptor, DirectSampling, GainConfig, Result, TunerKind,
    config::{Config, ConfigBuilder, F32Iq, RawIq, SampleMode},
    discovery::Selector,
    maybe_future::{MaybeFutureExt, ready},
    rtl2832u::Applied,
    rx::RxStream,
    session::{RxStreamClaim, Session},
    usb::NusbTransport,
};
use nusb::MaybeFuture;
use std::sync::Arc;

/// Immutable identity captured during device initialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    /// USB identity.
    pub descriptor: DeviceDescriptor,
    /// Detected tuner family.
    pub tuner: TunerKind,
    /// Whether USB branding identifies an RTL-SDR Blog V4.
    pub rtl_sdr_blog_v4: bool,
}

/// Owned device handle with a separately owned receive stream.
/// Dropping the handle does not interrupt an outstanding stream, even if stopped.
#[derive(Debug)]
pub struct Device<M: SampleMode = F32Iq> {
    session: Arc<Session>,
    config: Config<M>,
    applied: Applied,
}
impl Device<F32Iq> {
    /// List visible receivers with the standard Realtek USB IDs, without opening them.
    pub fn list() -> impl MaybeFuture<Output = Result<Vec<DeviceDescriptor>>> {
        crate::discovery::list()
    }
    /// Start a device builder with default configuration.
    pub fn builder() -> DeviceBuilder {
        DeviceBuilder::default()
    }
    /// Open the first visible receiver with default configuration.
    pub fn open() -> impl MaybeFuture<Output = Result<Self>> {
        Self::builder().open()
    }
    /// Open a device by its exact USB serial string.
    pub fn open_serial(serial: impl Into<String>) -> impl MaybeFuture<Output = Result<Self>> {
        Self::builder().serial(serial).open()
    }
    /// Ask for browser WebUSB permission from a user gesture, without opening the receiver.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_permission() -> Result<()> {
        Self::builder().request_permission().await
    }
}
impl<M: SampleMode> Device<M> {
    /// Immutable device metadata.
    pub fn info(&self) -> &DeviceInfo {
        self.session.info.get().expect("initialized metadata")
    }
    /// Last successfully applied configuration. A failed/canceled configuration
    /// leaves this snapshot unchanged and requires a complete `configure` retry.
    pub fn config(&self) -> &Config<M> {
        &self.config
    }
    /// Last successfully programmed center frequency after divider quantization.
    pub fn actual_frequency_hz(&self) -> u64 {
        self.applied.frequency_hz
    }
    /// Last successfully programmed complex sample rate after divider quantization.
    pub fn actual_sample_rate_hz(&self) -> u32 {
        self.applied.sample_rate_hz
    }
    /// Apply a complete configuration. This also recovers from a failed or canceled
    /// control operation. Controls are serialized; overlapping operations from a
    /// stream return [`crate::Error::Busy`] and can be retried.
    ///
    /// Queued samples may predate a live configuration change. Stop and restart
    /// the stream when the caller needs previously queued samples discarded.
    pub fn configure(&mut self, config: &Config<M>) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.update(config.clone())
    }
    fn update(&mut self, proposed: Config<M>) -> impl MaybeFuture<Output = Result<()>> + '_ {
        ready(proposed.settings.validate()).and_then(move |_| {
            self.session
                .configure(proposed.settings.clone())
                .map(move |result| {
                    self.applied = result?;
                    self.config = proposed;
                    Ok(())
                })
        })
    }
    /// Tune the center frequency in Hz, preserving other settings.
    pub fn set_frequency_hz(&mut self, hz: u64) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let mut config = self.config.clone();
        config.settings.frequency_hz = hz;
        self.update(config)
    }
    /// Set complex sample rate, preserving other settings.
    pub fn set_sample_rate_hz(&mut self, hz: u32) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let mut config = self.config.clone();
        config.settings.sample_rate_hz = hz;
        self.update(config)
    }
    /// Change tuner gain policy, preserving other settings.
    pub fn set_gain(&mut self, gain: GainConfig) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let mut config = self.config.clone();
        config.settings.gain = gain;
        self.update(config)
    }
    /// Correct the sampling clock and reapply tuning with the corrected crystal frequency.
    pub fn set_correction_ppm(&mut self, ppm: i32) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let mut config = self.config.clone();
        config.settings.correction_ppm = ppm;
        self.update(config)
    }
    /// Enable or disable GPIO 0 bias power, preserving other settings.
    pub fn set_bias_tee(&mut self, enabled: bool) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let mut config = self.config.clone();
        config.settings.bias_tee = enabled;
        self.update(config)
    }
    /// Set low-frequency direct sampling policy, preserving other settings.
    pub fn set_direct_sampling(
        &mut self,
        mode: DirectSampling,
    ) -> impl MaybeFuture<Output = Result<()>> + '_ {
        let mut config = self.config.clone();
        config.settings.direct_sampling = mode;
        self.update(config)
    }
    /// Claim the single receive stream. The stream starts only when `start` is
    /// waited/awaited. Dormant and stopped streams retain the claim until close/drop.
    pub fn rx_stream(&self) -> Result<RxStream<M>> {
        self.session.ensure_configured()?;
        Ok(RxStream::new(RxStreamClaim::acquire(&self.session)?))
    }
    /// Stop the hardware and disable RF bias power.
    ///
    /// Returns Busy without changing lifecycle while a stream claim is held.
    /// Close/drop the stream first. Unpolled shutdown has no effects; once begun,
    /// shutdown is terminal, idempotent after success, and retryable on failure or
    /// cancellation. All independent cleanup steps are attempted on error.
    ///
    /// Final-owner Drop performs the same cleanup best-effort, blocking on native
    /// targets and scheduling a background task on WebUSB. Explicit shutdown is
    /// required to observe cleanup errors/completion.
    pub fn shutdown(&mut self) -> impl MaybeFuture<Output = Result<()>> + '_ {
        self.session.shutdown()
    }
}

/// Select and configure a receiver. Standard IDs are `0bda:2832` and `0bda:2838`.
#[derive(Clone, Debug)]
pub struct DeviceBuilder<M: SampleMode = F32Iq> {
    selector: Selector,
    config: ConfigBuilder<M>,
}
impl Default for DeviceBuilder<F32Iq> {
    fn default() -> Self {
        Self {
            selector: Selector::default(),
            config: ConfigBuilder::default(),
        }
    }
}
impl<M: SampleMode> DeviceBuilder<M> {
    /// Select by exact serial; this replaces a previously chosen index.
    pub fn serial(mut self, serial: impl Into<String>) -> Self {
        self.selector.serial = Some(serial.into());
        self.selector.index = None;
        self
    }
    /// Select by enumeration index; this replaces a previously chosen serial.
    pub fn index(mut self, index: usize) -> Self {
        self.selector.index = Some(index);
        self.selector.serial = None;
        self
    }
    /// Match a custom USB VID/PID instead of the standard Realtek IDs.
    pub fn usb_id(mut self, vid: u16, pid: u16) -> Self {
        self.selector.usb_id = Some((vid, pid));
        self
    }
    /// Set center frequency in Hz.
    pub fn frequency_hz(mut self, value: u64) -> Self {
        self.config = self.config.frequency_hz(value);
        self
    }
    /// Set complex sample rate in samples/s.
    pub fn sample_rate_hz(mut self, value: u32) -> Self {
        self.config = self.config.sample_rate_hz(value);
        self
    }
    /// Set gain policy.
    pub fn gain(mut self, value: GainConfig) -> Self {
        self.config = self.config.gain(value);
        self
    }
    /// Set crystal correction in ppm.
    pub fn correction_ppm(mut self, value: i32) -> Self {
        self.config = self.config.correction_ppm(value);
        self
    }
    /// Enable or disable RF bias power.
    pub fn bias_tee(mut self, value: bool) -> Self {
        self.config = self.config.bias_tee(value);
        self
    }
    /// Select low-frequency direct sampling behavior.
    pub fn direct_sampling(mut self, value: DirectSampling) -> Self {
        self.config = self.config.direct_sampling(value);
        self
    }
    /// Validate the configuration without opening hardware.
    pub fn config(self) -> Result<Config<M>> {
        self.config.build()
    }
    /// Validate, open, probe, and configure the selected receiver.
    /// After interface claim, failure/cancellation arms final-owner cleanup.
    pub fn open(self) -> impl MaybeFuture<Output = Result<Device<M>>> {
        ready(self.config.build()).and_then(move |config| {
            NusbTransport::open(self.selector).and_then(move |(control, descriptor)| {
                Session::initialize(control, descriptor, config.settings.clone()).map_ok(
                    move |(session, applied)| Device {
                        session,
                        config,
                        applied,
                    },
                )
            })
        })
    }
    /// Request WebUSB permission from a browser user gesture. Opening afterwards
    /// can take place in either a Window or a Worker and never prompts.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_permission(&self) -> Result<()> {
        crate::discovery::request_permission(&self.selector).await
    }
}
impl DeviceBuilder<F32Iq> {
    /// Select raw unsigned eight-bit I/Q blocks instead of converted floats.
    pub fn raw_iq(self) -> DeviceBuilder<RawIq> {
        DeviceBuilder {
            selector: self.selector,
            config: self.config.raw_iq(),
        }
    }
}
