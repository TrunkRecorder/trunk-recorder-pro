//! The RAM spool (`recording.ramSpool`): where a call's files wait for the
//! upload plugins when nothing keeps them afterwards, so a call that's only
//! uploaded never touches the disk. macOS: a RAM disk the app makes
//! (`hdiutil`, no administrator needed), mounted out of sight at
//! `<data>/spool`; Linux: a folder in /dev/shm (tmpfs). Either outlives the
//! app — after a restart the plugins still find the calls they had queued —
//! but not the computer.
//!
//! What's kept goes to the recordings folder: a call whose upload failed is
//! moved there ([`crate::plugins::Archive`]), and so is anything left here
//! long after ([`STALE`]: a plugin that never reported). When the spool is
//! full, calls are written to the recordings folder as without one.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::config::RamSpool;

/// Files here this long belong to calls nothing settled (a plugin's retries
/// run ~20 minutes): they're moved to the recordings folder.
pub const STALE: Duration = Duration::from_secs(2 * 3600);
/// Room left free: below this much, calls go to the recordings folder.
const HEADROOM_PCT: u64 = 5;

pub struct Spool {
    pub dir: PathBuf,
    /// "RAM disk" | "tmpfs" | "folder"
    pub kind: &'static str,
    /// The most it may hold, bytes.
    cap: u64,
    /// Calls written to the recordings folder for want of room.
    overflowed: AtomicU64,
}

impl Spool {
    /// Make (or find again) the spool `cfg` asks for; `data`: the app's data folder.
    pub fn open(cfg: &RamSpool, data: &Path) -> Result<Spool, String> {
        let cap = u64::from(cfg.size_mb.clamp(16, 65536)) << 20;
        let (dir, kind) = if !cfg.dir.is_empty() {
            let dir = PathBuf::from(&cfg.dir);
            fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            (dir, "folder")
        } else {
            (sys::open(data, cap)?, sys::KIND)
        };
        Ok(Spool { dir, kind, cap, overflowed: AtomicU64::new(0) })
    }

    /// Size and free space, bytes.
    pub fn usage(&self) -> Option<(u64, u64)> {
        let (total, free) = statvfs(&self.dir)?;
        if self.kind == "RAM disk" {
            return Some((total, free));
        }
        // A share of a filesystem others use too: its cap, less what's here.
        let total = total.min(self.cap);
        Some((total, total.saturating_sub(du(&self.dir)).min(free)))
    }

    /// Room for `bytes` more, keeping some free. False counts an overflow.
    pub fn room_for(&self, bytes: u64) -> bool {
        let ok = self.usage().is_some_and(|(total, free)| free > bytes + total * HEADROOM_PCT / 100);
        if !ok {
            self.overflowed.fetch_add(1, Ordering::Relaxed);
        }
        ok
    }

    /// Calls that went to the recordings folder for want of room, so far.
    pub fn overflowed(&self) -> u64 {
        self.overflowed.load(Ordering::Relaxed)
    }

    /// Move files older than `age` to the same place under `capture`; how many.
    pub fn sweep(&self, capture: &Path, age: Duration) -> usize {
        sweep(&self.dir, capture, age)
    }
}

/// Move the files under `dir` older than `age` to the same place under `to`
/// (hidden ones left), then drop the folders that emptied; how many moved.
fn sweep(dir: &Path, to: &Path, age: Duration) -> usize {
    let now = SystemTime::now();
    let mut moved = 0;
    let mut stack = vec![dir.to_path_buf()];
    let mut dirs = Vec::new();
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(p.clone());
                dirs.push(p);
            } else if m.modified().ok().and_then(|t| now.duration_since(t).ok()).unwrap_or_default() >= age {
                let Ok(rel) = p.strip_prefix(dir) else { continue };
                match move_file(&p, &to.join(rel)) {
                    Ok(()) => moved += 1,
                    Err(e) => log::warn!("Couldn't move {} out of the spool: {e}", p.display()),
                }
            }
        }
    }
    // Deepest first; one still in use stays.
    for d in dirs.iter().rev() {
        let _ = fs::remove_dir(d);
    }
    moved
}

