//! The app layer shared by the desktop app (`trunk-pro`) and the web build
//! (`trunk-web`): the config, conventional channel CSVs, a
//! platform-independent recording [`Session`] and the first-run
//! [`survey`] that finds a system.

#[macro_use]
mod choice;
pub mod calls;
pub mod channels;
pub mod config;
pub mod ekc;
pub mod filename;
pub mod heard;
pub mod keys;
pub mod licenses;
pub mod log;
pub mod migrate;
pub mod profile;
pub mod protocol;
pub mod samples;
pub mod saving;
pub mod session;
pub mod share;
pub mod stats;
pub mod survey;

pub use config::Config;
pub use session::{Feeder, Output, PluginTopics, Session};
