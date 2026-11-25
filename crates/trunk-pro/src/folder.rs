//! The folders calls are written to — the recordings folder, and the RAM
//! spool while it's on — each a [`Folder`]. Every file goes into one through
//! it ([`Folder::write`], [`Folder::move_in`]), and it's checked with a file
//! of its own ([`Folder::check`]) at startup and whenever its path changes;
//! after that, the calls' own files are the check (nothing is written to it
//! just to see). Both say how writing to it goes, in one place: the first failure is logged with why and what to do about it, and
//! is an event; while it lasts, the files not written are counted (logged at
//! most once a minute); and the folder taking files again is said once. The
//! platform message's `folders` ([`Folders::report`]) has it for the
//! dashboard's light and the folder's field in Setup.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
pub use trunk_app::files::Problem;
use trunk_app::files::{self, Doing};
use trunk_app::stats::MonitorEvent;

/// Files not written, said at most this often while a folder fails.
const LOG_EVERY: Duration = Duration::from_secs(60);

/// Where drives are mounted: a folder right under one of these that isn't
/// there is a drive that isn't (and making it would put the calls on the
/// system disk instead).
const MOUNTS: &[&str] = &["/Volumes", "/media", "/run/media", "/mnt"];

/// The drive `dir` is on, when that's one of [`MOUNTS`]'s and it isn't
/// mounted ("/Volumes/Calls"; for /media and /run/media, a user's folder first).
fn unmounted_drive(dir: &Path) -> Option<PathBuf> {
    let parts: Vec<Component> = dir.components().collect();
    MOUNTS.iter().find_map(|m| {
        let base: Vec<Component> = Path::new(m).components().collect();
        if parts.len() <= base.len() || parts[..base.len()] != base[..] {
            return None;
        }
        // (/media/<user>/<drive>, /run/media/<user>/<drive>: either may be the mount.)
        let depth = if *m == "/media" || *m == "/run/media" { 2 } else { 1 };
        let drive: PathBuf = parts[..(base.len() + depth).min(parts.len())].iter().collect();
        (!drive.exists()).then_some(drive)
    })
}

/// Write a file of our own into `dir` (made first when `make`), make sure it
/// reached the disk, and delete it: whether files can be written there, and
/// if not, why.
pub fn probe(dir: &Path, make: bool) -> Result<(), Problem> {
    if dir.as_os_str().is_empty() {
        return Err(Problem::new("No folder set", "the recordings folder is empty in the settings".into(), Some("Choose one in Setup → Recording".into())));
    }
    if !dir.is_dir() {
        if let Some(drive) = unmounted_drive(dir) {
            let detail = format!("{} isn't there, so neither is {}", drive.display(), dir.display());
            return Err(Problem::new("Drive not mounted", detail, Some("Connect (or mount) the drive: the next call after that is saved".into())));
        }
        if !make {
            let detail = format!("{} was there, and isn't now", dir.display());
            return Err(Problem::new("Folder is gone", detail, Some("Its drive was disconnected, or the folder deleted: reconnect or make it again, and the next call is saved".into())));
        }
        files::make_dir(dir)?;
    }
    let file = dir.join(format!(".trunk-pro-write-check-{}", std::process::id()));
    let written = fs::File::create(&file).and_then(|mut f| f.write_all(b"Trunk Recorder Pro checks it can write here. Safe to delete.\n").and_then(|_| f.sync_all()));
    if let Err(e) = written {
        let _ = fs::remove_file(&file);
        return Err(Problem::of(Doing::Write, &file, &e));
    }
    // (Calls not kept are deleted once the plugins are done: that has to work too.)
    files::remove_file(&file)
}

/// A folder failing: since when (Unix s), why, the files not written since,
/// and those not yet logged.
struct Failing {
    problem: Problem,
    since: f64,
    lost: u64,
    unlogged: u64,
    logged: Instant,
}

#[derive(Default)]
struct State {
    /// Where it is (empty: none, as the spool when it's off).
    path: PathBuf,
    /// Checked with [`probe`] at this path (none: not yet).
    checked: Option<Instant>,
    /// It was there (or made) at this path: should it go, it isn't made again.
    seen: bool,
    /// A check is running (on a thread of its own: a network share can hang).
    checking: bool,
    failing: Option<Failing>,
}

type Tell = OnceLock<Box<dyn Fn(MonitorEvent) + Send + Sync>>;

