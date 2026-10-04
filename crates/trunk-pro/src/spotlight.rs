//! Is Spotlight indexing the recordings folder? (macOS.) It reads every file
//! saved there into its index: disk writes for calls nobody searches for.
//! Its exclusion list (System Settings → Spotlight → Search Privacy) is
//! private to the system, so this asks Spotlight itself, as any program may:
//! a folder whose name ends in `.noindex` never is; one on a disk with
//! indexing off isn't; one where a search finds files (or a file written
//! for the purpose, given a couple of minutes) is. A search that finds
//! nothing proves nothing (Spotlight may be busy, or not answer this
//! process), so that's "unknown".

use std::path::Path;

use serde_json::{json, Value};

/// `{path, state: "indexed" | "excluded" | "unknown", why}`; None off macOS
/// or when the folder isn't there.
pub fn check(dir: &Path) -> Option<Value> {
    if !cfg!(target_os = "macos") || !dir.is_dir() {
        return None;
    }
    let (state, why) = state(dir);
    Some(json!({ "path": dir.display().to_string(), "state": state, "why": why }))
}

fn state(dir: &Path) -> (&'static str, String) {
    if dir.components().any(|c| c.as_os_str().to_string_lossy().ends_with(".noindex")) {
        return ("excluded", "its name ends in .noindex".into());
    }
    let s = run("/usr/bin/mdutil", &["-s", &dir.to_string_lossy()]).unwrap_or_default();
    if s.contains("Indexing disabled") {
        return ("excluded", "Spotlight is off for its disk".into());
    }
    let query = "kMDItemFSName == \"*.json\" || kMDItemFSName == \"*.wav\" || kMDItemFSName == \"*.m4a\"";
    let n: u64 = run("/usr/bin/mdfind", &["-onlyin", &dir.to_string_lossy(), "-count", query]).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    if n > 0 {
        return ("indexed", format!("Spotlight finds {n} of its calls"));
    }
    // Nothing found (an empty folder, or one excluded): a file of our own.
    let name = format!("trunk-pro-spotlight-check-{}.txt", std::process::id());
    let probe = dir.join(&name);
    if std::fs::write(&probe, "Trunk Recorder checks whether Spotlight indexes this folder. Safe to delete.\n").is_err() {
        return ("unknown", "couldn't write to it".into());
    }
    let query = format!("kMDItemFSName == \"{name}\"");
    let mut found = false;
    for _ in 0..24 {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if run("/usr/bin/mdfind", &["-onlyin", &dir.to_string_lossy(), &query]).is_some_and(|s| !s.trim().is_empty()) {
            found = true;
            break;
        }
    }
    let _ = std::fs::remove_file(&probe);
    if found {
        ("indexed", "Spotlight indexed a file written to it".into())
    } else {
        ("unknown", "Spotlight didn't find a file written to it within two minutes: it's probably excluded, but macOS doesn't let apps read the list".into())
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(cmd).args(args).stderr(std::process::Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}
