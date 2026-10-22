//! What the app installs from a registry: plugins ([`crate::plugins::store`],
//! github.com/TrunkRecorder/plugins) and interfaces
//! ([`crate::interfaces::store`], github.com/TrunkRecorder/interfaces). Both
//! work the same way, and this is the part they share:
//!
//! - **The list** ([`Registry`]): the registry's `index.json` when it can be
//!   fetched; else the one fetched last (kept in the config folder); else the
//!   copy built into this version. [`Shelf`] keeps it a while and runs
//!   installs, telling the browsers how each goes.
//! - **Installing**: a download is checked against the SHA-256 the registry
//!   pinned ([`download`]), unpacked beside where it goes, plain files only and
//!   nothing outside ([`unpack`]), checked by the caller, and only then moved
//!   into place, the old copy kept until that worked ([`put_in_place`]).
//!   Where it came from is kept beside it ([`Provenance`]).
//! - **Releases that aren't listed** ([`Release`]): a GitHub release's files,
//!   checked against its own `SHA256SUMS`. That proves they arrived intact,
//!   not that anyone reviewed them, and the interface says so.

use trunk_app::protocol::{StoreKind, ToClient};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::runtime::{publish, Hub};

/// What an archive may unpack to, in all.
const MAX_UNPACKED: u64 = 1 << 30;
/// How long a fetched list is used before it's fetched again; sooner when it couldn't be fetched.
const FRESH: Duration = Duration::from_secs(15 * 60);
const RETRY: Duration = Duration::from_secs(60);

/// A registry: where its list is, and what this version knows of it.
pub struct Registry {
    pub index_url: &'static str,
    /// The list as last fetched, in the config folder.
    pub cache_file: &'static str,
    /// The registry's `index.json` when this version was built.
    pub built_in: &'static str,
    /// The index format this version reads.
    pub format: u32,
}

/// A registry's `index.json`.
pub trait Index: DeserializeOwned + Default + Clone {
    type Listing: Listed;
    /// What the list is called to the user ("registry", "store").
    const LIST: &'static str;
    /// What it lists, plural ("plugins").
    const KINDS: &'static str;
    fn format(&self) -> u32;
    fn listings(&self) -> &[Self::Listing];
}

/// One entry of a registry's list, as far as the command lines care.
pub trait Listed {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn repository(&self) -> &str;
    fn tier(&self) -> &str;
    fn version(&self) -> &str;
    /// Why this recorder can't install it, when it can't.
    fn unavailable(&self) -> Option<String>;
    /// Said after the description, with its own separator.
    fn note(&self) -> &'static str {
        ""
    }
}

/// A registry's list, and where it came from.
#[derive(Clone, Debug, Serialize)]
pub struct Catalog<I> {
    pub index: I,
    /// "registry" (just fetched) | "saved" (fetched before) | "built-in"
    pub source: &'static str,
    /// When it was fetched, Unix seconds.
    pub fetched: Option<f64>,
    /// Why the registry couldn't be fetched.
    pub problem: Option<String>,
}

impl<I: Index> Catalog<I> {
    pub fn find(&self, id: &str) -> Option<&I::Listing> {
        self.index.listings().iter().find(|l| l.id() == id)
    }

    /// Say on stderr when the list isn't the registry's own.
    pub fn say_source(&self) {
        let problem = self.problem.as_deref().unwrap_or("");
        match self.source {
            "registry" => {}
            "saved" => eprintln!("(The {} couldn't be reached: {problem}. Using the list fetched before.)", I::LIST),
            _ => eprintln!("(The {} couldn't be reached: {problem}. Using the list built into this version.)", I::LIST),
        }
    }

    /// Print the listings whose id, name or description has `text`, each with what is installed
    /// (`installed` gives the version of one, if it is).
    pub fn search(&self, text: &str, installed: &dyn Fn(&str) -> Option<String>) {
        let text = text.to_lowercase();
        let found: Vec<&I::Listing> = self.index.listings().iter().filter(|l| [l.id(), l.name(), l.description()].iter().any(|f| f.to_lowercase().contains(&text))).collect();
        if found.is_empty() {
            println!("No {}{}.", I::KINDS, if text.is_empty() { String::new() } else { format!(" match {text:?}") });
        }
        for l in found {
            let status = match (installed(l.id()), l.unavailable()) {
                (Some(v), _) if newer(l.version(), &v) => format!(" — installed {v}, update available"),
                (Some(v), _) => format!(" — installed {v}"),
                (None, Some(why)) => format!(" — {why}"),
                (None, None) => String::new(),
            };
            println!("{} {} [{}]{status}\n    {}{}\n    {}", l.id(), l.version(), l.tier(), l.description(), l.note(), l.repository());
        }
    }