/// A folder the app writes calls to ([`Folders`]): every write into it goes
/// through here, and how they go is kept, said and reported.
pub struct Folder {
    /// The platform message's name for it ("recordings", "spool").
    pub name: &'static str,
    /// What the log calls it ("recordings folder").
    what: &'static str,
    /// Checked and reported with no path too (the recordings folder: calls
    /// need one); else a folder with none is off (the spool).
    needed: bool,
    state: Mutex<State>,
    tell: Arc<Tell>,
}

impl Folder {
    fn new(name: &'static str, what: &'static str, needed: bool, tell: Arc<Tell>) -> Folder {
        Folder { name, what, needed, state: Default::default(), tell }
    }

    pub fn path(&self) -> PathBuf {
        self.state.lock().unwrap().path.clone()
    }

    /// It's at `path` now (empty: none), not yet checked there; true when
    /// that's new. (What failed at the old place doesn't follow it.)
    pub fn set_path(&self, path: &Path) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.path == path {
            return false;
        }
        *s = State { path: path.to_path_buf(), checking: s.checking, ..Default::default() };
        true
    }

    /// It's at `path` now: a new place checked at once, in the background
    /// ([`Folder::check_new`]).
    pub fn follow(self: &Arc<Self>, path: &Path) {
        if self.set_path(path) {
            self.check_new();
        }
    }

    /// Check it now ([`probe`]): made when it hasn't been there at this path. Gives what was found.
    pub fn check(&self) -> Result<(), Problem> {
        let (path, seen) = {
            let s = self.state.lock().unwrap();
            (s.path.clone(), s.seen)
        };
        if path.as_os_str().is_empty() && !self.needed {
            return Ok(());
        }
        let r = probe(&path, !seen);
        let mut s = self.state.lock().unwrap();
        if s.path != path {
            return r;
        }
        s.checked = Some(Instant::now());
        drop(s);
        self.note(&r, false);
        r
    }

    /// Checked on a thread of its own (a network share can hang), when it
    /// hasn't been at this path; one check at a time.
    fn check_new(self: &Arc<Self>) {
        {
            let mut s = self.state.lock().unwrap();
            let due = (self.needed || !s.path.as_os_str().is_empty()) && s.checked.is_none();
            if !due || s.checking {
                return;
            }
            s.checking = true;
        }
        let me = self.clone();
        let started = std::thread::Builder::new().name(format!("check-{}", self.name)).spawn(move || {
            let _ = me.check();
            me.state.lock().unwrap().checking = false;
        });
        if started.is_err() {
            self.state.lock().unwrap().checking = false;
        }
    }

    /// Write `bytes` to `file` (in this folder; its own folders made as needed).
    pub fn write(&self, file: &Path, bytes: &[u8]) -> Result<(), Problem> {
        let r = self.make_parent(file).and_then(|_| files::write(file, bytes));
        self.note(&r, true);
        r
    }

    /// The folders `file` goes in, made, so something else can write it there
    /// (an encoder); said as a write is when they can't be.
    pub fn prepare(&self, file: &Path) -> Result<(), Problem> {
        let r = self.make_parent(file);
        if r.is_err() {
            self.note(&r, true);
        }
        r
    }

    /// Move `from` (elsewhere: the spool) to `to`, in this folder.
    pub fn move_in(&self, from: &Path, to: &Path) -> Result<(), Problem> {
        let r = self.make_parent(to).and_then(|_| crate::spool::move_file(from, to));
        self.note(&r, true);
        r
    }

    /// The folders of `file` made, beneath this folder — never the folder
    /// itself: one that went (a drive disconnected) isn't made again on
    /// the disk beneath, only said.
    fn make_parent(&self, file: &Path) -> Result<(), Problem> {
        let (root, seen) = {
            let s = self.state.lock().unwrap();
            (s.path.clone(), s.seen)
        };
        if !root.as_os_str().is_empty() && !root.is_dir() {
            probe(&root, !seen)?;
        }
        match file.parent() {
            Some(d) => files::make_dir(d),
            None => Ok(()),
        }
    }

    /// How a write (`file`: of a call's file; else a check) went: the edges
    /// logged and told as events, the failures between counted.
    fn note(&self, r: &Result<(), Problem>, file: bool) {
        let mut s = self.state.lock().unwrap();
        s.seen |= r.is_ok();
        let path = s.path.display().to_string();
        let event = match (r, s.failing.as_mut()) {
            (Ok(()), None) => None,
            (Ok(()), Some(_)) => {
                let f = s.failing.take().unwrap();
                let lost = if f.lost > 0 { format!(" ({} not written meanwhile)", files(f.lost)) } else { String::new() };
                log::info!("The {} takes files again: {path}{lost}", self.what);
                Some(MonitorEvent::FolderWritable { folder: self.name.into(), path, lost: f.lost })
            }
            (Err(p), None) => {
                log::error!("Can't write to the {} {path}: {p}", self.what);
                let lost = file as u64;
                s.failing = Some(Failing { problem: p.clone(), since: crate::clock::unix_s(), lost, unlogged: 0, logged: Instant::now() });
                Some(MonitorEvent::FolderUnwritable { folder: self.name.into(), path, why: p.why.clone(), detail: p.detail.clone(), fix: p.fix.clone() })
            }
            (Err(p), Some(f)) => {
                f.lost += file as u64;
                f.unlogged += file as u64;
                // (Another reason now: its own line at once.)
                let changed = f.problem.why != p.why;
                if changed || (f.unlogged > 0 && f.logged.elapsed() >= LOG_EVERY) {
                    log::error!("Can't write to the {} {path}: {p} ({} not written since {})", self.what, files(f.lost), since(f.since));
                    (f.unlogged, f.logged, f.problem) = (0, Instant::now(), p.clone());
                }
                changed.then(|| MonitorEvent::FolderUnwritable { folder: self.name.into(), path, why: p.why.clone(), detail: p.detail.clone(), fix: p.fix.clone() })
            }
        };
        drop(s);
        if let (Some(e), Some(tell)) = (event, self.tell.get()) {
            tell(e);
        }
    }

    /// Its row of the platform message's `folders` (none: it has no path).
    fn report(&self) -> Option<Value> {
        let s = self.state.lock().unwrap();
        if s.path.as_os_str().is_empty() && !self.needed {
            return None;
        }
        // (Known once checked, or written to.)
        let known = s.checked.is_some() || s.seen;
        let mut v = json!({ "name": self.name, "path": s.path.display().to_string(), "ok": if known { json!(true) } else { Value::Null } });
        if let Some(f) = &s.failing {
            v["ok"] = json!(false);
            v["why"] = json!(f.problem.why);
            v["detail"] = json!(f.problem.detail);
            v["fix"] = json!(f.problem.fix);
            v["since"] = json!(f.since.round());
            v["lost"] = json!(f.lost);
        }
        Some(v)
    }
}

