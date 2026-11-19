//! Trunk Recorder Pro core: everything between wideband IQ samples and
//! recorded calls, with no platform dependencies, so the same code runs in the
//! native app and in the browser (WebAssembly).
//!
//! ```text
//! u8 IQ → dsp::Channelizer ─ control channel → receivers → p25 framer → TSBKs
//!                          │                                  → trunk::Engine (calls)
//!                          └ voice channels  → receivers → p25 framer → IMBE → mbe → audio
//! ```

/// Serde for a type that names itself: written as `$name()`, read by `$parse` (None: refused, as
/// "`text` isn't a `$what`").
macro_rules! serde_by_name {
    ($t:ty, $name:ident, $parse:ident, $what:expr) => {
        impl serde::Serialize for $t {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.$name())
            }
        }
        impl<'de> serde::Deserialize<'de> for $t {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = <String as serde::Deserialize>::deserialize(d)?;
                <$t>::$parse(&text).ok_or_else(|| serde::de::Error::custom(format!("`{}` isn't a {}", text.trim(), $what)))
            }
        }
    };
}

pub mod alias;
pub mod ambe;
pub mod bits;
pub mod dmr;
pub mod dsp;
pub mod edacs;
pub mod loudness;
pub mod mbe;
pub mod nxdn;
pub mod position;
pub mod metrics;
pub mod p25;
pub mod smartnet;
pub mod sitecheck;
pub mod survey;
pub mod tables;
pub mod trunk;
pub mod wav;

pub use num_complex::Complex32;
