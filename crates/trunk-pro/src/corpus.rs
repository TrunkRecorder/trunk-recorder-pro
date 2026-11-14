//! `trunk-pro tool corpus`: the recordings regression tests run on
//! (research/iq-corpus.md). A manifest lists each capture's
//! files by SHA-256; they're fetched by that hash into a local cache, and a
//! capture is its files under their own names again, for `replay`.
//!
//! `tool corpus add <capture folder> --manifest FILE --id ID --protocol P [--tier wideband]`
//! puts a capture in a local manifest, and its files in the cache.
//! `tool corpus fetch [--only a,b] [--tier t]` downloads what the cache lacks.
//! `tool corpus run --out DIR [--only a,b] [--tier t] [--against V] [--bless next|V]`
//! replays each capture into `DIR/<id>`, as `captures/regress.sh` lays its runs
//! out, and compares each with its expected output; `--bless` keeps what it got
//! as the expected output of the next release, or of version V.
//! `tool corpus release V` makes the unreleased expected output V's.
//!
//! Expected output is by TRP version: `expected/<id>/<version>.jsonl` beside the
//! manifest, one for each version whose output changed, and `next.jsonl` for
//! what's unreleased. Hashes and counts: no decoded traffic.
//!
//! The list of captures lives on the service, where maintainers edit it.
//! All take `--manifest` (a URL or a file: the service's list), `--expected`
//! (corpus/expected), `--cache` (`TRP_CORPUS_CACHE`,
//! or captures/.corpus) and `--base`, the URL files are under.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::iq_capture;
use crate::toolkit::{hex, sha256_hex};
use crate::{die, Args};

const EXPECTED: &str = "corpus/expected";
const CACHE: &str = "captures/.corpus";
const BASE: &str = "https://iq.trunkrecorder.pro/blobs/";
const API: &str = "https://trunkrecorder.pro/corpus/api/v1";

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    captures: Vec<Entry>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Entry {
    id: String,
    tier: String,
    protocol: String,
    seconds: f64,
    bytes: u64,
    files: Vec<FileRef>,
    /// `active`, `known-failing` or `retired` (not run).
    state: String,
    issue: Option<u64>,
    /// Messages, or a call's good frames, it may fall short by before that's a regression.
    #[serde(default, skip_serializing_if = "is_zero")]
    tolerance: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct FileRef {
    name: String,
    sha256: String,
    bytes: u64,
}

/// A name that's one plain file or folder name: a manifest can come from anyone's pull request.
fn safe_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn safe_hash(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

fn parse_manifest(text: &str, from: &str) -> Result<Manifest, String> {
    let m: Manifest = serde_json::from_str(text).map_err(|e| format!("{from}: {e}"))?;
    for e in &m.captures {
        if !safe_name(&e.id) {
            return Err(format!("{from}: {:?} isn't a usable capture id", e.id));
        }
        if let Some(f) = e.files.iter().find(|f| !safe_name(&f.name) || !safe_hash(&f.sha256)) {
            return Err(format!("{from}: capture {} has a bad file entry: {:?}", e.id, f.name));
        }
    }
    Ok(m)
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    parse_manifest(&fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?, &path.display().to_string())
}

/// `url`'s text; None if it isn't there.
fn get_text(url: &str) -> Result<Option<String>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION"))).build().into();
    let mut r = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    if r.status().as_u16() == 404 {
        return Ok(None);
    }
    if !r.status().is_success() {
        return Err(format!("{url}: {}", r.status()));
    }
    r.body_mut().read_to_string().map(Some).map_err(|e| format!("{url}: {e}"))
}

/// The list of captures, from the service or a file.
fn load_manifest(from: &str) -> Result<Manifest, String> {
    if from.starts_with("http://") || from.starts_with("https://") {
        return parse_manifest(&get_text(from)?.ok_or_else(|| format!("{from}: not found"))?, from);
    }
    read_manifest(Path::new(from))
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut file = File::open(path).map_err(fail)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(fail)?;
        if n == 0 {
            return Ok(hex(&hash.finalize()));
        }
        hash.update(&buf[..n]);
    }
}

fn blob_path(cache: &Path, sha256: &str) -> PathBuf {
    cache.join("blobs").join(sha256)
}

/// A second name for `from`, or a copy where the disk can't do that.
fn link(from: &Path, to: &Path) -> Result<(), String> {
    fs::hard_link(from, to).or_else(|_| fs::copy(from, to).map(|_| ())).map_err(|e| format!("{}: {e}", to.display()))
}

/// Put the capture in `folder` in the manifest (replacing one of the same id) and its files in the cache.
fn add(manifest: &Path, cache: &Path, folder: &Path, id: &str, tier: &str, protocol: &str) -> Result<(), String> {
    if !safe_name(id) {
        return Err(format!("{id:?} isn't a usable capture id (letters, digits, . - _)"));
    }
    let name = folder.to_str().ok_or("The folder's path isn't text")?;
    let opened = iq_capture::open(name)?;
    let first = opened.recordings.first().ok_or("It has no recordings")?;
    let seconds = (fs::metadata(&first.data).map_err(|e| format!("{}: {e}", first.data.display()))?.len() / first.format.bytes_per_sample() as u64) as f64 / first.rate_hz;
    let mut names: Vec<String> = fs::read_dir(folder).map_err(|e| format!("{name}: {e}"))?.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n == iq_capture::DIAGNOSTICS || [".sigmf-meta", ".sigmf-data", ".sigmf-collection"].iter().any(|x| n.ends_with(x))).collect();
    names.sort();
    fs::create_dir_all(cache.join("blobs")).map_err(|e| format!("{}: {e}", cache.display()))?;
    let mut files = Vec::new();
    for n in names {
        let path = folder.join(&n);
        let sha256 = sha256_of(&path)?;
        let blob = blob_path(cache, &sha256);
        if !blob.exists() {
            link(&path, &blob)?;
        }
        files.push(FileRef { bytes: fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?.len(), name: n, sha256 });
    }
    let entry = Entry { id: id.into(), tier: tier.into(), protocol: protocol.into(), seconds, bytes: files.iter().map(|f| f.bytes).sum(), files, state: "active".into(), issue: None, tolerance: 0 };
    let mut m = if manifest.exists() { read_manifest(manifest)? } else { Manifest { version: 1, captures: vec![] } };
    match m.captures.iter_mut().find(|e| e.id == id) {
        Some(e) => *e = entry,
        None => m.captures.push(entry),
    }
    if let Some(dir) = manifest.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(manifest, serde_json::to_string_pretty(&m).map_err(|e| e.to_string())? + "\n").map_err(|e| format!("{}: {e}", manifest.display()))
}