/// Move a file, to another disk too (copied, then removed), keeping its time.
pub fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(d) = to.parent() {
        fs::create_dir_all(d)?;
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let tmp = PathBuf::from(format!("{}.part", to.display()));
    fs::copy(from, &tmp)?;
    if let Ok(t) = fs::metadata(from).and_then(|m| m.modified()) {
        let _ = fs::File::options().write(true).open(&tmp).and_then(|f| f.set_modified(t));
    }
    fs::rename(&tmp, to)?;
    fs::remove_file(from)
}

/// Bytes in the files under `dir`.
fn du(dir: &Path) -> u64 {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => n += m.len(),
                Err(_) => {}
            }
        }
    }
    n
}

/// A file under `dir`, hidden ones (the volume's own) aside.
fn has_files(dir: &Path) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(_) => return true,
                Err(_) => {}
            }
        }
    }
    false
}

/// A filesystem's size and the space free to us, bytes.
#[cfg(unix)]
pub fn statvfs(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let unit = s.f_frsize as u64;
    Some((s.f_blocks as u64 * unit, s.f_bavail as u64 * unit))
}

#[cfg(not(unix))]
pub fn statvfs(_: &Path) -> Option<(u64, u64)> {
    None
}

/// The spool when it's off (or a folder of the user's): what's in the one the
/// app made goes to `capture`, and then it goes too.
pub fn retire(data: &Path, capture: &Path) {
    if let Some(dir) = sys::existing(data) {
        let n = sweep(&dir, capture, Duration::ZERO);
        if has_files(&dir) {
            log::warn!("The RAM spool at {} still has files: left as it is", dir.display());
            return;
        }
        match sys::remove(&dir) {
            Ok(()) => log::info!("RAM spool removed ({n} file(s) moved to {})", capture.display()),
            Err(e) => log::warn!("Couldn't remove the RAM spool at {}: {e}", dir.display()),
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub const KIND: &str = "RAM disk";

    fn mount_point(data: &Path) -> PathBuf {
        data.join("spool")
    }

    /// A volume is mounted on `dir`.
    fn mounted(dir: &Path) -> bool {
        match (std::fs::metadata(dir), dir.parent().map(std::fs::metadata)) {
            (Ok(d), Some(Ok(p))) => d.dev() != p.dev(),
            _ => false,
        }
    }

    pub fn existing(data: &Path) -> Option<PathBuf> {
        Some(mount_point(data)).filter(|d| mounted(d))
    }

    fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
        let o = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
        if !o.status.success() {
            return Err(format!("{cmd} {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&o.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    pub fn remove(dir: &Path) -> Result<(), String> {
        let d = dir.to_string_lossy();
        run("/usr/bin/hdiutil", &["detach", &d]).or_else(|_| run("/usr/bin/hdiutil", &["detach", "-force", &d]))?;
        let _ = std::fs::remove_dir(dir);
        Ok(())
    }

    /// The RAM disk at `<data>/spool`: the one already there (kept from an
    /// earlier run, with what it holds), or a new one of `cap` bytes.
    pub fn open(data: &Path, cap: u64) -> Result<PathBuf, String> {
        let dir = mount_point(data);
        if mounted(&dir) {
            let total = super::statvfs(&dir).map_or(0, |u| u.0);
            // Resized in the config: made again, unless calls are waiting in it.
            if total.abs_diff(cap) <= cap / 10 || super::has_files(&dir) {
                return Ok(dir);
            }
            remove(&dir)?;
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let out = run("/usr/bin/hdiutil", &["attach", "-nomount", &format!("ram://{}", cap / 512)])?;
        let dev = out.split_whitespace().next().filter(|d| d.starts_with("/dev/disk")).ok_or_else(|| format!("hdiutil attach said {out:?}"))?.to_string();
        let made = run("/sbin/newfs_hfs", &["-v", "Trunk Recorder spool", &dev])
            // nobrowse: not in the Finder's sidebar, nor indexed by Spotlight.
            .and_then(|_| run("/usr/sbin/diskutil", &["mount", "-mountOptions", "nobrowse", "-mountPoint", &dir.to_string_lossy(), &dev]));
        if let Err(e) = made {
            let _ = run("/usr/bin/hdiutil", &["detach", "-force", &dev]);
            return Err(e);
        }
        // Nor logged by fseventsd.
        let _ = std::fs::write(dir.join(".metadata_never_index"), b"");
        let _ = std::fs::create_dir(dir.join(".fseventsd")).and_then(|_| std::fs::write(dir.join(".fseventsd/no_log"), b""));
        log::info!("RAM spool: {} MB RAM disk ({dev}) at {}", cap >> 20, dir.display());
        Ok(dir)
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::path::{Path, PathBuf};

    pub const KIND: &str = "tmpfs";
    const SHM: &str = "/dev/shm";
    const TMPFS_MAGIC: u64 = 0x0102_1994;

    /// One per user and data folder (two instances don't share it).
    fn folder(data: &Path) -> PathBuf {
        let h = data.to_string_lossy().bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
        Path::new(SHM).join(format!("trunk-pro-{}-{h:08x}", unsafe { libc::getuid() }))
    }

    fn is_tmpfs(p: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
        let mut s: libc::statfs = unsafe { std::mem::zeroed() };
        unsafe { libc::statfs(c.as_ptr(), &mut s) == 0 && s.f_type as u64 == TMPFS_MAGIC }
    }

    pub fn existing(data: &Path) -> Option<PathBuf> {
        Some(folder(data)).filter(|d| d.is_dir())
    }

    pub fn remove(dir: &Path) -> Result<(), String> {
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
    }

    pub fn open(data: &Path, _cap: u64) -> Result<PathBuf, String> {
        if !is_tmpfs(Path::new(SHM)) {
            return Err(format!("{SHM} isn't a RAM filesystem here: set ramSpool.dir to one (a tmpfs mount)"));
        }
        let dir = folder(data);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        Ok(dir)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod sys {
    use std::path::{Path, PathBuf};

    pub const KIND: &str = "folder";

    pub fn existing(_: &Path) -> Option<PathBuf> {
        None
    }

    pub fn remove(_: &Path) -> Result<(), String> {
        Ok(())
    }

    pub fn open(_: &Path, _: u64) -> Result<PathBuf, String> {
        Err("a RAM spool is made on macOS and Linux only: set ramSpool.dir to a RAM-backed folder".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_moves_old_files_and_keeps_their_place() {
        let root = std::env::temp_dir().join(format!("trunk-pro-spool-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (spool, capture) = (root.join("spool"), root.join("capture"));
        fs::create_dir_all(spool.join("wmata/2026/10/4")).unwrap();
        fs::write(spool.join("wmata/2026/10/4/a.m4a"), b"audio").unwrap();
        fs::write(spool.join(".metadata_never_index"), b"").unwrap();
        // Too new to move.
        assert_eq!(sweep(&spool, &capture, Duration::from_secs(3600)), 0);
        assert_eq!(sweep(&spool, &capture, Duration::ZERO), 1);
        assert_eq!(fs::read(capture.join("wmata/2026/10/4/a.m4a")).unwrap(), b"audio");
        // The emptied folders go; the hidden file stays.
        assert!(!spool.join("wmata").exists() && spool.join(".metadata_never_index").exists());
        assert!(!has_files(&spool));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_spool_counts_only_its_own_files() {
        let root = std::env::temp_dir().join(format!("trunk-pro-spool-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let s = Spool::open(&RamSpool { enabled: true, size_mb: 16, dir: root.display().to_string() }, &root).unwrap();
        let (total, free) = s.usage().unwrap();
        assert_eq!(total, 16 << 20);
        fs::write(root.join("x"), vec![0u8; 1 << 20]).unwrap();
        assert_eq!(s.usage().unwrap().1, free - (1 << 20));
        assert!(s.room_for(1 << 20) && !s.room_for(15 << 20));
        assert_eq!(s.overflowed(), 1);
        let _ = fs::remove_dir_all(&root);
    }
}
