//! A system's encryption keys file (P25), named by its `keys` setting and
//! kept apart from the config, which the interface reads and edits:
//!
//! ```json
//! { "keys": [
//!     { "algId": "0xAA", "keyId": "0x1B50", "key": "1234567890" },
//!     { "algId": "0x84", "keyId": "0x0002", "key": "<64 hex digits>", "talkgroups": [1201] }
//! ]}
//! ```
//!
//! - `algId`, `keyId`: hex in a string (`"0xAA"`, `"AA"`), or a number.
//! - `key`: hex, exactly as long as the cipher's key (ADP 10 digits, DES 16,
//!   3DES 48, AES-128 32, AES-256 64; spaces allowed). Never padded: a short
//!   key is an error.
//! - `talkgroups`: use this key for these talkgroups, under any key ID (or
//!   the one given). Without it, the key is used by key ID on any talkgroup.
//!
//! Keys are never logged, sent to plugins or shown in the interface, and
//! the memory that held them (the file's text, the parsed keys) is wiped.

use std::path::{Path, PathBuf};

use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};
use trunk_core::p25::crypt::Keyring;

/// Read a keys file's text.
pub fn parse(text: &str) -> Result<Keyring, String> {
    let mut v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let ring = read(&v);
    wipe(&mut v);
    ring
}

/// Overwrite the strings of a parsed keys file (the keys are among them).
fn wipe(v: &mut Value) {
    match v {
        Value::String(s) => s.zeroize(),
        Value::Array(a) => a.iter_mut().for_each(wipe),
        Value::Object(o) => o.iter_mut().for_each(|(_, v)| wipe(v)),
        _ => {}
    }
}

fn read(v: &Value) -> Result<Keyring, String> {
    let Some(list) = v.get("keys").and_then(Value::as_array) else {
        return Err("no \"keys\" list".into());
    };
    let mut ring = Keyring::default();
    for (i, k) in list.iter().enumerate() {
        let at = |e: String| format!("key {}: {e}", i + 1);
        let algid = number(k.get("algId"), "algId", 0xFF).map_err(at)?.ok_or_else(|| at("no algId".into()))?;
        let keyid = number(k.get("keyId"), "keyId", 0xFFFF).map_err(at)?;
        let key = k.get("key").and_then(Value::as_str).ok_or_else(|| at("no key".into()))?;
        let key = hex(key).map_err(at)?;
        let talkgroups = match k.get("talkgroups") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(a)) => a.iter().map(|t| t.as_u64().filter(|&t| t <= u32::MAX as u64).map(|t| t as u32)).collect::<Option<_>>().ok_or_else(|| at("talkgroups must be numbers".into()))?,
            Some(_) => return Err(at("talkgroups must be a list".into())),
        };
        ring.add(algid as u8, keyid.map(|k| k as u16), key, talkgroups).map_err(at)?;
    }
    Ok(ring)
}

/// A number, or hex in a string, up to `max`.
fn number(v: Option<&Value>, name: &str, max: u64) -> Result<Option<u64>, String> {
    let n = match v {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => {
            let s = s.trim();
            let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
            u64::from_str_radix(s, 16).ok()
        }
        Some(_) => None,
    };
    n.filter(|&n| n <= max).map(Some).ok_or_else(|| format!("{name} must be a number up to {max:#X}, or hex in a string"))
}

fn hex(s: &str) -> Result<Vec<u8>, String> {
    let digits: Zeroizing<Vec<u8>> = Zeroizing::new(s.bytes().filter(|b| !b.is_ascii_whitespace()).collect());
    if !digits.len().is_multiple_of(2) {
        return Err(format!("the key has an odd number of hex digits ({})", digits.len()));
    }
    digits
        .chunks(2)
        .map(|c| std::str::from_utf8(c).ok().and_then(|c| u8::from_str_radix(c, 16).ok()).ok_or_else(|| "the key isn't hex".to_string()))
        .collect()
}

/// Where a `keys` setting points: absolute, or relative to the config file's folder.
pub fn path(setting: &str, config_path: &Path) -> Option<PathBuf> {
    let f = setting.trim();
    if f.is_empty() {
        return None;
    }
    let p = PathBuf::from(f);
    Some(if p.is_absolute() { p } else { config_path.parent().unwrap_or(Path::new(".")).join(p) })
}

/// Read the keys file at `p`.
pub fn load(p: &Path) -> Result<Keyring, String> {
    let text = Zeroizing::new(std::fs::read_to_string(p).map_err(|e| format!("Keys file {}: {e}", p.display()))?);
    parse(&text).map_err(|e| format!("Keys file {}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_core::p25::crypt::{Algorithm, Lookup};

    #[test]
    fn reads_keys_and_says_what_is_wrong() {
        let r = parse(
            r#"{ "keys": [
                { "algId": "0xAA", "keyId": "0x1B50", "key": "12 34 56 78 90" },
                { "algId": 132, "keyId": 2, "key": "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", "talkgroups": [1201] }
            ]}"#,
        )
        .unwrap();
        assert_eq!(r.lookup(0xAA, 0x1B50, None), Lookup::Key(Algorithm::Adp, &[0x12, 0x34, 0x56, 0x78, 0x90]));
        assert!(matches!(r.lookup(0x84, 2, Some(1201)), Lookup::Key(Algorithm::Aes256, _)));
        assert_eq!(r.lookup(0x84, 2, Some(1)), Lookup::NoKey);

        let e = parse(r#"{ "keys": [{ "algId": "84", "keyId": "0x0002", "key": "0011" }] }"#).unwrap_err();
        assert!(e.starts_with("key 1: Key ID 0x0002: AES-256") && e.contains("64 hex digits"), "{e}");
        assert!(parse(r#"{ "keys": [{ "algId": "AA", "keyId": 1, "key": "12345678zz" }] }"#).unwrap_err().contains("isn't hex"));
        assert!(parse(r#"{ "keys": [{ "algId": "AA", "keyId": "10000", "key": "1234567890" }] }"#).unwrap_err().contains("keyId"));
        assert!(parse(r#"{ "keys": [{ "keyId": 1, "key": "1234567890" }] }"#).unwrap_err().contains("no algId"));
        assert!(parse(r#"{ "key": [] }"#).is_err());
    }
}