    /// The newer release of installed `id` (at `have`), if there is one; otherwise says why not on stdout.
    /// `from` is the repository it was installed from when not from the list; `release` finds that one's latest.
    pub fn newer_release(&self, id: &str, have: &str, from: Option<&str>, release: &dyn Fn(&str) -> Result<I::Listing, String>) -> Option<I::Listing>
    where
        I::Listing: Clone,
    {
        let latest = match from {
            None => match self.find(id) {
                Some(l) => l.clone(),
                None => {
                    println!("{id} {have}: not in the {}", I::LIST);
                    return None;
                }
            },
            Some(repo) => match release(repo) {
                Ok(l) => l,
                Err(e) => {
                    println!("{id} {have}: {e}");
                    return None;
                }
            },
        };
        if !newer(latest.version(), have) {
            println!("{id} {have}: up to date");
            return None;
        }
        println!("{id}: {have} → {}", latest.version());
        Some(latest)
    }
}

impl Registry {
    fn cache_path(&self) -> PathBuf {
        crate::paths::data_dir().join(self.cache_file)
    }

    fn parse<I: Index>(&self, text: &str) -> Result<I, String> {
        let index: I = serde_json::from_str(text).map_err(|e| format!("the registry's list isn't readable ({e})"))?;
        if index.format() != self.format {
            return Err(format!("the registry's list is format {}; this version reads {}: update Trunk Recorder Pro", index.format(), self.format));
        }
        Ok(index)
    }

    /// The list: fetched if it can be, else saved or built in.
    pub fn catalog<I: Index>(&self) -> Catalog<I> {
        let problem = match get(self.index_url, 10 << 20, Duration::from_secs(15)).and_then(|b| {
            let text = String::from_utf8(b).map_err(|_| "the registry's list isn't text".to_string())?;
            self.parse(&text).map(|i| (text, i))
        }) {
            Ok((text, index)) => {
                let _ = std::fs::create_dir_all(crate::paths::data_dir());
                let _ = crate::config::write_atomic(&self.cache_path(), text);
                return Catalog { index, source: "registry", fetched: Some(crate::clock::unix_s()), problem: None };
            }
            Err(e) => e,
        };
        if let Ok(text) = std::fs::read_to_string(self.cache_path()) {
            if let Ok(index) = self.parse(&text) {
                let fetched = std::fs::metadata(self.cache_path()).and_then(|m| m.modified()).ok().map(crate::clock::unix_s_of);
                return Catalog { index, source: "saved", fetched, problem: Some(problem) };
            }
        }
        Catalog { problem: Some(problem), ..self.built_in() }
    }

    /// The copy of the list built into this version.
    pub fn built_in<I: Index>(&self) -> Catalog<I> {
        Catalog { index: self.parse(self.built_in).unwrap_or_default(), source: "built-in", fetched: None, problem: None }
    }
}

/// A registry as the running app uses it: its list, kept a while, and the
/// installs under way.
pub struct Shelf<I> {
    registry: &'static Registry,
    cached: Mutex<Option<(Instant, Catalog<I>)>>,
    /// By what was asked for (an id, or a repository).
    installing: Mutex<BTreeSet<String>>,
}

impl<I: Index> Shelf<I> {
    pub fn new(registry: &'static Registry) -> Shelf<I> {
        Shelf { registry, cached: Mutex::new(None), installing: Mutex::new(BTreeSet::new()) }
    }

    /// The list: fetched again when it's old, or when `refresh`.
    pub fn catalog(&self, refresh: bool) -> Catalog<I> {
        let mut c = self.cached.lock().unwrap();
        if let Some((t, cat)) = c.as_ref() {
            let fresh = if cat.source == "registry" { FRESH } else { RETRY };
            if !refresh && t.elapsed() < fresh {
                return cat.clone();
            }
        }
        let cat = self.registry.catalog();
        *c = Some((Instant::now(), cat.clone()));
        cat
    }

