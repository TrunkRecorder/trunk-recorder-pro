//! What becomes of a call's files once the upload plugins have had them
//! (Trunk Recorder's audioArchive, callLog and archiveFilesOnFailure): each
//! plugin that takes concluded calls reports how it went, and when all have,
//! the audio and the JSON are deleted unless they're to be kept. An .m4a made
//! only for the plugins (compressWav off) goes then too, unless it's all the
//! audio there is (the WAV was never written). A call no plugin takes keeps
//! its files: deleting them would leave nothing of it at all.
//!
//! With a RAM spool ([`crate::spool`]), the files only the plugins needed
//! wait there: those kept after all (an upload failed) move to the
//! recordings folder then.

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
    /// The same in the spool, where the ones only for the plugins are.
    spooled: Option<PathBuf>,
    since: Instant,
}

#[derive(Default)]
pub struct Archive {
    pending: Mutex<HashMap<String, Pending>>,
}

impl Archive {
    /// Call `rel` (at `base`, and `spooled`) goes to `plugins` plugins, each of which will report on it.
    pub fn expect(&self, rel: &str, plugins: usize, rules: FileRules, base: &Path, spooled: Option<&Path>) {
        let mut p = self.pending.lock().unwrap();
        // (Their files stay; anything in the spool goes to the recordings folder with time.)
        p.retain(|_, x| x.since.elapsed() < GIVE_UP);
        if plugins == 0 || (rules.keeps_all() && spooled.is_none()) {
            return;
        }
        let x = Pending { left: plugins, failed: false, rules, base: base.to_path_buf(), spooled: spooled.map(Path::to_path_buf), since: Instant::now() };
        p.insert(rel.to_string(), x);
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
    let audio = r.audio_archive || keep_all;
    let at = |base: &Path, ext: &str| PathBuf::from(format!("{}.{ext}", base.display()));
    let places = |ext: &str| [Some(at(&x.base, ext)), x.spooled.as_ref().map(|s| at(s, ext))].into_iter().flatten().collect::<Vec<_>>();
    let wav = places("wav").iter().any(|p| p.exists());
    // An .m4a made only for the plugins goes, unless it's the only audio.
    let keep = [("wav", audio), ("frames.jsonl", audio), ("m4a", audio && (r.compress_wav || !wav)), ("json", r.call_log || keep_all)];
    for (ext, kept) in keep {
        if !kept {
            for p in places(ext) {
                let _ = std::fs::remove_file(p);
            }
        } else if let Some(s) = x.spooled.as_ref().map(|s| at(s, ext)).filter(|p| p.exists()) {
            if let Err(e) = crate::spool::move_file(&s, &at(&x.base, ext)) {
                log::warn!("Couldn't move {} out of the spool: {e}", s.display());
            }
        }
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
        a.expect("a", 2, no_audio, &b, None);
        a.result("a", Outcome::Ok);
        assert_eq!(left(&b), ["wav", "json", "m4a"]);
        a.result("a", Outcome::Skipped);
        assert_eq!(left(&b), ["json"]);
        // A failed upload keeps the call (archiveFilesOnFailure), but not an M4A made only for uploading…
        let b = files(&dir, "b");
        a.expect("b", 1, no_audio, &b, None);
        a.result("b", Outcome::Failed);
        assert_eq!(left(&b), ["wav", "json"]);
        // …unless that's off too.
        let b = files(&dir, "c");
        a.expect("c", 1, FileRules { archive_files_on_failure: false, call_log: false, ..no_audio }, &b, None);
        a.result("c", Outcome::Failed);
        assert!(left(&b).is_empty());
        // Audio kept, M4A made only for the plugins: just the M4A goes.
        let b = files(&dir, "d");
        a.expect("d", 1, FileRules { audio_archive: true, ..no_audio }, &b, None);
        a.result("d", Outcome::Ok);
        assert_eq!(left(&b), ["wav", "json"]);
        // No plugin takes it: kept.
        let b = files(&dir, "e");
        a.expect("e", 0, no_audio, &b, None);
        a.result("e", Outcome::Ok);
        assert_eq!(left(&b), ["wav", "json", "m4a"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spooled_files_go_or_move_to_the_recordings_folder() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-archive-spool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (capture, spool) = (dir.join("capture"), dir.join("spool"));
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::create_dir_all(spool.join("sys")).unwrap();
        let a = Archive::default();
        let upload_only = FileRules { compress_wav: false, audio_archive: false, call_log: false, archive_files_on_failure: true };
        // Uploaded: nothing is left anywhere.
        let (b, s) = (capture.join("sys/a"), spool.join("sys/a"));
        for ext in ["json", "m4a"] {
            std::fs::write(format!("{}.{ext}", s.display()), b"x").unwrap();
        }
        a.expect("a", 1, upload_only, &b, Some(&s));
        a.result("a", Outcome::Ok);
        assert!(left(&b).is_empty() && left(&s).is_empty());
        // Failed: the M4A (the only audio) and the JSON move to the recordings folder.
        let (b, s) = (capture.join("sys/b"), spool.join("sys/b"));
        for ext in ["json", "m4a"] {
            std::fs::write(format!("{}.{ext}", s.display()), b"x").unwrap();
        }
        a.expect("b", 1, upload_only, &b, Some(&s));
        a.result("b", Outcome::Failed);
        assert_eq!(left(&b), ["json", "m4a"]);
        assert!(left(&s).is_empty());
        // The JSON kept (callLog), written to the recordings folder; the spooled M4A goes.
        let (b, s) = (capture.join("sys/c"), spool.join("sys/c"));
        std::fs::write(format!("{}.json", b.display()), b"x").unwrap();
        std::fs::write(format!("{}.m4a", s.display()), b"x").unwrap();
        a.expect("c", 1, FileRules { call_log: true, ..upload_only }, &b, Some(&s));
        a.result("c", Outcome::Ok);
        assert_eq!(left(&b), ["json"]);
        assert!(left(&s).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
