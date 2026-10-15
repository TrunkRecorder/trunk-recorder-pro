//! The hold: what plugins are given late, kept until their delay is up
//! ([`super::delay`] says how long).
//!
//! The host hands the hold what a plugin is to have late instead of putting
//! it in the plugin's queue, and the hold gives it to that plugin (by its
//! label) when the time comes, to whichever host is running then: plugins
//! restart when their settings change, and the hold outlives them. A
//! finished call is also kept on disk until it's given (one file each, in
//! `<data>/plugin-hold/`), so it's still given after the recorder restarts;
//! what's live (a call starting, a unit's event) is kept in memory only.
//!
//! Each thing's delay counts from when it came, and is asked again whenever
//! the delays change: shortening one lets what's waiting go sooner. When
//! it's due and the plugin is behind (its queue full), it's tried again a
//! second later; when that plugin isn't running any more (turned off,
//! removed, set up wrong) or may no longer have it (`shareDecrypted` turned
//! off), it's dropped, and said. While nothing is recording, finished calls
//! wait for the next recording, and live events are dropped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use trunk_recorder_plugin::Level;

use super::archive::{Archive, Ticket};
use super::delay::{About, Delays};
use super::host::{Note, Notes};

/// What happened to something offered to a plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    Sent,
    /// It's behind: try again later.
    Full,
    /// It isn't running, or may not have it.
    Gone,
}

/// Where held things go when they're due: the running plugins.
pub trait Outlet: Send + Sync {
    /// Give plugin `label` the line `line` (`decrypted`: of a call decrypted with our keys).
    fn offer(&self, label: &str, line: &Arc<str>, decrypted: bool) -> Offer;
}

/// Live things held, at most; beyond this they're dropped (counted as the plugin's queue's are).
const LIVE_MAX: usize = 100_000;
/// A plugin that's behind is offered what's due again after this.
const RETRY: Duration = Duration::from_secs(1);

/// Something held for a plugin.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    /// Its plugin's label (`openmhz`, `openmhz#2`).
    plugin: String,
    about: About,
    /// When it came, Unix ms (its delay counts from then).
    at_ms: u64,
    /// The message, as the plugin is to read it (a JSON line).
    #[serde(with = "shared_str")]
    line: Arc<str>,
    /// A finished call (its path): kept on disk, given after a restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    call: Option<String>,
    /// What the archive does with its files.
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket: Option<Ticket>,
    /// Its file in the hold folder.
    #[serde(skip)]
    file: Option<PathBuf>,
}

