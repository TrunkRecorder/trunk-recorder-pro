//! What becomes of a call's files once the upload plugins have had them
//! (Trunk Recorder's audioArchive, callLog and archiveFilesOnFailure): each
//! plugin that takes concluded calls reports how it went, and when all have,
//! the audio and the JSON are deleted unless they're to be kept. An .m4a made
//! only for the plugins (compressWav off) goes then too. A call no plugin
//! takes keeps its files: deleting them would leave nothing of it at all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use trunk_recorder_plugin::Outcome;

/// Calls not settled after this keep their files (a plugin lost them).
const GIVE_UP: Duration = Duration::from_secs(3600);

/// A call's file rules (its system's recording settings).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileRules {
    /// Keep an .m4a of it.
    pub compress_wav: bool,
    /// Keep the audio.
    pub audio_archive: bool,
    /// Keep the JSON.
    pub call_log: bool,
    /// Keep everything when an upload failed.
    pub archive_files_on_failure: bool,
}

impl FileRules {
    pub fn of(r: &crate::config::Recording) -> FileRules {
        FileRules { compress_wav: r.compress_wav, audio_archive: r.audio_archive, call_log: r.call_log, archive_files_on_failure: r.archive_files_on_failure }
    }
    /// Nothing to do once the plugins are done.
    fn keeps_all(&self) -> bool {
        self.audio_archive && self.call_log && self.compress_wav
    }
}

struct Pending {
    /// Plugins still to report.
    left: usize,
    failed: bool,
    rules: FileRules,
    /// The files, without the extension.
    base: PathBuf,
    since: Instant,
}

#[derive(Default)]
pub struct Archive {
    pending: Mutex<HashMap<String, Pending>>,
}

impl Archive {
    /// Call `rel` (at `base`) goes to `plugins` plugins, each of which will report on it.
    pub fn expect(&self, rel: &str, plugins: usize, rules: FileRules, base: &Path) {
        let mut p = self.pending.lock().unwrap();
        p.retain(|_, x| x.since.elapsed() < GIVE_UP);
        if plugins == 0 || rules.keeps_all() {
            return;
        }
        p.insert(rel.to_string(), Pending { left: plugins, failed: false, rules, base: base.to_path_buf(), since: Instant::now() });
    }

    /// A plugin reported on call `rel`.
    pub fn result(&self, rel: &str, outcome: Outcome) {
        let mut p = self.pending.lock().unwrap();
        let Some(x) = p.get_mut(rel) else { return };
        x.failed |= outcome == Outcome::Failed;
        x.left = x.left.saturating_sub(1);
        if x.left == 0 {
            let x = p.remove(rel).unwrap();
            drop(p);
            settle(&x);
        }
    }
}

fn settle(x: &Pending) {
    let r = x.rules;
    let keep_all = x.failed && r.archive_files_on_failure;
    let file = |ext: &str| PathBuf::from(format!("{}.{ext}", x.base.display()));
    let gone = |ext: &str| {
        let _ = std::fs::remove_file(file(ext));
    };
    if !r.audio_archive && !keep_all {
        gone("wav");
        gone("m4a");
        gone("frames.jsonl");
    } else if !r.compress_wav {
        // Made only for the plugins.
        gone("m4a");
    }
    if !r.call_log && !keep_all {
        gone("json");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path, name: &str) -> PathBuf {
        let base = dir.join(name);
        for ext in ["wav", "json", "m4a"] {
            std::fs::write(format!("{}.{ext}", base.display()), b"x").unwrap();
        }
        base
    }
    fn left(base: &Path) -> Vec<&'static str> {
        ["wav", "json", "m4a"].into_iter().filter(|e| Path::new(&format!("{}.{e}", base.display())).exists()).collect()
    }

    #[test]
    fn files_go_once_every_plugin_has_reported() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Archive::default();
        let no_audio = FileRules { compress_wav: false, audio_archive: false, call_log: true, archive_files_on_failure: true };
        // Two plugins: nothing goes until both have reported.
        let b = files(&dir, "a");
        a.expect("a", 2, no_audio, &b);
        a.result("a", Outcome::Ok);
        assert_eq!(left(&b), ["wav", "json", "m4a"]);
        a.result("a", Outcome::Skipped);
        assert_eq!(left(&b), ["json"]);
        // A failed upload keeps the call (archiveFilesOnFailure), but not an M4A made only for uploading…
        let b = files(&dir, "b");
        a.expect("b", 1, no_audio, &b);
        a.result("b", Outcome::Failed);
        assert_eq!(left(&b), ["wav", "json"]);
        // …unless that's off too.
        let b = files(&dir, "c");
        a.expect("c", 1, FileRules { archive_files_on_failure: false, call_log: false, ..no_audio }, &b);
        a.result("c", Outcome::Failed);
        assert!(left(&b).is_empty());
        // Audio kept, M4A made only for the plugins: just the M4A goes.
        let b = files(&dir, "d");
        a.expect("d", 1, FileRules { audio_archive: true, ..no_audio }, &b);
        a.result("d", Outcome::Ok);
        assert_eq!(left(&b), ["wav", "json"]);
        // No plugin takes it: kept.
        let b = files(&dir, "e");
        a.expect("e", 0, no_audio, &b);
        a.result("e", Outcome::Ok);
        assert_eq!(left(&b), ["wav", "json", "m4a"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