    /// Run install `key` (what was asked for), saying how it goes to
    /// `report` ([`Progress`]: stage "finding", then what `work` reports —
    /// "downloading", "checking", "installing" — then "done" or "failed").
    /// `work` sets the id once it knows it, and returns the notice for
    /// "done". Blocks until done.
    pub fn install(&self, key: &str, report: &dyn Fn(Progress), work: impl FnOnce(&mut String, &dyn Fn(&str, &'static str)) -> Result<String, String>) {
        let say = |id: &str, stage: &'static str, message: Option<&str>| report(Progress { key, id, stage, message });
        if !self.installing.lock().unwrap().insert(key.to_string()) {
            return say("", "failed", Some("That's being installed already."));
        }
        say("", "finding", None);
        let mut id = String::new();
        let r = work(&mut id, &|id, stage| say(id, stage, None));
        self.installing.lock().unwrap().remove(key);
        match r {
            Ok(notice) => say(&id, "done", Some(&notice)),
            Err(e) => say(&id, "failed", Some(&e)),
        }
    }
}

/// How an install goes ([`Shelf::install`]).
pub struct Progress<'a> {
    /// What was asked for: an id, or a repository.
    pub key: &'a str,
    /// What it installs, once known.
    pub id: &'a str,
    /// "finding", "downloading", "checking", "installing", "done", "failed".
    pub stage: &'static str,
    /// For "done" and "failed": what to tell the user.
    pub message: Option<&'a str>,
}

/// Progress told to every browser with control access, as `installProgress`.
pub fn announce(hub: &Hub, kind: StoreKind) -> impl Fn(Progress) + '_ {
    move |p| publish(hub, ToClient::InstallProgress { kind, key: p.key.into(), id: p.id.into(), stage: p.stage, message: p.message.map(String::from) })
}

/// GET `url`: its body, at most `limit` bytes.
pub fn get(url: &str, limit: u64, timeout: Duration) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION"))).build().into();
    let mut r = agent.get(url).header("Accept", "application/vnd.github+json, */*").call().map_err(|e| format!("{url}: {e}"))?;
    r.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| format!("{url}: {e}"))
}

/// Download `url`, at most `limit` bytes, and check it's what `sha256` says
/// (`what` names it in the error).
pub fn download(url: &str, sha256: &str, limit: u64, what: &str) -> Result<Vec<u8>, String> {
    let data = get(url, limit, Duration::from_secs(300))?;
    if sha256_hex(&data) != sha256.to_ascii_lowercase() {
        return Err(format!("The download of {what} doesn't match its checksum, so it wasn't installed. Try again; if it happens again, report it."));
    }
    Ok(data)
}

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
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

/// An id is a folder name: nothing that could leave the folder it's in
/// (plugins' and interfaces' alike).
pub fn valid_id(id: &str) -> bool {
    crate::config::Interface::valid_id(id)
}