/// An `Arc<str>` as a string.
mod shared_str {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;
    pub fn serialize<S: Serializer>(v: &Arc<str>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<str>, D::Error> {
        Ok(String::deserialize(d)?.into())
    }
}

enum To {
    /// Not recording: finished calls wait.
    Nobody,
    /// Recording, with these plugins (None: none running).
    Host(Option<Arc<dyn Outlet>>),
}

struct State {
    /// By when it's due (Unix ms), then order of coming.
    waiting: BTreeMap<(u64, u64), Item>,
    delays: Delays,
    to: To,
    next: u64,
    /// Finished calls held, by plugin label.
    calls: BTreeMap<String, usize>,
    live: usize,
    started: bool,
}

pub struct Hold {
    state: Mutex<State>,
    wake: Condvar,
    /// Where finished calls are kept (None: in memory only).
    dir: Mutex<Option<PathBuf>>,
    archive: Arc<Archive>,
    notes: Mutex<Option<Notes>>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl Hold {
    pub fn new(archive: Arc<Archive>) -> Arc<Hold> {
        let state = State { waiting: BTreeMap::new(), delays: Delays::default(), to: To::Nobody, next: 0, calls: BTreeMap::new(), live: 0, started: false };
        Arc::new(Hold { state: Mutex::new(state), wake: Condvar::new(), dir: Mutex::new(None), archive, notes: Mutex::new(None) })
    }

    /// Who to tell of what's dropped.
    pub fn set_notes(&self, notes: Notes) {
        *self.notes.lock().unwrap() = Some(notes);
    }

    fn note(&self, plugin: &str, level: Level, text: String) {
        if let Some(n) = self.notes.lock().unwrap().as_ref() {
            n(Note::Log { plugin: plugin.to_string(), level, text });
        }
    }

    /// Keep finished calls in `dir`, and take back those kept there by the run before.
    pub fn open(self: &Arc<Self>, dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        *self.dir.lock().unwrap() = Some(dir.to_path_buf());
        let mut items = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            match std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Item>(&b).ok()).filter(|i| i.call.is_some()) {
                Some(i) => items.push(Item { file: Some(path), ..i }),
                None => {
                    log::warn!("Plugin hold: {} isn't a held call; removed", path.display());
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        if items.is_empty() {
            return;
        }
        // The archive forgot them with the run before: what's to be done with each call's files, once its plugins have it.
        let mut by_call: BTreeMap<&str, (usize, Option<&Ticket>)> = BTreeMap::new();
        for i in &items {
            let e = by_call.entry(i.call.as_deref().unwrap()).or_default();
            e.0 += 1;
            e.1 = e.1.or(i.ticket.as_ref());
        }
        for (rel, (n, t)) in by_call {
            if let Some(t) = t {
                self.archive.restore(rel, n, t.clone());
            }
        }
        log::info!("Plugin hold: {} call(s) still held for plugins", items.len());
        let mut st = self.state.lock().unwrap();
        for i in items {
            Self::insert(&mut st, i);
        }
        self.start(&mut st);
    }

    /// The delays now (what's waiting is timed again by them).
    pub fn set_delays(self: &Arc<Self>, delays: Delays) {
        let mut st = self.state.lock().unwrap();
        if st.delays == delays {
            return;
        }
        st.delays = delays;
        let items: Vec<Item> = std::mem::take(&mut st.waiting).into_values().collect();
        for i in items {
            Self::insert(&mut st, i);
        }
        self.wake.notify_all();
    }

    /// How long plugin `label` waits for something about `about`.
    pub fn delay(&self, label: &str, about: &About) -> Duration {
        self.state.lock().unwrap().delays.delay(label, about)
    }

    /// The longest any delay is (how long a held call's files may wait).
    pub fn longest(&self) -> Duration {
        self.state.lock().unwrap().delays.longest()
    }

    /// Finished calls held, by plugin label.
    pub fn calls(&self) -> BTreeMap<String, usize> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Recording, with these plugins (None: none running): what's due goes to them.
    pub fn attach(self: &Arc<Self>, to: Option<Arc<dyn Outlet>>) {
        let mut st = self.state.lock().unwrap();
        st.to = To::Host(to);
        self.start(&mut st);
        self.wake.notify_all();
    }

    /// Not recording: finished calls wait for the next recording.
    pub fn detach(&self) {
        self.state.lock().unwrap().to = To::Nobody;
        self.wake.notify_all();
    }

    /// Hold `line` for plugin `label` (`call`: it's that finished call's).
    /// False: too much is held already, and it's dropped.
    pub fn push(self: &Arc<Self>, label: &str, about: &About, line: &Arc<str>, call: Option<&str>) -> bool {
        let mut item = Item { plugin: label.to_string(), about: about.clone(), at_ms: now_ms(), line: line.clone(), call: call.map(str::to_string), ticket: None, file: None };
        if let Some(rel) = call {
            item.ticket = self.archive.ticket(rel);
            self.archive.hold(rel);
            item.file = self.save(&item);
        } else if self.state.lock().unwrap().live >= LIVE_MAX {
            return false;
        }
        let mut st = self.state.lock().unwrap();
        Self::insert(&mut st, item);
        self.start(&mut st);
        self.wake.notify_all();
        true
    }

    /// Write a finished call's item to the hold folder; its file.
    fn save(&self, item: &Item) -> Option<PathBuf> {
        let dir = self.dir.lock().unwrap().clone()?;
        let name: String = item.plugin.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!("{}-{n}-{name}.json", item.at_ms));
        match serde_json::to_vec(item).map_err(|e| e.to_string()).and_then(|b| crate::config::write_atomic(&path, &b).map_err(|e| e.to_string())) {
            Ok(()) => Some(path),
            Err(e) => {
                self.note(&item.plugin, Level::Warn, format!("couldn't keep a held call on disk ({e}): it's lost if the recorder restarts first"));
                None
            }
        }
    }

    fn insert(st: &mut State, item: Item) {
        let due = item.at_ms + st.delays.delay(&item.plugin, &item.about).as_millis() as u64;
        Self::insert_at(st, due, item);
    }

    fn insert_at(st: &mut State, due: u64, item: Item) {
        st.next += 1;
        match &item.call {
            Some(_) => *st.calls.entry(item.plugin.clone()).or_default() += 1,
            None => st.live += 1,
        }
        st.waiting.insert((due, st.next), item);
    }

    fn take(st: &mut State, key: (u64, u64)) -> Item {
        let item = st.waiting.remove(&key).unwrap();
        match &item.call {
            Some(_) => {
                if let Some(n) = st.calls.get_mut(&item.plugin) {
                    *n -= 1;
                    if *n == 0 {
                        st.calls.remove(&item.plugin);
                    }
                }
            }
            None => st.live -= 1,
        }
        item
    }

    fn start(self: &Arc<Self>, st: &mut State) {
        if !std::mem::replace(&mut st.started, true) {
            let me = self.clone();
            std::thread::Builder::new().name("plugin-hold".into()).spawn(move || me.run()).expect("thread");
        }
    }

    fn run(&self) {
        let mut st = self.state.lock().unwrap();
        loop {
            let now = now_ms();
            let due: Vec<(u64, u64)> = st.waiting.range(..(now + 1, 0)).map(|(k, _)| *k).collect();
            let mut given = Vec::new();
            let mut dropped = Vec::new();
            let mut retry = Vec::new();
            for k in due {
                let item = Self::take(&mut st, k);
                let offer = match &st.to {
                    To::Nobody if item.call.is_some() => {
                        retry.push(item);
                        continue;
                    }
                    To::Nobody | To::Host(None) => Offer::Gone,
                    To::Host(Some(o)) => o.offer(&item.plugin, &item.line, item.about.decrypted),
                };
                match offer {
                    Offer::Sent => given.push(item),
                    Offer::Full => retry.push(item),
                    Offer::Gone => dropped.push(item),
                }
            }
            let nobody = matches!(st.to, To::Nobody);
            for item in retry {
                Self::insert_at(&mut st, now + RETRY.as_millis() as u64, item);
            }
            let wait = if nobody && st.waiting.values().all(|i| i.call.is_some()) {
                // Nothing to give until a recording starts (or something live comes).
                None
            } else {
                st.waiting.keys().next().map(|k| Duration::from_millis(k.0.saturating_sub(now)))
            };
            if !given.is_empty() || !dropped.is_empty() {
                drop(st);
                self.settle(given, dropped);
                st = self.state.lock().unwrap();
                continue;
            }
            st = match wait {
                Some(w) => self.wake.wait_timeout(st, w.max(Duration::from_millis(5))).unwrap().0,
                None => self.wake.wait(st).unwrap(),
            };
        }
    }

    /// What was given and dropped: its files go, and the archive is told.
    fn settle(&self, given: Vec<Item>, dropped: Vec<Item>) {
        for i in &given {
            if let Some(rel) = &i.call {
                self.archive.release(rel);
            }
        }
        // Dropped calls, by plugin, to say once each.
        let mut said: BTreeMap<&str, usize> = BTreeMap::new();
        for i in &dropped {
            if let Some(rel) = &i.call {
                self.archive.release(rel);
                self.archive.result(rel, trunk_recorder_plugin::Outcome::Skipped);
                *said.entry(&i.plugin).or_default() += 1;
            }
        }
        for i in given.iter().chain(&dropped) {
            if let Some(f) = &i.file {
                let _ = std::fs::remove_file(f);
            }
        }
        for (plugin, n) in said {
            self.note(plugin, Level::Warn, format!("{n} held call(s) dropped: it isn't running, or may no longer have them"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    /// Plugins that take everything, or are full, or aren't there.
    struct Plugins {
        got: Mutex<Vec<(String, String)>>,
        full: Mutex<bool>,
    }
    impl Outlet for Plugins {
        fn offer(&self, label: &str, line: &Arc<str>, decrypted: bool) -> Offer {
            if label == "gone" || (decrypted && label != "trusted") {
                return Offer::Gone;
            }
            if *self.full.lock().unwrap() {
                return Offer::Full;
            }
            self.got.lock().unwrap().push((label.to_string(), line.to_string()));
            Offer::Sent
        }
    }

    fn delays(v: serde_json::Value) -> Delays {
        Delays::of(&serde_json::from_value(v).unwrap())
    }
    fn about(tg: u32) -> About {
        About { system: "dcfd".into(), talkgroup: Some(tg), decrypted: false }
    }
    fn wait_for(f: impl Fn() -> bool) -> bool {
        let t0 = Instant::now();
        while !f() {
            if t0.elapsed() > Duration::from_secs(5) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trunk-pro-hold-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Held until its delay is up; shortening the delay lets it go now.
    #[test]
    fn held_until_due_and_retimed() {
        let hold = Hold::new(Arc::default());
        hold.set_delays(delays(json!({ "pluginDelays": [{ "delayS": 3600 }] })));
        let out = Arc::new(Plugins { got: Mutex::default(), full: Mutex::new(false) });
        hold.attach(Some(out.clone()));
        assert_eq!(hold.delay("openmhz", &about(1)), Duration::from_secs(3600));
        assert!(hold.push("openmhz", &about(1), &"a\n".into(), None));
        std::thread::sleep(Duration::from_millis(50));
        assert!(out.got.lock().unwrap().is_empty(), "an hour to wait");
        hold.set_delays(delays(json!({ "pluginDelays": [{ "delayS": 3600, "plugins": ["other"] }] })));
        assert!(wait_for(|| out.got.lock().unwrap().len() == 1), "no delay for it now");
        assert_eq!(out.got.lock().unwrap()[0], ("openmhz".to_string(), "a\n".to_string()));
    }

    /// A plugin that's behind gets it a moment later; one not there loses it.
    #[test]
    fn behind_is_retried_and_gone_is_dropped() {
        let archive: Arc<Archive> = Arc::default();
        let hold = Hold::new(archive);
        let out = Arc::new(Plugins { got: Mutex::default(), full: Mutex::new(true) });
        hold.attach(Some(out.clone()));
        hold.push("openmhz", &about(1), &"a\n".into(), Some("dcfd/a"));
        hold.push("gone", &about(1), &"b\n".into(), Some("dcfd/b"));
        hold.push("openmhz", &About { decrypted: true, ..about(1) }, &"c\n".into(), Some("dcfd/c"));
        assert!(wait_for(|| !hold.calls().contains_key("gone") && hold.calls().get("openmhz") == Some(&1)), "the one not there, and the decrypted one, dropped");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(hold.calls().get("openmhz"), Some(&1), "the full one waits");
        *out.full.lock().unwrap() = false;
        assert!(wait_for(|| hold.calls().is_empty()));
        assert_eq!(out.got.lock().unwrap().iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>(), ["a\n"], "a decrypted call isn't given to a plugin that may not have it");
    }

    /// Not recording: a finished call waits for the next recording, live events don't.
    #[test]
    fn calls_wait_for_a_recording() {
        let hold = Hold::new(Arc::default());
        hold.push("openmhz", &about(1), &"call\n".into(), Some("dcfd/a"));
        hold.push("openmhz", &about(1), &"live\n".into(), None);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(hold.calls().get("openmhz"), Some(&1));
        let out = Arc::new(Plugins { got: Mutex::default(), full: Mutex::new(false) });
        hold.attach(Some(out.clone()));
        assert!(wait_for(|| out.got.lock().unwrap().len() == 1));
        assert_eq!(out.got.lock().unwrap()[0].1, "call\n");
    }

    /// Finished calls are kept on disk, and a new run takes them back, with
    /// the archive told what to do with their files.
    #[test]
    fn calls_held_through_a_restart() {
        let dir = folder("restart");
        let archive: Arc<Archive> = Arc::default();
        let rules = super::super::FileRules { compress_wav: false, audio_archive: false, call_log: false, archive_files_on_failure: false };
        let base = dir.join("rec/dcfd/a");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(base.with_extension("wav"), b"x").unwrap();
        archive.expect("dcfd/a", 1, rules, &base, None);
        let hold = Hold::new(archive);
        hold.open(&dir.join("hold"));
        hold.set_delays(delays(json!({ "pluginDelays": [{ "delayS": 3600 }] })));
        hold.push("openmhz", &about(1), &"call\n".into(), Some("dcfd/a"));
        assert_eq!(std::fs::read_dir(dir.join("hold")).unwrap().count(), 1);

        // The next run.
        let archive: Arc<Archive> = Arc::default();
        let hold = Hold::new(archive.clone());
        hold.set_delays(delays(json!({ "pluginDelays": [{ "delayS": 3600 }] })));
        hold.open(&dir.join("hold"));
        assert_eq!(hold.calls().get("openmhz"), Some(&1));
        assert_eq!(archive.ticket("dcfd/a").map(|t| t.base), Some(base.clone()));
        hold.set_delays(Delays::default());
        let out = Arc::new(Plugins { got: Mutex::default(), full: Mutex::new(false) });
        hold.attach(Some(out.clone()));
        assert!(wait_for(|| out.got.lock().unwrap().len() == 1));
        assert!(wait_for(|| std::fs::read_dir(dir.join("hold")).unwrap().count() == 0), "its file goes once it's given");
        // The plugin reports: nothing keeps the call, so its files go.
        archive.result("dcfd/a", trunk_recorder_plugin::Outcome::Ok);
        assert!(!base.with_extension("wav").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
