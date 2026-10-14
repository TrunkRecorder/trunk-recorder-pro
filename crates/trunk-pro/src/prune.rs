//! Retention: delete the oldest calls from the recordings folder when it is
//! older, bigger, or a bigger share of its disk than `recording.retention`
//! allows. A pass runs soon after start and every [`EVERY`] after, reading
//! the setting each time, so changes apply at once.
//!
//! Only a call's own files go (see [`SUFFIXES`]), all of a call's together,
//! oldest call first; nothing else in the folder is touched. A call changed
//! in the last [`RECENT`] may still be recording, or waiting for a plugin,
//! and stays. Folders left empty go, the recordings folder itself not.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sysinfo::Disks;
use trunk_app::config::Retention;

use crate::runtime::Ctx;

/// Between passes.
const EVERY: Duration = Duration::from_secs(600);
/// Before the first pass after start.
const FIRST: Duration = Duration::from_secs(60);
/// A call changed this recently is never removed.
pub const RECENT: Duration = Duration::from_secs(600);
/// What a call's files end in; the rest of the name is the call's.
const SUFFIXES: [&str; 5] = [".frames.jsonl", ".wav", ".m4a", ".json", ".sdr"];
/// What the size limit's gigabyte is.
const GB: f64 = 1e9;

/// One call's files.
#[derive(Debug)]
struct Call {
    files: Vec<(PathBuf, u64)>,
    /// The latest any of its files changed.
    newest: SystemTime,
    bytes: u64,
}

/// What a pass found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub calls: usize,
    pub files: usize,
    pub bytes: u64,
    /// Why, as the log tells it.
    pub why: Vec<&'static str>,
}

/// The calls under `dir`, oldest first.
fn scan(dir: &Path) -> Vec<Call> {
    let mut by_call: HashMap<String, Call> = HashMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            let p = e.path();
            if m.is_dir() {
                stack.push(p);
            } else if m.is_file() {
                let Some(stem) = SUFFIXES.iter().find_map(|s| name.strip_suffix(s)) else { continue };
                let key = d.join(stem).display().to_string();
                let t = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let c = by_call.entry(key).or_insert_with(|| Call { files: Vec::new(), newest: SystemTime::UNIX_EPOCH, bytes: 0 });
                c.files.push((p, m.len()));
                c.newest = c.newest.max(t);
                c.bytes += m.len();
            }
        }
    }
    let mut calls: Vec<Call> = by_call.into_values().collect();
    calls.sort_by_key(|c| c.newest);
    calls
}

/// The size the folder may be, bytes: the smaller of the two limits set.
fn size_limit(r: &Retention, disk_total: Option<u64>) -> Option<u64> {
    let gb = r.size_limit_gb().map(|g| (g * GB) as u64);
    let pct = r.disk_limit_pct().and_then(|p| disk_total.filter(|&t| t > 0).map(|t| (t as f64 * p / 100.0) as u64));
    gb.into_iter().chain(pct).min()
}

/// Which of `calls` (oldest first) to delete at `now`: the indexes, and why.
fn plan(calls: &[Call], r: &Retention, disk_total: Option<u64>, now: SystemTime) -> (Vec<usize>, Vec<&'static str>) {
    let mut gone = vec![false; calls.len()];
    let mut why = Vec::new();
    // Oldest first, so what follows the first call too recent to remove is too.
    let removable = |c: &Call| now.duration_since(c.newest).is_ok_and(|d| d >= RECENT);
    if let Some(days) = r.age_limit_days() {
        let age = Duration::from_secs_f64(days * 86_400.0);
        for (i, c) in calls.iter().enumerate() {
            if removable(c) && now.duration_since(c.newest).is_ok_and(|d| d > age) {
                gone[i] = true;
            }
        }
        if gone.iter().any(|&g| g) {
            why.push("older than the age limit");
        }
    }
    if let Some(limit) = size_limit(r, disk_total) {
        let mut total: u64 = calls.iter().zip(&gone).filter(|(_, g)| !**g).map(|(c, _)| c.bytes).sum();
        let mut cut = false;
        for (i, c) in calls.iter().enumerate() {
            if total <= limit {
                break;
            }
            if gone[i] {
                continue;
            }
            if !removable(c) {
                break;
            }
            gone[i] = true;
            total -= c.bytes;
            cut = true;
        }
        if cut {
            why.push("over the size limit");
        }
    }
    ((0..calls.len()).filter(|&i| gone[i]).collect(), why)
}

