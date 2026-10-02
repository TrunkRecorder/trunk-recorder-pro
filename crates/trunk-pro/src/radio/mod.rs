//! Radios whose drivers are vendor C libraries — USRP (UHD), Airspy
//! (libairspy) and anything with a SoapySDR module. Nothing links against
//! them: each library is looked for at run time, so the same binary records
//! from RTL-SDRs out of the box and from these once their driver is
//! installed. `TRUNK_PRO_UHD` / `TRUNK_PRO_AIRSPY` / `TRUNK_PRO_SOAPY` name a
//! library file to use instead of searching.

pub mod airspy;
pub mod soapy;
pub mod uhd;

use std::path::PathBuf;

use libloading::Library;

/// Open the first library that loads: `env` if set, else each candidate
/// name (the system's search path), else versioned files in the usual
/// install folders (`lib<stem>.so.<version>`, highest first). `global`: its
/// symbols resolve the plug-ins it loads itself (SoapySDR's modules).
pub fn load(env: &str, names: &[&str], stem: &str, global: bool) -> Result<(Library, String), String> {
    let try_one = |p: &str| -> Result<Library, String> {
        // SAFETY: loading a vendor driver runs its initialisers; that is the point.
        #[cfg(unix)]
        if global {
            use libloading::os::unix::{Library as Unix, RTLD_GLOBAL, RTLD_NOW};
            return unsafe { Unix::open(Some(p), RTLD_NOW | RTLD_GLOBAL) }.map(Library::from).map_err(|e| e.to_string());
        }
        let _ = global;
        unsafe { Library::new(p) }.map_err(|e| e.to_string())
    };
    if let Ok(p) = std::env::var(env) {
        return try_one(&p).map(|l| (l, p.clone())).map_err(|e| format!("{env}={p}: {e}"));
    }
    let mut tried: Vec<String> = Vec::new();
    // A library that exists but won't load (wrong architecture, refused by
    // code signing, missing its own dependencies) says more than "not found".
    let mut refused: Option<String> = None;
    let mut note = |p: &str, e: String| {
        tried.push(p.to_string());
        if refused.is_none() && std::path::Path::new(p).exists() {
            refused = Some(e);
        }
    };
    for n in names {
        match try_one(n) {
            Ok(l) => return Ok((l, n.to_string())),
            Err(e) => note(n, e),
        }
    }
    for dir in lib_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut found: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.starts_with(&format!("lib{stem}.so.")))).collect();
        found.sort();
        for p in found.iter().rev() {
            let s = p.display().to_string();
            match try_one(&s) {
                Ok(l) => return Ok((l, s)),
                Err(e) => note(&s, e),
            }
        }
    }
    match refused {
        Some(e) => Err(format!("found but couldn't be loaded: {e}")),
        None => Err(format!("not found (looked for {})", tried.join(", "))),
    }
}

fn lib_dirs() -> Vec<PathBuf> {
    ["/usr/lib", "/usr/lib64", "/usr/local/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib/arm-linux-gnueabihf"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// Where a driver usually lives, per platform, after the bare names.
pub fn candidates(unix_name: &str, mac_name: &str, win_names: &[&'static str]) -> Vec<String> {
    if cfg!(target_os = "macos") {
        ["", "/opt/homebrew/lib/", "/usr/local/lib/", "/opt/local/lib/"].iter().map(|d| format!("{d}{mac_name}")).collect()
    } else if cfg!(windows) {
        win_names.iter().map(|s| s.to_string()).collect()
    } else {
        vec![unix_name.to_string()]
    }
}

/// Drivers and the devices found through them, for the interface:
/// `{usrp: {available, detail, devices}, airspy: {…}, soapy: {…, modules}}`.
/// USRPs and SoapySDR devices are searched only with `search` (a device
/// search can take seconds).
pub fn radios_json(search: bool) -> serde_json::Value {
    let u = uhd::info();
    let mut uj = u.json();
    uj["devices"] = if search && u.loaded {
        match uhd::find("") {
            Ok(v) => v.iter().map(|s| serde_json::json!({ "args": usrp_args(s), "label": usrp_label(s) })).collect(),
            Err(e) => {
                uj["detail"] = serde_json::json!(e);
                serde_json::json!([])
            }
        }
    } else {
        serde_json::Value::Null
    };
    serde_json::json!({ "usrp": uj, "airspy": airspy_json(), "soapy": soapy::json(search) })
}

fn airspy_json() -> serde_json::Value {
    let a = airspy::info();
    let mut j = a.json();
    j["devices"] = serde_json::Value::Array(if a.loaded { airspy::devices() } else { vec![] });
    j
}

/// "type=b200,name=,serial=3070E68,product=B200" → "serial=3070E68" (what opens exactly that one).
fn usrp_args(found: &str) -> String {
    let kv = |k: &str| found.split(',').find_map(|p| p.strip_prefix(&format!("{k}=")).filter(|v| !v.is_empty()).map(str::to_string));
    kv("serial").map(|s| format!("serial={s}")).or_else(|| kv("addr").map(|a| format!("addr={a}"))).unwrap_or_else(|| found.to_string())
}
fn usrp_label(found: &str) -> String {
    let kv = |k: &str| found.split(',').find_map(|p| p.strip_prefix(&format!("{k}=")).filter(|v| !v.is_empty()).map(str::to_string));
    let product = kv("product").or_else(|| kv("type")).unwrap_or_else(|| "USRP".into());
    match (kv("serial"), kv("addr")) {
        (Some(s), _) => format!("{product} · SN {s}"),
        (None, Some(a)) => format!("{product} · {a}"),
        _ => product,
    }
}

/// A driver's state for the interface.
#[derive(Clone, Debug)]
pub struct DriverInfo {
    pub loaded: bool,
    /// The library file and version, or why it isn't available.
    pub detail: String,
}

impl DriverInfo {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "available": self.loaded, "detail": self.detail })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usrp_found_strings() {
        let s = "type=b200,name=MyB200,serial=3070E68,product=B200";
        assert_eq!(usrp_args(s), "serial=3070E68");
        assert_eq!(usrp_label(s), "B200 · SN 3070E68");
        let n = "type=usrp2,addr=192.168.10.2,name=,serial=";
        assert_eq!(usrp_args(n), "addr=192.168.10.2");
        assert_eq!(usrp_label(n), "usrp2 · 192.168.10.2");
    }
}