/// A semantic version: `1.2.3`, `1.2.3-beta.1`.
pub fn valid_version(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let n: Vec<&str> = core.split('.').collect();
    n.len() == 3 && n.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
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

/// A GitHub release and its `SHA256SUMS`.
pub struct Release {
    pub owner: String,
    pub repo: String,
    pub tag: String,
    /// Its files' download URLs, by name.
    pub files: BTreeMap<String, String>,
    /// Its `SHA256SUMS`, by file name.
    pub sums: BTreeMap<String, String>,
}

impl Release {
    /// Release `tag` (or the latest) of `repository`, which must have a
    /// `SHA256SUMS` (`made_with` names the workflow that writes one).
    pub fn of(repository: &str, tag: Option<&str>, made_with: &str) -> Result<Release, String> {
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
        let sums_url = files.get("SHA256SUMS").ok_or_else(|| format!("{owner}/{repo} {tag} has no SHA256SUMS: it wasn't made with {made_with}"))?;
        let sums = parse_sums(&String::from_utf8_lossy(&get(sums_url, 1 << 20, Duration::from_secs(30))?));
        Ok(Release { owner, repo, tag, files, sums })
    }

    pub fn repository(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    /// File `name`'s URL and checksum, when the release has both.
    pub fn asset(&self, name: &str) -> Option<(String, String)> {
        Some((self.files.get(name)?.clone(), self.sums.get(name)?.clone()))
    }

    /// Download file `name`, checked against the release's SHA256SUMS.
    pub fn fetch(&self, name: &str, limit: u64) -> Result<Vec<u8>, String> {
        let (url, sum) = self.asset(name).ok_or_else(|| format!("{}/{} {} has no {name} (or it isn't in its SHA256SUMS)", self.owner, self.repo, self.tag))?;
        let data = get(&url, limit, Duration::from_secs(120))?;
        if sha256_hex(&data) != sum {
            return Err(format!("{name} doesn't match the release's SHA256SUMS"));
        }
        Ok(data)
    }
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

// ─── Installing ────────────────────────────────────────────────────────────

/// Unpack `data` (a .zip, else a .tar.gz) beside `dest`, on the same disk so
/// it can be moved into place. The archive holds one folder (or its files
/// loose): returns the staging folder (removed when dropped) and that folder.
pub fn stage(dest: &Path, data: &[u8], zip: bool) -> Result<(Staging, PathBuf), String> {
    let dir = dest.parent().ok_or("nowhere to install")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let staging = Staging(dir.join(format!(".installing-{name}-{}", unique())));
    let _ = std::fs::remove_dir_all(&staging.0);
    std::fs::create_dir_all(&staging.0).map_err(|e| format!("{}: {e}", staging.0.display()))?;
    unpack(data, zip, &staging.0)?;
    let root = only_folder(&staging.0).unwrap_or_else(|| staging.0.clone());
    Ok((staging, root))
}

/// Removed when dropped: what's left of a failed (or finished) install.
pub struct Staging(PathBuf);

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
pub fn put_in_place(new: &Path, dest: &Path) -> Result<(), String> {
    let old = dest.with_file_name(format!(".old-{}-{}", dest.file_name().unwrap_or_default().to_string_lossy(), unique()));
    let _ = std::fs::remove_dir_all(&old);
    let had = dest.exists();
    if had {
        std::fs::rename(dest, &old).map_err(|e| format!("Couldn't replace {} ({e}). If it's running, stop recording and try again.", dest.display()))?;
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
pub fn unpack(data: &[u8], zip: bool, into: &Path) -> Result<(), String> {
    let bad = |e: &dyn std::fmt::Display| format!("The download isn't a readable archive ({e})");
    let mut total = 0u64;
    if zip {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| bad(&e))?;
        for i in 0..z.len() {
            let mut f = z.by_index(i).map_err(|e| bad(&e))?;
            let rel = f.enclosed_name().ok_or_else(|| format!("The download has an unsafe path ({})", f.name()))?;
            if f.is_symlink() {
                return Err(format!("The download has a link ({}), which isn't allowed", f.name()));
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
            return Err(format!("The download has a link or special file ({name}), which isn't allowed"));
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

/// Where an installed copy came from, kept beside it (`.install.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Provenance {
    pub repository: String,
    pub version: String,
    /// From the registry (false: a GitHub release nobody reviewed).
    pub reviewed: bool,
}

const PROVENANCE: &str = ".install.json";

impl Provenance {
    pub fn write(&self, dir: &Path) {
        let _ = std::fs::write(dir.join(PROVENANCE), serde_json::to_string_pretty(self).unwrap_or_default());
    }

    pub fn read(dir: &Path) -> Option<Provenance> {
        serde_json::from_str(&std::fs::read_to_string(dir.join(PROVENANCE)).ok()?).ok()
    }
}


/// A name part no other install (in this process or another) is using:
/// two installs of one thing at once mustn't share a staging folder.
fn unique() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
pub(crate) mod tests {
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

    pub(crate) fn tar_gz(entries: &[(&str, &[u8])], link: Option<&str>) -> Vec<u8> {
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

    pub(crate) fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    pub(crate) fn temp(name: &str) -> PathBuf {
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
        let d = temp("zip");
        unpack(&zip_of(&[("pager-1.0.0/pager.exe", b"exe")]), true, &d).unwrap();
        assert_eq!(std::fs::read(d.join("pager-1.0.0/pager.exe")).unwrap(), b"exe");
    }

    #[test]
    fn stages_beside_and_cleans_up() {
        let d = temp("stage");
        let (staging, root) = stage(&d.join("wall"), &zip_of(&[("wall-1.0.0/index.html", b"hi")]), true).unwrap();
        assert!(root.ends_with("wall-1.0.0") && root.join("index.html").is_file());
        drop(staging);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0, "the staging folder is gone");
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
