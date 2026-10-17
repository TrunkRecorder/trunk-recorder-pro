//! Native RTL2832U / R820T / R828D driver built on `nusb`.
//!
//! Device and stream operations can be awaited, or synchronously executed with
//! [`MaybeFuture::wait`] on native targets. Enable `smol` or `tokio` when awaiting
//! native discovery, opening, and endpoint setup; no runtime is required by the
//! blocking API. WebUSB supports awaiting only and requires
//! `--cfg=web_sys_unstable_apis` in the application's Rust flags.
//!
//! An independently owned [`RxStream`] keeps the hardware alive. Stop preserves
//! its exclusive claim; consuming close releases it. [`Device::shutdown`] is
//! terminal and returns [`Error::Busy`] until the stream has been closed/dropped.
//!
//! ```no_run
//! use rtlsdr_nusb::{Complex32, Device, MaybeFuture};
//! use std::time::Duration;
//!
//! let mut device = Device::builder()
//!     .frequency_hz(100_000_000)
//!     .sample_rate_hz(2_048_000)
//!     .open().wait()?;
//! let mut rx = device.rx_stream()?;
//! rx.start().wait()?;
//! let mut samples = [Complex32::default(); 4096];
//! let count = rx.read(&mut samples, Some(Duration::from_secs(1))).wait()?;
//! println!("received {count} samples");
//! rx.close().wait()?;
//! device.shutdown().wait()?;
//! # Ok::<(), rtlsdr_nusb::Error>(())
//! ```

#![deny(missing_docs)]

mod config;
mod device;
mod discovery;
mod errors;
mod maybe_future;
mod protocol;
mod rtl2832u;
mod rx;
mod session;
mod tuners;
mod usb;

pub use config::{
    Config, ConfigBuilder, DirectSampling, F32Iq, GainConfig, RawIq, SampleFormat, SampleMode,
};
pub use device::{Device, DeviceBuilder, DeviceInfo};
pub use discovery::DeviceDescriptor;
pub use errors::{Error, ErrorKind, Result};
pub use num_complex::Complex32;
pub use nusb::MaybeFuture;
pub use rx::{MAX_F32_IQ_SAMPLES_PER_TRANSFER, RxStream, SampleBlock, StreamingStats};
pub use tuners::TunerKind;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod test_support;

// Compile the README examples as part of the documentation checks.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}
