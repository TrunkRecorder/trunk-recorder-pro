//! What signing in keeps that isn't a setting: when each user was last signed
//! out everywhere, each passkey's record as last counted, and the apps users
//! let sign in as them ([`super::app`]). It changes as
//! people sign in and out, so it's kept in its own file beside the config
//! ([`STATE_FILE`]), not in it: signing in changes no config (nobody's unsaved
//! edit in Setup is refused for it), and no browser is sent it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trunk_app::config::{ApiAccess, Auth, AuthPasskey};

/// The file beside the config it's kept in.
pub const STATE_FILE: &str = "access-state.json";

/// The file's contents.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Kept {
    /// By user: when they were last signed out everywhere (Unix s).
    pub signed_out: BTreeMap<String, i64>,
    /// By passkey id: webauthn-rs's record of it as last counted (the
    /// config's, its counter raised: the config keeps it as it was added).
    pub passkeys: BTreeMap<String, Value>,
    /// By id: the apps signed in ([`super::app`]).
    pub apps: BTreeMap<String, App>,
}

/// An app a user let sign in as them: what its token is good for, and while.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct App {
    /// Whose it is.
    pub user: String,
    /// What the app calls itself, as the user saw it when they allowed it.
    pub name: String,
    /// The most it may do: the user's role caps it too, as it is at each request.
    pub access: ApiAccess,
    /// What the user was signed in with when they allowed it (as a session
    /// keeps it, or `proxy:` and the name the proxy in front gave): gone, so
    /// is the app.
    pub credential: String,
    /// When it was allowed, and when it ends (Unix s).
    pub created: u64,
    pub until: u64,
    /// Its token's SHA-256 (hex): the token itself is the app's alone.
    pub hash: String,
}

/// See the module. In memory, and in its file when it has one (the
/// recorder's: [`State::load`]); a change that can't be written isn't made.
pub struct State {
    path: Option<PathBuf>,
    kept: Mutex<Kept>,
    /// Told of each change: open connections check what they may still do.
    changed: tokio::sync::watch::Sender<()>,
}

impl Default for State {
    /// One kept nowhere.
    fn default() -> State {
        State { path: None, kept: Default::default(), changed: tokio::sync::watch::channel(()).0 }
    }
}

impl State {
    /// The recorder's, kept in [`STATE_FILE`] in `dir`. One that can't be
    /// read starts empty, and says so: nobody's signed out everywhere, and
    /// passkeys count from where they were added.
    pub fn load(dir: &Path) -> State {
        let path = dir.join(STATE_FILE);
        let kept = match read(&path) {
            Ok(k) => k.unwrap_or_default(),
            Err(e) => {
                log::warn!("Couldn't read {} ({e}): starting it again (sessions signed out everywhere count again until they expire)", path.display());
                Kept::default()
            }
        };
        State { path: Some(path), kept: Mutex::new(kept), ..Default::default() }
    }

    /// When `user` was last signed out everywhere (Unix s).
    pub fn signed_out(&self, user: &str) -> Option<i64> {
        self.kept.lock().unwrap().signed_out.get(user).copied()
    }

    /// Sign `user` out everywhere as of `at` (Unix s), their apps with them: kept, or why not.
    pub fn sign_out(&self, auth: &Auth, user: &str, at: i64) -> Result<(), String> {
        self.change(auth, |k| {
            k.signed_out.insert(user.to_string(), at);
            k.apps.retain(|_, a| a.user != user);
            true
        })
        .map(drop)
    }

    /// webauthn-rs's record of passkey `p`: as last counted, else as the config has it.
    pub fn passkey(&self, p: &AuthPasskey) -> Value {
        self.kept.lock().unwrap().passkeys.get(&p.id).cloned().unwrap_or_else(|| p.key.clone())
    }

    /// App `id`, as allowed.
    pub fn app(&self, id: &str) -> Option<App> {
        self.kept.lock().unwrap().apps.get(id).cloned()
    }

    /// The app whose token's hash is `hash`, and its id.
    pub fn app_of(&self, hash: &str) -> Option<(String, App)> {
        self.kept.lock().unwrap().apps.iter().find(|(_, a)| a.hash == hash).map(|(id, a)| (id.clone(), a.clone()))
    }

    /// Every app, by id.
    pub fn apps(&self) -> Vec<(String, App)> {
        self.kept.lock().unwrap().apps.iter().map(|(id, a)| (id.clone(), a.clone())).collect()
    }

    /// Keep `app` as `id`: whether it was (not when there's one by that id already), or why it couldn't be kept.
    pub fn add_app(&self, auth: &Auth, id: &str, app: App) -> Result<bool, String> {
        self.change(auth, |k| match k.apps.contains_key(id) {
            true => false,
            false => k.apps.insert(id.to_string(), app).is_none(),
        })
    }

    /// App `id` gone, its token good for nothing: what it was (none: there was none), or why it couldn't be kept so.
    pub fn remove_app(&self, auth: &Auth, id: &str) -> Result<Option<App>, String> {
        let mut was = None;
        self.change(auth, |k| {
            was = k.apps.remove(id);
            was.is_some()
        })?;
        Ok(was)
    }

