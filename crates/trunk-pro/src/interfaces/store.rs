//! The interface store: the registry's list of interfaces
//! (github.com/TrunkRecorder/interfaces), and installing one. A listing pins
//! one release: its archive's URL and SHA-256, the tag and the commit it was
//! built from. An interface is web files only, the same on every computer,
//! so a release has one archive, `<id>-<version>.zip`, holding one folder
//! with its `interface.json` and `index.html`.
//!
//! An install downloads it, checks the checksum, unpacks it beside the
//! interfaces folder, checks it says it's what the listing says, and only
//! then puts it in place of what was there (`interfaces/<id>/`). The parts
//! plugins share are in [`crate::store`].
//!
//! Its screenshots are in the archive too, and each is also a file of the
//! release of its own (`<id>-<version>-<file name>`), which the listing pins
//! by URL and SHA-256: the store shows them before anything is installed.

use trunk_app::protocol::{body, StoreKind, ToClient};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{interfaces_dir, Manifest, API_VERSION, MANIFEST};
use crate::config::ApiAccess;
use crate::store::{Provenance, Registry, Release};

pub static REGISTRY: Registry = Registry {
    index_url: "https://raw.githubusercontent.com/TrunkRecorder/interfaces/main/index.json",
    cache_file: "interface-registry.json",
    built_in: include_str!("registry.json"),
    format: 1,
};
const MAX_DOWNLOAD: u64 = 100 << 20;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub format: u32,
    pub interfaces: Vec<Listing>,
}

impl crate::store::Index for Index {
    fn format(&self) -> u32 {
        self.format
    }
}

pub type Catalog = crate::store::Catalog<Index>;

impl Catalog {
    pub fn find(&self, id: &str) -> Option<&Listing> {
        self.index.interfaces.iter().find(|l| l.id == id)
    }
}

/// An interface in the registry: one release of it, pinned.
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
    /// What it needs ("listen" or "control"), as its interface.json says.
    pub access: ApiAccess,
    pub tag: String,
    pub commit: String,
    pub asset: Asset,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<ListedScreenshot>,
}

/// A screenshot of a listed interface: a file of its release.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListedScreenshot {
    pub url: String,
    pub sha256: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub caption: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

impl Listing {
    /// Why this recorder can't install it, when it can't.
    pub fn unavailable(&self) -> Option<String> {
        (self.api == 0 || self.api > API_VERSION).then(|| format!("Needs a newer Trunk Recorder Pro (interface API {})", self.api))
    }
}

/// The interfaces' `store` message for a catalog.
pub fn message(cat: &Catalog) -> ToClient {
    let list: Vec<Value> = cat
        .index
        .interfaces
        .iter()
        .map(|l| {
            let mut v = serde_json::to_value(l).unwrap_or_default();
            v["unavailable"] = json!(l.unavailable());
            v
        })
        .collect();
    ToClient::Store { kind: StoreKind::Interface, list: body(json!({ "source": cat.source, "fetched": cat.fetched, "problem": cat.problem, "listings": list })) }
}

/// The registry's list: fetched if it can be, else saved or built in.
pub fn catalog() -> Catalog {
    REGISTRY.catalog()
}

/// A GitHub release made with the interface template's release workflow, as
/// a listing (tier "unlisted"): its checksum is the release's own SHA256SUMS.
pub fn release_listing(repository: &str, tag: Option<&str>) -> Result<Listing, String> {
    let rel = Release::of(repository, tag, "the interface template's release workflow")?;
    let zips: Vec<&String> = rel.sums.keys().filter(|n| n.ends_with(".zip")).collect();
    let [name] = zips.as_slice() else {
        return Err(format!("{}/{} {} should have one <id>-<version>.zip in its SHA256SUMS; it has {}", rel.owner, rel.repo, rel.tag, zips.len()));
    };
    let data = rel.fetch(name, MAX_DOWNLOAD)?;
    let m = manifest_in_zip(&data)?;
    if rel.tag != format!("v{}", m.version) {
        return Err(format!("{}/{} {}: its interface.json says version {}, so its tag should be v{}", rel.owner, rel.repo, rel.tag, m.version, m.version));
    }
    let (url, sha256) = rel.asset(name).expect("fetched");
    let screenshots = m
        .screenshots
        .iter()
        .map(|shot| {
            let file = format!("{}-{}-{}", m.id, m.version, shot.path.rsplit('/').next().unwrap_or_default());
            let (url, sha256) = rel.asset(&file).ok_or_else(|| format!("{}/{} {} has no {file} (a screenshot) in its SHA256SUMS", rel.owner, rel.repo, rel.tag))?;
            Ok(ListedScreenshot { url, sha256, caption: shot.caption.clone() })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Listing {
        id: m.id,
        name: m.name,
        description: m.description,
        repository: rel.repository(),
        homepage: m.homepage,
        license: m.license,
        tier: "unlisted".into(),
        version: m.version,
        api: m.api,
        access: m.access,
        tag: rel.tag,
        commit: String::new(),
        asset: Asset { url, sha256 },
        screenshots,
    })
}

/// The interface.json in an archive's folder.
fn manifest_in_zip(data: &[u8]) -> Result<Manifest, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| format!("The release's archive isn't readable ({e})"))?;
    let name = z.file_names().find(|n| {
        let parts: Vec<&str> = n.split('/').filter(|p| !p.is_empty()).collect();
        parts.len() == 2 && parts[1] == MANIFEST
    }).map(String::from).ok_or(format!("The release's archive has no {MANIFEST}"))?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut z.by_name(&name).map_err(|e| e.to_string())?, &mut text).map_err(|e| e.to_string())?;
    Manifest::parse(&text)
}