/// `f` in the cache, downloaded if it isn't there: an interrupted download
/// goes on where it stopped, and nothing is kept under its hash until it matches.
fn fetch(base: &str, cache: &Path, f: &FileRef) -> Result<PathBuf, String> {
    let blob = blob_path(cache, &f.sha256);
    if fs::metadata(&blob).is_ok_and(|m| m.len() == f.bytes) {
        return Ok(blob);
    }
    fs::create_dir_all(cache.join("blobs")).map_err(|e| format!("{}: {e}", cache.display()))?;
    let part = blob.with_extension("part");
    let fail = |e: std::io::Error| format!("{}: {e}", part.display());
    let have = fs::metadata(&part).map_or(0, |m| m.len());
    if have < f.bytes {
        let url = format!("{base}{}", f.sha256);
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION"))).build().into();
        let mut req = agent.get(&url);
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let mut r = req.call().map_err(|e| format!("{url}: {e}"))?;
        let resumed = r.status().as_u16() == 206;
        if !resumed && r.status().as_u16() != 200 {
            return Err(format!("{url}: {}", r.status()));
        }
        let mut out = OpenOptions::new().create(true).write(true).append(resumed).truncate(!resumed).open(&part).map_err(fail)?;
        std::io::copy(&mut r.body_mut().as_reader(), &mut out).map_err(fail)?;
        out.flush().map_err(fail)?;
    }
    let got = sha256_of(&part)?;
    if got != f.sha256 {
        let _ = fs::remove_file(&part);
        return Err(format!("{}: downloaded file doesn't match its hash (got {got})", f.name));
    }
    fs::rename(&part, &blob).map_err(fail)?;
    Ok(blob)
}

/// A capture's files under their own names, in the cache: what `replay` is given.
fn lay_out(base: &str, cache: &Path, e: &Entry) -> Result<PathBuf, String> {
    let dir = cache.join("captures").join(&e.id);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    for f in &e.files {
        link(&fetch(base, cache, f)?, &dir.join(&f.name))?;
    }
    Ok(dir)
}

/// The manifest's captures the options pick, in its order (retired ones never).
fn chosen<'a>(m: &'a Manifest, only: Option<&str>, tier: Option<&str>) -> Result<Vec<&'a Entry>, String> {
    let wanted: Option<Vec<&str>> = only.map(|o| o.split(',').map(str::trim).collect());
    if let Some(missing) = wanted.iter().flatten().find(|w| !m.captures.iter().any(|e| e.id == **w)) {
        return Err(format!("No capture {missing:?} in the manifest"));
    }
    Ok(m.captures.iter().filter(|e| e.state != "retired" && wanted.as_ref().is_none_or(|w| w.contains(&e.id.as_str())) && tier.is_none_or(|t| e.tier == t)).collect())
}

/// A call as expected output keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CallSummary {
    /// A hash of its talkgroup, slot and encryption.
    pub c: String,
    pub f: u64,
    /// When it started, and for how long, ms.
    pub t: i64,
    pub l: i64,
    #[serde(default)]
    pub frames: u64,
    #[serde(default)]
    pub bad: u64,
    #[serde(default)]
    pub errs: u64,
    /// Digital: a hash of each [`FRAMES_PER_HASH`] frames' bits.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub h: String,
    /// Analog: its loudness each [`ENVELOPE_MS`] ms, 0–255 of its peak, in hex.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub env: String,
}

impl CallSummary {
    fn good(&self) -> u64 {
        self.frames.saturating_sub(self.bad)
    }
}

/// What a replay decoded, as expected output keeps it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Results {
    /// How many of each control message, by a hash of it.
    pub messages: BTreeMap<String, u64>,
    /// What type of message each hash is ("grant", "update"), so a report can say.
    pub kinds: BTreeMap<String, String>,
    pub calls: Vec<CallSummary>,
}

impl Results {
    /// One more control message: its text (hashed, and not kept) and its type.
    pub fn add_message(&mut self, text: &str, kind: &str) {
        let hash = short_hash(text.as_bytes(), 8);
        *self.messages.entry(hash.clone()).or_insert(0) += 1;
        self.kinds.insert(hash, kind.to_string());
    }
}

const FRAMES_PER_HASH: usize = 20;
const ENVELOPE_MS: usize = 100;
/// Analog calls whose loudness over time matches worse than this have changed for the worse.
const MIN_ENVELOPE_R: f64 = 0.9;
const SAME_ENVELOPE_R: f64 = 0.999;
/// A call whose length moved by more than this has changed length, ms.
const SAME_LENGTH_MS: i64 = 500;
/// How many of a capture's differences the log shows (the report files have them all).
const LOG_DIFFERENCES: usize = 15;

pub(crate) fn short_hash(bytes: &[u8], hex_chars: usize) -> String {
    sha256_hex(bytes)[..hex_chars].to_string()
}

/// The messages among a replay's `--messages` lines: `  12.34s  [site] kind text`, by what they say, not when.
fn add_messages(stdout: &str, r: &mut Results) {
    for line in stdout.lines() {
        let Some((at, rest)) = line.trim_start().split_once("s  ") else { continue };
        if at.parse::<f64>().is_err() || rest.contains("CALL ") {
            continue;
        }
        let kind = rest.split_whitespace().find(|w| !w.starts_with('[')).unwrap_or("unknown");
        r.add_message(rest, kind);
    }
}

/// A WAV's loudness every [`ENVELOPE_MS`], in hex (none: it isn't 16-bit audio).
fn envelope(wav: &[u8]) -> String {
    let (Some(fmt), Some(data)) = (wav.windows(4).position(|w| w == b"fmt "), wav.windows(4).position(|w| w == b"data")) else { return String::new() };
    if wav.len() < fmt + 24 || wav.len() < data + 8 {
        return String::new();
    }
    let rate = u32::from_le_bytes([wav[fmt + 12], wav[fmt + 13], wav[fmt + 14], wav[fmt + 15]]) as usize;
    let samples: Vec<f64> = wav[data + 8..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f64).collect();
    envelope_of(&samples, rate)
}

/// Audio's loudness every [`ENVELOPE_MS`], 0–255 of its peak, in hex.
pub(crate) fn envelope_of(samples: &[f64], rate: usize) -> String {
    let rms: Vec<f64> = samples.chunks((rate * ENVELOPE_MS / 1000).max(1)).map(|w| (w.iter().map(|v| v * v).sum::<f64>() / w.len() as f64).sqrt()).collect();
    let peak = rms.iter().copied().fold(1e-9, f64::max);
    hex(&rms.iter().map(|v| (v / peak * 255.0).round() as u8).collect::<Vec<u8>>())
}

/// A hash of each [`FRAMES_PER_HASH`] frames' bits, joined.
pub(crate) fn frame_hashes(frames: &[trunk_core::trunk::frames::VoiceFrame]) -> String {
    frames.chunks(FRAMES_PER_HASH).map(|w| short_hash(&w.iter().flat_map(|f| f.bits.iter().copied()).collect::<Vec<u8>>(), 4)).collect()
}

