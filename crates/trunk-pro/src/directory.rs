//! Sharing with the community directory (trunkrecorder.pro/directory): a
//! system whose `share` is on sends what this recorder hears of it
//! ([`trunk_app::share`] builds it, and says when) to
//! `POST /directory/api/v1/observations`, under the install's anonymous
//! recorder ID. The answer links the system and its sites to the
//! directory's records, kept in the config so the next share lands on them.

use std::time::Duration;

use serde_json::Value;

/// Where a share went: the directory's system, its version, and each site sent's ID (in order).
pub struct Shared {
    pub system: String,
    pub rev: u64,
    pub sites: Vec<String>,
}

/// This install's recorder ID, made the first time (kept in the data folder).
pub fn recorder_id() -> String {
    let path = crate::paths::data_dir().join("directory-recorder-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim();
        if s.starts_with("r_") && s.len() >= 12 {
            return s.to_string();
        }
    }
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        return String::new();
    }
    let id = trunk_app::share::recorder_id(&b);
    if let Err(e) = std::fs::write(&path, &id) {
        log::warn!("Couldn't keep the directory recorder ID in {}: {e}", path.display());
    }
    id
}

/// Send one share; what the directory made of it, or why it didn't take it (for the user).
pub fn share(api_base: &str, observation: &Value) -> Result<Shared, String> {
    let url = format!("{api_base}/directory/api/v1/observations");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION")))
        // Its errors are JSON too: read them.
        .http_status_as_error(false)
        .build()
        .into();
    let unreachable = |e: ureq::Error| format!("Couldn't reach {api_base} ({e}).");
    let mut r = agent.post(&url).header("Content-Type", "application/json").send(observation.to_string()).map_err(unreachable)?;
    let status = r.status().as_u16();
    let body = r.body_mut().with_config().limit(1 << 20).read_to_vec().map_err(unreachable)?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match status {
        201 | 202 => answer(&v).ok_or_else(|| format!("{url} answered with something other than a share.")),
        429 => Err("Too many shares from this address for now: it tries again later.".into()),
        _ => Err(format!("The directory didn't take it: {}", v["error"].as_str().map_or_else(|| format!("HTTP {status}"), str::to_string))),
    }
}

/// The directory's answer: `{system, rev, sites}`. (A quarantined recorder's, 202, has no system: nothing to link.)
fn answer(v: &Value) -> Option<Shared> {
    let sites = v["sites"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default();
    match v["system"].as_str() {
        Some(s) => Some(Shared { system: s.to_string(), rev: v["rev"].as_u64().unwrap_or(0), sites }),
        None if v.is_object() => Some(Shared { system: String::new(), rev: 0, sites }),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        let a = answer(&serde_json::json!({"system": "sys_1", "rev": 4, "sites": ["site_a", "site_b"], "created": false})).unwrap();
        assert_eq!((a.system.as_str(), a.rev, a.sites.len()), ("sys_1", 4, 2));
        let q = answer(&serde_json::json!({"observation": "obs_1"})).unwrap();
        assert!(q.system.is_empty());
        assert!(answer(&Value::Null).is_none());
    }
}
