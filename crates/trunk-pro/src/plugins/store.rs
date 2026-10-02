//! The plugin store: the registry's list of plugins (github.com/TrunkRecorder/plugins),
//! and installing one. An install downloads the archive for this computer,
//! checks its SHA-256 against the registry's, unpacks it beside the plugins
//! folder, asks the plugin who it is, and only then puts it in place of what
//! was there (`plugins/<id>/`).
//!
//! The list is the registry's when it can be fetched; else the one fetched
//! last (`plugin-registry.json` in the config folder); else the copy built
//! into this version (`registry.json`, the registry's `index.json`).
//!
//! A plugin that isn't in the registry can be installed from its GitHub
//! release instead. Its files are checked against the release's own
//! `SHA256SUMS`, which proves they arrived intact, not that anyone reviewed
//! them; the interface says so.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use trunk_recorder_plugin::{Manifest, API_VERSION};

use super::{describe, plugins_dir};

pub const INDEX_URL: &str = "https://raw.githubusercontent.com/TrunkRecorder/plugins/main/index.json";
const BUILT_IN: &str = include_str!("registry.json");
/// The index format this version reads.
const INDEX_FORMAT: u32 = 1;
const MAX_DOWNLOAD: u64 = 200 << 20;
/// What an archive may unpack to, in all.
const MAX_UNPACKED: u64 = 1 << 30;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub format: u32,
    pub plugins: Vec<Listing>,
}

/// A plugin in the registry: one release of it, pinned.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Listing {
    pub id: String,
    pub name: String,
    pub description: String,
    pub repository: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    pub license: String,
    /// "official" | "community"; "unlisted" for a release installed from GitHub.
    pub tier: String,
    pub version: String,
    pub api: u32,
    pub tag: String,
    pub commit: String,
    /// By target (`universal-apple-darwin`, …).
    pub assets: BTreeMap<String, Asset>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

/// The list, and where it came from.
#[derive(Clone, Debug, Serialize)]
pub struct Catalog {
    pub index: Index,
    /// "registry" (just fetched) | "saved" (fetched before) | "built-in"
    pub source: &'static str,
    /// When it was fetched, Unix seconds.
    pub fetched: Option<f64>,
    /// Why the registry couldn't be fetched.
    pub problem: Option<String>,
}

/// The release target this computer runs: what the registry's assets are keyed by.
pub fn target() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("universal-apple-darwin")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-gnu")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else {
        None
    }
}

impl Listing {
    pub fn asset(&self) -> Option<&Asset> {
        target().and_then(|t| self.assets.get(t))
    }

    /// Why this recorder can't install it, when it can't.
    pub fn unavailable(&self) -> Option<String> {
        if self.api == 0 || self.api > API_VERSION {
            return Some(format!("Needs a newer Trunk Recorder Pro (plugin API {})", self.api));
        }
        if self.asset().is_none() {
            return Some(format!("Not built for this computer ({})", target().unwrap_or(std::env::consts::OS)));
        }
        None
    }
}

impl Catalog {
    pub fn find(&self, id: &str) -> Option<&Listing> {
        self.index.plugins.iter().find(|l| l.id == id)
    }
}

fn cache_path() -> PathBuf {
    crate::config::config_dir().join("plugin-registry.json")
}

fn parse_index(text: &str) -> Result<Index, String> {
    let index: Index = serde_json::from_str(text).map_err(|e| format!("the registry's list isn't readable ({e})"))?;
    if index.format != INDEX_FORMAT {
        return Err(format!("the registry's list is format {}; this version reads {INDEX_FORMAT}: update Trunk Recorder Pro", index.format));
    }
    Ok(index)
}

/// The registry's list: fetched if it can be, else saved or built in.
pub fn catalog() -> Catalog {
    let problem = match get(INDEX_URL, 10 << 20, Duration::from_secs(15)).and_then(|b| {
        let text = String::from_utf8(b).map_err(|_| "the registry's list isn't text".to_string())?;
        parse_index(&text).map(|i| (text, i))
    }) {
        Ok((text, index)) => {
            let _ = std::fs::create_dir_all(crate::config::config_dir());
            let _ = std::fs::write(cache_path(), text);
            return Catalog { index, source: "registry", fetched: Some(now()), problem: None };
        }
        Err(e) => e,
    };
    if let Ok(text) = std::fs::read_to_string(cache_path()) {
        if let Ok(index) = parse_index(&text) {
            let fetched = std::fs::metadata(cache_path()).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64());
            return Catalog { index, source: "saved", fetched, problem: Some(problem) };
        }
    }
    let index = parse_index(BUILT_IN).unwrap_or_default();
    Catalog { index, source: "built-in", fetched: None, problem: Some(problem) }
}