fn summarize_call(json_path: &Path) -> Result<CallSummary, String> {
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(json_path).map_err(|e| format!("{}: {e}", json_path.display()))?).map_err(|e| format!("{}: {e}", json_path.display()))?;
    let n = |k: &str| v[k].as_i64().unwrap_or(0);
    let mut c = CallSummary { c: short_hash(format!("{}|{}|{}", n("talkgroup"), n("tdma_slot"), n("encrypted")).as_bytes(), 8), f: n("freq") as u64, t: n("start_time_ms"), l: n("call_length_ms"), ..Default::default() };
    let frames = [json_path.with_extension("sdr"), json_path.with_extension("frames.jsonl")].into_iter().find(|p| p.exists());
    if let Some(path) = frames {
        let frames = crate::tool::load_frames(path.to_str().ok_or("A path isn't text")?);
        c.frames = frames.len() as u64;
        c.bad = frames.iter().filter(|f| f.bad()).count() as u64;
        c.errs = frames.iter().map(|f| f.errs as u64).sum();
        c.h = frame_hashes(&frames);
    } else if v["audio_type"] == "analog" {
        c.env = envelope(&fs::read(json_path.with_extension("wav")).unwrap_or_default());
    }
    Ok(c)
}

/// Every call's JSON under `dir` (several systems: a folder each).
fn call_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            call_files(&p, found);
        } else if p.extension().is_some_and(|x| x == "json") {
            found.push(p);
        }
    }
}

pub(crate) fn summarize(out: &Path, id: &str) -> Result<Results, String> {
    let stdout_path = out.join(format!("{id}.stdout"));
    let stdout = fs::read_to_string(&stdout_path).map_err(|e| format!("{}: {e}", stdout_path.display()))?;
    let mut files = Vec::new();
    call_files(&out.join(id), &mut files);
    let mut calls = files.iter().map(|p| summarize_call(p)).collect::<Result<Vec<_>, _>>()?;
    calls.sort_by(|a, b| (a.t, &a.c, a.f).cmp(&(b.t, &b.c, b.f)));
    let mut r = Results { calls, ..Default::default() };
    add_messages(&stdout, &mut r);
    Ok(r)
}

/// The lines of an expected-output file after its header, as text.
fn results_text(r: &Results) -> String {
    let mut out = String::new();
    for (m, n) in &r.messages {
        out += &match r.kinds.get(m) {
            Some(k) => json!({ "m": m, "n": n, "k": k }),
            None => json!({ "m": m, "n": n }),
        }
        .to_string();
        out.push('\n');
    }
    for c in &r.calls {
        out += &serde_json::to_string(c).unwrap_or_default();
        out.push('\n');
    }
    out
}

fn parse_results(text: &str) -> Result<Results, String> {
    let mut r = Results::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        if v.get("capture").is_some() {
            continue;
        }
        match v["m"].as_str() {
            Some(m) => {
                r.messages.insert(m.to_string(), v["n"].as_u64().unwrap_or(0));
                if let Some(k) = v["k"].as_str() {
                    r.kinds.insert(m.to_string(), k.to_string());
                }
            }
            None => r.calls.push(serde_json::from_value(v).map_err(|e| e.to_string())?),
        }
    }
    Ok(r)
}

fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// Which expected file to compare with, of those in `dir`: `against` if said
/// (a version: the newest at or below it), else `next`, else the newest at or below `version`.
fn expected_file(dir: &Path, version: &str, against: Option<&str>) -> Option<PathBuf> {
    let next = dir.join("next.jsonl");
    if against.is_none_or(|a| a == "next") && next.exists() {
        return Some(next);
    }
    let limit = parse_version(against.filter(|a| *a != "next").unwrap_or(version))?;
    let mut versions: Vec<_> = fs::read_dir(dir).ok()?.flatten().filter_map(|e| e.path().file_stem()?.to_str().and_then(parse_version).map(|v| (v, e.path()))).filter(|(v, _)| *v <= limit).collect();
    versions.sort();
    versions.pop().map(|(_, p)| p)
}

/// What's different about one call.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct CallDifference {
    /// `missing`, `worse`, `shorter`, `audio`, `new`, `longer`, `better` or `bits`.
    change: &'static str,
    /// The hash of its talkgroup, slot and encryption.
    call: String,
    freq_hz: u64,
    start_s: f64,
    detail: String,
}

/// How a run compares with what was expected: counts and hashes, as the log is public.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct Comparison {
    expected_messages: u64,
    decoded_messages: u64,
    missing_messages: u64,
    extra_messages: u64,
    /// How many of each type of message are missing, and new.
    missing_by_type: BTreeMap<String, u64>,
    extra_by_type: BTreeMap<String, u64>,
    expected_calls: u64,
    decoded_calls: u64,
    missing_calls: u64,
    extra_calls: u64,
    /// Calls that got shorter, or longer, by more than [`SAME_LENGTH_MS`].
    shorter_calls: u64,
    longer_calls: u64,
    /// Good frames gained on the calls that fell short of none, and lost on those that did.
    good_frames_gained: u64,
    good_frames_lost: u64,
    /// Calls with a good-frame count no worse, but other bits or errors.
    changed_calls: u64,
    /// Analog calls whose loudness over time no longer matches.
    bad_envelopes: u64,
    /// Every call that differs, the worst first.
    calls: Vec<CallDifference>,
}

fn envelope_r(a: &str, b: &str) -> Option<f64> {
    let bytes = |s: &str| (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()).map(f64::from).collect::<Vec<_>>();
    let (a, b) = (bytes(a), bytes(b));
    let n = a.len().min(b.len());
    if n < 2 {
        return None;
    }
    let (ma, mb) = (a[..n].iter().sum::<f64>() / n as f64, b[..n].iter().sum::<f64>() / n as f64);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for i in 0..n {
        ab += (a[i] - ma) * (b[i] - mb);
        aa += (a[i] - ma).powi(2);
        bb += (b[i] - mb).powi(2);
    }
    if aa == 0.0 || bb == 0.0 {
        return Some(if aa == bb { 1.0 } else { 0.0 });
    }
    Some(ab / (aa * bb).sqrt())
}

/// A call's match among those not yet taken: same call and frequency, starting within half a second.
const SAME_CALL_MS: i64 = 500;

