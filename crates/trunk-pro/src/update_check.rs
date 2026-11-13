//! Is a newer Trunk Recorder Pro release on GitHub than the one running?
//! Checked once a day ([`EVERY`]), from [`crate::monitor`], and folded into
//! the `platform` field of the `host` message so every connection just sees
//! it (or doesn't) the way it sees Spotlight's state. Off when
//! `server.checkForUpdates` is false (`crate::licenses`'s FCC lookups are the
//! same idea: nothing goes out that wasn't asked for).

use std::time::Duration;

use serde_json::{json, Value};

const REPO: &str = "TrunkRecorder/trunk-recorder-pro";

/// How often GitHub is asked.
pub const EVERY: Duration = Duration::from_secs(24 * 3600);

/// `{version, url}` of the latest release, only when it's newer than
/// `current` (so a client that sees nothing needs no "you're up to date"
/// logic of its own). `None` on any error too — offline, rate-limited,
/// GitHub down — never surfaced as a problem; it's just checked again next time.
pub fn check(current: &str) -> Option<Value> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(10))).user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION"))).build().into();
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut r = agent.get(&url).header("Accept", "application/vnd.github+json").call().ok()?;
    if !r.status().is_success() {
        return None;
    }
    let text = r.body_mut().read_to_string().ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let tag = v["tag_name"].as_str()?.trim_start_matches('v');
    let html_url = v["html_url"].as_str()?;
    newer(tag, current).then(|| json!({ "version": tag, "url": html_url }))
}

/// `a.b.c`, missing parts taken as 0 ("0.4" is "0.4.0"); None if `a` isn't a number.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next().map_or(Ok(0), str::parse).ok()?;
    let c = it.next().map_or(Ok(0), str::parse).ok()?;
    Some((a, b, c))
}

fn newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_releases_beat_the_version_running_now() {
        assert!(newer("0.4.0", "0.3.7"));
        assert!(newer("0.3.8", "0.3.7"));
        assert!(!newer("0.3.7", "0.3.7"), "same version isn't newer");
        assert!(!newer("0.3.0", "0.3.7"), "older isn't newer");
        assert!(!newer("bogus", "0.3.7"), "not a version: never newer");
        assert!(!newer("0.4.0", "bogus"), "can't tell what's running: never newer");
    }

    #[test]
    fn parses_short_and_long_versions() {
        assert_eq!(parse("0.4"), Some((0, 4, 0)));
        assert_eq!(parse("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse(""), None);
        assert_eq!(parse("v1.2.3"), None, "the leading v is stripped by the caller, not here");
    }
}
