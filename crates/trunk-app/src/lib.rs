//! The app layer shared by the desktop app (`trunk-lite`) and the web build
//! (`trunk-web`): the config and a platform-independent recording [`Session`].

pub mod config;
pub mod session;

pub use config::Config;
pub use session::{Output, Session};
