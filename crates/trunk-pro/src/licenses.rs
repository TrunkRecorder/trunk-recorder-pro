//! FCC license lookups at the trunkrecorder.pro API: who is licensed on some
//! frequencies near the location the user gave (`POST /api/v1/fcc/identify`),
//! or who holds some call signs heard on the air (`POST /api/v1/fcc/callsigns`,
//! no location). Only when the user asks ("Identify"); nothing is sent otherwise.

use std::time::Duration;

use serde_json::Value;

/// The licenses on each frequency: the API's answer, or why there is none (for the user).
pub fn identify(api_base: &str, request: &Value) -> Result<Value, String> {
    post(api_base, "identify", request)
}

/// Who holds each call sign.
pub fn call_signs(api_base: &str, call_signs: &[String]) -> Result<Value, String> {
    post(api_base, "callsigns", &serde_json::json!({ "callSigns": call_signs }))
}

fn post(api_base: &str, what: &str, request: &Value) -> Result<Value, String> {
    let url = format!("{api_base}/api/v1/fcc/{what}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION")))
        // Its errors are JSON too: read them.
        .http_status_as_error(false)
        .build()
        .into();
    let unreachable = |e: ureq::Error| format!("Couldn't reach {api_base} to look up licenses ({e}).");
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
