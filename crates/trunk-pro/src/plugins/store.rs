//! The plugin store: the registry's list of plugins (github.com/TrunkRecorder/plugins),
//! and installing one. An install downloads the archive for this computer,
//! checks its SHA-256 against the registry's, unpacks it beside the plugins
//! folder, asks the plugin who it is, and only then puts it in place of what
//! was there (`plugins/<id>/`). What plugins and interfaces share of this is
//! in [`crate::store`].
//!
//! A plugin that isn't in the registry can be installed from its GitHub
//! release instead, checked against the release's own `SHA256SUMS`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use trunk_recorder_plugin::{Manifest, API_VERSION};

pub use crate::store::{newer, valid_id};
use crate::store::{Provenance, Registry, Release};

use super::{describe, plugins_dir};

pub static REGISTRY: Registry = Registry {
    index_url: "https://raw.githubusercontent.com/TrunkRecorder/plugins/main/index.json",
    cache_file: "plugin-registry.json",
    built_in: include_str!("registry.json"),
    format: 1,
};
const MAX_DOWNLOAD: u64 = 200 << 20;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub format: u32,
    pub plugins: Vec<Listing>,
}

impl crate::store::Index for Index {
    fn format(&self) -> u32 {
        self.format
    }
}

pub type Catalog = crate::store::Catalog<Index>;

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

/// The registry's list: fetched if it can be, else saved or built in.
pub fn catalog() -> Catalog {
    REGISTRY.catalog()
}

/// The copy of the registry's list built into this version.
#[cfg(test)]
pub fn built_in() -> Catalog {
    REGISTRY.built_in()
}

// ─── Releases on GitHub (unlisted) ─────────────────────────────────────────

/// A GitHub release made with the template's release workflow, as a listing
/// (tier "unlisted"): its checksums are the release's own SHA256SUMS.
pub fn release_listing(repository: &str, tag: Option<&str>) -> Result<Listing, String> {
    let rel = Release::of(repository, tag, "the plugin template's release workflow")?;
    let release = format!("{}/{} {}", rel.owner, rel.repo, rel.tag);
    let manifest_name = rel.files.keys().find(|n| n.ends_with(".manifest.json")).cloned().ok_or_else(|| format!("{release} has no <id>-<version>.manifest.json"))?;
    let raw = rel.fetch(&manifest_name, 1 << 20)?;
    let m: Manifest = serde_json::from_slice(&raw).map_err(|e| format!("{manifest_name}: {e}"))?;
    let assets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "universal-apple-darwin", "x86_64-pc-windows-msvc"]
        .into_iter()
        .filter_map(|t| {
            let ext = if t.contains("windows") { "zip" } else { "tar.gz" };
            let (url, sha256) = rel.asset(&format!("{}-{}-{t}.{ext}", m.id, m.version))?;
            Some((t.to_string(), Asset { url, sha256 }))
        })
        .collect();
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
        tag: rel.tag,
        commit: String::new(),
        assets,
    })
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
    let data = crate::store::download(&a.url, &a.sha256, MAX_DOWNLOAD, &format!("{} {}", l.name, l.version))?;
    progress("installing");
    let dest = plugins_dir().join(&l.id);
    let (_staging, root) = crate::store::stage(&dest, &data, a.url.ends_with(".zip"))?;
    // The archive holds one folder (`<id>-<version>-<target>/`) with the executable in it.
    let exe = root.join(super::executable_name(&l.id));
    if !exe.is_file() {
        return Err(format!("The download of {} has no {}", l.name, super::executable_name(&l.id)));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
    }
    progress("checking");
    let m = describe(&exe)?;
    if m.id != l.id || m.version != l.version {
        return Err(format!("The download says it's {} {}, not {} {}: not installed", m.id, m.version, l.id, l.version));
    }
    Provenance { repository: l.repository.clone(), version: l.version.clone(), reviewed: l.tier != "unlisted" }.write(&root);
    crate::store::put_in_place(&root, &dest)?;
    Ok(m)
}

/// The repository an installed plugin came from, when that wasn't the registry.
pub fn unlisted_from(id: &str) -> Option<String> {
    let p = Provenance::read(&plugins_dir().join(id))?;
    (!p.reviewed && !p.repository.is_empty()).then_some(p.repository)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_list_reads() {
        let i = built_in().index;
        assert!(i.plugins.iter().any(|l| l.id == "openmhz" && l.tier == "official"));
        assert!(i.plugins.iter().all(|l| valid_id(&l.id) && l.assets.values().all(|a| a.sha256.len() == 64)));
    }
}