/// One pass over `dir`.
pub fn pass(dir: &Path, r: &Retention, disk_total: Option<u64>, now: SystemTime) -> Report {
    let calls = scan(dir);
    let (doomed, why) = plan(&calls, r, disk_total, now);
    let mut rep = Report { why, ..Default::default() };
    let mut parents = std::collections::BTreeSet::new();
    for i in doomed {
        let mut removed = 0;
        for (p, len) in &calls[i].files {
            match fs::remove_file(p) {
                Ok(()) => {
                    rep.files += 1;
                    rep.bytes += len;
                    removed += 1;
                    if let Some(d) = p.parent() {
                        parents.insert(d.to_path_buf());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => log::warn!("Retention: couldn't delete {}: {e}", p.display()),
            }
        }
        rep.calls += usize::from(removed > 0);
    }
    // Folders that emptied, deepest first, up to (not including) `dir`.
    let mut dirs: Vec<PathBuf> = parents.into_iter().flat_map(|p| p.ancestors().take_while(|a| *a != dir).map(Path::to_path_buf).collect::<Vec<_>>()).collect();
    dirs.sort();
    dirs.dedup();
    for d in dirs.iter().rev() {
        let _ = fs::remove_dir(d);
    }
    rep
}

/// The capacity of the disk `path` is on.
fn disk_total(path: &Path) -> Option<u64> {
    let p = fs::canonicalize(path).ok()?;
    let disks = Disks::new_with_refreshed_list();
    let d = disks.list().iter().filter(|d| p.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len())?;
    Some(d.total_space())
}

pub fn start(ctx: Arc<Ctx>) {
    let _ = std::thread::Builder::new().name("retention".into()).spawn(move || {
        std::thread::sleep(FIRST);
        loop {
            let (dir, r) = {
                let c = ctx.config.lock().unwrap();
                (c.recording.capture_dir.clone(), c.recording.retention.clone())
            };
            if r.active() && !dir.is_empty() {
                let dir = PathBuf::from(dir);
                let rep = pass(&dir, &r, disk_total(&dir), SystemTime::now());
                if rep.files > 0 {
                    log::info!("Retention: deleted {} call(s), {} file(s), {:.1} MB from {} ({})", rep.calls, rep.files, rep.bytes as f64 / 1e6, dir.display(), rep.why.join(", "));
                }
            }
            std::thread::sleep(EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(age_h: u64, bytes: u64, now: SystemTime) -> Call {
        Call { files: vec![(PathBuf::from(format!("/x/{age_h}-{bytes}.wav")), bytes)], newest: now - Duration::from_secs(age_h * 3600), bytes }
    }

    fn limits(days: f64, gb: f64, pct: f64) -> Retention {
        Retention { max_age_days: days, max_size_gb: gb, max_disk_pct: pct }
    }

    #[test]
    fn nothing_is_removed_with_no_limit() {
        let now = SystemTime::now();
        let calls = vec![call(5000, 10, now), call(1, 10, now)];
        assert_eq!(plan(&calls, &limits(0.0, 0.0, 0.0), None, now).0, Vec::<usize>::new());
    }

    #[test]
    fn age_removes_calls_older_than_the_limit() {
        let now = SystemTime::now();
        let calls = vec![call(80, 1, now), call(50, 1, now), call(20, 1, now)];
        let (gone, why) = plan(&calls, &limits(2.0, 0.0, 0.0), None, now);
        assert_eq!(gone, [0, 1]);
        assert_eq!(why, ["older than the age limit"]);
    }

    #[test]
    fn size_removes_the_oldest_until_it_fits() {
        let now = SystemTime::now();
        let gb = GB as u64;
        let calls = vec![call(30, gb, now), call(20, gb, now), call(10, gb, now), call(5, gb, now)];
        let (gone, why) = plan(&calls, &limits(0.0, 2.5, 0.0), None, now);
        assert_eq!(gone, [0, 1], "4 GB down to 2");
        assert_eq!(why, ["over the size limit"]);
        assert!(plan(&calls, &limits(0.0, 4.0, 0.0), None, now).0.is_empty(), "exactly at the limit is fine");
    }

    #[test]
    fn the_share_of_the_disk_and_the_size_limit_apply_together() {
        let now = SystemTime::now();
        let gb = GB as u64;
        let calls = vec![call(30, gb, now), call(20, gb, now), call(10, gb, now), call(5, gb, now)];
        // 10% of a 20 GB disk is 2 GB; the size limit says 3.
        assert_eq!(plan(&calls, &limits(0.0, 3.0, 10.0), Some(20 * gb), now).0, [0, 1]);
        // 50% of it is 10 GB; the size limit says 3.
        assert_eq!(plan(&calls, &limits(0.0, 3.0, 50.0), Some(20 * gb), now).0, [0]);
        // A share of a disk of unknown size does nothing.
        assert!(plan(&calls, &limits(0.0, 0.0, 10.0), None, now).0.is_empty());
    }

    #[test]
    fn age_and_size_together_count_only_what_is_left() {
        let now = SystemTime::now();
        let gb = GB as u64;
        // Age removes the first; the remaining 2 GB fit in 2.5.
        let calls = vec![call(80, gb, now), call(10, gb, now), call(5, gb, now)];
        assert_eq!(plan(&calls, &limits(2.0, 2.5, 0.0), None, now).0, [0]);
    }

    #[test]
    fn a_recent_call_is_never_removed() {
        let now = SystemTime::now();
        let gb = GB as u64;
        let mut calls = vec![call(5, gb, now), call(0, gb, now)];
        calls[1].newest = now - Duration::from_secs(30);
        let (gone, _) = plan(&calls, &limits(0.0, 0.5, 0.0), None, now);
        assert_eq!(gone, [0], "the one 30 s old stays, though the folder is still over");
        let (gone, _) = plan(&calls, &limits(0.000001, 0.0, 0.0), None, now);
        assert_eq!(gone, [0]);
    }

    fn write(p: &Path, n: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![0u8; n]).unwrap();
    }

    fn set_age(p: &Path, now: SystemTime, hours: u64) {
        let f = fs::File::options().write(true).open(p).unwrap();
        f.set_modified(now - Duration::from_secs(hours * 3600)).unwrap();
    }

    #[test]
    fn a_pass_deletes_whole_calls_and_only_calls() {
        let root = std::env::temp_dir().join(format!("trunk-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let now = SystemTime::now();
        let old = root.join("a/2026/01/01");
        let new = root.join("a/2026/10/13");
        for ext in ["wav", "json", "m4a", "frames.jsonl"] {
            write(&old.join(format!("5-100_851.0.{ext}")), 10);
            set_age(&old.join(format!("5-100_851.0.{ext}")), now, 24 * 60);
        }
        write(&old.join("notes.txt"), 10);
        set_age(&old.join("notes.txt"), now, 24 * 60);
        write(&new.join("5-200_851.0.wav"), 10);
        write(&new.join("5-200_851.0.json"), 10);
        set_age(&new.join("5-200_851.0.wav"), now, 2);
        set_age(&new.join("5-200_851.0.json"), now, 2);
        let rep = pass(&root, &limits(30.0, 0.0, 0.0), None, now);
        assert_eq!((rep.calls, rep.files, rep.bytes), (1, 4, 40));
        assert!(old.join("notes.txt").exists(), "other files stay, and so does their folder");
        assert!(!old.join("5-100_851.0.wav").exists() && !old.join("5-100_851.0.json").exists());
        assert!(new.join("5-200_851.0.wav").exists() && new.join("5-200_851.0.json").exists());
        // Delete the note: the folders that held only the old call go on the next pass.
        fs::remove_file(old.join("notes.txt")).unwrap();
        write(&old.join("5-100_851.0.wav"), 10);
        set_age(&old.join("5-100_851.0.wav"), now, 24 * 60);
        pass(&root, &limits(30.0, 0.0, 0.0), None, now);
        assert!(!root.join("a/2026/01").exists(), "emptied folders go");
        assert!(root.exists() && new.exists(), "the recordings folder and busy ones stay");
        let _ = fs::remove_dir_all(&root);
    }
}
