//! What a page may do with the recorder's API, by the port it came in on.
//!
//! The built-in interface's port is the recorder's own: whoever reaches it
//! runs the recorder (as do pages listed in `server.allowedOrigins`, and
//! programs that aren't browsers). Every other interface has a port of its
//! own ([`crate::interfaces`]), and its connections get the access the config
//! gives it:
//!
//! - **listen**: everything the recorder shows and plays (status, calls on
//!   the air, recorded calls, live audio, the dashboard's figures), and
//!   nothing it could change. The config it's shown leaves out plugin
//!   settings and key sources (API keys, passwords' files); plugin and
//!   interface management messages aren't sent to it.
//! - **control**: what the built-in interface can do.

use serde_json::Value;

use crate::config::ApiAccess;

/// Which port a request came in on.
#[derive(Clone, Debug)]
pub enum Door {
    /// The built-in interface's.
    Builtin,
    /// Interface `id`'s; `stop` turns true when it's no longer served there.
    Interface { id: String, stop: tokio::sync::watch::Receiver<bool> },
}

/// Commands a listen-only page may send.
const LISTEN_COMMANDS: &[&str] = &["subscribe", "statsQuery", "radioQuery", "olderCalls", "listen"];

/// Messages only control access hears (plugin and interface management, key containers).
const CONTROL_MESSAGES: &[&str] = &["plugins", "pluginRuntime", "pluginStore", "pluginInstall", "interfaces", "interfaceStore", "interfaceInstall", "keys"];

/// May `access` send command `ty`?
pub fn may(access: ApiAccess, ty: &str) -> bool {
    access == ApiAccess::Control || LISTEN_COMMANDS.contains(&ty)
}

/// Is a message of type `ty` for control access only?
pub fn control_only(ty: &str) -> bool {
    CONTROL_MESSAGES.contains(&ty)
}

/// The config as a listen-only page sees it: without plugin settings and
/// key sources, the recorder's or any system's.
pub fn redact_config(mut c: Value) -> Value {
    if let Some(o) = c.as_object_mut() {
        o.remove("plugins");
        o.remove("keys");
    }
    for list in ["systems", "conventional"] {
        for s in c[list].as_array_mut().into_iter().flatten() {
            if let Some(o) = s.as_object_mut() {
                o.remove("plugins");
                o.remove("keys");
            }
        }
    }
    c
}

/// The `hello` a listen-only connection gets.
pub fn redact_hello(mut h: Value) -> Value {
    h["config"] = redact_config(h["config"].take());
    if let Some(o) = h.as_object_mut() {
        for k in ["configPath", "keys", "pluginRuntime"] {
            o.remove(k);
        }
    }
    h
}

/// Why a command was refused.
pub fn refusal(ty: &str) -> String {
    format!("This interface may only listen, so it can't send \"{ty}\". Give it control access on the recorder's Interfaces page if it should.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listen_sees_no_secrets_and_changes_nothing() {
        let c = json!({ "plugins": { "openmhz": { "settings": { "apiKey": "k" } } }, "keys": { "a": { "file": "k.json" } },
                        "systems": [{ "shortName": "x", "plugins": { "openmhz": { "apiKey": "k" } }, "keys": "a" }],
                        "conventional": [{ "shortName": "c", "plugins": {} }], "server": { "port": 8080 } });
        let r = redact_config(c);
        assert!(!r.to_string().contains("apiKey") && !r.to_string().contains("k.json"));
        assert_eq!(r["systems"][0]["shortName"], "x");
        assert_eq!(r["server"]["port"], 8080);
        assert!(may(ApiAccess::Listen, "listen") && may(ApiAccess::Listen, "olderCalls"));
        for c in ["setConfig", "start", "stop", "quit", "installPlugin", "listDir", "unlockKeys", "installInterface"] {
            assert!(!may(ApiAccess::Listen, c) && may(ApiAccess::Control, c), "{c}");
        }
        let h = redact_hello(json!({ "type": "hello", "config": { "keys": {} }, "configPath": "/x", "history": [] }));
        assert!(h.get("configPath").is_none() && h["config"].get("keys").is_none() && h["history"].is_array());
    }
}
