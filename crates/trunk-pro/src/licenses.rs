//! The survey's license lookup: what it heard, and the location the user
//! gave, to the trunkrecorder.pro API (`POST /api/v1/fcc/identify`), which
//! answers with the FCC licenses near there on each frequency. Only when the
//! user asks ("Identify surveyed sites"); nothing is sent otherwise.

use std::time::Duration;

use serde_json::Value;

/// The API's answer, or why there is none (for the user).
pub fn identify(api_base: &str, request: &Value) -> Result<Value, String> {
    let url = format!("{api_base}/api/v1/fcc/identify");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION")))
        // Its errors are JSON too: read them.
        .http_status_as_error(false)
        .build()
        .into();
    let unreachable = |e: ureq::Error| format!("Couldn't reach {api_base} to look up licenses ({e}). The survey works without it.");
    let mut r = agent.post(&url).header("Content-Type", "application/json").send(request.to_string()).map_err(unreachable)?;
    let status = r.status().as_u16();
    let body = r.body_mut().with_config().limit(4 << 20).read_to_vec().map_err(unreachable)?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match status {
        200 if v["results"].is_array() => Ok(v),
        200 => Err(format!("{url} answered with something other than licenses.")),
        429 => Err("Too many license lookups for now: try again in a minute.".into()),
        _ => Err(format!("The license lookup failed: {}", v["error"].as_str().map_or_else(|| format!("HTTP {status}"), str::to_string))),
    }
}
