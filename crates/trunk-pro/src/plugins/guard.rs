//! Which plugins may be given decrypted calls.
//!
//! A call decrypted with keys the operator holds is not for upload by default, anywhere: not to a
//! public service (which would publish what the system's owner encrypted), and not to a server of
//! your own either (which may be reachable from outside, or copy calls onward). Every plugin is
//! withheld from decrypted calls (their live audio and concluded files) unless the config says,
//! in that plugin's own entry (`plugins.<id>.shareDecrypted`), the sentence in [`PHRASE`]. That is
//! for the config file only: the interface never offers it. Where a plugin sends to (read from
//! its settings) is only reported, in the log, so the choice is made with it in view.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde_json::Value;

/// What a config must say, for each plugin that is to be given decrypted calls.
pub const PHRASE: &str = "I accept that decrypted calls will be sent to this plugin and may become public, and I am authorized to share them";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The config said, in the words asked for, that it may: where it sends to, for the log.
    Acknowledged(String),
    /// Not given decrypted calls: why.
    Withheld(String),
}

impl Verdict {
    pub fn allowed(&self) -> bool {
        matches!(self, Verdict::Acknowledged(_))
    }
}

/// Whether plugin `id` (a plugin's id, not a copy's label) may be given decrypted calls: `ack` is its
/// `shareDecrypted`; its settings (recorder-wide `global`, and each system's) say where it sends.
pub fn judge(id: &str, ack: &str, global: &Value, per_system: &BTreeMap<String, Value>) -> Verdict {
    let mut hosts = destinations(global);
    for v in per_system.values() {
        hosts.extend(destinations(v));
    }
    hosts.sort();
    hosts.dedup();
    let sends_to = if hosts.is_empty() {
        "somewhere its settings don't say".to_string()
    } else {
        hosts.iter().map(|h| format!("{h} ({})", if is_local_host(h) { "on your network" } else { "OUTSIDE your network" })).collect::<Vec<_>>().join(", ")
    };
    match ack.trim() {
        p if p == PHRASE => Verdict::Acknowledged(sends_to),
        "" => Verdict::Withheld(format!("it sends to {sends_to}, and plugins.{id}.shareDecrypted doesn't allow it")),
        _ => Verdict::Withheld(format!("plugins.{id}.shareDecrypted is not the exact sentence asked for (it sends to {sends_to})")),
    }
}

/// Whether a call's JSON says it was decrypted with our keys. A JSON that can't be read at all is
/// taken as decrypted: it is withheld, never released on a classification that isn't certain.
pub fn call_json_decrypted(json: &str) -> bool {
    match serde_json::from_str::<Value>(json) {
        // Only a record with no `decrypted` at all is a clear one (what the recorder writes when
        // no key was named). A null, a flag of another kind, or JSON that isn't a record is withheld.
        Ok(Value::Object(m)) => match m.get("decrypted") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_i64() != Some(0),
            Some(_) => true,
        },
        _ => true,
    }
}