/// Install `l` into `interfaces/<id>/`, replacing what's there only once
/// the new one checks out. `progress` hears "downloading", "checking" and
/// "installing".
pub fn install(l: &Listing, progress: &dyn Fn(&'static str)) -> Result<Manifest, String> {
    if !crate::store::valid_id(&l.id) {
        return Err(format!("{:?} isn't an interface id", l.id));
    }
    if let Some(why) = l.unavailable() {
        return Err(format!("{}: {why}", l.name));
    }
    progress("downloading");
    let data = crate::store::download(&l.asset.url, &l.asset.sha256, MAX_DOWNLOAD, &format!("{} {}", l.name, l.version))?;
    install_from(l, &data, progress)
}

/// Install `l` from its archive, already checked against its checksum.
fn install_from(l: &Listing, data: &[u8], progress: &dyn Fn(&'static str)) -> Result<Manifest, String> {
    progress("checking");
    let dest = interfaces_dir().join(&l.id);
    let (_staging, root) = crate::store::stage(&dest, data, !l.asset.url.ends_with(".tar.gz"))?;
    if !root.join("index.html").is_file() {
        return Err(format!("The download of {} has no index.html", l.name));
    }
    let m = Manifest::read(&root)?.ok_or_else(|| format!("The download of {} has no {MANIFEST}", l.name))?;
    if m.id != l.id || m.version != l.version {
        return Err(format!("The download says it's {} {}, not {} {}: not installed", m.id, m.version, l.id, l.version));
    }
    if m.api != l.api || m.access != l.access {
        return Err(format!("The download of {} says it needs other than its listing says (its API or access): not installed", l.name));
    }
    if let Some(shot) = (0..m.screenshots.len()).find(|n| m.screenshot(&root, *n).is_none()) {
        return Err(format!("The download of {} has no {} (or it's over 2 MB)", l.name, m.screenshots[shot].path));
    }
    // The screenshots the store showed are the ones it has.
    let same = l.screenshots.len() == m.screenshots.len()
        && l.screenshots.iter().enumerate().all(|(n, s)| m.screenshot(&root, n).and_then(|(f, _)| std::fs::read(f).ok()).is_some_and(|d| crate::store::sha256_hex(&d) == s.sha256));
    if !same {
        return Err(format!("The download of {}'s screenshots aren't the ones listed: not installed", l.name));
    }
    progress("installing");
    Provenance { repository: l.repository.clone(), version: l.version.clone(), reviewed: l.tier != "unlisted" }.write(&root);
    crate::store::put_in_place(&root, &dest)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::zip_of;

    #[test]
    fn the_built_in_list_reads() {
        let c: Catalog = REGISTRY.built_in();
        assert_eq!(c.index.format, 1);
        assert!(c.index.interfaces.iter().all(|l| crate::store::valid_id(&l.id) && l.asset.sha256.len() == 64));
    }

    #[test]
    fn installs_from_an_archive() {
        let dir = crate::store::tests::temp("iface");
        crate::paths::init(&dir.join("config.json"));
        let manifest = br#"{ "id": "trunkamp", "name": "Trunkamp", "version": "1.0.0", "api": 1, "access": "listen", "screenshots": [{ "path": "shots/main.png", "caption": "Playing" }] }"#;
        let data = zip_of(&[("trunkamp-1.0.0/interface.json", manifest), ("trunkamp-1.0.0/index.html", b"<h1>hi"), ("trunkamp-1.0.0/skin/a.css", b"b{}"), ("trunkamp-1.0.0/shots/main.png", b"png")]);
        assert_eq!(manifest_in_zip(&data).unwrap().id, "trunkamp");
        let shot = ListedScreenshot { sha256: crate::store::sha256_hex(b"png"), ..Default::default() };
        let l = Listing { id: "trunkamp".into(), name: "Trunkamp".into(), version: "1.0.0".into(), api: 1, tier: "community".into(), asset: Asset { url: "x.zip".into(), sha256: String::new() }, screenshots: vec![shot], ..Default::default() };
        let dest = interfaces_dir().join("trunkamp");
        // (paths::init keeps the first folder it's given: this test's, or another's.)
        let _ = std::fs::remove_dir_all(&dest);
        install_from(&l, &data, &|_| {}).unwrap();
        assert!(dest.join("index.html").is_file() && dest.join("skin/a.css").is_file());
        let m = Manifest::read(&dest).unwrap().unwrap();
        assert_eq!(m.screenshot(&dest, 0).unwrap().1, "image/png");
        assert!(m.screenshot(&dest, 1).is_none());
        assert!(Provenance::read(&dest).unwrap().reviewed);
        // Another version than listed: refused, and what's there stays.
        let wrong = Listing { version: "2.0.0".into(), ..l.clone() };
        assert!(install_from(&wrong, &data, &|_| {}).unwrap_err().contains("not installed"));
        assert!(dest.join("index.html").is_file());
        // Another screenshot than listed: refused.
        let mut other = l.clone();
        other.screenshots[0].sha256 = crate::store::sha256_hex(b"gif");
        assert!(install_from(&other, &data, &|_| {}).unwrap_err().contains("screenshots"));
        // No index.html, or a screenshot missing: refused.
        let bare = zip_of(&[("trunkamp-1.0.0/interface.json", manifest)]);
        assert!(install_from(&l, &bare, &|_| {}).unwrap_err().contains("index.html"));
        let no_shot = zip_of(&[("trunkamp-1.0.0/interface.json", manifest), ("trunkamp-1.0.0/index.html", b"<h1>hi")]);
        assert!(install_from(&l, &no_shot, &|_| {}).unwrap_err().contains("shots/main.png"));
        let _ = std::fs::remove_dir_all(&dest);
    }
}
