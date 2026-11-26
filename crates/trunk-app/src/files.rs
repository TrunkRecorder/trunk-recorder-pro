//! Writing files, and why it didn't work. Every file the app writes, and
//! every folder it makes, moves or deletes, goes through one of these: a
//! failure comes back as a [`Problem`], which says in words what happened,
//! to which path, and what to do about it — whose folder it is and who the
//! app runs as, macOS's privacy protection, a read-only or full disk — not
//! the bare "Permission denied (os error 13)".

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::Path;

/// Why a file or folder couldn't be written: `why`, in a few words;
/// `detail`, what was being done and what the system said; `fix`, what to
/// do, when known.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub why: String,
    pub detail: String,
    pub fix: Option<String>,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}: {}", self.why, self.detail)?;
        if let Some(fix) = &self.fix {
            write!(f, ". {fix}")?;
        }
        Ok(())
    }
}

impl From<Problem> for String {
    fn from(p: Problem) -> String {
        p.to_string()
    }
}

/// What was being done when it failed.
#[derive(Clone, Copy, Debug)]
pub enum Doing {
    Make,
    Write,
    Delete,
    Move,
}

impl Doing {
    fn verb(self) -> &'static str {
        match self {
            Doing::Make => "making",
            Doing::Write => "writing",
            Doing::Delete => "deleting",
            Doing::Move => "moving a file to",
        }
    }
}

impl Problem {
    pub fn new(why: &str, detail: String, fix: Option<String>) -> Problem {
        Problem { why: why.into(), detail, fix }
    }

    /// `e`, from `doing` `path`, in words: the kinds of failure told apart,
    /// each with what to do about it.
    pub fn of(doing: Doing, path: &Path, e: &std::io::Error) -> Problem {
        let detail = format!("{} {}: {e}", doing.verb(), path.display());
        // (macOS's privacy protection: "Operation not permitted", not "Permission denied".)
        #[cfg(target_os = "macos")]
        if e.raw_os_error() == Some(libc::EPERM) {
            let fix = "Allow trunk-pro in System Settings → Privacy & Security → Files and Folders (or Full Disk Access), or choose a folder outside Documents, Desktop and Downloads";
            return Problem::new("Blocked by macOS privacy", detail, Some(fix.into()));
        }
        match e.kind() {
            ErrorKind::PermissionDenied => {
                let (owner, fix) = owner_of(path);
                Problem::new("Permission denied", format!("{detail}{owner}"), fix)
            }
            ErrorKind::ReadOnlyFilesystem => Problem::new("Disk is read-only", detail, Some("It's mounted read-only (a Docker volume with :ro?), or the system made it so after errors (a failing SD card or drive)".into())),
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Problem::new("Disk is full", detail, Some("Free some room, or choose a folder on another disk".into())),
            ErrorKind::NotADirectory | ErrorKind::AlreadyExists => Problem::new("Not a folder", detail, Some("Something in its path is a file: choose another folder".into())),
            ErrorKind::NotFound => Problem::new("Folder is gone", detail, Some("Its drive was disconnected, or the folder was deleted".into())),
            _ => Problem::new("Can't write", detail, None),
        }
    }
}

/// Who owns the nearest folder of `path` there is, and who the app runs as
/// (", owned by root (755); the app runs as luke"), and what to do.
#[cfg(unix)]
fn owner_of(path: &Path) -> (String, Option<String>) {
    use std::os::unix::fs::MetadataExt;
    let Some((at, m)) = path.ancestors().find_map(|a| fs::metadata(a).ok().filter(|m| m.is_dir()).map(|m| (a, m))) else {
        return (String::new(), None);
    };
    let uid = unsafe { libc::geteuid() };
    let (me, owner, mode) = (user_name(uid), user_name(m.uid()), m.mode() & 0o777);
    let said = format!(" — {} is owned by {owner} ({mode:o}); the app runs as {me}", at.display());
    let fix = if m.uid() == uid {
        format!("Let its owner write to it (chmod u+w \"{}\"), or choose another folder", at.display())
    } else {
        format!("Give {me} the folder (sudo chown -R {me} \"{}\"), or choose one {me} can write to", at.display())
    };
    (said, Some(fix))
}

/// Windows: what to do (a folder under Program Files needs administrator
/// rights, and the config is often left there by an installer).
#[cfg(not(unix))]
fn owner_of(path: &Path) -> (String, Option<String>) {
    let program_files = path.components().any(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase().starts_with("program files"));
    let fix = if program_files {
        "Folders under Program Files can't be written without administrator rights: keep the config and recordings in a folder of your own (start Trunk Recorder Pro with --config there)"
    } else {
        "Choose a folder this account can write to, or give it write access to this one"
    };
    (String::new(), Some(fix.into()))
}

#[cfg(unix)]
fn user_name(uid: u32) -> String {
    let mut buf = vec![0u8; 1024];
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out = std::ptr::null_mut();
    let found = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut out) } == 0 && !out.is_null();
    if found {
        unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }.to_string_lossy().into_owned()
    } else {
        format!("uid {uid}")
    }
}