fn seconds(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

fn length_and_frames(c: &CallSummary) -> String {
    if c.frames > 0 {
        format!("{:.1} s long, {} of {} frames good", seconds(c.l), c.good(), c.frames)
    } else {
        format!("{:.1} s long", seconds(c.l))
    }
}

fn compare(expected: &Results, got: &Results) -> Comparison {
    let kind = |r: &Results, m: &str| r.kinds.get(m).cloned().unwrap_or_else(|| "other".to_string());
    let mut c = Comparison { expected_messages: expected.messages.values().sum(), decoded_messages: got.messages.values().sum(), expected_calls: expected.calls.len() as u64, decoded_calls: got.calls.len() as u64, ..Default::default() };
    for (m, n) in &expected.messages {
        let short = n.saturating_sub(got.messages.get(m).copied().unwrap_or(0));
        c.missing_messages += short;
        if short > 0 {
            *c.missing_by_type.entry(kind(expected, m)).or_insert(0) += short;
        }
    }
    for (m, n) in &got.messages {
        let more = n.saturating_sub(expected.messages.get(m).copied().unwrap_or(0));
        c.extra_messages += more;
        if more > 0 {
            *c.extra_by_type.entry(kind(got, m)).or_insert(0) += more;
        }
    }
    let difference = |change, call: &CallSummary, detail: String| CallDifference { change, call: call.c.clone(), freq_hz: call.f, start_s: seconds(call.t), detail };
    let mut taken = vec![false; got.calls.len()];
    for e in &expected.calls {
        let found = got.calls.iter().enumerate().filter(|(i, g)| !taken[*i] && g.c == e.c && g.f == e.f && (g.t - e.t).abs() <= SAME_CALL_MS).min_by_key(|(_, g)| (g.t - e.t).abs());
        let Some((i, g)) = found else {
            c.missing_calls += 1;
            c.calls.push(difference("missing", e, length_and_frames(e)));
            continue;
        };
        taken[i] = true;
        let (gained, lost) = (g.good().saturating_sub(e.good()), e.good().saturating_sub(g.good()));
        c.good_frames_gained += gained;
        c.good_frames_lost += lost;
        // What's different about it, the worst first: the first names the change.
        let mut found: Vec<(&'static str, String)> = Vec::new();
        if lost > 0 {
            found.push(("worse", format!("good frames {} → {} (-{lost})", e.good(), g.good())));
        }
        let moved = g.l - e.l;
        if moved < -SAME_LENGTH_MS {
            c.shorter_calls += 1;
            found.push(("shorter", format!("length {:.1} s → {:.1} s", seconds(e.l), seconds(g.l))));
        }
        let r = (!e.env.is_empty()).then(|| envelope_r(&e.env, &g.env).unwrap_or(0.0));
        if let Some(r) = r.filter(|r| *r < MIN_ENVELOPE_R) {
            c.bad_envelopes += 1;
            found.push(("audio", format!("loudness over time matches at r = {r:.3}, under {MIN_ENVELOPE_R}")));
        }
        if moved > SAME_LENGTH_MS {
            c.longer_calls += 1;
            found.push(("longer", format!("length {:.1} s → {:.1} s", seconds(e.l), seconds(g.l))));
        }
        if gained > 0 {
            found.push(("better", format!("good frames {} → {} (+{gained})", e.good(), g.good())));
        }
        if g.good() == e.good() && (g.h != e.h || g.errs != e.errs) {
            c.changed_calls += 1;
            found.push(("bits", format!("same good frames, other bits (bit errors {} → {})", e.errs, g.errs)));
        }
        if let Some(r) = r.filter(|r| (MIN_ENVELOPE_R..SAME_ENVELOPE_R).contains(r)) {
            c.changed_calls += 1;
            found.push(("bits", format!("loudness over time matches at r = {r:.3}")));
        }
        if let Some((change, _)) = found.first().cloned() {
            c.calls.push(difference(change, e, found.into_iter().map(|f| f.1).collect::<Vec<_>>().join("; ")));
        }
    }
    for (_, g) in got.calls.iter().enumerate().filter(|(i, _)| !taken[*i]) {
        c.extra_calls += 1;
        c.calls.push(difference("new", g, length_and_frames(g)));
    }
    let rank = |d: &CallDifference| ["missing", "worse", "shorter", "audio", "new", "longer", "better", "bits"].iter().position(|x| *x == d.change);
    c.calls.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.start_s.total_cmp(&b.start_s)));
    c
}

/// The result of a comparison, given the capture's state and tolerance. A
/// call nobody expected, or one that runs on, may be a false one: never called better.
fn verdict(c: &Comparison, state: &str, tolerance: u64) -> &'static str {
    let worse = c.missing_messages > tolerance || c.good_frames_lost > tolerance || c.missing_calls > 0 || c.shorter_calls > 0 || c.bad_envelopes > 0;
    let unsure = c.extra_calls > 0 || c.longer_calls > 0;
    let better = c.extra_messages > 0 || c.good_frames_gained > 0;
    let plain = match (worse, unsure, better) {
        (true, _, _) => "regressed",
        (false, true, _) => "changed",
        (false, false, true) => "improved",
        _ if c.changed_calls > 0 || c.missing_messages > 0 || c.good_frames_lost > 0 => "changed",
        _ => "match",
    };
    match (state, plain) {
        ("known-failing", "improved") => "fixed",
        ("known-failing", _) => "known-failing",
        _ => plain,
    }
}

fn header(id: &str, version: &str) -> String {
    let commit = Command::new("git").args(["rev-parse", "--short", "HEAD"]).output().ok().filter(|o| o.status.success()).map_or_else(String::new, |o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    json!({ "capture": id, "trp": version, "commit": commit, "date": chrono::Utc::now().format("%Y-%m-%d").to_string() }).to_string() + "\n"
}

/// Keep `got` as capture `id`'s expected output for `name` (`next`, or a version).
fn bless(dir: &Path, id: &str, name: &str, got: &Results) -> Result<(), String> {
    if name != "next" && parse_version(name).is_none() {
        return Err(format!("--bless {name}: it's next, or a version like 0.4.0"));
    }
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.jsonl"));
    fs::write(&path, expected_text(id, name, got)).map_err(|e| format!("{}: {e}", path.display()))
}

/// An expected-output file's contents.
pub(crate) fn expected_text(id: &str, version: &str, got: &Results) -> String {
    header(id, version) + &results_text(got)
}

/// Make each capture's unreleased expected output `version`'s: how many there were.
fn release(expected_root: &Path, version: &str) -> Result<usize, String> {
    if parse_version(version).is_none() {
        return Err(format!("tool corpus release {version}: not a version like 0.4.0"));
    }
    let mut done = 0;
    for e in fs::read_dir(expected_root).into_iter().flatten().flatten() {
        let next = e.path().join("next.jsonl");
        if !next.exists() {
            continue;
        }
        let to = e.path().join(format!("{version}.jsonl"));
        if to.exists() {
            return Err(format!("{} already exists", to.display()));
        }
        let text = fs::read_to_string(&next).map_err(|err| format!("{}: {err}", next.display()))?;
        let (first, rest) = text.split_once('\n').ok_or_else(|| format!("{}: empty", next.display()))?;
        let mut head: serde_json::Value = serde_json::from_str(first).map_err(|err| format!("{}: {err}", next.display()))?;
        head["trp"] = json!(version);
        fs::write(&to, format!("{head}\n{rest}")).map_err(|err| format!("{}: {err}", to.display()))?;
        fs::remove_file(&next).map_err(|err| format!("{}: {err}", next.display()))?;
        done += 1;
    }
    Ok(done)
}

struct RunOptions<'a> {
    base: &'a str,
    api: &'a str,
    expected: PathBuf,
    against: Option<&'a str>,
    bless: Option<&'a str>,
}