    /// Each change, to wait on.
    pub fn changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changed.subscribe()
    }

    /// Change it with `f` (whether it did), from the file as it is now (the
    /// command line's `access signout` may have written it since): written,
    /// then so in memory, or neither. What's kept for users and passkeys
    /// `auth` no longer has goes with it, and apps past their time. Blocking: it writes a file.
    pub(super) fn change(&self, auth: &Auth, f: impl FnOnce(&mut Kept) -> bool) -> Result<bool, String> {
        let mut kept = self.kept.lock().unwrap();
        let mut next = self.path.as_deref().and_then(|p| read(p).ok().flatten()).unwrap_or_else(|| kept.clone());
        let changed = f(&mut next);
        if changed {
            next.signed_out.retain(|user, _| auth.users.contains_key(user));
            next.passkeys.retain(|id, _| auth.users.values().any(|u| u.passkeys.iter().any(|p| &p.id == id)));
            let now = crate::clock::unix_s() as u64;
            // (A user the proxy in front names needn't be one of the config's.)
            next.apps.retain(|_, a| now < a.until && (a.credential.starts_with(super::app::PROXY) || auth.users.contains_key(&a.user)));
            if let Some(path) = &self.path {
                write(path, &next)?;
            }
        }
        if *kept != next {
            *kept = next;
            self.changed.send_replace(());
        }
        Ok(changed)
    }
}

/// The file's contents; none: there's no file yet.
fn read(path: &Path) -> Result<Option<Kept>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map(Some).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Written whole or not at all: beside it, then moved over it.
fn write(path: &Path, kept: &Kept) -> Result<(), String> {
    let text = serde_json::to_string_pretty(kept).map_err(|e| e.to_string())?;
    trunk_app::files::make_parent(path)?;
    Ok(trunk_app::files::write_private(path, text)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiAccess;

    /// Kept in its file, read back by the next run, and by this one before it
    /// changes anything (the command line's change isn't written over); what's
    /// kept for a user who's gone goes; a change that can't be written isn't made.
    #[test]
    fn kept_in_its_file_and_changed_from_it() {
        let dir = crate::db::TestDir::new("access-state");
        let mut auth = Auth::default();
        for name in ["luke", "ann"] {
            crate::access::add_user(&mut auth, name, ApiAccess::Control).unwrap();
        }
        let state = State::load(&dir);
        assert_eq!(state.signed_out("luke"), None);
        let changes = state.changes();
        state.sign_out(&auth, "luke", 100).unwrap();
        assert!(changes.has_changed().unwrap(), "told");
        assert_eq!(State::load(&dir).signed_out("luke"), Some(100), "the next run's");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(STATE_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // The command line's, while this one runs: kept, and seen at the next change.
        State::load(&dir).sign_out(&auth, "ann", 200).unwrap();
        state.sign_out(&auth, "luke", 300).unwrap();
        assert_eq!((state.signed_out("luke"), state.signed_out("ann")), (Some(300), Some(200)));
        // A user removed: theirs goes at the next change.
        auth.users.remove("ann");
        state.sign_out(&auth, "luke", 400).unwrap();
        assert_eq!(State::load(&dir).signed_out("ann"), None);
        // An app: kept by its id once, found by its token's hash, gone when removed, or with its user.
        let app = |user: &str, hash: &str| App { user: user.into(), name: "Trunk Monitor".into(), access: ApiAccess::Listen, credential: "c".into(), created: 1, until: u64::MAX, hash: hash.into() };
        assert_eq!(state.add_app(&auth, "a1", app("luke", "h1")), Ok(true));
        assert_eq!(state.add_app(&auth, "a1", app("luke", "h2")), Ok(false), "that id's taken");
        assert_eq!(State::load(&dir).app_of("h1").map(|(id, a)| (id, a.user)), Some(("a1".into(), "luke".into())), "the next run's");
        assert_eq!(state.app_of("h2"), None);
        assert_eq!(state.add_app(&auth, "old", App { until: 5, ..app("luke", "h3") }), Ok(true));
        assert_eq!(state.apps().len(), 1, "one past its time isn't kept");
        assert_eq!(state.remove_app(&auth, "a1").map(|a| a.map(|a| a.hash)), Ok(Some("h1".into())));
        assert_eq!(state.remove_app(&auth, "a1"), Ok(None));
        state.add_app(&auth, "a2", app("luke", "h4")).unwrap();
        state.add_app(&auth, "a3", App { credential: "proxy:ann@example.com".into(), ..app("ann", "h5") }).unwrap();
        let none = Auth::default();
        state.sign_out(&none, "luke", 450).unwrap();
        assert_eq!(state.apps().into_iter().map(|(id, _)| id).collect::<Vec<_>>(), ["a3"], "luke's went with luke; the proxy's user is no user of the config's");
        // Not written: not so.
        let nowhere = State { path: Some(dir.join(STATE_FILE).join("under-a-file")), ..Default::default() };
        assert!(nowhere.sign_out(&auth, "luke", 500).is_err());
        assert_eq!(nowhere.signed_out("luke"), None);
        // A file that isn't one: started again.
        std::fs::write(dir.join(STATE_FILE), "{").unwrap();
        assert_eq!(State::load(&dir).signed_out("luke"), None);
    }
}
