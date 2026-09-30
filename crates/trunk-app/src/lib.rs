//! The app layer shared by the desktop app (`trunk-pro`) and the web build
//! (`trunk-web`): the config, conventional channel CSVs, a
//! platform-independent recording [`Session`] and the first-run
//! [`survey`] that finds a system.

pub mod channels;
pub mod config;
pub mod samples;
pub mod session;
pub mod survey;

pub use config::Config;
pub use session::{Output, PluginTopics, Session};
