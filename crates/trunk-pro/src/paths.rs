//! Where the app keeps what it learns and installs: the folder its config
//! file is in. With the default config that is the app's config folder
//! ([`crate::config::config_dir`]); with `--config elsewhere/config.json`,
//! `elsewhere/` — so two recorders with their own configs never share band
//! plans, talker aliases, heard codes or plugin data.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The config file `--config` names, or the default one.
pub fn config_path(arg: Option<&str>) -> PathBuf {
    arg.map(PathBuf::from).unwrap_or_else(|| crate::config::config_dir().join("config.json"))
}

/// Keep everything beside `config_path` from now on. Called once, when the
/// config is known (a later call is ignored).
pub fn init(config_path: &Path) {
    let dir = match config_path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    // Absolute: plugins run in their own data folders, where a relative path would point elsewhere.
    let _ = DATA_DIR.set(std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()));
}

/// The folder band plans, talker aliases, heard codes and plugins are kept in.
pub fn data_dir() -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(crate::config::config_dir)
}
