//! The archive: what of a call stays in the recordings folder once the
//! plugins are done with it (Trunk Recorder's audioArchive and compressWav,
//! here a setting for each kind of audio file, callLog and
//! archiveFilesOnFailure).
//!
//! | Setting | On | Off |
//! |---|---|---|
//! | Keep the WAV (`archiveWav`) | kept once the plugins are done | deleted once the plugins are done |
//! | Keep an M4A (`archiveM4a`) | one is made, and kept | one made for a plugin is deleted once the plugins are done |
//! | Keep call JSON (`callLog`) | kept | deleted |
//! | Keep when a plugin fails (`archiveFilesOnFailure`) | any plugin's failure keeps every file written | failures change nothing |
//!
//! Every plugin that takes finished calls reports on each one: Ok and
//! Skipped both mean it's done, Failed that it gave up. A plugin that
//! doesn't report within [`GIVE_UP`] has failed. With no plugin taking
//! calls there's nothing to wait for: the two Keep settings alone decide
//! ([`plan`] writes nothing that won't be kept). One kind of audio kept and
//! not there (no .m4a could be made), the other is kept in its place: a
//! call whose audio is to be kept never loses it all. The frames file
//! (captureFrames) stays with any audio kept.
//!
//! With a RAM spool ([`crate::spool`]), the files that may be deleted wait
//! there: those kept after all (a failure) move to the recordings folder.
//!
//! A call held back from a plugin ([`super::hold`]) isn't given up on while
//! it waits: that plugin's [`GIVE_UP`] counts from when it's given the call.
//! The hold keeps what the archive needs of it ([`Ticket`]) with the call,
//! so it's settled the same after a restart.
//!
//! It follows every finished call to every plugin it goes to, so it's also
//! where a call's delivery is timed ([`super::timing`]): from the call's end,
//! less what the hold kept it, to each plugin's Ok.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use trunk_recorder_plugin::Outcome;
use trunk_core::metrics::Sink;

use super::timing::Totals;

/// A call not settled after this counts as failed (a plugin lost it).
const GIVE_UP: Duration = Duration::from_secs(3600);

/// A call's file rules (its system's recording settings).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "SavedRules")]
pub struct FileRules {
    /// Keep the WAV.
    pub archive_wav: bool,
    /// Keep an .m4a.
    pub archive_m4a: bool,
    /// Keep the JSON.
    pub call_log: bool,
    /// Keep every file when a plugin fails.
    pub archive_files_on_failure: bool,
}

/// [`FileRules`] as a held call's [`Ticket`] kept them: before v0.2.4 one
/// setting kept the audio (`audioArchive`), and `compressWav` an .m4a too.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedRules {
    archive_wav: Option<bool>,
    archive_m4a: Option<bool>,
    #[serde(default)]
    audio_archive: bool,
    #[serde(default)]
    compress_wav: bool,
    call_log: bool,
    archive_files_on_failure: bool,
}

impl From<SavedRules> for FileRules {
    fn from(s: SavedRules) -> FileRules {
        FileRules {
            archive_wav: s.archive_wav.unwrap_or(s.audio_archive),
            archive_m4a: s.archive_m4a.unwrap_or(s.audio_archive && s.compress_wav),
            call_log: s.call_log,
            archive_files_on_failure: s.archive_files_on_failure,
        }
    }
}

/// Where a finished call's files go, decided before the plugins have it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Kept whatever the plugins say (written to the recordings folder).
    pub keep_wav: bool,
    pub keep_m4a: bool,
    pub keep_json: bool,
    /// Some audio is kept (and the frames file with it).
    pub keep_audio: bool,
    /// Written at all: what's kept, and what the plugins have.
    pub write_wav: bool,
    pub write_json: bool,
    pub write_frames: bool,
    /// An .m4a is encoded: to keep (archiveM4a), or for a plugin that wants
    /// one. Should that fail, the WAV is written instead, when it wasn't.
    pub make_m4a: bool,
    /// What isn't kept may go to the RAM spool.
    pub spool: bool,
}

/// What the plugins take of a finished call ([`super::PluginHost::plugin_audio`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PluginAudio {
    /// The plugins it goes to.
    pub takers: usize,
    /// One takes the WAV file.
    pub wav: bool,
    /// One wants an .m4a.
    pub m4a: bool,
}

