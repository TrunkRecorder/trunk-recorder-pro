//! `trunk-pro tool corpus`: the recordings regression tests run on
//! (research/iq-corpus.md). A manifest, in the repo, lists each capture's
//! files by SHA-256; they're fetched by that hash into a local cache, and a
//! capture is its files under their own names again, for `replay`.
//!
//! `tool corpus add <capture folder> --id ID --protocol P [--tier wideband]`
//! puts a capture in the manifest, and its files in the cache.
//! `tool corpus fetch [--only a,b] [--tier t]` downloads what the cache lacks.
//! `tool corpus run --out DIR [--only a,b] [--tier t]` replays each capture
//! into `DIR/<id>`, as `captures/regress.sh` lays its runs out.
//! All take `--manifest` (corpus/corpus.json), `--cache` (`TRP_CORPUS_CACHE`,
//! or captures/.corpus) and `--base`, the URL files are under.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::iq_capture;
use crate::toolkit::hex;
use crate::{die, Args};

const MANIFEST: &str = "corpus/corpus.json";
const CACHE: &str = "captures/.corpus";
const BASE: &str = "https://iq.trunkrecorder.pro/blobs/";

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

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m: Manifest = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for e in &m.captures {
        if !safe_name(&e.id) {
            return Err(format!("{}: {:?} isn't a usable capture id", path.display(), e.id));
        }
        if let Some(f) = e.files.iter().find(|f| !safe_name(&f.name) || !safe_hash(&f.sha256)) {
            return Err(format!("{}: capture {} has a bad file entry: {:?}", path.display(), e.id, f.name));
        }
    }
    Ok(m)
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
    let mut names: Vec<String> = fs::read_dir(folder).map_err(|e| format!("{name}: {e}"))?.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| [".sigmf-meta", ".sigmf-data", ".sigmf-collection"].iter().any(|x| n.ends_with(x))).collect();
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
    let entry = Entry { id: id.into(), tier: tier.into(), protocol: protocol.into(), seconds, bytes: files.iter().map(|f| f.bytes).sum(), files, state: "active".into(), issue: None };
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

/// Replay each capture: its calls in `out/<id>`, what it printed beside that.
fn run_all(base: &str, cache: &Path, entries: &[&Entry], out: &Path) -> Result<usize, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut failed = 0;
    for e in entries {
        let folder = lay_out(base, cache, e)?;
        let dir = out.join(&e.id);
        fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        let log = |ext: &str| File::create(out.join(format!("{}.{ext}", e.id))).map(Stdio::from).map_err(|err| format!("{}: {err}", out.display()));
        let status = Command::new(&exe).arg("replay").arg(&folder).arg("--out").arg(&dir).args(["--capture-frames", "--messages"]).stdout(log("stdout")?).stderr(log("stderr")?).status().map_err(|err| err.to_string())?;
        let calls = fs::read_dir(&dir).map_or(0, |d| d.flatten().filter(|f| f.path().extension().is_some_and(|x| x == "json")).count());
        println!("{}: exit {}, {calls} calls", e.id, status.code().unwrap_or(-1));
        failed += !status.success() as usize;
    }
    Ok(failed)
}

pub fn run(a: &Args) {
    let what = a.positional.get(1).map(String::as_str).unwrap_or_else(|| die("tool corpus add|fetch|run … (trunk-pro --help)"));
    let manifest = PathBuf::from(a.get("manifest").unwrap_or(MANIFEST));
    let cache = PathBuf::from(a.get("cache").map(str::to_string).or_else(|| std::env::var("TRP_CORPUS_CACHE").ok()).unwrap_or_else(|| CACHE.into()));
    let base = a.get("base").unwrap_or(BASE);
    let ok = |r: Result<(), String>| r.unwrap_or_else(|e| die(&e));
    match what {
        "add" => {
            let folder = a.positional.get(2).unwrap_or_else(|| die("tool corpus add <capture folder> --id ID --protocol P"));
            let need = |k: &str| a.get(k).unwrap_or_else(|| die(&format!("tool corpus add: --{k} is needed")));
            ok(add(&manifest, &cache, Path::new(folder), need("id"), a.get("tier").unwrap_or("wideband"), need("protocol")));
        }
        "fetch" | "run" => {
            let m = read_manifest(&manifest).unwrap_or_else(|e| die(&e));
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
            let failed = run_all(base, &cache, &entries, &out).unwrap_or_else(|e| die(&e));
            if failed > 0 {
                die(&format!("{failed} of {} captures failed", entries.len()));
            }
        }
        _ => die(&format!("tool corpus {what}: no such command (add, fetch, run)")),
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
        FileRef { name: name.into(), sha256: hex(&Sha256::digest(bytes)), bytes: bytes.len() as u64 }
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
