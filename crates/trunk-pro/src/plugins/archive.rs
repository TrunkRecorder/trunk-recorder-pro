//! The archive: what of a call stays in the recordings folder once the
//! plugins are done with it (Trunk Recorder's audioArchive, callLog and
//! archiveFilesOnFailure).
//!
//! | Setting | On | Off |
//! |---|---|---|
//! | Keep audio (`audioArchive`) | kept once the plugins are done | deleted once the plugins are done |
//! | Keep call JSON (`callLog`) | kept | deleted |
//! | Keep when a plugin fails (`archiveFilesOnFailure`) | any plugin's failure keeps the audio and the JSON | failures change nothing |
//!
//! Every plugin that takes finished calls reports on each one: Ok and
//! Skipped both mean it's done, Failed that it gave up. A plugin that
//! doesn't report within [`GIVE_UP`] has failed. With no plugin taking
//! calls there's nothing to wait for: the two Keep settings alone decide
//! ([`plan`] writes nothing that won't be kept). An .m4a made only for the
//! plugins (compressWav off) goes with what isn't kept, and when the audio
//! is kept, unless it's all the audio there is (the WAV was never written).
//!
//! With a RAM spool ([`crate::spool`]), the files that may be deleted wait
//! there: those kept after all (a failure) move to the recordings folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use trunk_recorder_plugin::Outcome;

/// A call not settled after this counts as failed (a plugin lost it).
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
    /// Keep the audio and the JSON when a plugin fails.
    pub archive_files_on_failure: bool,
}

/// Where a finished call's files go, decided before the plugins have it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Kept whatever the plugins say (written to the recordings folder).
    pub keep_audio: bool,
    pub keep_json: bool,
    pub keep_m4a: bool,
    /// Written at all: what's kept, and what the plugins have.
    pub write_wav: bool,
    pub write_json: bool,
    pub write_frames: bool,
    pub make_m4a: bool,
    /// What isn't kept may go to the RAM spool.
    pub spool: bool,
}

/// A call's [`Plan`]: `takers` plugins take finished calls, one of which
/// `needs_wav` (the WAV itself, or no `encoder` to make it an M4A).
pub fn plan(rules: FileRules, takers: usize, needs_wav: bool, encoder: bool) -> Plan {
    let plugins = takers > 0;
    let (keep_audio, keep_json) = (rules.audio_archive, rules.call_log);
    let keep_m4a = keep_audio && rules.compress_wav;
    Plan {
        keep_audio,
        keep_json,
        keep_m4a,
        write_wav: keep_audio || (plugins && needs_wav),
        write_json: keep_json || plugins,
        write_frames: keep_audio || plugins,
        make_m4a: rules.compress_wav && encoder && (keep_audio || plugins),
        spool: plugins && !(keep_audio && keep_json && keep_m4a),
    }
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

/// Told what a call kept once its plugins are done: (its path, audio kept, JSON kept).
pub type OnSettle = Box<dyn Fn(&str, bool, bool) + Send + Sync>;

#[derive(Default)]
pub struct Archive {
    pending: Mutex<HashMap<String, Pending>>,
    on_settle: Mutex<Option<OnSettle>>,
}

impl Archive {
    /// Who to tell what each call kept (the history list).
    pub fn on_settle(&self, f: OnSettle) {
        *self.on_settle.lock().unwrap() = Some(f);
    }

    fn settled(&self, rel: &str, audio: bool, json: bool) {
        if let Some(f) = self.on_settle.lock().unwrap().as_ref() {
            f(rel, audio, json);
        }
    }

    /// Call `rel` (at `base`, and `spooled`) goes to `plugins` plugins, each
    /// of which will report on it. With none, it's settled now.
    pub fn expect(&self, rel: &str, plugins: usize, rules: FileRules, base: &Path, spooled: Option<&Path>) {
        let x = Pending { left: plugins, failed: false, rules, base: base.to_path_buf(), spooled: spooled.map(Path::to_path_buf), since: Instant::now() };
        let mut p = self.pending.lock().unwrap();
        // Calls a plugin never reported on: failed.
        let lost: Vec<(String, Pending)> = p.extract_if(|_, x| x.since.elapsed() >= GIVE_UP).collect();
        let now = if plugins == 0 {
            Some(x)
        } else {
            if !(rules.keeps_all() && spooled.is_none()) {
                p.insert(rel.to_string(), x);
            }
            None
        };
        drop(p);
        for (k, mut x) in lost {
            x.failed = true;
            let (audio, json) = settle(&x);
            self.settled(&k, audio, json);
        }
        // (Nothing waits on it, and its entry already says what it keeps.)
        if let Some(x) = now {
            settle(&x);
        }
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
            let (audio, json) = settle(&x);
            self.settled(rel, audio, json);
        }
    }
}