/// GET `url`: its body, at most `limit` bytes.
fn get(url: &str, limit: u64, timeout: Duration) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION"))).build().into();
    let mut r = agent.get(url).header("Accept", "application/vnd.github+json, */*").call().map_err(|e| format!("{url}: {e}"))?;
    r.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| format!("{url}: {e}"))
}

/// Is version `a` newer than `b`? Semver: a prerelease is older than its release.
pub fn newer(a: &str, b: &str) -> bool {
    /// (major, minor, patch) and the prerelease.
    type Parsed<'a> = ((u64, u64, u64), Option<&'a str>);
    fn parse(v: &str) -> Option<Parsed<'_>> {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (v.split('+').next()?, None),
        };
        let mut n = core.split('.').map(|x| x.parse::<u64>().ok());
        Some(((n.next()??, n.next()??, n.next()??), pre))
    }
    match (parse(a), parse(b)) {
        (Some((ca, pa)), Some((cb, pb))) => ca > cb || (ca == cb && pa.is_none() && pb.is_some()) || (ca == cb && matches!((pa, pb), (Some(x), Some(y)) if x > y)),
        _ => false,
    }
}

/// A plugin id is a folder name: nothing that could leave the plugins folder.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !id.starts_with('-')
}

// ─── Releases on GitHub (unlisted) ─────────────────────────────────────────

/// `owner/repo`, `https://github.com/owner/repo[.git]` or a release page
/// (`…/releases/tag/<tag>`): (owner, repo, the tag if the URL names one).
pub fn parse_github(s: &str) -> Result<(String, String, Option<String>), String> {
    let s = s.trim().trim_end_matches('/');
    let rest = s.strip_prefix("https://github.com/").or_else(|| s.strip_prefix("http://github.com/")).or_else(|| s.strip_prefix("github.com/")).unwrap_or(s);
    let parts: Vec<&str> = rest.split('/').collect();
    let ok = |p: &str| p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) && !p.bytes().all(|b| b == b'.');
    if parts.len() < 2 || !ok(parts[0]) || !ok(parts[1]) {
        return Err(format!("{s}: not a GitHub repository (https://github.com/<owner>/<repo>)"));
    }
    let repo = parts[1].trim_end_matches(".git").to_string();
    let tag = match parts.get(2..) {
        Some(["releases", "tag", t]) => Some(t.to_string()),
        _ => None,
    };
    Ok((parts[0].to_string(), repo, tag))
}

/// A GitHub release made with the template's release workflow, as a listing
/// (tier "unlisted"): its checksums are the release's own SHA256SUMS.
pub fn release_listing(repository: &str, tag: Option<&str>) -> Result<Listing, String> {
    let (owner, repo, url_tag) = parse_github(repository)?;
    let tag = tag.map(str::trim).filter(|t| !t.is_empty()).map(String::from).or(url_tag);
    let api = match &tag {
        Some(t) => format!("https://api.github.com/repos/{owner}/{repo}/releases/tags/{t}"),
        None => format!("https://api.github.com/repos/{owner}/{repo}/releases/latest"),
    };
    let rel: Value = serde_json::from_slice(&get(&api, 4 << 20, Duration::from_secs(20)).map_err(|e| {
        if e.contains("404") {
            format!("{owner}/{repo} has no {} release", tag.as_deref().unwrap_or("published"))
        } else {
            e
        }
    })?)
    .map_err(|e| format!("GitHub's answer isn't readable ({e})"))?;
    let tag = rel["tag_name"].as_str().unwrap_or_default().to_string();
    let files: BTreeMap<String, String> = rel["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str()?.to_string())))
        .collect();
    let release = format!("{owner}/{repo} {tag}");
    let sums_url = files.get("SHA256SUMS").ok_or_else(|| format!("{release} has no SHA256SUMS: it wasn't made with the plugin template's release workflow"))?;
    let sums = parse_sums(&String::from_utf8_lossy(&get(sums_url, 1 << 20, Duration::from_secs(30))?));
    let (manifest_name, manifest_url) =
        files.iter().find(|(n, _)| n.ends_with(".manifest.json")).ok_or_else(|| format!("{release} has no <id>-<version>.manifest.json"))?;
    let raw = get(manifest_url, 1 << 20, Duration::from_secs(30))?;
    if sums.get(manifest_name.as_str()) != Some(&sha256_hex(&raw)) {
        return Err(format!("{manifest_name} doesn't match the release's SHA256SUMS"));
    }
    let m: Manifest = serde_json::from_slice(&raw).map_err(|e| format!("{manifest_name}: {e}"))?;
    let assets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "universal-apple-darwin", "x86_64-pc-windows-msvc"]
        .into_iter()
        .filter_map(|t| {
            let ext = if t.contains("windows") { "zip" } else { "tar.gz" };
            let name = format!("{}-{}-{t}.{ext}", m.id, m.version);
            Some((t.to_string(), Asset { url: files.get(&name)?.clone(), sha256: sums.get(&name)?.clone() }))
        })
        .collect();
    Ok(Listing {
        id: m.id,
        name: m.name,
        description: m.description,
        repository: format!("https://github.com/{owner}/{repo}"),
        homepage: m.homepage,
        license: m.license,
        tier: "unlisted".into(),
        version: m.version,
        api: m.api,
        tag,
        commit: String::new(),
        assets,
    })
}