fn files(n: u64) -> String {
    if n == 1 { "1 file".into() } else { format!("{n} files") }
}

fn since(t: f64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0).map_or_else(String::new, |d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
}

/// The folders calls are written to.
pub struct Folders {
    pub recordings: Arc<Folder>,
    /// The RAM spool's, while it's on.
    pub spool: Arc<Folder>,
    tell: Arc<Tell>,
}

impl Default for Folders {
    fn default() -> Self {
        let tell: Arc<Tell> = Default::default();
        Folders { recordings: Arc::new(Folder::new("recordings", "recordings folder", true, tell.clone())), spool: Arc::new(Folder::new("spool", "RAM spool", false, tell.clone())), tell }
    }
}

impl Folders {
    /// Who hears of a folder failing and taking files again (the event feed).
    pub fn on_event(&self, f: Box<dyn Fn(MonitorEvent) + Send + Sync>) {
        let _ = self.tell.set(f);
    }

    /// Each tick of the monitor: the recordings folder where the config has
    /// it now (checked, should that be new).
    pub fn tick(&self, capture_dir: &Path) {
        self.recordings.follow(capture_dir);
    }

    /// The platform message's `folders`: each `{name, path, ok}` (ok: null
    /// while not yet checked), and while one fails, `why`, `detail`, `fix`,
    /// `since` (Unix s) and `lost` (files not written since).
    pub fn report(&self) -> Value {
        Value::Array([&self.recordings, &self.spool].iter().filter_map(|f| f.report()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trunk-pro-folder-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn events(f: &Folders) -> Arc<Mutex<Vec<MonitorEvent>>> {
        let seen: Arc<Mutex<Vec<MonitorEvent>>> = Default::default();
        let s = seen.clone();
        f.on_event(Box::new(move |e| s.lock().unwrap().push(e)));
        seen
    }

    /// A folder that isn't there is made at the first check, and nothing of the check is left in it.
    #[test]
    fn a_new_folder_is_made_and_checked() {
        let d = dir("new").join("calls");
        let f = Folders::default();
        assert!(f.recordings.set_path(&d));
        f.recordings.check().unwrap();
        assert!(d.is_dir());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0, "the check's file is deleted");
        assert_eq!(f.report()[0]["ok"], true);
        assert!(!f.recordings.set_path(&d), "the same path: nothing new");
        let _ = fs::remove_dir_all(d.parent().unwrap());
    }

    /// A file where the folder should be: said once (logged, an event, the
    /// report), its writes counted, and the folder taking files again said once.
    #[test]
    fn failing_and_back_are_said_once_and_the_files_between_counted() {
        let root = dir("blocked");
        fs::create_dir_all(&root).unwrap();
        let d = root.join("calls");
        fs::write(&d, b"in the way").unwrap();
        let f = Folders::default();
        let seen = events(&f);
        f.recordings.set_path(&d);
        assert!(f.recordings.check().is_err());
        let r = f.report();
        assert_eq!((r[0]["ok"].as_bool(), r[0]["why"].as_str()), (Some(false), Some("Not a folder")));
        for _ in 0..3 {
            assert!(f.recordings.write(&d.join("2026/11/22/1-2.wav"), b"RIFF").is_err());
        }
        assert_eq!(f.report()[0]["lost"], 3);
        assert_eq!(seen.lock().unwrap().len(), 1, "the edge, not every write");
        fs::remove_file(&d).unwrap();
        fs::create_dir(&d).unwrap();
        f.recordings.write(&d.join("2026/11/22/1-3.wav"), b"RIFF").unwrap();
        assert_eq!(f.report()[0]["ok"], true);
        let seen = seen.lock().unwrap();
        assert!(matches!(&seen[1], MonitorEvent::FolderWritable { lost: 3, .. }), "{:?}", seen[1]);
        let _ = fs::remove_dir_all(&root);
    }

    /// A folder that went while the app ran isn't made again (it would be on
    /// the disk beneath a drive that's gone): its writes say so instead.
    #[test]
    fn a_folder_that_went_is_not_made_again() {
        let d = dir("went");
        let f = Folders::default();
        f.recordings.set_path(&d);
        f.recordings.check().unwrap();
        fs::remove_dir_all(&d).unwrap();
        let e = f.recordings.write(&d.join("a/b.json"), b"{}").unwrap_err();
        assert_eq!(e.why, "Folder is gone");
        assert!(!d.exists());
        assert_eq!(f.recordings.check().unwrap_err().why, "Folder is gone");
        assert!(!d.exists());
    }

    /// A folder on a drive that isn't mounted is said to be that, and not made.
    #[test]
    fn a_drive_not_mounted_is_said_and_not_made() {
        assert_eq!(unmounted_drive(Path::new("/Volumes/NoSuchDrive-trp/calls")), Some(PathBuf::from("/Volumes/NoSuchDrive-trp")));
        assert_eq!(unmounted_drive(Path::new("/media/luke/usb/calls")).is_some(), !Path::new("/media/luke/usb").exists());
        assert_eq!(unmounted_drive(Path::new("/home/luke/calls")), None);
        assert_eq!(unmounted_drive(Path::new("/mnt")), None, "the mounts' own folder is no drive");
        let e = probe(Path::new("/Volumes/NoSuchDrive-trp/calls"), true).unwrap_err();
        assert_eq!(e.why, "Drive not mounted");
    }

    /// A folder this account can't write to: said so, with who owns it.
    #[cfg(unix)]
    #[test]
    fn permission_denied_says_who_owns_it() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // (root writes anywhere)
        }
        let d = dir("denied");
        fs::create_dir_all(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o555)).unwrap();
        let e = probe(&d, true).unwrap_err();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(e.why, "Permission denied");
        assert!(e.detail.contains("555") && e.detail.contains("the app runs as"), "{}", e.detail);
        assert!(e.fix.as_deref().is_some_and(|f| f.contains("chmod u+w")), "its own: made writable, not given away");
        let _ = fs::remove_dir_all(&d);
    }

    /// The spool has no row while it's off; the recordings folder always has one.
    #[test]
    fn the_spool_is_reported_only_while_on() {
        let f = Folders::default();
        assert_eq!(f.report().as_array().unwrap().len(), 1);
        assert_eq!(f.report()[0]["ok"], Value::Null, "not checked yet");
        let d = dir("spool");
        f.spool.set_path(&d);
        assert_eq!(f.report()[1]["name"], "spool");
        f.spool.set_path(Path::new(""));
        assert_eq!(f.report().as_array().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&d);
    }
}