/// The hosts a plugin's settings name: the host of every URL in them, and the value of keys that
/// are plainly an address (`server`, `url`, `host`, `address`, `endpoint`, `ip`, `hostname`).
pub fn destinations(v: &Value) -> Vec<String> {
    fn walk(v: &Value, key: &str, out: &mut Vec<String>) {
        match v {
            Value::String(s) => {
                let s = s.trim();
                if s.is_empty() {
                    return;
                }
                if let Some(h) = url_host(s) {
                    out.push(h);
                } else if matches!(key.to_ascii_lowercase().as_str(), "server" | "url" | "host" | "address" | "endpoint" | "ip" | "hostname") {
                    out.push(strip_port(s).to_string());
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, key, out)),
            Value::Object(m) => m.iter().for_each(|(k, x)| walk(x, k, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out
}

/// `scheme://user@host:port/path` → host.
fn url_host(s: &str) -> Option<String> {
    let rest = s.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = strip_port(authority);
    (!host.is_empty()).then(|| host.to_string())
}

fn strip_port(h: &str) -> &str {
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match h.rsplit_once(':') {
        Some((a, p)) if !a.contains(':') && p.chars().all(|c| c.is_ascii_digit()) => a,
        _ => h,
    }
}

/// A host that is on this network: a loopback, private or link-local address, `localhost`, a
/// name with no dot, or one under a name reserved for local use. A name that merely resolves to
/// a private address doesn't count (it isn't looked up).
pub fn is_local_host(h: &str) -> bool {
    let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        return match ip.to_canonical() {
            IpAddr::V4(a) => a.is_loopback() || a.is_private() || a.is_link_local(),
            IpAddr::V6(a) => a.is_loopback() || a.is_unique_local() || a.is_unicast_link_local(),
        };
    }
    !h.contains('.') || ["local", "localhost", "lan", "internal", "home.arpa", "localdomain"].iter().any(|s| h.ends_with(&format!(".{s}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_hosts_are_this_network_only() {
        for h in ["localhost", "127.0.0.1", "::1", "10.1.2.3", "192.168.1.5", "172.16.0.9", "172.31.255.1", "169.254.1.1", "fd00::1", "scanner", "openmhz.lan", "pi.local", "a.b.internal", "x.home.arpa", "::ffff:192.168.1.5"] {
            assert!(is_local_host(h), "{h}");
        }
        for h in ["api.openmhz.com", "8.8.8.8", "172.32.0.1", "192.169.1.1", "2001:db8::1", "openmhz.example.com", "lan.example.com", "", "notlocal"] {
            // ("notlocal" has no dot, so it is a bare name: local.)
            if h == "notlocal" {
                assert!(is_local_host(h));
            } else {
                assert!(!is_local_host(h), "{h}");
            }
        }
    }

    #[test]
    fn destinations_are_found_in_urls_and_address_keys() {
        let v = json!({ "server": "https://api.openmhz.com", "apiKey": "abc", "nested": { "url": "http://user:pw@192.168.1.5:3000/api/call-upload", "note": "see http://x" }, "host": "pi.local:8080" });
        let mut d = destinations(&v);
        d.sort();
        assert_eq!(d, ["192.168.1.5", "api.openmhz.com", "pi.local", "x"]);
        assert!(destinations(&json!({ "apiKey": "k", "systemId": 7, "server": "" })).is_empty());
        assert_eq!(destinations(&json!({ "server": "[::1]:8080" })), ["::1"]);
    }

    #[test]
    fn no_plugin_is_given_decrypted_calls_without_the_sentence() {
        // Not a LAN one, not a public one, not one that names nowhere.
        for settings in [json!({ "server": "http://192.168.1.5:3000" }), json!({ "server": "https://api.openmhz.com" }), json!({})] {
            let v = judge("p", "", &settings, &BTreeMap::new());
            assert!(!v.allowed(), "{settings}: {v:?}");
        }
        // It says where the plugin sends to, for the one deciding.
        let v = judge("p", "", &json!({ "server": "https://api.openmhz.com" }), &BTreeMap::new());
        assert!(matches!(&v, Verdict::Withheld(w) if w.contains("api.openmhz.com (OUTSIDE your network)")), "{v:?}");
        let v = judge("p", "", &json!({ "server": "http://192.168.1.5:3000" }), &BTreeMap::new());
        assert!(matches!(&v, Verdict::Withheld(w) if w.contains("192.168.1.5 (on your network)")), "{v:?}");
    }

    #[test]
    fn a_call_json_that_cannot_be_read_counts_as_decrypted() {
        assert!(!call_json_decrypted(r#"{"talkgroup":1}"#));
        assert!(!call_json_decrypted(r#"{"decrypted":0}"#));
        assert!(call_json_decrypted(r#"{"decrypted":1}"#));
        assert!(call_json_decrypted(r#"{"decrypted":true}"#));
        // Truncated, not JSON, or a flag of a strange kind: withheld.
        assert!(call_json_decrypted(r#"{"talkgroup":1,"decrypt"#));
        assert!(call_json_decrypted(""));
        assert!(call_json_decrypted(r#"{"decrypted":"yes"}"#));
        // Valid JSON that isn't a record with a proper flag: withheld too.
        assert!(call_json_decrypted(r#"{"decrypted":null}"#));
        assert!(call_json_decrypted("[]"));
        assert!(call_json_decrypted("null"));
        assert!(call_json_decrypted(r#""decrypted""#));
        assert!(call_json_decrypted("42"));
    }

    #[test]
    fn only_the_exact_sentence_lets_a_plugin_through() {
        let public = json!({ "server": "https://api.openmhz.com" });
        let v = judge("openmhz", PHRASE, &public, &BTreeMap::new());
        assert!(matches!(&v, Verdict::Acknowledged(w) if w.contains("OUTSIDE")), "{v:?}");
        // The same for a plugin on the LAN: it is no exception.
        assert!(judge("openmhz", PHRASE, &json!({ "server": "http://10.0.0.5" }), &BTreeMap::new()).allowed());
        // (Spaces around it don't matter.)
        assert!(judge("openmhz", &format!("  {PHRASE}\n"), &public, &BTreeMap::new()).allowed());
        for wrong in ["yes", "true", "I understand decrypted calls will leave my own network", &PHRASE.to_ascii_uppercase(), ""] {
            assert!(!judge("openmhz", wrong, &public, &BTreeMap::new()).allowed(), "{wrong:?}");
        }
    }
}