/// A call's [`Plan`], the one place that says which of its files are
/// written: by its system's `rules`, what its `plugins` take, and whether
/// there's an `encoder` (for an .m4a). The finish thread writes them, and
/// the plugins are handed them as they are.
pub fn plan(rules: FileRules, plugins: PluginAudio, encoder: bool) -> Plan {
    let to_plugins = plugins.takers > 0;
    let (keep_wav, keep_m4a, keep_json) = (rules.archive_wav, rules.archive_m4a, rules.call_log);
    let keep_audio = keep_wav || keep_m4a;
    Plan {
        keep_wav,
        keep_m4a,
        keep_json,
        keep_audio,
        // (With no .m4a to be made, the WAV in its place: kept, or for a plugin that wants one.)
        write_wav: keep_wav || (keep_m4a && !encoder) || (to_plugins && (plugins.wav || (plugins.m4a && !encoder))),
        write_json: keep_json || to_plugins,
        write_frames: keep_audio || to_plugins,
        make_m4a: encoder && (keep_m4a || (to_plugins && plugins.m4a)),
        spool: to_plugins && !rules.keeps_all(),
    }
}

impl FileRules {
    pub fn of(r: &crate::config::Recording) -> FileRules {
        FileRules { archive_wav: r.archive_wav, archive_m4a: r.archive_m4a, call_log: r.call_log, archive_files_on_failure: r.archive_files_on_failure }
    }
    /// Nothing to do once the plugins are done.
    fn keeps_all(&self) -> bool {
        self.archive_wav && self.archive_m4a && self.call_log
    }
}

struct Pending {
    /// Plugins still to report.
    left: usize,
    /// Of those, the ones it's held back from (not given up on meanwhile).
    held: usize,
    failed: bool,
    rules: FileRules,
    /// The files, without the extension.
    base: PathBuf,
    /// The same in the spool, where the ones only for the plugins are.
    spooled: Option<PathBuf>,
    /// Whether a WAV was written (None: look, for a ticket from before this was kept).
    wav: Option<bool>,
    since: Instant,
    /// When the call ended, Unix ms (None: not known, a ticket from before this was kept).
    ended_ms: Option<u64>,
    /// How long it was held back from each plugin it was then given to, ms, by label.
    held_ms: Vec<(String, u64)>,
}

/// What the archive is to do with a call's files once its plugins are done,
/// kept with a held call so that's done after a restart too ([`Archive::restore`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub rules: FileRules,
    pub base: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spooled: Option<PathBuf>,
    /// A WAV was written (absent in tickets from before this was kept: looked for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wav: Option<bool>,
    /// When the call ended, Unix ms, so its delivery is timed after a restart too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
}

/// Told what a call kept once its plugins are done: (its path, audio kept, JSON kept).
pub type OnSettle = Box<dyn Fn(&str, bool, bool) + Send + Sync>;

#[derive(Default)]
pub struct Archive {
    pending: Mutex<HashMap<String, Pending>>,
    on_settle: Mutex<Option<OnSettle>>,
    /// Each plugin's delivery timing, by id (its copies together).
    timing: Mutex<BTreeMap<String, Totals>>,
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

