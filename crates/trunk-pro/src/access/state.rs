//! What signing in keeps that isn't a setting: when each user was last signed
//! out everywhere, and each passkey's record as last counted. It changes as
//! people sign in and out, so it's kept in its own file beside the config
//! ([`STATE_FILE`]), not in it: signing in changes no config (nobody's unsaved
//! edit in Setup is refused for it), and no browser is sent it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trunk_app::config::{Auth, AuthPasskey};

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

    /// Sign `user` out everywhere as of `at` (Unix s): kept, or why not.
    pub fn sign_out(&self, auth: &Auth, user: &str, at: i64) -> Result<(), String> {
        self.change(auth, |k| {
            k.signed_out.insert(user.to_string(), at);
            true
        })
        .map(drop)
    }

    /// webauthn-rs's record of passkey `p`: as last counted, else as the config has it.
    pub fn passkey(&self, p: &AuthPasskey) -> Value {
        self.kept.lock().unwrap().passkeys.get(&p.id).cloned().unwrap_or_else(|| p.key.clone())
    }

    /// Each change, to wait on.
    pub fn changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changed.subscribe()
    }

    /// Change it with `f` (whether it did), from the file as it is now (the
    /// command line's `access signout` may have written it since): written,
    /// then so in memory, or neither. What's kept for users and passkeys
    /// `auth` no longer has goes with it. Blocking: it writes a file.
    pub(super) fn change(&self, auth: &Auth, f: impl FnOnce(&mut Kept) -> bool) -> Result<bool, String> {
        let mut kept = self.kept.lock().unwrap();
        let mut next = self.path.as_deref().and_then(|p| read(p).ok().flatten()).unwrap_or_else(|| kept.clone());
        let changed = f(&mut next);
        if changed {
            next.signed_out.retain(|user, _| auth.users.contains_key(user));
            next.passkeys.retain(|id, _| auth.users.values().any(|u| u.passkeys.iter().any(|p| &p.id == id)));
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
        // Not written: not so.
        let nowhere = State { path: Some(dir.join(STATE_FILE).join("under-a-file")), ..Default::default() };
        assert!(nowhere.sign_out(&auth, "luke", 500).is_err());
        assert_eq!(nowhere.signed_out("luke"), None);
        // A file that isn't one: started again.
        std::fs::write(dir.join(STATE_FILE), "{").unwrap();
        assert_eq!(State::load(&dir).signed_out("luke"), None);
    }
}
