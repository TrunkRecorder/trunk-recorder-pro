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


/// Which port a request came in on.
#[derive(Clone, Debug)]
pub enum Door {
    /// The built-in interface's.
    Builtin,
    /// Interface `id`'s; `stop` turns true when it's no longer served there.
    Interface { id: String, stop: tokio::sync::watch::Receiver<bool> },
}

// Which commands a listen-only page may send, and which messages only
// control access hears, are the protocol's to say:
// [`trunk_app::protocol::Command::listen_may_send`], [`trunk_app::protocol::ToClient::audience`].

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
        let parse = |t: &str| trunk_app::protocol::Command::parse(t).unwrap();
        assert!(parse(r#"{"type":"listen","on":true}"#).listen_may_send() && parse(r#"{"type":"olderCalls"}"#).listen_may_send());
        for c in [r#"{"type":"setConfig","config":{},"version":1}"#, r#"{"type":"start"}"#, r#"{"type":"quit"}"#, r#"{"type":"installPlugin","id":"x"}"#, r#"{"type":"listDir"}"#, r#"{"type":"unlockKeys","source":"a","password":"b"}"#] {
            assert!(!parse(c).listen_may_send(), "{c}");
        }
    }
}