    /// Call `rel` (at `base`, and `spooled`; `wav`: whether a WAV was
    /// written; `ended_ms`: when it ended, Unix ms) goes to `plugins`
    /// plugins, each of which will report on it. With none, it's settled now.
    #[allow(clippy::too_many_arguments)]
    pub fn expect(&self, rel: &str, plugins: usize, rules: FileRules, base: &Path, spooled: Option<&Path>, wav: Option<bool>, ended_ms: Option<u64>) {
        let x = Pending { left: plugins, held: 0, failed: false, rules, base: base.to_path_buf(), spooled: spooled.map(Path::to_path_buf), wav, since: Instant::now(), ended_ms, held_ms: Vec::new() };
        let mut p = self.pending.lock().unwrap();
        // Calls a plugin never reported on: failed.
        let lost: Vec<(String, Pending)> = p.extract_if(|_, x| x.held == 0 && x.since.elapsed() >= GIVE_UP).collect();
        // (Followed even when nothing waits on its files: its delivery is timed.)
        let now = if plugins == 0 {
            Some(x)
        } else {
            p.insert(rel.to_string(), x);
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

    /// What's to be done with call `rel`'s files (None: nothing waits on its plugins).
    pub fn ticket(&self, rel: &str) -> Option<Ticket> {
        self.pending.lock().unwrap().get(rel).map(|x| Ticket { rules: x.rules, base: x.base.clone(), spooled: x.spooled.clone(), wav: x.wav, ended_ms: x.ended_ms })
    }

    /// Call `rel` is held back from one of its plugins.
    pub fn hold(&self, rel: &str) {
        if let Some(x) = self.pending.lock().unwrap().get_mut(rel) {
            x.held += 1;
        }
    }

    /// Call `rel` was given to plugin `label` after it was held back `held_ms`
    /// (whose hour to report starts now).
    pub fn release(&self, rel: &str, label: &str, held_ms: u64) {
        if let Some(x) = self.pending.lock().unwrap().get_mut(rel) {
            x.held = x.held.saturating_sub(1);
            x.since = Instant::now();
            x.held_ms.push((label.to_string(), held_ms));
        }
    }

    /// After a restart: call `rel` is still held back from `plugins` plugins.
    /// (Those that had it already are forgotten with the run before.)
    pub fn restore(&self, rel: &str, plugins: usize, t: Ticket) {
        let x = Pending { left: plugins, held: plugins, failed: false, rules: t.rules, base: t.base, spooled: t.spooled, wav: t.wav, since: Instant::now(), ended_ms: t.ended_ms, held_ms: Vec::new() };
        self.pending.lock().unwrap().insert(rel.to_string(), x);
    }

    /// Plugin `label` (a copy's: `openmhz#2`) reported on call `rel`, its own
    /// time on it `took_ms` when it said.
    pub fn result(&self, rel: &str, label: &str, outcome: Outcome, took_ms: Option<u64>) {
        self.result_at(rel, label, outcome, took_ms, crate::clock::unix_ms());
    }

    /// The same, at `now_ms` (Unix ms).
    fn result_at(&self, rel: &str, label: &str, outcome: Outcome, took_ms: Option<u64>, now_ms: u64) {
        let mut p = self.pending.lock().unwrap();
        let Some(x) = p.get_mut(rel) else { return };
        let held = x.held_ms.iter().position(|(l, _)| l == label).map_or(0, |i| x.held_ms.swap_remove(i).1);
        if let (Outcome::Ok, Some(ended)) = (outcome, x.ended_ms) {
            let id = super::parse_label(label).0.to_string();
            self.timing.lock().unwrap().entry(id).or_default().delivered(now_ms.saturating_sub(ended), held, took_ms);
        }
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

impl Archive {
    /// Each plugin's delivery timing into `sink` ([`Totals::report`]).
    pub fn report(&self, sink: &mut dyn Sink) {
        for (id, t) in self.timing.lock().unwrap().iter_mut() {
            t.report(id, sink);
        }
    }

    #[cfg(test)]
    pub fn timing(&self, id: &str) -> Totals {
        self.timing.lock().unwrap().get(id).copied().unwrap_or_default()
    }
}

/// Delete what isn't kept; (audio kept, JSON kept).
fn settle(x: &Pending) -> (bool, bool) {
    let r = x.rules;
    let keep_all = x.failed && r.archive_files_on_failure;
    let at = |base: &Path, ext: &str| PathBuf::from(format!("{}.{ext}", base.display()));
    let places = |ext: &str| [Some(at(&x.base, ext)), x.spooled.as_ref().map(|s| at(s, ext))].into_iter().flatten().collect::<Vec<_>>();
    let wav = x.wav.unwrap_or_else(|| places("wav").iter().any(|p| p.exists()));
    let m4a = places("m4a").iter().any(|p| p.exists());
    // One kind of audio kept and not there: the other in its place.
    let (keep_wav, keep_m4a) = (r.archive_wav || keep_all || (r.archive_m4a && !m4a), r.archive_m4a || keep_all || (r.archive_wav && !wav));
    let audio = r.archive_wav || r.archive_m4a || keep_all;
    let json = r.call_log || keep_all;
    let keep = [("wav", keep_wav), ("sdr", audio), ("frames.jsonl", audio), ("m4a", keep_m4a), ("json", json)];
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
    fn rules(wav: bool, m4a: bool, json: bool, on_failure: bool) -> FileRules {
        FileRules { archive_wav: wav, archive_m4a: m4a, call_log: json, archive_files_on_failure: on_failure }
    }
    /// The archive follows each call to every plugin, so it times each
    /// delivery: from the call's end, the hold's part, to each plugin's Ok.
    #[test]
    fn deliveries_are_timed_as_the_call_is_followed() {
        let a = Archive::default();
        let keep = rules(true, true, true, false);
        let base = std::env::temp_dir().join("trunk-pro-archive-timed");
        a.expect("dcfd/a", 3, keep, &base, None, Some(false), Some(10_000));
        a.result_at("dcfd/a", "broadcastify", Outcome::Ok, Some(400), 12_500);
        a.release("dcfd/a", "openmhz#2", 60_000);
        a.result_at("dcfd/a", "openmhz#2", Outcome::Ok, Some(300), 71_200);
        a.result_at("dcfd/a", "rdioscanner", Outcome::Failed, Some(9), 13_000);
        let b = a.timing("broadcastify");
        assert_eq!((b.delivered, b.age_ms, b.held_ms, b.upload_ms, b.wait_ms), (1, 2500, 0, 400, 2100));
        let o = a.timing("openmhz");
        assert_eq!((o.age_ms, o.held_ms, o.upload_ms, o.wait_ms), (61_200, 60_000, 300, 900), "a copy counts as its plugin");
        assert_eq!(a.timing("rdioscanner").delivered, 0, "only a delivery is timed");
        assert!(a.ticket("dcfd/a").is_none(), "every plugin has said: settled, though nothing waited on its files");
        // Held through a restart: the ticket keeps when it ended.
        a.expect("dcfd/b", 1, keep, &base, None, Some(false), Some(20_000));
        let t = a.ticket("dcfd/b").unwrap();
        assert_eq!(t.ended_ms, Some(20_000));
        let next = Archive::default();
        next.restore("dcfd/b", 1, t);
        next.release("dcfd/b", "openmhz", 5000);
        next.result_at("dcfd/b", "openmhz", Outcome::Ok, None, 26_000);
        assert_eq!((next.timing("openmhz").age_ms, next.timing("openmhz").held_ms, next.timing("openmhz").uploads), (6000, 5000, 0));
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

    /// The rules, as the documentation's table states them: (WAV, M4A, JSON) kept.
    fn kept(r: FileRules, failed: bool) -> (bool, bool, bool) {
        let rescue = failed && r.archive_files_on_failure;
        (r.archive_wav || rescue, r.archive_m4a || rescue, r.call_log || rescue)
    }

    /// Run a call with the WAV, frames, M4A and JSON written past every
    /// pattern of answers, for every combination of the settings: what's
    /// left is what the table says, and the history is told the same.
    #[test]
    fn what_is_kept_follows_the_table_for_every_case() {
        let dir = folder("table");
        let mut n = 0;
        for (wav, m4a, json, on_failure) in BOOLS.iter().flat_map(|&w| BOOLS.iter().flat_map(move |&m| BOOLS.iter().flat_map(move |&j| BOOLS.map(|f| (w, m, j, f))))) {
            let r = rules(wav, m4a, json, on_failure);
            for answers in ANSWERS {
                n += 1;
                let rel = format!("c{n}");
                let base = dir.join(&rel);
                write(&base, &["wav", "sdr", "m4a", "json"]);
                let a = Archive::default();
                let told = std::sync::Arc::new(Mutex::new(None));
                let t = told.clone();
                a.on_settle(Box::new(move |_, audio, json| *t.lock().unwrap() = Some((audio, json))));
                a.expect(&rel, answers.len(), r, &base, None, None, None);
                for o in answers.iter().flatten() {
                    a.result(&rel, "p", *o, None);
                }
                let failed = answers.iter().any(|o| *o != Some(Outcome::Ok) && *o != Some(Outcome::Skipped));
                if answers.contains(&None) {
                    // A plugin that never answers: an hour on, the next call settles it.
                    a.pending.lock().unwrap().get_mut(&rel).unwrap().since = Instant::now() - GIVE_UP;
                    a.expect("next", 0, rules(true, true, true, true), &dir.join("next"), None, None, None);
                }
                let (keep_wav, keep_m4a, keep_json) = kept(r, failed);
                let want: Vec<&str> = [("wav", keep_wav), ("sdr", keep_wav || keep_m4a), ("m4a", keep_m4a), ("json", keep_json)].into_iter().filter(|x| x.1).map(|x| x.0).collect();
                let case = format!("wav {wav} m4a {m4a} json {json} keep-on-failure {on_failure}, answers {answers:?}");
                assert_eq!(left(&base), want, "{case}");
                assert_eq!(*told.lock().unwrap(), Some((keep_wav || keep_m4a, keep_json)), "{case}");
                assert!(a.pending.lock().unwrap().is_empty(), "{case}: settled");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A held call's ticket from before v0.2.4 (one setting for the audio)
    /// is settled as it would have been then.
    #[test]
    fn an_older_ticket_keeps_what_it_kept() {
        let r = |json: &str| serde_json::from_str::<FileRules>(json).unwrap();
        let old = r#"{"compressWav":true,"audioArchive":true,"callLog":false,"archiveFilesOnFailure":true}"#;
        assert_eq!(r(old), rules(true, true, false, true));
        assert_eq!(r(r#"{"compressWav":true,"audioArchive":false,"callLog":true,"archiveFilesOnFailure":false}"#), rules(false, false, true, false));
        let now = rules(false, true, true, false);
        assert_eq!(r(&serde_json::to_string(&now).unwrap()), now);
    }

    /// Skipped is the same as Ok, whatever the settings.
    #[test]
    fn skipped_counts_as_done() {
        let dir = folder("skipped");
        for (audio, json, on_failure) in BOOLS.iter().flat_map(|&a| BOOLS.iter().flat_map(move |&j| BOOLS.map(|f| (a, j, f)))) {
            let r = rules(audio, false, json, on_failure);
            let mut got = Vec::new();
            for o in [Outcome::Ok, Outcome::Skipped] {
                let base = dir.join(format!("{audio}{json}{on_failure}{o:?}"));
                write(&base, &["wav", "json"]);
                let a = Archive::default();
                a.expect("x", 1, r, &base, None, None, None);
                a.result("x", "p", o, None);
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
        a.expect("a", 3, rules(false, false, false, true), &base, None, None, None);
        a.result("a", "p", Outcome::Ok, None);
        a.result("a", "p", Outcome::Skipped, None);
        assert_eq!(left(&base), ["wav", "json"]);
        a.result("a", "p", Outcome::Ok, None);
        assert!(left(&base).is_empty());
        // An answer about a call that isn't waiting changes nothing.
        a.result("a", "p", Outcome::Failed, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No plugin takes calls: the two Keep settings alone decide, at once,
    /// and nothing that won't be kept is written in the first place.
    #[test]
    fn with_no_plugin_the_keep_settings_alone_decide() {
        let dir = folder("none");
        for (audio, json, on_failure) in BOOLS.iter().flat_map(|&a| BOOLS.iter().flat_map(move |&j| BOOLS.map(|f| (a, j, f)))) {
            let r = rules(audio, false, json, on_failure);
            let base = dir.join(format!("{audio}{json}{on_failure}"));
            write(&base, &["wav", "json"]);
            let a = Archive::default();
            a.expect("x", 0, r, &base, None, None, None);
            let want: Vec<&str> = [("wav", audio), ("json", json)].into_iter().filter(|x| x.1).map(|x| x.0).collect();
            assert_eq!(left(&base), want, "audio {audio} json {json} keep-on-failure {on_failure}");
            assert!(a.pending.lock().unwrap().is_empty());
            for (wav, m4a, encoder) in [(false, false, false), (true, true, true), (false, true, true)] {
                let p = plan(r, PluginAudio { takers: 0, wav, m4a }, encoder);
                assert_eq!((p.write_wav, p.write_json, p.write_frames, p.make_m4a, p.spool), (audio, json, audio, false, false));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What's written, and where, before the plugins have the call: the
    /// Keep settings exactly (the WAV for an M4A that can't be made); with
    /// plugins, also what they take (the JSON, the WAV or an M4A made for
    /// them) and the frames, which a failure may keep; the spool only when
    /// something may be deleted.
    #[test]
    fn the_plan_for_every_case() {
        for (wav, m4a, json, on_failure) in BOOLS.iter().flat_map(|&w| BOOLS.iter().flat_map(move |&m| BOOLS.iter().flat_map(move |&j| BOOLS.map(|f| (w, m, j, f))))) {
            let r = rules(wav, m4a, json, on_failure);
            for takers in [0, 1, 2] {
                for (takes_wav, wants_m4a, encoder) in BOOLS.iter().flat_map(|&w| BOOLS.iter().flat_map(move |&m| BOOLS.map(|e| (w, m, e)))) {
                    let p = plan(r, PluginAudio { takers, wav: takes_wav, m4a: wants_m4a }, encoder);
                    let plugins = takers > 0;
                    let case = format!("wav {wav} m4a {m4a} json {json} keep-on-failure {on_failure}, {takers} plugins taking WAV {takes_wav} M4A {wants_m4a}, encoder {encoder}: {p:?}");
                    assert_eq!((p.keep_wav, p.keep_m4a, p.keep_json, p.keep_audio), (wav, m4a, json, wav || m4a), "{case}");
                    // The WAV: kept, in place of an M4A that can't be made, or a plugin's.
                    assert_eq!(p.write_wav, wav || (m4a && !encoder) || (plugins && (takes_wav || (wants_m4a && !encoder))), "{case}");
                    assert_eq!(p.write_json, json || plugins, "{case}");
                    assert_eq!(p.write_frames, wav || m4a || plugins, "{case}");
                    assert_eq!(p.make_m4a, encoder && (m4a || (plugins && wants_m4a)), "{case}");
                    assert_eq!(p.spool, plugins && !(wav && m4a && json), "{case}");
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
        a.expect("a", 1, plugins_only, &b, Some(&s), None, None);
        a.result("a", "p", Outcome::Ok, None);
        assert!(left(&b).is_empty() && left(&s).is_empty());
        // Failed: the M4A (the only audio) and the JSON move to the recordings folder.
        let (b, s) = call("b", &[], &["json", "m4a"]);
        a.expect("b", 1, plugins_only, &b, Some(&s), None, None);
        a.result("b", "p", Outcome::Failed, None);
        assert_eq!((left(&b), left(&s)), (vec!["m4a", "json"], vec![]));
        // Failed, but failures change nothing: gone.
        let (b, s) = call("c", &[], &["json", "m4a"]);
        a.expect("c", 1, rules(false, false, false, false), &b, Some(&s), None, None);
        a.result("c", "p", Outcome::Failed, None);
        assert!(left(&b).is_empty() && left(&s).is_empty());
        // The JSON kept, written to the recordings folder; the spooled M4A goes.
        let (b, s) = call("d", &["json"], &["m4a"]);
        a.expect("d", 1, rules(false, false, true, true), &b, Some(&s), None, None);
        a.result("d", "p", Outcome::Skipped, None);
        assert_eq!((left(&b), left(&s)), (vec!["json"], vec![]));
        // Never answered: after an hour it has failed, so it moves to the recordings folder…
        let (b, s) = call("e", &[], &["json", "m4a"]);
        a.expect("e", 1, plugins_only, &b, Some(&s), None, None);
        a.pending.lock().unwrap().get_mut("e").unwrap().since = Instant::now() - GIVE_UP;
        a.expect("next", 0, plugins_only, &capture.join("sys/next"), None, None, None);
        assert_eq!((left(&b), left(&s)), (vec!["m4a", "json"], vec![]));
        // …or, with failures changing nothing, goes.
        let (b, s) = call("f", &[], &["json", "m4a"]);
        a.expect("f", 1, rules(false, false, false, false), &b, Some(&s), None, None);
        a.pending.lock().unwrap().get_mut("f").unwrap().since = Instant::now() - GIVE_UP;
        a.expect("next", 0, plugins_only, &capture.join("sys/next"), None, None, None);
        assert!(left(&b).is_empty() && left(&s).is_empty() && a.pending.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
