//! Plugins for Trunk Recorder Lite.
//!
//! A plugin is a program of its own that the recorder starts, sends events to
//! (calls starting, ending and landing on disk; radios registering; live
//! audio; status) and stops. It only watches: nothing it does changes what is
//! recorded. The [`protocol`] is JSON lines over stdin/stdout, so a plugin
//! can be written in anything; this crate is the Rust side of it.
//!
//! ```no_run
//! use trunk_recorder_plugin::{topic, ConcludedCall, Host, Manifest, NoConfig, Plugin, Setup};
//!
//! struct Hello { host: Host }
//!
//! impl Plugin for Hello {
//!     type Config = NoConfig;
//!     type SystemConfig = NoConfig;
//!     fn manifest() -> Manifest {
//!         Manifest { name: "Hello".into(), subscribe: vec![topic::CALL_CONCLUDED.into()], ..trunk_recorder_plugin::manifest!() }
//!     }
//!     fn start(host: Host, _: Setup<NoConfig, NoConfig>) -> Result<Self, String> {
//!         Ok(Hello { host })
//!     }
//!     fn call_concluded(&mut self, call: ConcludedCall) {
//!         self.host.info(format!("TG {} for {} s", call.call.talkgroup, call.call.call_length));
//!     }
//! }
//!
//! fn main() {
//!     trunk_recorder_plugin::run::<Hello>();
//! }
//! ```

mod multipart;
pub mod protocol;
#[cfg(feature = "sdk")]
mod queue;
#[cfg(feature = "sdk")]
pub mod schema;
#[cfg(feature = "sdk")]
mod sdk;
#[cfg(feature = "sdk")]
pub mod testing;
#[cfg(feature = "sdk")]
pub use queue::{Attempt, CallQueue, QueueOptions};

pub use multipart::Multipart;
pub use protocol::*;
#[cfg(feature = "sdk")]
pub use sdk::*;
