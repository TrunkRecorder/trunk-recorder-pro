//! Sending a capture to the public corpus (research/iq-corpus.md), after its
//! owner has reviewed it. Resumable: what's been sent is kept in the capture's
//! `.send.json`, and a send interrupted by a restart goes on from there.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use trunk_app::protocol::IqCaptureSend;

use crate::runtime::Ctx;
use crate::toolkit::hex;

const API: &str = "https://trunkrecorder.pro/corpus/api/v1";
/// The service is up. Until then only a test's `TRP_CORPUS_API` makes the Send button appear.
const SERVICE_LIVE: bool = false;
const PART_BYTES: u64 = 32 << 20;
const STATE: &str = ".send.json";
/// The most sent per second, so a send doesn't take over a home connection.
const MAX_BYTES_PER_S: f64 = 5e6;
const TRIES: u32 = 6;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct SendFile {
    name: String,
    sha256: String,
    bytes: u64,
    /// The service has it already, or it's all sent.
    done: bool,
    /// Parts sent so far.
    parts: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct SendState {
    /// `sending`, `sent` or `failed`.
    state: String,
    id: String,
    receipt: String,
    error: String,
    files: Vec<SendFile>,
}

impl SendState {
    fn progress(&self) -> (u64, u64) {
        let total = self.files.iter().map(|f| f.bytes).sum();
        let done = self.files.iter().map(|f| if f.done { f.bytes } else { (f.parts * PART_BYTES).min(f.bytes) }).sum();
        (done, total)
    }
}

/// Whether there's a service to send to.
pub fn available() -> bool {
    SERVICE_LIVE || std::env::var_os("TRP_CORPUS_API").is_some()
}

fn api() -> String {
    std::env::var("TRP_CORPUS_API").unwrap_or_else(|_| API.to_string())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(300))).user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION"))).build().into()
}

fn read_state(dir: &Path) -> Option<SendState> {
    serde_json::from_str(&fs::read_to_string(dir.join(STATE)).ok()?).ok()
}

fn write_state(dir: &Path, s: &SendState) -> Result<(), String> {
    fs::write(dir.join(STATE), serde_json::to_string(s).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", dir.display()))
}

/// How the sending of the capture in `dir` stands, for the list.
pub fn status(dir: &Path) -> Option<IqCaptureSend> {
    let s = read_state(dir)?;
    let (done, total) = s.progress();
    Some(IqCaptureSend { state: s.state.clone(), receipt: s.receipt.clone(), id: s.id.clone(), done_bytes: done, total_bytes: total, error: (!s.error.is_empty()).then(|| s.error.clone()) })
}

/// The answer to `r`, or why there isn't one.
fn answer(r: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value, String> {
    let mut r = r.map_err(|e| format!("Couldn't reach the corpus service ({e})."))?;
    let code = r.status().as_u16();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    if !(200..300).contains(&code) {
        return Err(serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_else(|| format!("The corpus service said {code}.")));
    }
    Ok(serde_json::from_str(&text).unwrap_or(json!({})))
}

/// This install's token, asked for once.
fn token() -> Result<String, String> {
    let path = crate::paths::data_dir().join("corpus-install.json");
    if let Some(t) = fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["token"].as_str().map(str::to_string)) {
        return Ok(t);
    }
    let v = answer(agent().post(format!("{}/installs", api())).send_empty())?;
    let t = v["token"].as_str().ok_or("The corpus service gave no token.")?.to_string();
    fs::write(&path, json!({ "token": t }).to_string()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(t)
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut f = File::open(path).map_err(fail)?;
    let mut h = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(fail)?;
        if n == 0 {
            return Ok(hex(&h.finalize()));
        }
        h.update(&buf[..n]);
    }
}

/// The files of the capture in `dir` that get sent.
fn files_of(dir: &Path) -> Result<Vec<SendFile>, String> {
    let mut names: Vec<String> = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n == crate::iq_capture::DIAGNOSTICS || [".sigmf-meta", ".sigmf-data", ".sigmf-collection"].iter().any(|x| n.ends_with(x))).collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let p = dir.join(&name);
            Ok(SendFile { bytes: fs::metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?.len(), sha256: sha256_of(&p)?, name, done: false, parts: 0 })
        })
        .collect()
}

/// What the corpus list should say this capture is of.
fn protocol_of(meta: &Value) -> String {
    let g = &meta["global"];
    if let Some(t) = g["trp:config"]["systems"][0]["type"].as_str() {
        return t.to_string();
    }
    if g["trp:config"]["conventional"].as_array().is_some_and(|c| !c.is_empty()) {
        return "conventional".into();
    }
    g["trp:channel"]["spec"]["kind"].as_str().unwrap_or("unknown").to_string()
}

fn with_retries<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut last = String::new();
    for attempt in 0..TRIES {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
    }
    Err(last)
}

