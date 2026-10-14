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
//! A KFDtool key container (`.ekc`) can be named instead, with the file
//! holding its password in `keysPasswordFile` (see [`password`]): the keys
//! stay encrypted on disk, and the password sits apart from them. Every
//! voice key in it is used by key ID on any talkgroup; keys for algorithms
//! that can't be decrypted (key-encryption keys, DES-XL) are listed in the
//! log and skipped.
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

/// Read the password file `p` names: its first line, with a byte order mark
/// and the line ending dropped. On Unix, a file others can read is refused
/// (the password is as secret as the keys), except under `/run/secrets`,
/// where a container runtime sets the modes.
pub fn password(p: &Path) -> Result<zeroize::Zeroizing<String>, String> {
    let at = |e: String| format!("Keys password file {}: {e}", p.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(p).map_err(|e| at(e.to_string()))?.permissions().mode();
        if mode & 0o077 != 0 && !p.starts_with("/run/secrets") {
            return Err(at(format!("can be read by other users (mode {:03o}); chmod 600 it", mode & 0o777)));
        }
    }
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(p).map_err(|e| at(e.to_string()))?);
    let line = text.strip_prefix('\u{feff}').unwrap_or(&text).lines().next().unwrap_or("");
    if line.is_empty() {
        return Err(at("empty".into()));
    }
    Ok(zeroize::Zeroizing::new(line.to_string()))
}

/// What reading a keys file found.
pub struct Loaded {
    pub ring: Keyring,
    /// Container keys left out, as "name (reason)" (never a key).
    pub skipped: Vec<String>,
}

/// Read the keys file at `p`: JSON, or a KFDtool container (anything that
/// doesn't start with `{`), whose password is in the file `password_file` names.
pub fn load(p: &Path, password_file: Option<&Path>) -> Result<Loaded, String> {
    let at = |e: String| format!("Keys file {}: {e}", p.display());
    let bytes = zeroize::Zeroizing::new(std::fs::read(p).map_err(|e| at(e.to_string()))?);
    let json = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes).iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{');
    if json {
        if password_file.is_some() {
            return Err(at("is a JSON keys file, which takes no keysPasswordFile".into()));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| at("not UTF-8".into()))?;
        return parse(text.strip_prefix('\u{feff}').unwrap_or(text)).map(|ring| Loaded { ring, skipped: vec![] }).map_err(at);
    }
    let Some(pw) = password_file else {
        return Err(at("is a key container, which needs keysPasswordFile (a file holding its password)".into()));
    };
    let pw = password(pw)?;
    let c = crate::ekc::parse(&bytes, &pw).map_err(|e| at(e.to_string()))?;
    let mut ring = Keyring::default();
    let mut skipped: Vec<String> = c.skipped.iter().map(|s| format!("{} ({})", s.name, s.reason)).collect();
    for k in &c.keys {
        // An algorithm that can't be decrypted (DES-XL, say) isn't an error: the radios' other keys are still wanted.
        if let Err(e) = ring.add(k.algid, Some(k.kid), k.bytes.to_vec(), vec![]) {
            skipped.push(format!("{} ({e})", k.name));
        }
    }
    Ok(Loaded { ring, skipped })
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

    const CONTAINER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata/dummy-test-keys.ekc");

    fn scratch(name: &str, body: &str, mode: u32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trunk-keys-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("pw");
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        p
    }

    #[test]
    fn reads_a_key_container_with_its_password_file() {
        let pw = scratch("ok", "\u{feff}testing1234\r\nnot the password\n", 0o600);
        let l = load(Path::new(CONTAINER), Some(&pw)).unwrap();
        // ADP, DES-OFB and AES-256 are loaded; DES-XL can't be decrypted, so it is listed and left out.
        assert!(matches!(l.ring.lookup(0xAA, 1234, None), Lookup::Key(Algorithm::Adp, k) if k.len() == 5));
        assert!(matches!(l.ring.lookup(0x81, 1237, None), Lookup::Key(Algorithm::DesOfb, _)));
        assert!(matches!(l.ring.lookup(0x84, 1235, None), Lookup::Key(Algorithm::Aes256, _)));
        assert_eq!(l.ring.keys().len(), 3);
        assert_eq!(l.skipped.len(), 1);
        assert!(l.skipped[0].starts_with("DES-XL_Key"), "{:?}", l.skipped);
    }

    #[test]
    fn a_container_without_or_with_the_wrong_password_is_an_error() {
        let wrong = scratch("wrong", "testing1235\n", 0o600);
        let e = load(Path::new(CONTAINER), Some(&wrong)).err().unwrap();
        assert!(e.contains("wrong password"), "{e}");
        assert!(!e.contains("testing"), "the password is never in an error: {e}");
        let e = load(Path::new(CONTAINER), None).err().unwrap();
        assert!(e.contains("keysPasswordFile"), "{e}");
        let empty = scratch("empty", "\n", 0o600);
        assert!(load(Path::new(CONTAINER), Some(&empty)).err().unwrap().contains("empty"));
    }

    #[test]
    fn a_json_file_takes_no_password_file() {
        let dir = std::env::temp_dir().join(format!("trunk-keys-test-{}-json", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("keys.json");
        std::fs::write(&f, r#"{ "keys": [{ "algId": "AA", "keyId": 1, "key": "1234567890" }] }"#).unwrap();
        assert_eq!(load(&f, None).unwrap().ring.keys().len(), 1);
        let pw = scratch("json", "x\n", 0o600);
        assert!(load(&f, Some(&pw)).err().unwrap().contains("takes no keysPasswordFile"));
    }

    #[cfg(unix)]
    #[test]
    fn a_password_file_others_can_read_is_refused() {
        let pw = scratch("open", "testing1234\n", 0o644);
        let e = load(Path::new(CONTAINER), Some(&pw)).err().unwrap();
        assert!(e.contains("chmod 600"), "{e}");
    }
}