/// Make folder `dir`, and those it's in.
pub fn make_dir(dir: &Path) -> Result<(), Problem> {
    fs::create_dir_all(dir).map_err(|e| Problem::of(Doing::Make, dir, &e))
}

/// Make the folder `file` goes in.
pub fn make_parent(file: &Path) -> Result<(), Problem> {
    match file.parent() {
        Some(d) if !d.as_os_str().is_empty() => make_dir(d),
        _ => Ok(()),
    }
}

/// Write `data` to `path`.
pub fn write(path: &Path, data: impl AsRef<[u8]>) -> Result<(), Problem> {
    fs::write(path, data).map_err(|e| Problem::of(Doing::Write, path, &e))
}

/// Write `data` to `path` so that a crash or power cut leaves the old file
/// or the new one, never part of either: into a temporary file beside it,
/// flushed to disk, then renamed over it.
pub fn write_atomic(path: &Path, data: impl AsRef<[u8]>) -> Result<(), Problem> {
    let name = path.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let written = File::create(&tmp).and_then(|mut f| {
        f.write_all(data.as_ref())?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(Problem::of(Doing::Write, &tmp, &e));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Problem::of(Doing::Move, path, &e)
    })
}

/// [`write_atomic`], readable by this user only (a secret: a key, sessions).
pub fn write_private(path: &Path, data: impl AsRef<[u8]>) -> Result<(), Problem> {
    let name = path.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let _ = fs::remove_file(&tmp);
    let written = private(&tmp).and_then(|mut f| {
        f.write_all(data.as_ref())?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(Problem::of(Doing::Write, &tmp, &e));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Problem::of(Doing::Move, path, &e)
    })
}

fn private(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

/// A new (or emptied) file at `path`, to write as it goes.
pub fn create(path: &Path) -> Result<File, Problem> {
    File::create(path).map_err(|e| Problem::of(Doing::Write, path, &e))
}

/// The file at `path` to add to (made when it isn't there).
pub fn append(path: &Path) -> Result<File, Problem> {
    OpenOptions::new().create(true).append(true).open(path).map_err(|e| Problem::of(Doing::Write, path, &e))
}

/// Rename `from` to `to` (on one disk).
pub fn rename(from: &Path, to: &Path) -> Result<(), Problem> {
    fs::rename(from, to).map_err(|e| Problem::of(Doing::Move, to, &e))
}

/// Copy `from` to `to`.
pub fn copy(from: &Path, to: &Path) -> Result<u64, Problem> {
    fs::copy(from, to).map_err(|e| Problem::of(Doing::Write, to, &e))
}

/// Delete file `path` (it not being there is fine).
pub fn remove_file(path: &Path) -> Result<(), Problem> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(Problem::of(Doing::Delete, path, &e)),
        _ => Ok(()),
    }
}

/// Delete folder `dir` and all in it (it not being there is fine).
pub fn remove_dir_all(dir: &Path) -> Result<(), Problem> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(Problem::of(Doing::Delete, dir, &e)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("trunk-app-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn writes_go_in_and_atomic_ones_leave_nothing_behind() {
        let d = dir("write");
        make_dir(&d.join("a/b")).unwrap();
        write_atomic(&d.join("a/b/c.json"), "{}").unwrap();
        write_private(&d.join("a/key"), "secret").unwrap();
        assert_eq!(fs::read_to_string(d.join("a/b/c.json")).unwrap(), "{}");
        assert_eq!(fs::read_dir(d.join("a/b")).unwrap().count(), 1, "no temporary file left");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(d.join("a/key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        remove_file(&d.join("not-there")).unwrap();
        remove_dir_all(&d).unwrap();
        remove_dir_all(&d).unwrap();
    }

    /// Without permission, the folder is named with whose it is, who the
    /// app runs as and what to do — not the bare OS error.
    #[cfg(unix)]
    #[test]
    fn permission_denied_says_whose_folder_and_what_to_do() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("denied");
        make_dir(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o555)).unwrap();
        let r = write_atomic(&d.join("config.json"), "{}");
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_dir_all(&d);
        let Err(p) = r else { return }; // root writes anywhere
        assert_eq!(p.why, "Permission denied");
        assert!(p.detail.contains(&d.display().to_string()) && p.detail.contains("the app runs as"), "{p}");
        assert!(p.fix.as_deref().is_some_and(|f| f.contains("chmod u+w")), "{p}");
    }

    #[test]
    fn a_file_in_the_way_is_not_a_folder() {
        let d = dir("in-the-way");
        make_dir(&d).unwrap();
        write(&d.join("f"), "x").unwrap();
        let p = make_dir(&d.join("f/sub")).unwrap_err();
        let _ = fs::remove_dir_all(&d);
        assert_eq!(p.why, "Not a folder", "{p}");
    }
}