fn parse_sums(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            let (sum, name) = (p.next()?, p.next()?);
            Some((name.trim_start_matches('*').to_string(), sum.to_ascii_lowercase()))
        })
        .collect()
}

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

// ─── Installing ────────────────────────────────────────────────────────────

/// Install `l` into `plugins/<id>/`, replacing what's there only once the new
/// one has said who it is. `progress` hears "downloading", "checking" and
/// "installing".
pub fn install(l: &Listing, progress: &dyn Fn(&'static str)) -> Result<Manifest, String> {
    if !valid_id(&l.id) {
        return Err(format!("{:?} isn't a plugin id", l.id));
    }
    if let Some(why) = l.unavailable() {
        return Err(format!("{}: {why}", l.name));
    }
    let a = l.asset().expect("checked by unavailable");
    progress("downloading");
    let data = get(&a.url, MAX_DOWNLOAD, Duration::from_secs(300))?;
    progress("checking");
    if sha256_hex(&data) != a.sha256.to_ascii_lowercase() {
        return Err(format!("The download of {} {} doesn't match its checksum, so it wasn't installed. Try again; if it happens again, report it.", l.name, l.version));
    }
    progress("installing");
    let dir = plugins_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Unpacked beside the plugins (same disk, so it can be moved into place).
    let staging = Staging(dir.join(format!(".installing-{}-{}", l.id, std::process::id())));
    let _ = std::fs::remove_dir_all(&staging.0);
    std::fs::create_dir_all(&staging.0).map_err(|e| format!("{}: {e}", staging.0.display()))?;
    unpack(&data, a.url.ends_with(".zip"), &staging.0)?;
    // The archive holds one folder (`<id>-<version>-<target>/`) with the executable in it.
    let root = only_folder(&staging.0).unwrap_or_else(|| staging.0.clone());
    let exe = root.join(super::executable_name(&l.id));
    if !exe.is_file() {
        return Err(format!("The download of {} has no {}", l.name, super::executable_name(&l.id)));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
    }
    let m = describe(&exe)?;
    if m.id != l.id || m.version != l.version {
        return Err(format!("The download says it's {} {}, not {} {}: not installed", m.id, m.version, l.id, l.version));
    }
    put_in_place(&root, &dir.join(&l.id))?;
    Ok(m)
}

/// Removed when dropped: what's left of a failed (or finished) install.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn only_folder(dir: &Path) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    match entries.as_slice() {
        [e] if e.path().is_dir() => Some(e.path()),
        _ => None,
    }
}

