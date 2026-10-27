//! The files kept beside the config (talkgroup files, talker aliases, band
//! plans, the radio registry, …), written on a thread of their own so a slow
//! disk (an SD card) never holds up the recording or a browser's request.
//! Whoever changes one keeps it in memory and hands it over: what queued up
//! for a file meanwhile is written once, as last handed over. Each write is
//! whole and atomic ([`crate::config::write_atomic`]); one that fails is
//! logged. [`crate::runtime::Ctx::saver`] is the one for the process.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;

/// What a file is to hold, made on the writer's thread (nothing: left as it is).
type Make = Box<dyn FnOnce() -> Option<String> + Send>;

enum Msg {
    /// A file to write (`None`: to remove).
    File(PathBuf, Option<Make>),
    /// Answer once everything before is done.
    Flush(mpsc::Sender<()>),
}

/// Hands files to the writer thread.
pub struct Saver {
    tx: mpsc::Sender<Msg>,
}

impl Saver {
    pub fn start() -> Saver {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("saver".into()).spawn(move || run(rx)).expect("thread");
        Saver { tx }
    }

    /// `path` is to hold `text`.
    pub fn text(&self, path: PathBuf, text: String) {
        self.later(path, move || Some(text));
    }

    /// `path` is to hold what `make` gives, made on the writer's thread (a
    /// large JSON, say) and only if it's still the latest for `path` then.
    pub fn later(&self, path: PathBuf, make: impl FnOnce() -> Option<String> + Send + 'static) {
        let _ = self.tx.send(Msg::File(path, Some(Box::new(make))));
    }

    /// `path` is to be removed.
    pub fn remove(&self, path: PathBuf) {
        let _ = self.tx.send(Msg::File(path, None));
    }

    /// Wait until everything handed over before is on disk (or failed).
    pub fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

fn run(rx: mpsc::Receiver<Msg>) {
    while let Ok(first) = rx.recv() {
        let (mut files, mut flushed) = (BTreeMap::new(), Vec::new());
        for m in std::iter::once(first).chain(rx.try_iter()) {
            match m {
                Msg::File(path, make) => {
                    files.insert(path, make);
                }
                Msg::Flush(done) => flushed.push(done),
            }
        }
        for (path, make) in files {
            write(path, make);
        }
        for done in flushed {
            let _ = done.send(());
        }
    }
}

fn write(path: PathBuf, make: Option<Make>) {
    let Some(make) = make else {
        if let Err(e) = std::fs::remove_file(&path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }) {
            log::warn!("Couldn't delete {}: {e}", path.display());
        }
        return;
    };
    let Some(text) = make() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = crate::config::write_atomic(&path, text) {
        log::warn!("Couldn't save {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trunk-pro-saver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn what_queued_up_is_written_once_as_last_handed_over() {
        let d = dir("coalesce");
        let s = Saver::start();
        // The writer is held up (a slow disk) while a file is handed over again and again.
        let (go, wait) = mpsc::channel::<()>();
        s.later(d.join("slow"), move || wait.recv().ok().map(|_| "slow".into()));
        let made = Arc::new(AtomicUsize::new(0));
        for i in 0..50 {
            let made = made.clone();
            s.later(d.join("sub/a.json"), move || {
                made.fetch_add(1, Ordering::SeqCst);
                Some(format!("{i}"))
            });
        }
        go.send(()).unwrap();
        s.flush();
        assert_eq!(std::fs::read_to_string(d.join("sub/a.json")).unwrap(), "49");
        assert_eq!(made.load(Ordering::SeqCst), 1, "only the latest is made");
        assert_eq!(std::fs::read_to_string(d.join("slow")).unwrap(), "slow");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_removed_and_nothing_made() {
        let d = dir("remove");
        let s = Saver::start();
        s.text(d.join("a.csv"), "1,a".into());
        s.flush();
        assert!(d.join("a.csv").exists());
        s.later(d.join("a.csv"), || None);
        s.flush();
        assert_eq!(std::fs::read_to_string(d.join("a.csv")).unwrap(), "1,a", "nothing made: left as it was");
        s.remove(d.join("a.csv"));
        s.remove(d.join("never.csv"));
        s.flush();
        assert!(!d.join("a.csv").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