/// One line of what differs: for the summary table.
fn describe(c: &Comparison) -> String {
    let parts = [
        (c.missing_messages, "messages missing"),
        (c.missing_calls, "calls missing"),
        (c.good_frames_lost, "good frames lost"),
        (c.shorter_calls, "calls shorter"),
        (c.bad_envelopes, "analog calls unlike before"),
        (c.extra_calls, "new calls"),
        (c.longer_calls, "calls longer"),
        (c.extra_messages, "more messages"),
        (c.good_frames_gained, "good frames gained"),
        (c.changed_calls, "calls with other bits"),
    ];
    let singular = |what: &str| what.replacen("messages", "message", 1).replacen("calls", "call", 1).replacen("frames", "frame", 1);
    parts.iter().filter(|(n, _)| *n > 0).map(|(n, what)| if *n == 1 { format!("1 {}", singular(what)) } else { format!("{n} {what}") }).collect::<Vec<_>>().join(", ")
}

fn by_type(counts: &BTreeMap<String, u64>) -> String {
    let mut v: Vec<_> = counts.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
}

/// What differs, line by line: at most `max` calls. Hashes, counts and times, nothing decoded: this goes in a public log.
fn details(c: &Comparison, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    if c.expected_messages + c.decoded_messages > 0 && (c.missing_messages + c.extra_messages > 0) {
        out.push(format!("Control messages: {} expected, {} decoded ({} missing, {} new)", c.expected_messages, c.decoded_messages, c.missing_messages, c.extra_messages));
        if !c.missing_by_type.is_empty() {
            out.push(format!("  missing by type: {}", by_type(&c.missing_by_type)));
        }
        if !c.extra_by_type.is_empty() {
            out.push(format!("  new by type: {}", by_type(&c.extra_by_type)));
        }
    }
    if !c.calls.is_empty() {
        out.push(format!("Calls: {} expected, {} decoded", c.expected_calls, c.decoded_calls));
        for d in c.calls.iter().take(max) {
            out.push(format!("  {:<8}{:.5} MHz at {:.1} s: {} (call {})", d.change, d.freq_hz as f64 / 1e6, d.start_s, d.detail, d.call));
        }
        if c.calls.len() > max {
            out.push(format!("  … and {} more in the report", c.calls.len() - max));
        }
    }
    out
}

/// A capture's place in the report.
#[derive(Clone, Debug, Default)]
struct Row {
    id: String,
    tier: String,
    result: &'static str,
    against: String,
    comparison: Option<Comparison>,
}

/// What each result means, for whoever reads the report.
fn result_meaning(result: &str) -> &'static str {
    match result {
        "match" => "decoded the same as expected",
        "improved" => "everything expected, and more",
        "changed" => "not clearly better or worse: look at it, and bless it if it's right",
        "regressed" => "something expected is missing or worse",
        "known-failing" => "a known bug, still failing as expected",
        "fixed" => "was known to fail, and now does better",
        "no-baseline" => "nothing to compare with yet",
        _ => "the replay itself failed",
    }
}

fn result_counts(rows: &[Row]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in rows {
        *counts.entry(r.result).or_insert(0) += 1;
    }
    let order = ["failed", "regressed", "changed", "no-baseline", "known-failing", "fixed", "improved", "match"];
    order.iter().filter_map(|k| counts.get(k).map(|n| format!("{n} {k}"))).collect::<Vec<_>>().join(", ")
}

/// The report, for people: a table of every capture, then all that differs in each that did.
fn markdown(rows: &[Row], version: &str, out: &Path) -> String {
    let rank = |r: &Row| ["failed", "regressed", "changed", "no-baseline", "known-failing", "fixed", "improved", "match"].iter().position(|x| *x == r.result);
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by_key(|r| rank(r));
    let mut md = format!("# Corpus report\n\nTrunk Recorder Pro {version}: {} captures: {}.\n\n| Capture | Tier | Result | Compared with | What differs |\n|---|---|---|---|---|\n", rows.len(), result_counts(rows));
    for r in &sorted {
        md += &format!("| {} | {} | **{}** | {} | {} |\n", r.id, r.tier, r.result, r.against, r.comparison.as_ref().map_or_else(String::new, describe));
    }
    let different: Vec<&&Row> = sorted.iter().filter(|r| r.comparison.as_ref().is_some_and(|c| !details(c, usize::MAX).is_empty())).collect();
    for r in &different {
        md += &format!("\n## {}: {}\n\n{}.\n\n```\n{}\n```\n", r.id, r.result, result_meaning(r.result), details(r.comparison.as_ref().unwrap(), usize::MAX).join("\n"));
    }
    if !different.is_empty() {
        md += &format!("\n## Looking closer\n\nExpected output holds hashes, not what was said, so to see a message or a call, replay the capture on both builds and compare. On this machine the replay's calls are in `{0}/<capture>`, and its messages in `{0}/<capture>.stdout`.\n\nIf a change is right, record it in the same pull request: `trunk-pro tool corpus run --out dir --only <capture> --bless next`.\n", out.display());
    }
    md
}

/// What the service holds as capture `e`'s expected output, from when it was checked, if this
/// version is at least that one: a capture that's new to the tests has none in the repo.
fn service_expected(o: &RunOptions, e: &Entry, version: &str) -> Result<Option<(String, String)>, String> {
    if o.against.is_some() {
        return Ok(None);
    }
    let Some(text) = get_text(&format!("{}/regression/{}/expected", o.api, e.id))? else { return Ok(None) };
    let head: serde_json::Value = serde_json::from_str(text.lines().next().unwrap_or("")).map_err(|err| format!("{}: {err}", e.id))?;
    let made = head["trp"].as_str().unwrap_or("").to_string();
    Ok(match (parse_version(&made), parse_version(version)) {
        (Some(a), Some(b)) if a <= b => Some((format!("service {made}"), text)),
        _ => None,
    })
}

/// `got` against capture `e`'s expected output, said on the log: its result, what it was compared with, and how.
fn judge(o: &RunOptions, e: &Entry, expected_dir: &Path, version: &str, got: &Results) -> Result<(&'static str, String, Option<Comparison>), String> {
    let baseline = match expected_file(expected_dir, version, o.against) {
        Some(path) => {
            let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            Some((path.file_stem().unwrap_or_default().to_string_lossy().to_string(), text))
        }
        None => service_expected(o, e, version)?,
    };
    let (result, comparison) = match &baseline {
        Some((name, text)) => {
            let c = compare(&parse_results(text).map_err(|err| format!("{name}: {err}"))?, got);
            (verdict(&c, &e.state, e.tolerance), Some(c))
        }
        None => ("no-baseline", None),
    };
    let against = baseline.map_or_else(String::new, |b| b.0);
    println!("{}: {result}{}", e.id, if against.is_empty() { String::new() } else { format!(" (against {against})") });
    for line in comparison.as_ref().map_or_else(Vec::new, |c| details(c, LOG_DIFFERENCES)) {
        println!("    {line}");
    }
    Ok((result, against, comparison))
}

