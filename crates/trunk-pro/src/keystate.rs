//! Key containers unlocked with a password typed into the interface: held in memory only, for as
//! long as the app runs, and wiped by Lock. What this tells a browser is which containers are
//! locked or unlocked and, to the computer's own browser, which keys they hold by algorithm, key
//! id and name; never a key or a password.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trunk_app::keys::Held;
use trunk_core::p25::crypt::Keyring;

/// The least time between two tries at a password.
const TRY_EVERY: Duration = Duration::from_secs(1);

struct Open {
    ring: Arc<Keyring>,
    held: Vec<Held>,
}

#[derive(Default)]
struct Inner {
    /// Unlocked containers, by resolved path.
    open: BTreeMap<PathBuf, Open>,
    /// Counts every change of what is held, for browsers to know to ask again.
    version: u64,
    last_try: Option<Instant>,
    /// Counts every change (an unlock, a lock, a new config): an unlock that was opening a
    /// container while one of the others happened is dropped, not applied.
    generation: u64,
}

#[derive(Default)]
pub struct KeyState(Mutex<Inner>);

impl KeyState {
    /// The keys unlocked from the container at `path`.
    pub fn held(&self, path: &Path) -> Option<Arc<Keyring>> {
        self.0.lock().unwrap().open.get(path).map(|o| o.ring.clone())
    }

    /// Where the key state is: read it before the work that decides an unlock, and give it to
    /// [`KeyState::unlock`], so that work begun before a newer change is dropped.
    pub fn generation(&self) -> u64 {
        self.0.lock().unwrap().generation
    }

    /// Open the container at `path` with `password`, typed in; the keys are held, in memory, if it
    /// opens (and nothing changed meanwhile, see [`KeyState::generation`]). Returns the keys left
    /// out. Tries are at least a second apart.
    pub fn unlock(&self, started: u64, path: &Path, password: &str) -> Result<Vec<String>, String> {
        {
            let mut g = self.0.lock().unwrap();
            if g.last_try.is_some_and(|t| t.elapsed() < TRY_EVERY) {
                return Err("Wait a moment before trying again.".into());
            }
            g.last_try = Some(Instant::now());
        }
        if password.is_empty() {
            return Err("Enter the container's password.".into());
        }
        let loaded = trunk_app::keys::open(path, password)?;
        if loaded.ring.is_empty() {
            return Err("The container holds no key this program can use.".into());
        }
        let mut g = self.0.lock().unwrap();
        if g.generation != started {
            return Err("The keys were changed while the container was opening; try again.".into());
        }
        g.generation += 1;
        g.version += 1;
        g.open.insert(path.to_path_buf(), Open { ring: Arc::new(loaded.ring), held: loaded.held });
        Ok(loaded.skipped)
    }

    /// Forget the container at `path` (None: all). Returns whether anything was held.
    pub fn lock(&self, path: Option<&Path>) -> bool {
        let mut g = self.0.lock().unwrap();
        g.generation += 1;
        let before = g.open.len();
        match path {
            Some(p) => {
                g.open.remove(p);
            }
            None => g.open.clear(),
        }
        let changed = g.open.len() != before;
        if changed {
            g.version += 1;
        }
        changed
    }

    /// A new config is being applied: forget containers no system names any more, and drop any
    /// unlock still opening one.
    pub fn retain(&self, named: &[PathBuf]) {
        let mut g = self.0.lock().unwrap();
        g.generation += 1;
        let before = g.open.len();
        g.open.retain(|p, _| named.contains(p));
        if g.open.len() != before {
            g.version += 1;
        }
    }

    /// The `keys` message: which containers are waiting for a password and which are open, and for
    /// which systems; state only, safe for any browser. `containers`: (as written, resolved path, systems).
    pub fn status_json(&self, containers: &[(String, PathBuf, Vec<String>)]) -> Value {
        let g = self.0.lock().unwrap();
        let list: Vec<Value> = containers
            .iter()
            .map(|(file, path, systems)| {
                let open = g.open.get(path);
                json!({ "file": file, "systems": systems, "state": if open.is_some() { "unlocked" } else { "locked" }, "count": open.map_or(0, |o| o.held.len()) })
            })
            .collect();
        json!({ "type": "keys", "containers": list, "version": g.version })
    }