/// Move `new` to `dest`, keeping the old `dest` until the move has worked.
fn put_in_place(new: &Path, dest: &Path) -> Result<(), String> {
    let old = dest.with_file_name(format!(".old-{}-{}", dest.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    let _ = std::fs::remove_dir_all(&old);
    let had = dest.exists();
    if had {
        std::fs::rename(dest, &old).map_err(|e| {
            format!("Couldn't replace {} ({e}). If it's running, stop recording and try again.", dest.display())
        })?;
    }
    if let Err(e) = std::fs::rename(new, dest) {
        if had {
            let _ = std::fs::rename(&old, dest);
        }
        return Err(format!("Couldn't install into {} ({e})", dest.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Unpack a .tar.gz (or a .zip) into `into`: plain files and folders only,
/// and nothing that would land outside it.
fn unpack(data: &[u8], zip: bool, into: &Path) -> Result<(), String> {
    let bad = |e: &dyn std::fmt::Display| format!("The download isn't a readable archive ({e})");
    let mut total = 0u64;
    if zip {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| bad(&e))?;
        for i in 0..z.len() {
            let mut f = z.by_index(i).map_err(|e| bad(&e))?;
            let rel = f.enclosed_name().ok_or_else(|| format!("The download has an unsafe path ({})", f.name()))?;
            if f.is_symlink() {
                return Err(format!("The download has a link ({}), which plugins mayn't", f.name()));
            }
            let path = into.join(rel);
            if f.is_dir() {
                std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
                continue;
            }
            total += f.size();
            if total > MAX_UNPACKED {
                return Err("The download unpacks to more than 1 GB".into());
            }
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            std::io::copy(&mut f.by_ref().take(MAX_UNPACKED), &mut out).map_err(|e| bad(&e))?;
        }
        return Ok(());
    }
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(data));
    for entry in ar.entries().map_err(|e| bad(&e))? {
        let mut e = entry.map_err(|e| bad(&e))?;
        let kind = e.header().entry_type();
        let name = e.path().map(|p| p.display().to_string()).unwrap_or_default();
        if !(kind.is_file() || kind.is_dir()) {
            if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() || kind.is_gnu_longname() {
                continue;
            }
            return Err(format!("The download has a link or special file ({name}), which plugins mayn't"));
        }
        total += e.size();
        if total > MAX_UNPACKED {
            return Err("The download unpacks to more than 1 GB".into());
        }
        // unpack_in refuses paths that leave `into` (false).
        if !e.unpack_in(into).map_err(|e| bad(&e))? {
            return Err(format!("The download has an unsafe path ({name})"));
        }
    }
    Ok(())
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.1.1", "0.1.0"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("1.0.0", "1.0.0-beta.1"));
        assert!(!newer("1.0.0-beta.1", "1.0.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.1.1"));
        assert!(!newer("junk", "0.1.0"));
    }

    #[test]
    fn github_urls() {
        let p = |s| parse_github(s).unwrap();
        assert_eq!(p("someone/trunk-plugin-pager"), ("someone".into(), "trunk-plugin-pager".into(), None));
        assert_eq!(p("https://github.com/someone/trunk-plugin-pager.git/"), ("someone".into(), "trunk-plugin-pager".into(), None));
        assert_eq!(p("https://github.com/someone/pager/releases/tag/v1.2.0"), ("someone".into(), "pager".into(), Some("v1.2.0".into())));
        assert!(parse_github("https://example.com").is_err());
        assert!(parse_github("someone").is_err());
        assert!(parse_github("../x/y").is_err());
    }

    #[test]
    fn ids() {
        assert!(valid_id("openmhz") && valid_id("upload-script"));
        assert!(!valid_id("") && !valid_id("../x") && !valid_id("A") && !valid_id("-x") && !valid_id(".old"));
    }

    #[test]
    fn the_built_in_list_reads() {
        let i = parse_index(BUILT_IN).unwrap();
        assert!(i.plugins.iter().any(|l| l.id == "openmhz" && l.tier == "official"));
        assert!(i.plugins.iter().all(|l| valid_id(&l.id) && l.assets.values().all(|a| a.sha256.len() == 64)));
    }

    fn tar_gz(entries: &[(&str, &[u8])], link: Option<&str>) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_entry_type(tar::EntryType::Regular);
            // Written raw: append_data would refuse the unsafe names these tests need.
            h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        if let Some(target) = link {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            b.append_link(&mut h, "pager/evil", target).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trunk-pro-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unpacks_safely() {
        let d = temp("ok");
        unpack(&tar_gz(&[("pager-1.0.0/pager", b"bin"), ("pager-1.0.0/README.md", b"hi")], None), false, &d).unwrap();
        assert_eq!(std::fs::read(d.join("pager-1.0.0/pager")).unwrap(), b"bin");
        assert_eq!(only_folder(&d), Some(d.join("pager-1.0.0")));
        for (what, data) in [("escape", tar_gz(&[("../escaped", b"x")], None)), ("link", tar_gz(&[], Some("/etc/passwd")))] {
            let d = temp(what);
            assert!(unpack(&data, false, &d).is_err(), "{what}");
            assert!(!d.parent().unwrap().join("escaped").exists());
        }
    }

    #[test]
    fn unpacks_zip() {
        use std::io::Write;
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        z.start_file("pager-1.0.0/pager.exe", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b"exe").unwrap();
        let data = z.finish().unwrap().into_inner();
        let d = temp("zip");
        unpack(&data, true, &d).unwrap();
        assert_eq!(std::fs::read(d.join("pager-1.0.0/pager.exe")).unwrap(), b"exe");
    }

    #[test]
    fn replaces_only_after_the_move() {
        let d = temp("swap");
        std::fs::create_dir_all(d.join("new")).unwrap();
        std::fs::write(d.join("new/x"), "2").unwrap();
        std::fs::create_dir_all(d.join("pager")).unwrap();
        std::fs::write(d.join("pager/x"), "1").unwrap();
        put_in_place(&d.join("new"), &d.join("pager")).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("pager/x")).unwrap(), "2");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "the old copy is gone");
    }
}