fn bless_if(o: &RunOptions, e: &Entry, expected_dir: &Path, result: &str, got: &Results) -> Result<(), String> {
    if let Some(name) = o.bless.filter(|_| result != "match") {
        bless(expected_dir, &e.id, name, got)?;
        println!("{}: expected output for {name} written", e.id);
    }
    Ok(())
}

/// Replay each capture, its calls in `out/<id>` and what it printed beside
/// that, and compare it with what's expected: how many failed or regressed.
fn run_all(o: &RunOptions, cache: &Path, entries: &[&Entry], out: &Path) -> Result<usize, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let version = env!("CARGO_PKG_VERSION");
    let mut failed = 0;
    let mut rows = Vec::new();
    for e in entries {
        let folder = lay_out(o.base, cache, e)?;
        let expected_dir = o.expected.join(&e.id);
        // (A channel tape goes through its decoder alone.)
        let got = if e.tier != "wideband" {
            crate::tape_file::run(&folder)?
        } else {
            let dir = out.join(&e.id);
            fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
            let log = |ext: &str| File::create(out.join(format!("{}.{ext}", e.id))).map(Stdio::from).map_err(|err| format!("{}: {err}", out.display()));
            let status = Command::new(&exe).arg("replay").arg(&folder).arg("--out").arg(&dir).args(["--capture-frames", "--messages"]).stdout(log("stdout")?).stderr(log("stderr")?).status().map_err(|err| err.to_string())?;
            if !status.success() {
                println!("{}: failed, exit {}", e.id, status.code().unwrap_or(-1));
                rows.push(Row { id: e.id.clone(), tier: e.tier.clone(), result: "failed", ..Default::default() });
                failed += 1;
                continue;
            }
            summarize(out, &e.id)?
        };
        let (result, against, comparison) = judge(o, e, &expected_dir, version, &got)?;
        rows.push(Row { id: e.id.clone(), tier: e.tier.clone(), result, against, comparison });
        failed += (result == "regressed") as usize;
        bless_if(o, e, &expected_dir, result, &got)?;
    }
    let write = |ext: &str, text: String| -> Result<PathBuf, String> {
        let path = PathBuf::from(format!("{}.report.{ext}", out.display()));
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    };
    let json = rows.iter().map(|r| json!({ "id": r.id, "tier": r.tier, "result": r.result, "against": r.against, "comparison": r.comparison })).collect::<Vec<_>>();
    write("json", serde_json::to_string_pretty(&json).map_err(|e| e.to_string())? + "\n")?;
    let md = markdown(&rows, version, out);
    let path = write("md", md.clone())?;
    // (GitHub shows this on the run's page.)
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(summary) {
            let _ = f.write_all(md.as_bytes());
        }
    }
    println!("\n{} captures: {}. Report: {}", rows.len(), result_counts(&rows), path.display());
    Ok(failed)
}