    /// The `keysList` message: which keys are held, by algorithm, key id and name (not the keys).
    pub fn list_json(&self, containers: &[(String, PathBuf, Vec<String>)]) -> Value {
        let g = self.0.lock().unwrap();
        let keys: Vec<Value> = containers
            .iter()
            .filter_map(|(file, path, _)| g.open.get(path).map(|o| (file, o)))
            .flat_map(|(file, o)| o.held.iter().map(move |k| json!({ "file": file, "algId": k.alg_id, "keyId": k.key_id, "name": k.name })))
            .collect();
        json!({ "type": "keysList", "version": g.version, "keys": keys })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../trunk-app/src/testdata/dummy-test-keys.ekc"))
    }

    fn named() -> Vec<(String, PathBuf, Vec<String>)> {
        vec![("k.ekc".into(), container(), vec!["site".into()])]
    }

    #[test]
    fn a_password_unlocks_lock_wipes_and_a_wrong_one_changes_nothing() {
        let k = KeyState::default();
        let state = |k: &KeyState| k.status_json(&named())["containers"][0]["state"].as_str().unwrap().to_string();
        assert_eq!(state(&k), "locked");
        assert!(k.unlock(k.generation(), &container(), "nope").unwrap_err().contains("could not decrypt"));
        assert_eq!(state(&k), "locked");
        // (Tries are spaced.)
        assert!(k.unlock(k.generation(), &container(), "testing1234").unwrap_err().contains("Wait"));
        std::thread::sleep(TRY_EVERY + Duration::from_millis(50));
        let skipped = k.unlock(k.generation(), &container(), "testing1234").unwrap();
        assert_eq!(skipped.len(), 1, "DES-XL can't be decrypted: {skipped:?}");
        assert_eq!(state(&k), "unlocked");
        assert_eq!(k.status_json(&named())["containers"][0]["count"], 3);
        assert!(k.held(&container()).is_some());
        assert!(k.lock(None));
        assert_eq!(state(&k), "locked");
        assert!(k.held(&container()).is_none());
        assert!(!k.lock(None));
    }

    #[test]
    fn the_key_list_names_keys_and_the_status_never_does() {
        let k = KeyState::default();
        k.unlock(k.generation(), &container(), "testing1234").unwrap();
        let list = k.list_json(&named());
        assert_eq!(list["keys"].as_array().unwrap().len(), 3);
        assert!(list["keys"][0]["name"].is_string());
        let status = k.status_json(&named()).to_string();
        assert!(!status.contains("AES") && !status.contains("Key\""), "{status}");
        // Nothing in either is key material: 16-hex-digit runs.
        for text in [status, list.to_string()] {
            assert!(!text.as_bytes().windows(16).any(|w| w.iter().all(u8::is_ascii_hexdigit)), "{text}");
        }
    }

    #[test]
    fn an_unlock_does_not_undo_a_change_that_came_after_it_began() {
        let k = KeyState::default();
        let started = k.generation();
        k.lock(None);
        assert!(k.unlock(started, &container(), "testing1234").unwrap_err().contains("changed while"));
        assert!(k.held(&container()).is_none(), "the lock stands");
        let started = k.generation();
        k.retain(&[]);
        assert!(k.unlock(started, &container(), "testing1234").is_err());
        // With nothing in between it takes; a config that no longer names the container forgets it.
        std::thread::sleep(TRY_EVERY + Duration::from_millis(50));
        k.unlock(k.generation(), &container(), "testing1234").unwrap();
        k.retain(&[container()]);
        assert!(k.held(&container()).is_some());
        k.retain(&[]);
        assert!(k.held(&container()).is_none());
    }
}
