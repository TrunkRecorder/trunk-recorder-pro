//! Phone alerts through ntfy, sent when the box's state changes and holds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::rules::Level;

/// Kept in the plugin's data folder across restarts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    /// The level last told (OK, WARN, BAD).
    pub reported: String,
    /// A new level seen, and in how many checks in a row.
    pub seen: String,
    pub count: u32,
    /// The recorder vanished without stopping the plugin (told once it's back).
    pub went_away: bool,
}

pub fn memory_path(data_dir: &Path) -> PathBuf {
    data_dir.join("health-state.json")
}

pub fn load(data_dir: &Path) -> Memory {
    std::fs::read_to_string(memory_path(data_dir)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Memory { reported: "OK".into(), ..Default::default() })
}

pub fn save(data_dir: &Path, m: &Memory) {
    if let Ok(s) = serde_json::to_string(m) {
        let _ = std::fs::write(memory_path(data_dir), s);
    }
}

/// Post one message to an ntfy topic URL.
pub fn ntfy(url: &str, token: &str, title: &str, body: &str, level: Level) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("no ntfy topic set".into());
    }
    let url = if url.starts_with("http://") || url.starts_with("https://") { url.to_string() } else { format!("https://ntfy.sh/{url}") };
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).http_status_as_error(true).build().into();
    let title: String = title.chars().map(|c| if c.is_ascii() && !c.is_ascii_control() { c } else { '-' }).collect();
    let mut req = agent
        .post(&url)
        .header("Title", &title)
        .header("Priority", match level {
            Level::Bad => "high",
            Level::Warn => "default",
            _ => "low",
        })
        .header("Tags", match level {
            Level::Bad => "rotating_light",
            Level::Warn => "warning",
            _ => "white_check_mark",
        });
    let auth;
    if !token.trim().is_empty() {
        auth = format!("Bearer {}", token.trim());
        req = req.header("Authorization", &auth);
    }
    req.send(body).map(|_| ()).map_err(|e| e.to_string())
}

/// What to tell, if anything, given this check's level: a change has to hold
/// for `after` checks in a row. `floor`: the least level worth telling.
pub fn decide(m: &mut Memory, now: Level, floor: Level, after: u32) -> Option<Level> {
    let eff = if now >= floor { now } else { Level::Ok };
    let word = eff.word().to_string();
    if m.reported.is_empty() {
        m.reported = "OK".into();
    }
    if word == m.reported {
        m.seen.clear();
        m.count = 0;
        return None;
    }
    if m.seen == word {
        m.count += 1;
    } else {
        m.seen = word;
        m.count = 1;
    }
    (m.count >= after.max(1)).then_some(eff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_is_told_once_it_holds() {
        let mut m = Memory { reported: "OK".into(), ..Default::default() };
        assert_eq!(decide(&mut m, Level::Warn, Level::Warn, 2), None);
        assert_eq!(decide(&mut m, Level::Warn, Level::Warn, 2), Some(Level::Warn));
        m.reported = "WARN".into();
        assert_eq!(decide(&mut m, Level::Warn, Level::Warn, 2), None);
        // A blip back to OK isn't news.
        assert_eq!(decide(&mut m, Level::Ok, Level::Warn, 2), None);
        assert_eq!(decide(&mut m, Level::Warn, Level::Warn, 2), None);
        // WARN below the floor counts as OK.
        assert_eq!(decide(&mut m, Level::Warn, Level::Bad, 1), Some(Level::Ok));
    }
}