/// Delete what isn't kept; (audio kept, JSON kept).
fn settle(x: &Pending) -> (bool, bool) {
    let r = x.rules;
    let keep_all = x.failed && r.archive_files_on_failure;
    let audio = r.audio_archive || keep_all;
    let at = |base: &Path, ext: &str| PathBuf::from(format!("{}.{ext}", base.display()));
    let places = |ext: &str| [Some(at(&x.base, ext)), x.spooled.as_ref().map(|s| at(s, ext))].into_iter().flatten().collect::<Vec<_>>();
    let wav = places("wav").iter().any(|p| p.exists());
    // An .m4a made only for the plugins goes, unless it's the only audio.
    let json = r.call_log || keep_all;
    let keep = [("wav", audio), ("sdr", audio), ("frames.jsonl", audio), ("m4a", audio && (r.compress_wav || !wav)), ("json", json)];
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
    (audio, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder for one test.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trunk-pro-archive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    /// A call's files at `base`, those of `exts`.
    fn write(base: &Path, exts: &[&str]) {
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        for ext in exts {
            std::fs::write(format!("{}.{ext}", base.display()), b"x").unwrap();
        }
    }
    fn left(base: &Path) -> Vec<&'static str> {
        ["wav", "sdr", "m4a", "json"].into_iter().filter(|e| Path::new(&format!("{}.{e}", base.display())).exists()).collect()
    }
    fn rules(audio: bool, json: bool, m4a: bool, on_failure: bool) -> FileRules {
        FileRules { audio_archive: audio, call_log: json, compress_wav: m4a, archive_files_on_failure: on_failure }
    }
    const BOOLS: [bool; 2] = [false, true];

    /// What a plugin may answer, or (None) never answer.
    const ANSWERS: [&[Option<Outcome>]; 9] = [
        &[Some(Outcome::Ok)],
        &[Some(Outcome::Skipped)],
        &[Some(Outcome::Failed)],
        &[Some(Outcome::Ok), Some(Outcome::Skipped)],
        &[Some(Outcome::Ok), Some(Outcome::Failed)],
        &[Some(Outcome::Skipped), Some(Outcome::Failed)],
        &[Some(Outcome::Failed), Some(Outcome::Failed)],
        &[None],
        &[Some(Outcome::Ok), None],
    ];

    /// The rules, as the documentation's table states them: (audio, JSON) kept.
    fn kept(r: FileRules, failed: bool) -> (bool, bool) {
        let rescue = failed && r.archive_files_on_failure;
        (r.audio_archive || rescue, r.call_log || rescue)
    }

    /// Run a call with the WAV, frames, M4A and JSON written past every
    /// pattern of answers, for every combination of the settings: what's
    /// left is what the table says, and the history is told the same.
    #[test]
    fn what_is_kept_follows_the_table_for_every_case() {
        let dir = folder("table");
        let mut n = 0;
        for (audio, json, m4a, on_failure) in BOOLS.iter().flat_map(|&a| BOOLS.iter().flat_map(move |&j| BOOLS.iter().flat_map(move |&m| BOOLS.map(|f| (a, j, m, f))))) {
            let r = rules(audio, json, m4a, on_failure);
            for answers in ANSWERS {
                n += 1;
                let rel = format!("c{n}");
                let base = dir.join(&rel);
                write(&base, &["wav", "sdr", "m4a", "json"]);
                let a = Archive::default();
                let told = std::sync::Arc::new(Mutex::new(None));
                let t = told.clone();
                a.on_settle(Box::new(move |_, audio, json| *t.lock().unwrap() = Some((audio, json))));
                a.expect(&rel, answers.len(), r, &base, None);
                for o in answers.iter().flatten() {
                    a.result(&rel, *o);
                }
                let failed = answers.iter().any(|o| *o != Some(Outcome::Ok) && *o != Some(Outcome::Skipped));
                if answers.contains(&None) && !r.keeps_all() {
                    // A plugin that never answers: an hour on, the next call settles it.
                    // (With everything kept, nothing waits on it.)
                    a.pending.lock().unwrap().get_mut(&rel).unwrap().since = Instant::now() - GIVE_UP;
                    a.expect("next", 0, rules(true, true, true, true), &dir.join("next"), None);
                }
                let (keep_audio, keep_json) = kept(r, failed);
                let mut want = Vec::new();
                if keep_audio {
                    want.extend(["wav", "sdr"]);
                    // An M4A made only for the plugins goes, the WAV being there.
                    if m4a {
                        want.push("m4a");
                    }
                }
                if keep_json {
                    want.push("json");
                }
                let case = format!("audio {audio} json {json} m4a {m4a} keep-on-failure {on_failure}, answers {answers:?}");
                assert_eq!(left(&base), want, "{case}");
                if !r.keeps_all() {
                    assert_eq!(*told.lock().unwrap(), Some((keep_audio, keep_json)), "{case}");
                }
                assert!(a.pending.lock().unwrap().is_empty(), "{case}: settled");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Skipped is the same as Ok, whatever the settings.
    #[test]
    fn skipped_counts_as_done() {
        let dir = folder("skipped");
        for (audio, json, on_failure) in BOOLS.iter().flat_map(|&a| BOOLS.iter().flat_map(move |&j| BOOLS.map(|f| (a, j, f)))) {
            let r = rules(audio, json, false, on_failure);
            let mut got = Vec::new();
            for o in [Outcome::Ok, Outcome::Skipped] {
                let base = dir.join(format!("{audio}{json}{on_failure}{o:?}"));
                write(&base, &["wav", "json"]);
                let a = Archive::default();
                a.expect("x", 1, r, &base, None);
                a.result("x", o);
                got.push(left(&base));
            }
            assert_eq!(got[0], got[1], "audio {audio} json {json} keep-on-failure {on_failure}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing is deleted until every plugin has answered.
    #[test]
    fn files_wait_for_every_plugin() {
        let dir = folder("wait");
        let base = dir.join("a");
        write(&base, &["wav", "json"]);
        let a = Archive::default();
        a.expect("a", 3, rules(false, false, false, true), &base, None);
        a.result("a", Outcome::Ok);
        a.result("a", Outcome::Skipped);
        assert_eq!(left(&base), ["wav", "json"]);
        a.result("a", Outcome::Ok);
        assert!(left(&base).is_empty());
        // An answer about a call that isn't waiting changes nothing.
        a.result("a", Outcome::Failed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No plugin takes calls: the two Keep settings alone decide, at once,
    /// and nothing that won't be kept is written in the first place.
    #[test]
    fn with_no_plugin_the_keep_settings_alone_decide() {
        let dir = folder("none");
        for (audio, json, on_failure) in BOOLS.iter().flat_map(|&a| BOOLS.iter().flat_map(move |&j| BOOLS.map(|f| (a, j, f)))) {
            let r = rules(audio, json, false, on_failure);
            let base = dir.join(format!("{audio}{json}{on_failure}"));
            write(&base, &["wav", "json"]);
            let a = Archive::default();
            a.expect("x", 0, r, &base, None);
            let want: Vec<&str> = [("wav", audio), ("json", json)].into_iter().filter(|x| x.1).map(|x| x.0).collect();
            assert_eq!(left(&base), want, "audio {audio} json {json} keep-on-failure {on_failure}");
            assert!(a.pending.lock().unwrap().is_empty());
            for (needs_wav, encoder) in [(false, false), (true, true), (false, true)] {
                let p = plan(r, 0, needs_wav, encoder);
                assert_eq!((p.write_wav, p.write_json, p.write_frames, p.make_m4a, p.spool), (audio, json, audio, false, false));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What's written, and where, before the plugins have the call: the
    /// Keep settings exactly; with plugins, also what they take (the JSON,
    /// the WAV or an M4A made for them) and the frames, which a failure may
    /// keep; the spool only when something may be deleted.
    #[test]
    fn the_plan_for_every_case() {
        for (audio, json, m4a, on_failure) in BOOLS.iter().flat_map(|&a| BOOLS.iter().flat_map(move |&j| BOOLS.iter().flat_map(move |&m| BOOLS.map(|f| (a, j, m, f))))) {
            let r = rules(audio, json, m4a, on_failure);
            for takers in [0, 1, 2] {
                for (needs_wav, encoder) in [(false, false), (false, true), (true, false), (true, true)] {
                    let p = plan(r, takers, needs_wav, encoder);
                    let plugins = takers > 0;
                    let case = format!("audio {audio} json {json} m4a {m4a} keep-on-failure {on_failure}, {takers} plugins, needs WAV {needs_wav}, encoder {encoder}: {p:?}");
                    assert_eq!((p.keep_audio, p.keep_json, p.keep_m4a), (audio, json, audio && m4a), "{case}");
                    assert_eq!(p.write_wav, audio || (plugins && needs_wav), "{case}");
                    assert_eq!(p.write_json, json || plugins, "{case}");
                    assert_eq!(p.write_frames, audio || plugins, "{case}");
                    assert_eq!(p.make_m4a, m4a && encoder && (audio || plugins), "{case}");
                    assert_eq!(p.spool, plugins && !(audio && json && m4a), "{case}");
                }
            }
        }
    }

    /// With a RAM spool: what may be deleted waits there, and what's kept
    /// after all (a failure) moves to the recordings folder.
    #[test]
    fn spooled_files_go_or_move_to_the_recordings_folder() {
        let dir = folder("spool");
        let (capture, spool) = (dir.join("capture"), dir.join("spool"));
        let a = Archive::default();
        let plugins_only = rules(false, false, false, true);
        let call = |name: &str, kept: &[&str], spooled: &[&str]| {
            let (b, s) = (capture.join("sys").join(name), spool.join("sys").join(name));
            write(&b, kept);
            write(&s, spooled);
            (b, s)
        };
        // Done: nothing is left anywhere.
        let (b, s) = call("a", &[], &["json", "m4a"]);
        a.expect("a", 1, plugins_only, &b, Some(&s));
        a.result("a", Outcome::Ok);
        assert!(left(&b).is_empty() && left(&s).is_empty());
        // Failed: the M4A (the only audio) and the JSON move to the recordings folder.
        let (b, s) = call("b", &[], &["json", "m4a"]);
        a.expect("b", 1, plugins_only, &b, Some(&s));
        a.result("b", Outcome::Failed);
        assert_eq!((left(&b), left(&s)), (vec!["m4a", "json"], vec![]));
        // Failed, but failures change nothing: gone.
        let (b, s) = call("c", &[], &["json", "m4a"]);
        a.expect("c", 1, rules(false, false, false, false), &b, Some(&s));
        a.result("c", Outcome::Failed);
        assert!(left(&b).is_empty() && left(&s).is_empty());
        // The JSON kept, written to the recordings folder; the spooled M4A goes.
        let (b, s) = call("d", &["json"], &["m4a"]);
        a.expect("d", 1, rules(false, true, false, true), &b, Some(&s));
        a.result("d", Outcome::Skipped);
        assert_eq!((left(&b), left(&s)), (vec!["json"], vec![]));
        // Never answered: after an hour it has failed, so it moves to the recordings folder…
        let (b, s) = call("e", &[], &["json", "m4a"]);
        a.expect("e", 1, plugins_only, &b, Some(&s));
        a.pending.lock().unwrap().get_mut("e").unwrap().since = Instant::now() - GIVE_UP;
        a.expect("next", 0, plugins_only, &capture.join("sys/next"), None);
        assert_eq!((left(&b), left(&s)), (vec!["m4a", "json"], vec![]));
        // …or, with failures changing nothing, goes.
        let (b, s) = call("f", &[], &["json", "m4a"]);
        a.expect("f", 1, rules(false, false, false, false), &b, Some(&s));
        a.pending.lock().unwrap().get_mut("f").unwrap().since = Instant::now() - GIVE_UP;
        a.expect("next", 0, plugins_only, &capture.join("sys/next"), None);
        assert!(left(&b).is_empty() && left(&s).is_empty() && a.pending.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