pub fn run(a: &Args) {
    let what = a.positional.get(1).map(String::as_str).unwrap_or_else(|| die("tool corpus add|fetch|run|release … (trunk-pro --help)"));
    let list = format!("{}/regression", a.get("api").unwrap_or(API));
    let manifest = a.get("manifest").unwrap_or(&list);
    let cache = PathBuf::from(a.get("cache").map(str::to_string).or_else(|| std::env::var("TRP_CORPUS_CACHE").ok()).unwrap_or_else(|| CACHE.into()));
    let base = a.get("base").unwrap_or(BASE);
    let ok = |r: Result<(), String>| r.unwrap_or_else(|e| die(&e));
    let expected = PathBuf::from(a.get("expected").unwrap_or(EXPECTED));
    match what {
        "add" => {
            let folder = a.positional.get(2).unwrap_or_else(|| die("tool corpus add <capture folder> --id ID --protocol P"));
            let need = |k: &str| a.get(k).unwrap_or_else(|| die(&format!("tool corpus add: --{k} is needed")));
            ok(add(Path::new(need("manifest")), &cache, Path::new(folder), need("id"), a.get("tier").unwrap_or("wideband"), need("protocol")));
        }
        "fetch" | "run" => {
            let m = load_manifest(manifest).unwrap_or_else(|e| die(&e));
            let entries = chosen(&m, a.get("only"), a.get("tier")).unwrap_or_else(|e| die(&e));
            if what == "fetch" {
                for e in &entries {
                    ok(e.files.iter().try_for_each(|f| fetch(base, &cache, f).map(|_| ())));
                    println!("{}: {} files, {} MB", e.id, e.files.len(), e.bytes / 1_000_000);
                }
                return;
            }
            let out = PathBuf::from(a.get("out").unwrap_or_else(|| die("tool corpus run: --out is needed")));
            fs::create_dir_all(&out).unwrap_or_else(|e| die(&format!("{}: {e}", out.display())));
            let o = RunOptions { base, api: a.get("api").unwrap_or(API), expected, against: a.get("against"), bless: a.get("bless") };
            let failed = run_all(&o, &cache, &entries, &out).unwrap_or_else(|e| die(&e));
            if failed > 0 {
                die(&format!("{failed} of {} captures failed or regressed", entries.len()));
            }
        }
        "release" => {
            let version = a.positional.get(2).unwrap_or_else(|| die("tool corpus release <version>"));
            println!("{} captures' expected output is now {version}'s", release(&expected, version).unwrap_or_else(|e| die(&e)));
        }
        _ => die(&format!("tool corpus {what}: no such command (add, fetch, run, release)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trp-corpus-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(name: &str, bytes: &[u8]) -> FileRef {
        FileRef { name: name.into(), sha256: sha256_hex(bytes), bytes: bytes.len() as u64 }
    }

    /// Serves `body` at any URL, `n` times, honouring a `Range: bytes=N-`; says what ranges it was asked for.
    fn serve(body: Vec<u8>, n: usize) -> (String, Arc<Mutex<Vec<Option<usize>>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten().take(n) {
                let mut range = None;
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                    line.clear();
                }
                log.lock().unwrap().push(range);
                let (status, from) = range.map_or(("200 OK", 0), |f| ("206 Partial Content", f));
                write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len() - from).unwrap();
                s.write_all(&body[from..]).unwrap();
            }
        });
        (url, asked)
    }

    #[test]
    fn names_from_a_manifest_cant_leave_the_cache() {
        for bad in ["", "..", "../x", "a/b", ".hidden", "a\\b", "c:x"] {
            assert!(!safe_name(bad), "{bad}");
        }
        assert!(safe_name("source-0.sigmf-data") && safe_name("p25"));
        assert!(!safe_hash("../../etc/passwd") && !safe_hash(&"A".repeat(64)) && safe_hash(&"a".repeat(64)));
        let dir = temp("bad");
        let path = dir.join("m.json");
        fs::write(&path, r#"{"version":1,"captures":[{"id":"x","tier":"t","protocol":"p","seconds":1,"bytes":1,"state":"active","issue":null,"files":[{"name":"../evil","sha256":"00","bytes":1}]}]}"#).unwrap();
        assert!(read_manifest(&path).unwrap_err().contains("bad file entry"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_download_is_kept_only_if_it_matches() {
        let cache = temp("fetch");
        let body = b"radio samples".to_vec();
        let (url, _) = serve(body.clone(), 2);
        let f = file("source-0.sigmf-data", &body);
        let blob = fetch(&url, &cache, &f).unwrap();
        assert_eq!(fs::read(&blob).unwrap(), body);
        assert_eq!(fetch("http://127.0.0.1:1/", &cache, &f).unwrap(), blob, "already here: nothing to ask for");
        let wrong = FileRef { sha256: "0".repeat(64), ..f };
        assert!(fetch(&url, &cache, &wrong).unwrap_err().contains("doesn't match"));
        assert!(!blob_path(&cache, &wrong.sha256).exists() && !blob_path(&cache, &wrong.sha256).with_extension("part").exists());
        fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn an_interrupted_download_goes_on_where_it_stopped() {
        let cache = temp("resume");
        fs::create_dir_all(cache.join("blobs")).unwrap();
        let body = b"0123456789".to_vec();
        let f = file("x", &body);
        fs::write(blob_path(&cache, &f.sha256).with_extension("part"), &body[..4]).unwrap();
        let (url, asked) = serve(body.clone(), 1);
        assert_eq!(fs::read(fetch(&url, &cache, &f).unwrap()).unwrap(), body);
        assert_eq!(*asked.lock().unwrap(), vec![Some(4)]);
        fs::remove_dir_all(&cache).unwrap();
    }

    fn capture_folder(dir: &Path) {
        let meta = serde_json::json!({ "global": { "core:datatype": "cu8", "core:sample_rate": 4.0 }, "captures": [{ "core:sample_start": 0, "core:frequency": 1e6 }] }).to_string();
        fs::write(dir.join("source-0.sigmf-meta"), meta).unwrap();
        fs::write(dir.join("source-0.sigmf-data"), [0u8; 16]).unwrap();
        fs::write(dir.join("capture.sigmf-collection"), r#"{"collection":{"core:streams":[{"name":"source-0"}]}}"#).unwrap();
        fs::write(dir.join("notes.txt"), "not part of it").unwrap();
    }

    #[test]
    fn add_lists_a_capture_and_it_can_be_laid_out_again() {
        let dir = temp("add");
        let (folder, cache, manifest) = (dir.join("c"), dir.join("cache"), dir.join("sub/corpus.json"));
        fs::create_dir_all(&folder).unwrap();
        capture_folder(&folder);
        add(&manifest, &cache, &folder, "one", "wideband", "p25").unwrap();
        add(&manifest, &cache, &folder, "two", "wideband", "p25").unwrap();
        add(&manifest, &cache, &folder, "one", "control", "dmr").unwrap();
        let m = read_manifest(&manifest).unwrap();
        assert_eq!(m.captures.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["one", "two"], "the same id replaced");
        let e = &m.captures[0];
        assert_eq!((e.tier.as_str(), e.seconds, e.files.len(), e.state.as_str()), ("control", 2.0, 3, "active"));
        assert!(e.bytes > 16);
        let laid = lay_out("http://127.0.0.1:1/", &cache, e).unwrap();
        assert!(iq_capture::open(laid.to_str().unwrap()).is_ok() && !laid.join("notes.txt").exists());
        assert_eq!(fs::read(laid.join("source-0.sigmf-data")).unwrap(), [0u8; 16]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn message_lines_count_by_what_they_say_and_not_when() {
        let out = "control channel 1 MHz\n   0.26s  grant tg 1\n   0.31s  grant tg 1\n   0.38s  [a] update tg 2\n   0.52s  CALL 2 start TG 3\n         wrote x.wav\n   1.00s  [a] CALL 3 end\n";
        let mut r = Results::default();
        add_messages(out, &mut r);
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages.values().sum::<u64>(), 3);
        let hash = short_hash(b"grant tg 1", 8);
        assert_eq!(r.messages[&hash], 2);
        assert_eq!(r.kinds[&hash], "grant");
        assert_eq!(r.kinds[&short_hash(b"[a] update tg 2", 8)], "update", "after a site's tag");
        assert!(!format!("{:?}", r.messages).contains("grant"), "hashes, not decoded messages");
    }

    fn results(messages: &[(&str, u64)], calls: Vec<CallSummary>) -> Results {
        Results { messages: messages.iter().map(|(m, n)| (m.to_string(), *n)).collect(), calls, ..Default::default() }
    }

    fn call(c: &str, t: i64, frames: u64, bad: u64) -> CallSummary {
        CallSummary { c: c.into(), f: 100, t, l: 1000, frames, bad, errs: 5, h: "ab".into(), env: String::new() }
    }

    #[test]
    fn results_say_what_changed() {
        let e = results(&[("a", 2), ("b", 1)], vec![call("x", 1000, 50, 2)]);
        let check = |g: &Results| verdict(&compare(&e, g), "active", 0);
        assert_eq!(check(&e), "match");
        assert_eq!(check(&results(&[("a", 2), ("b", 1), ("c", 1)], e.calls.clone())), "improved");
        assert_eq!(check(&results(&[("a", 1), ("b", 1)], e.calls.clone())), "regressed", "one fewer of a repeated message");
        assert_eq!(check(&results(&[("a", 2), ("b", 1)], vec![])), "regressed", "a call lost");
        assert_eq!(check(&results(&[("a", 2), ("b", 1)], vec![call("x", 1400, 50, 2)])), "match", "within half a second");
        assert_eq!(check(&results(&[("a", 2), ("b", 1)], vec![call("x", 1600, 50, 2)])), "regressed", "another call, and this one gone");
        assert_eq!(check(&results(&[("a", 2), ("b", 1)], vec![call("x", 1000, 50, 0)])), "improved");
        assert_eq!(check(&results(&[("a", 2), ("b", 1)], vec![call("x", 1000, 50, 3)])), "regressed");
        assert_eq!(check(&results(&[("a", 2), ("b", 1)], vec![CallSummary { h: "cd".into(), ..call("x", 1000, 50, 2) }])), "changed", "as many good frames, other bits");
        let lost_one = compare(&e, &results(&[("a", 1), ("b", 1)], e.calls.clone()));
        assert_eq!(verdict(&lost_one, "active", 1), "changed", "within its tolerance");
        assert_eq!(verdict(&lost_one, "known-failing", 0), "known-failing");
        let same = |c: CallSummary| results(&[("a", 2), ("b", 1)], vec![c]);
        assert_eq!(check(&same(CallSummary { l: 1400, ..call("x", 1000, 50, 2) })), "match", "under half a second's difference");
        assert_eq!(check(&same(CallSummary { l: 400, ..call("x", 1000, 50, 2) })), "regressed", "cut short");
        assert_eq!(check(&same(CallSummary { l: 1600, ..call("x", 1000, 50, 2) })), "changed", "runs on: maybe more, maybe a stuck call");
        let extra = results(&[("a", 2), ("b", 1)], vec![call("x", 1000, 50, 2), call("y", 5000, 50, 2)]);
        assert_eq!(check(&extra), "changed", "a call nobody expected may be a false one");
        let extra_and_better = results(&[("a", 3), ("b", 1)], extra.calls.clone());
        assert_eq!(check(&extra_and_better), "changed", "more messages don't make up for it");
        assert_eq!(verdict(&compare(&e, &results(&[("a", 3), ("b", 1)], e.calls.clone())), "known-failing", 0), "fixed");
    }

    #[test]
    fn a_report_says_which_call_changed_and_how() {
        let mut e = results(&[("a", 5), ("b", 1)], vec![call("gone", 1000, 50, 2), call("w", 9000, 50, 2), call("cut", 20000, 50, 2)]);
        e.kinds.insert("a".into(), "grant".into());
        let g = results(&[("a", 2), ("b", 1)], vec![call("w", 9000, 50, 10), CallSummary { l: 300, ..call("cut", 20000, 50, 2) }, call("odd", 30000, 20, 0)]);
        let c = compare(&e, &g);
        assert_eq!(verdict(&c, "active", 0), "regressed");
        assert_eq!(c.calls.iter().map(|d| d.change).collect::<Vec<_>>(), ["missing", "worse", "shorter", "new"], "the worst first");
        let text = details(&c, 10).join("\n");
        assert!(text.contains("Control messages: 6 expected, 3 decoded (3 missing, 0 new)"), "{text}");
        assert!(text.contains("missing by type: grant 3"), "{text}");
        assert!(text.contains("0.00010 MHz at 9.0 s: good frames 48 → 40 (-8)"), "{text}");
        assert!(text.contains("length 1.0 s → 0.3 s"), "{text}");
        assert_eq!(details(&c, 1).last().unwrap(), "  … and 3 more in the report");
        let row = |id: &str, result, comparison| Row { id: id.into(), tier: "wideband".into(), result, against: "0.3.2".into(), comparison };
        let md = markdown(&[row("fine", "match", Some(Comparison::default())), row("bad", "regressed", Some(c))], "1.0.0", Path::new("/tmp/o"));
        assert!(md.contains("2 captures: 1 regressed, 1 match"), "{md}");
        assert!(md.contains("1 call missing") && !md.contains("1 calls"), "{md}");
        assert!(md.find("| bad |").unwrap() < md.find("| fine |").unwrap(), "what needs looking at comes first");
        assert!(md.contains("## bad: regressed") && !md.contains("## fine") && md.contains("--bless next"), "{md}");
    }

    #[test]
    fn analog_calls_compare_by_loudness_over_time() {
        let env = |v: &[u8]| hex(v);
        let e = results(&[], vec![CallSummary { env: env(&[10, 200, 30, 180, 20]), h: String::new(), ..call("x", 0, 0, 0) }]);
        let with = |v: &[u8]| results(&[], vec![CallSummary { env: env(v), h: String::new(), ..call("x", 0, 0, 0) }]);
        assert_eq!(verdict(&compare(&e, &with(&[10, 200, 30, 180, 20])), "active", 0), "match");
        assert_eq!(verdict(&compare(&e, &with(&[200, 10, 180, 30, 200])), "active", 0), "regressed");
        assert_eq!(verdict(&compare(&e, &with(&[30, 190, 30, 150, 50])), "active", 0), "changed");
    }

    #[test]
    fn a_wavs_loudness_is_relative_to_its_peak() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.extend_from_slice(&[16, 0, 0, 0, 1, 0, 1, 0]);
        wav.extend_from_slice(&1000u32.to_le_bytes());
        wav.extend_from_slice(&[0; 8]);
        wav.extend_from_slice(b"data\0\0\0\0");
        for v in [1000i16, -1000].repeat(50).into_iter().chain([500i16, -500].repeat(50)) {
            wav.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(envelope(&wav), "ff80");
        assert_eq!(envelope(b"not a wav"), "");
    }

    #[test]
    fn expected_output_is_chosen_by_version() {
        let dir = temp("versions");
        let at = |n: &str| dir.join(format!("{n}.jsonl"));
        for n in ["0.4.0", "0.6.2", "0.10.0"] {
            fs::write(at(n), "").unwrap();
        }
        let name = |v: &str, a| expected_file(&dir, v, a).map(|p| p.file_stem().unwrap().to_string_lossy().to_string());
        assert_eq!(name("0.5.0", None).as_deref(), Some("0.4.0"), "the newest at or below its own");
        assert_eq!(name("0.10.1", None).as_deref(), Some("0.10.0"), "versions are numbers, not text");
        assert_eq!(name("0.3.0", None), None, "promoted after that");
        fs::write(at("next"), "").unwrap();
        assert_eq!(name("0.10.1", None).as_deref(), Some("next"));
        assert_eq!(name("0.10.1", Some("0.6.2")).as_deref(), Some("0.6.2"));
        assert_eq!(name("0.10.1", Some("0.5.0")).as_deref(), Some("0.4.0"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn blessed_output_reads_back_and_a_release_names_it() {
        let root = temp("bless");
        let dir = root.join("expected/p25");
        let r = results(&[("a", 2)], vec![call("x", 5, 9, 1)]);
        bless(&dir, "p25", "next", &r).unwrap();
        assert_eq!(parse_results(&fs::read_to_string(dir.join("next.jsonl")).unwrap()).unwrap(), r);
        assert_eq!(release(&root.join("expected"), "0.5.0").unwrap(), 1);
        let text = fs::read_to_string(dir.join("0.5.0.jsonl")).unwrap();
        assert!(!dir.join("next.jsonl").exists() && text.contains(r#""trp":"0.5.0""#));
        assert_eq!(parse_results(&text).unwrap(), r);
        bless(&dir, "p25", "next", &r).unwrap();
        assert!(release(&root.join("expected"), "0.5.0").unwrap_err().contains("already exists"), "a version's output isn't overwritten");
        assert!(bless(&dir, "p25", "../evil", &r).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn options_choose_captures() {
        let e = |id: &str, tier: &str, state: &str| Entry { id: id.into(), tier: tier.into(), state: state.into(), ..Default::default() };
        let m = Manifest { version: 1, captures: vec![e("a", "wideband", "active"), e("b", "voice", "known-failing"), e("c", "voice", "retired")] };
        let ids = |only, tier| chosen(&m, only, tier).unwrap().iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(None, None), ["a", "b"], "retired ones aren't run");
        assert_eq!(ids(Some("b, a"), None), ["a", "b"]);
        assert_eq!(ids(None, Some("voice")), ["b"]);
        assert!(chosen(&m, Some("zzz"), None).is_err());
    }
}