fn send(dir: &Path, s: &mut SendState) -> Result<(), String> {
    let token = token()?;
    let auth = format!("Bearer {token}");
    if s.id.is_empty() {
        let meta: Value = serde_json::from_str(&fs::read_to_string(dir.join("source-0.sigmf-meta")).map_err(|e| format!("{}: {e}", dir.display()))?).map_err(|e| e.to_string())?;
        s.files = files_of(dir)?;
        let g = &meta["global"];
        let body = json!({ "purpose": g["trp:purpose"], "tier": g["trp:tier"], "protocol": protocol_of(&meta), "note": g["core:description"], "files": s.files.iter().map(|f| json!({ "name": f.name, "sha256": f.sha256, "bytes": f.bytes })).collect::<Vec<_>>() });
        let v = answer(agent().post(format!("{}/submissions", api())).header("Authorization", &auth).header("Content-Type", "application/json").send(body.to_string()))?;
        s.id = v["id"].as_str().ok_or("The corpus service gave no id.")?.to_string();
        s.receipt = v["receipt"].as_str().unwrap_or("").to_string();
        for f in &mut s.files {
            f.done = v["have"].as_array().is_some_and(|h| h.iter().any(|x| x == f.sha256.as_str()));
        }
        write_state(dir, s)?;
    }
    let id = s.id.clone();
    for i in 0..s.files.len() {
        if s.files[i].done {
            continue;
        }
        let path = dir.join(&s.files[i].name);
        let parts = s.files[i].bytes.div_ceil(PART_BYTES).max(1);
        let mut file = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        while s.files[i].parts < parts {
            let n = s.files[i].parts;
            file.seek(SeekFrom::Start(n * PART_BYTES)).map_err(|e| e.to_string())?;
            let mut buf = Vec::new();
            file.by_ref().take(PART_BYTES).read_to_end(&mut buf).map_err(|e| e.to_string())?;
            let url = format!("{}/submissions/{id}/files/{}/parts/{}", api(), s.files[i].name, n + 1);
            let started = std::time::Instant::now();
            with_retries(|| answer(agent().put(&url).header("Authorization", &auth).send(&buf[..])))?;
            let pace = Duration::from_secs_f64(buf.len() as f64 / MAX_BYTES_PER_S).saturating_sub(started.elapsed());
            std::thread::sleep(pace);
            s.files[i].parts += 1;
            write_state(dir, s)?;
        }
        s.files[i].done = true;
        write_state(dir, s)?;
    }
    with_retries(|| answer(agent().post(format!("{}/submissions/{id}/complete", api())).header("Authorization", &auth).send_empty()))?;
    Ok(())
}

fn run(ctx: &Ctx, dir: PathBuf) {
    let mut s = read_state(&dir).unwrap_or_default();
    s.state = "sending".into();
    s.error.clear();
    let _ = write_state(&dir, &s);
    match send(&dir, &mut s) {
        Ok(()) => {
            s.state = "sent".into();
            log::info!("Capture sent to the corpus: {}", s.id);
        }
        Err(e) => {
            log::warn!("Capture not sent: {e}");
            s.state = "failed".into();
            s.error = e;
        }
    }
    let _ = write_state(&dir, &s);
    *ctx.iq.sending.lock().unwrap() = false;
}

/// Send the capture in `dir` in the background, one at a time.
pub fn start(ctx: &Arc<Ctx>, dir: PathBuf) -> Result<(), String> {
    {
        let mut sending = ctx.iq.sending.lock().unwrap();
        if *sending {
            return Err("Another capture is being sent. Try again when it's done.".into());
        }
        *sending = true;
    }
    let ctx2 = ctx.clone();
    if let Err(e) = std::thread::Builder::new().name("iq-send".into()).spawn(move || run(&ctx2, dir)) {
        *ctx.iq.sending.lock().unwrap() = false;
        return Err(e.to_string());
    }
    Ok(())
}

/// Withdraw a sent capture: the service forgets it, and it can be sent again.
pub fn withdraw(dir: &Path) -> Result<(), String> {
    let s = read_state(dir).ok_or("That capture hasn't been sent.")?;
    if !s.id.is_empty() {
        answer(agent().delete(format!("{}/submissions/{}", api(), s.id)).header("X-Receipt", &s.receipt).call())?;
    }
    fs::remove_file(dir.join(STATE)).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Sends a restart interrupted, going on.
pub fn resume(ctx: &Arc<Ctx>, root: &Path) {
    let unfinished = fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).find(|d| read_state(d).is_some_and(|s| s.state == "sending"));
    if let Some(dir) = unfinished {
        log::info!("Going on sending {}", dir.display());
        let _ = start(ctx, dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_counts_parts_and_whole_files() {
        let s = SendState { files: vec![SendFile { bytes: PART_BYTES * 2 + 5, parts: 1, ..Default::default() }, SendFile { bytes: 100, done: true, ..Default::default() }], ..Default::default() };
        assert_eq!(s.progress(), (PART_BYTES + 100, PART_BYTES * 2 + 105));
    }

    #[test]
    fn what_the_list_calls_a_capture() {
        assert_eq!(protocol_of(&json!({ "global": { "trp:config": { "systems": [{ "type": "smartnet" }] } } })), "smartnet");
        assert_eq!(protocol_of(&json!({ "global": { "trp:config": { "systems": [], "conventional": [{}] } } })), "conventional");
        assert_eq!(protocol_of(&json!({ "global": { "trp:channel": { "spec": { "kind": "p25" } } } })), "p25");
    }

    #[test]
    fn only_a_captures_own_files_are_sent() {
        let dir = std::env::temp_dir().join(format!("trp-send-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for n in ["source-0.sigmf-meta", "source-0.sigmf-data", "capture.sigmf-collection", "diagnostics.json", ".send.json", "notes.txt"] {
            fs::write(dir.join(n), n).unwrap();
        }
        let names: Vec<String> = files_of(&dir).unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["capture.sigmf-collection", "diagnostics.json", "source-0.sigmf-data", "source-0.sigmf-meta"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
