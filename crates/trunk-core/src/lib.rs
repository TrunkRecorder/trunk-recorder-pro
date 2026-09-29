//! Trunk Recorder Lite core: everything between wideband IQ samples and
//! recorded calls, with no platform dependencies, so the same code runs in the
//! native app and in the browser (WebAssembly).
//!
//! ```text
//! u8 IQ → dsp::Channelizer ─ control channel → receivers → p25 framer → TSBKs
//!                          │                                  → trunk::Engine (calls)
//!                          └ voice channels  → receivers → p25 framer → IMBE → mbe → audio
//! ```

pub mod dsp;
pub mod mbe;
pub mod p25;
pub mod tables;
pub mod trunk;
pub mod wav;

pub use num_complex::Complex32;
