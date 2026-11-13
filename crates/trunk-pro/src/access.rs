//! What a page may do with the recorder's API, by the port it came in on
//! and, with signing in on, the user it signed in as.
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
//!   settings, key sources (API keys, passwords' files) and the users;
//!   plugin and interface management messages aren't sent to it.
//! - **control**: what the built-in interface can do.
//!
//! Signing in (`server.auth`) is off unless enabled. On, a request also
//! gets no more than its user's role allows ([`Caller::access`]): a user
//! signs in with their access token, whose secret comes as `Authorization:
//! Bearer` or `?token=` (programs), or by way of the session cookie
//! `/api/login` sets (browsers: [`Sessions`]), and without one it gets
//! `server.auth.anonymous` (none: it must sign in first). A secret is 32
//! random bytes; the config keeps only its SHA-256.

use std::path::Path;

use axum::http::{header, HeaderMap};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};
use trunk_app::config::{Auth, AuthToken, AuthUser};
use trunk_app::protocol::Secret;

use crate::config::ApiAccess;
use crate::runtime::Ctx;
use crate::toolkit::{hex, percent_decode, percent_encode, sha256_hex, unhex};

pub mod cli;

/// Which port a request came in on.
#[derive(Clone, Debug)]
pub enum Door {
    /// The built-in interface's.
    Builtin,
    /// Interface `id`'s; `stop` turns true when it's no longer served there.
    Interface { id: String, stop: tokio::sync::watch::Receiver<bool> },
}

/// Who a request says it is.
#[derive(Clone, Debug, PartialEq)]
pub enum Who {
    /// Not signed in (or signing in is off).
    Anonymous,
    /// Signed in as user `name`, with the access token whose secret's hash
    /// is `hash` (as a secret sent, or a session names it; a token made
    /// again is another sign-in).
    User { name: String, hash: String },
}

/// A request (or a WebSocket, for as long as it lasts): its port and who it is.
#[derive(Clone, Debug)]
pub struct Caller {
    pub door: Door,
    pub who: Who,
}

impl Who {
    /// What its credential allows with signing in on (none: it must sign in),
    /// as the config has it now.
    pub fn grants(&self, auth: &Auth) -> Option<ApiAccess> {
        match self {
            Who::Anonymous => auth.anonymous,
            Who::User { name, hash } => auth.users.get(name).filter(|u| u.token.as_ref().is_some_and(|t| t.hash == *hash)).map(|u| u.access),
        }
    }

    /// The user's name, if it signed in.
    pub fn user(&self) -> Option<&str> {
        match self {
            Who::Anonymous => None,
            Who::User { name, .. } => Some(name),
        }
    }
}

impl Caller {
    /// What it may do now, by the config as it is now: its port's access
    /// (None once its interface is gone or off) and, with signing in on, no
    /// more than its credential's.
    pub fn access(&self, ctx: &Ctx) -> Option<ApiAccess> {
        let cfg = ctx.config.lock().unwrap();
        let door = match &self.door {
            Door::Builtin => Some(ApiAccess::Control),
            Door::Interface { id, .. } => ctx.interfaces.access(&cfg, id),
        };
        if !cfg.server.auth.enabled {
            return door;
        }
        Some(door?.min(self.who.grants(&cfg.server.auth)?))
    }
}

// Which commands a listen-only page may send, and which messages only
// control access hears, are the protocol's to say:
// [`trunk_app::protocol::Command::needs`], [`trunk_app::protocol::ToClient::audience`].

/// The user whose access token's secret `secret` is, by name. (The secret
/// is 256 random bits, so a plain SHA-256 needs no slowing down, and
/// comparing digests with `==` tells a guesser nothing of use: no
/// constant-time compare.)
pub fn user_of<'a>(auth: &'a Auth, secret: &str) -> Option<(&'a String, &'a AuthUser)> {
    let hash = sha256_hex(secret.trim().as_bytes());
    auth.users.iter().find(|(_, u)| u.token.as_ref().is_some_and(|t| t.hash == hash))
}

/// Who signs in with `secret`: [`Who::User`], or None.
pub fn signs_in(auth: &Auth, secret: &str) -> Option<Who> {
    let (name, u) = user_of(auth, secret)?;
    Some(Who::User { name: name.clone(), hash: u.token.as_ref()?.hash.clone() })
}

/// Who a request is: the first valid secret of its `Authorization: Bearer`
/// and its `token=` query parameter, else (`cookie_ok`) a valid session in
/// this recorder's cookie whose user still has its credential. Signing in
/// off, or none valid: anonymous.
pub fn identify(auth: &Auth, sessions: &Sessions, headers: &HeaderMap, query: Option<&str>, cookie_ok: bool) -> Who {
    if !auth.enabled {
        return Who::Anonymous;
    }
    // The scheme's case is anyone's (RFC 9110), and one space or more follows it.
    let bearer = headers.get_all(header::AUTHORIZATION).into_iter().filter_map(|v| {
        let (scheme, secret) = v.to_str().ok()?.trim().split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then(|| secret.trim_start().to_string())
    });
    let param = query.into_iter().flat_map(|q| q.split('&')).filter_map(|kv| kv.strip_prefix("token=").map(str::to_string));
    let secret = bearer.chain(param).find_map(|s| signs_in(auth, &s));
    // (A session's user removed, or their token made again since: nobody.)
    let session = || if cookie_ok { sessions.of(headers).filter(|w| w.grants(auth).is_some()) } else { None };
    secret.or_else(session).unwrap_or(Who::Anonymous)
}

/// How long a browser stays signed in: 30 days.
pub const SESSION_S: u64 = 30 * 24 * 3600;

/// The file beside the config the session key is kept in.
pub const SESSION_KEY_FILE: &str = "session-key";

/// Browsers' sessions: a browser signed in keeps a cookie saying who, with
/// what credential (an access token's hash) and until when, signed with
/// this recorder's key, which nothing else knows (it's never in the config,
/// which control pages are sent). Removing the user, or making their token
/// again, ends the session (it no longer [`Who::grants`] anything); so does
/// a new key.
pub struct Sessions {
    key: [u8; 32],
    /// The cookie's name: this key's, so recorders on one host name (other
    /// ports: cookies don't keep to one) don't take each other's.
    cookie: String,
}

impl Sessions {
    /// The recorder's: its key kept in [`SESSION_KEY_FILE`] in `dir`, made the
    /// first time (readable by its owner alone). One that can't be read or
    /// kept is made for this run alone: signing out every browser when it ends.
    pub fn load(dir: &Path) -> Sessions {
        let path = dir.join(SESSION_KEY_FILE);
        match std::fs::read_to_string(&path) {
            Ok(s) => match unhex(s.trim()).and_then(|b| <[u8; 32]>::try_from(b).ok()) {
                Some(key) => return Sessions::with_key(key),
                None => log::warn!("{} isn't a session key: making another (browsers sign in again)", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                log::warn!("Couldn't read the session key {} ({e}): browsers that sign in are signed out when the recorder stops", path.display());
                return Sessions::fresh();
            }
        }
        let s = Sessions::fresh();
        if let Err(e) = keep_private(&path, &hex(&s.key)) {
            log::warn!("Couldn't keep the session key in {} ({e}): browsers that sign in are signed out when the recorder stops", path.display());
        }
        s
    }

    /// One with a new key, kept nowhere.
    pub fn fresh() -> Sessions {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("No random numbers to make the session key with");
        Sessions::with_key(key)
    }

    fn with_key(key: [u8; 32]) -> Sessions {
        Sessions { cookie: format!("trp_{}", &hex(&Sha256::digest(key))[..8]), key }
    }

    /// A session for `user` signed in with `credential`, for [`SESSION_S`]
    /// from now: the cookie's value, `<user>.<credential>.<expires, Unix
    /// s>.<HMAC-SHA256 of the rest, hex>` (user and credential
    /// percent-encoded: no dots in them).
    pub fn issue(&self, user: &str, credential: &str) -> String {
        self.issue_until(user, credential, crate::clock::unix_s() as u64 + SESSION_S)
    }

    fn issue_until(&self, user: &str, credential: &str, expires: u64) -> String {
        let body = format!("{}.{}.{expires}", percent_encode(user), percent_encode(credential));
        let mac = hex(&self.mac(&body).finalize().into_bytes());
        format!("{body}.{mac}")
    }

    /// Who session `value` says, if this key signed it and it hasn't expired.
    pub fn verify(&self, value: &str) -> Option<Who> {
        self.verify_at(value, crate::clock::unix_s() as u64)
    }

    fn verify_at(&self, value: &str, now: u64) -> Option<Who> {
        let (body, mac) = value.rsplit_once('.')?;
        self.mac(body).verify_slice(&unhex(mac)?).ok()?;
        let mut parts = body.split('.');
        let (user, credential, expires) = (parts.next()?, parts.next()?, parts.next()?.parse::<u64>().ok()?);
        (parts.next().is_none() && now < expires).then_some(())?;
        Some(Who::User { name: percent_decode(user)?, hash: percent_decode(credential)? })
    }

    fn mac(&self, body: &str) -> Hmac<Sha256> {
        let mut m = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC takes a key of any length");
        m.update(body.as_bytes());
        m
    }

    /// Who a request's cookie of this recorder's says (the first valid, if it carries several).
    fn of(&self, headers: &HeaderMap) -> Option<Who> {
        let all = headers.get_all(header::COOKIE).into_iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';'));
        all.filter_map(|c| c.trim().split_once('=')).filter(|(n, _)| *n == self.cookie).find_map(|(_, v)| self.verify(v.trim_matches('"')))
    }

    /// The `Set-Cookie` signing a browser in as `user` with `credential`
    /// ([`Sessions::issue`]); `secure` when it came over https.
    pub fn set_cookie(&self, user: &str, credential: &str, secure: bool) -> String {
        format!("{}={}; Path=/; Max-Age={SESSION_S}; HttpOnly; SameSite=Lax{}", self.cookie, self.issue(user, credential), if secure { "; Secure" } else { "" })
    }

    /// The `Set-Cookie` signing a browser out (of this recorder alone).
    pub fn clear_cookie(&self) -> String {
        format!("{}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax", self.cookie)
    }
}

/// Write `text` to `path`, readable by its owner alone (on unix).
fn keep_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        o.mode(0o600);
        // (A file there already keeps its mode: made private too.)
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    o.open(path)?.write_all(text.as_bytes())
}

/// Make user `name` (trimmed) with `access` in `auth`, and an access token
/// for them: its secret, which is had this once, or why not (a bad name, or
/// one taken).
pub fn add_user(auth: &mut Auth, name: &str, access: ApiAccess) -> Result<Secret, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err("A name is 1 to 64 characters (no control characters).".into());
    }
    if auth.users.contains_key(name) {
        return Err(format!("There's a user named \"{name}\" already."));
    }
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| format!("No random numbers to make a secret with: {e}"))?;
    let secret = format!("trp_{}", hex(&b));
    let token = AuthToken { hash: sha256_hex(secret.as_bytes()), created: crate::clock::unix_s() as i64 };
    auth.users.insert(name.into(), AuthUser { access, token: Some(token) });
    Ok(Secret::new(secret))
}

/// The config as a listen-only page sees it: without plugin settings and
/// key sources, the recorder's or any system's, the database's connection
/// string (it can hold a password), or the users.
pub fn redact_config(mut c: Value) -> Value {
    if let Some(o) = c.as_object_mut() {
        o.remove("plugins");
        o.remove("keys");
    }
    if let Some(o) = c["storage"].as_object_mut() {
        o.remove("url");
    }
    if let Some(o) = c["server"].as_object_mut() {
        o.remove("auth");
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

/// Why `caller` (with listen access) can't send a `ty` command: its interface's access, or its user's.
pub fn refusal(ctx: &Ctx, caller: &Caller, ty: &str) -> String {
    let port_listens = match &caller.door {
        Door::Interface { id, .. } => ctx.interfaces.access(&ctx.config.lock().unwrap(), id) == Some(ApiAccess::Listen),
        Door::Builtin => false,
    };
    match &caller.who {
        _ if port_listens => format!("This interface may only listen, so it can't send \"{ty}\". Give it control access on the recorder's Interfaces page if it should."),
        Who::User { name, .. } => format!("The user \"{name}\" may only listen, so they can't send \"{ty}\". Give them control access if they should."),
        Who::Anonymous => format!("Not signed in, this page may only listen, so it can't send \"{ty}\". Sign in as a user with control access."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;

    #[test]
    fn listen_sees_no_secrets_and_changes_nothing() {
        let c = json!({ "plugins": { "openmhz": { "settings": { "apiKey": "k" } } }, "keys": { "a": { "file": "k.json" } },
                        "systems": [{ "shortName": "x", "plugins": { "openmhz": { "apiKey": "k" } }, "keys": "a" }],
                        "conventional": [{ "shortName": "c", "plugins": {} }], "server": { "port": 8080, "auth": { "enabled": true, "users": { "luke": { "access": "control", "token": { "hash": "hhh", "created": 1 } } } } },
                        "storage": { "backend": "postgres", "url": "postgres://trp:secret@db/trunk" } });
        let r = redact_config(c);
        assert!(!r.to_string().contains("apiKey") && !r.to_string().contains("k.json") && !r.to_string().contains("secret") && !r.to_string().contains("hhh") && r["server"].get("auth").is_none());
        assert_eq!(r["storage"]["backend"], "postgres", "which backend is no secret");
        assert_eq!(r["systems"][0]["shortName"], "x");
        assert_eq!(r["server"]["port"], 8080);
        let parse = |t: &str| trunk_app::protocol::Command::parse(t).unwrap();
        assert!(parse(r#"{"type":"listen","on":true}"#).needs() == ApiAccess::Listen && parse(r#"{"type":"callsQuery","id":1}"#).needs() == ApiAccess::Listen);
        for c in [r#"{"type":"setConfig","config":{},"version":1}"#, r#"{"type":"start"}"#, r#"{"type":"quit"}"#, r#"{"type":"install","kind":"plugin","id":"x"}"#, r#"{"type":"store","kind":"interface"}"#, r#"{"type":"listDir"}"#, r#"{"type":"unlockKeys","source":"a","password":"b"}"#, r#"{"type":"createToken","id":1,"name":"a","access":"listen"}"#] {
            assert_eq!(parse(c).needs(), ApiAccess::Control, "{c}");
        }
    }

    /// A secret is found by its hash wherever it comes: the Authorization
    /// header, `?token=`, a cookie (only when cookies count); anything else,
    /// or signing in off, is anonymous.
    #[test]
    fn a_request_is_who_its_secret_says() {
        let mut auth = Auth { enabled: true, ..Default::default() };
        let secret = add_user(&mut auth, " luke ", ApiAccess::Control).unwrap();
        let s = secret.expose();
        assert!(s.starts_with("trp_") && s.len() == 4 + 64, "{s}");
        let luke = Who::User { name: "luke".into(), hash: sha256_hex(s.as_bytes()) };
        let hash = auth.users["luke"].token.as_ref().unwrap().hash.clone();
        assert_eq!(hash, sha256_hex(s.as_bytes()), "the config keeps its hash, by its trimmed name");
        let with = |name: header::HeaderName, v: String| {
            let mut h = HeaderMap::new();
            h.insert(name, HeaderValue::from_str(&v).unwrap());
            h
        };
        let none = HeaderMap::new();
        let sessions = Sessions::fresh();
        assert_eq!(identify(&auth, &sessions, &with(header::AUTHORIZATION, format!("Bearer {s}")), None, false), luke);
        for v in [format!("bearer {s}"), format!("BEARER   {s}")] {
            assert_eq!(identify(&auth, &sessions, &with(header::AUTHORIZATION, v.clone()), None, false), luke, "{v}: any case, any spaces");
        }
        assert_eq!(identify(&auth, &sessions, &with(header::AUTHORIZATION, format!("Basic {s}")), None, false), Who::Anonymous, "another scheme");
        assert_eq!(identify(&auth, &sessions, &none, Some(&format!("a=1&token={s}")), false), luke);
        let cookie = with(header::COOKIE, format!("other=1; trp_00000000=nope; {}={}", sessions.cookie, sessions.issue("luke", &hash)));
        assert_eq!(identify(&auth, &sessions, &cookie, None, true), luke, "a session in this recorder's cookie");
        assert_eq!(identify(&auth, &sessions, &with(header::COOKIE, format!("{}={s}", sessions.cookie)), None, true), Who::Anonymous, "a secret is no session");
        assert_eq!(identify(&auth, &sessions, &cookie, None, false), Who::Anonymous, "a cookie from another site's page doesn't count");
        assert_eq!(identify(&auth, &sessions, &with(header::AUTHORIZATION, "Bearer trp_0123".into()), Some("token=trp_4567"), true), Who::Anonymous, "unknown secrets");
        assert_eq!(identify(&Auth { enabled: false, ..auth.clone() }, &sessions, &with(header::AUTHORIZATION, format!("Bearer {s}")), None, true), Who::Anonymous, "signing in off");
        assert!(add_user(&mut auth, "luke", ApiAccess::Listen).unwrap_err().contains("already"));
        for bad in ["", "  ", "a\nb", &"x".repeat(65)] {
            assert!(add_user(&mut auth, bad, ApiAccess::Listen).is_err(), "{bad:?}");
        }
        // A user with no token signs in with nothing.
        auth.users.get_mut("luke").unwrap().token = None;
        assert_eq!(identify(&auth, &sessions, &with(header::AUTHORIZATION, format!("Bearer {s}")), None, false), Who::Anonymous, "no token");
        assert_eq!(luke.grants(&auth), None);
    }

    /// Signing in off, a request may do what its port allows; on, no more
    /// than its user's role (or anonymous) allows either, by the config as it
    /// is now: a user removed, or made again under their name, gives nothing.
    #[test]
    fn access_is_the_lower_of_the_port_and_the_user() {
        let dir = crate::db::TestDir::new("access-caller");
        let ctx = crate::runtime::test_ctx(&dir.join("access-caller-cap"));
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let wall = Door::Interface { id: "wall".into(), stop };
        let as_ = |door: &Door, who: &Who| Caller { door: door.clone(), who: who.clone() }.access(&ctx);
        ctx.config.lock().unwrap().server.interfaces.insert("wall".into(), trunk_app::config::Interface { port: 8081, ..Default::default() });
        let (control, listen) = {
            let mut c = ctx.config.lock().unwrap();
            let auth = &mut c.server.auth;
            (add_user(auth, "luke", ApiAccess::Control).unwrap(), add_user(auth, "kiosk", ApiAccess::Listen).unwrap())
        };
        let who = |s: &Secret| signs_in(&ctx.config.lock().unwrap().server.auth, s.expose()).unwrap();
        let (luke, kiosk) = (who(&control), who(&listen));
        for w in [&Who::Anonymous, &luke, &kiosk] {
            assert_eq!((as_(&Door::Builtin, w), as_(&wall, w)), (Some(ApiAccess::Control), Some(ApiAccess::Listen)), "off: the port's, whoever");
        }
        ctx.config.lock().unwrap().server.auth.enabled = true;
        assert_eq!((as_(&Door::Builtin, &Who::Anonymous), as_(&wall, &Who::Anonymous)), (None, None));
        assert_eq!((as_(&Door::Builtin, &luke), as_(&wall, &luke)), (Some(ApiAccess::Control), Some(ApiAccess::Listen)), "a listen interface stays listen");
        assert_eq!((as_(&Door::Builtin, &kiosk), as_(&wall, &kiosk)), (Some(ApiAccess::Listen), Some(ApiAccess::Listen)));
        ctx.config.lock().unwrap().server.auth.anonymous = Some(ApiAccess::Listen);
        assert_eq!(as_(&Door::Builtin, &Who::Anonymous), Some(ApiAccess::Listen));
        assert!(refusal(&ctx, &Caller { door: Door::Builtin, who: kiosk.clone() }, "start").contains("\"kiosk\""));
        assert!(refusal(&ctx, &Caller { door: wall.clone(), who: luke.clone() }, "start").contains("This interface"));
        // Removed, then made again under their name: the old secret is nobody.
        ctx.config.lock().unwrap().server.auth.users.remove("luke");
        assert_eq!(as_(&Door::Builtin, &luke), None);
        add_user(&mut ctx.config.lock().unwrap().server.auth, "luke", ApiAccess::Control).unwrap();
        assert_eq!(as_(&Door::Builtin, &luke), None, "the new luke has another token");
    }

    /// A session is who it says only as this key signed it, until it
    /// expires, and while the user still has the credential it names; the
    /// key is kept beside the config, for its owner alone, and read back;
    /// signing out clears this recorder's cookie alone.
    #[test]
    fn a_session_is_signed_and_ends() {
        let sessions = Sessions::fresh();
        let now = crate::clock::unix_s() as u64;
        let mut auth = Auth { enabled: true, ..Default::default() };
        add_user(&mut auth, "luke.b ÿ", ApiAccess::Control).unwrap();
        let hash = auth.users["luke.b ÿ"].token.as_ref().unwrap().hash.clone();
        let luke = Who::User { name: "luke.b ÿ".into(), hash: hash.clone() };
        let v = sessions.issue("luke.b ÿ", &hash);
        assert!(v.starts_with("luke%2Eb%20%C3%BF.") && v.split('.').count() == 4, "{v}");
        assert_eq!(sessions.verify(&v), Some(luke.clone()), "dots and more in a name");
        assert_eq!(luke.grants(&auth), Some(ApiAccess::Control));
        let parts: Vec<&str> = v.split('.').collect();
        let with = |i: usize, p: &str| parts.iter().enumerate().map(|(j, x)| if j == i { p } else { *x }).collect::<Vec<_>>().join(".");
        let expires = parts[2].parse::<u64>().unwrap();
        assert!(expires.abs_diff(now + SESSION_S) <= 2, "30 days");
        for (bad, why) in [(with(0, "kiosk"), "another user"), (with(1, &"0".repeat(64)), "another credential"), (with(2, &(expires + 1).to_string()), "a later expiry"),
                           (with(3, &"0".repeat(64)), "another mac"), (with(3, "zz"), "not hex"), (parts[..3].join("."), "no mac"), (format!("x.{v}"), "a part more"), (String::new(), "nothing")] {
            assert_eq!(sessions.verify(&bad), None, "{why}");
        }
        assert_eq!(sessions.verify_at(&v, expires), None, "expired");
        assert_eq!(sessions.verify_at(&sessions.issue_until("luke.b ÿ", &hash, now - 1), now), None, "expired");
        assert_eq!(Sessions::fresh().verify(&v), None, "another key's");
        // Its token made again: the session names a credential it no longer has.
        auth.users.get_mut("luke.b ÿ").unwrap().token = Some(AuthToken { hash: sha256_hex(b"another"), created: 2 });
        assert_eq!(sessions.verify(&v).unwrap().grants(&auth), None, "remade: signed out");
        // The key file: made once, private, read back (the same cookie name); another folder, another recorder.
        let (dir, elsewhere) = (crate::db::TestDir::new("session-key"), crate::db::TestDir::new("session-key-2"));
        let (a, b, other) = (Sessions::load(&dir), Sessions::load(&dir), Sessions::load(&elsewhere));
        assert_eq!((a.key, &a.cookie), (b.key, &b.cookie));
        assert!(a.cookie != other.cookie && a.cookie.starts_with("trp_") && a.cookie.len() == 12, "{}", a.cookie);
        assert_eq!(b.verify(&a.issue("luke", "h")), Some(Who::User { name: "luke".into(), hash: "h".into() }), "a restart keeps sessions");
        let kept = std::fs::read_to_string(dir.join(SESSION_KEY_FILE)).unwrap();
        assert_eq!(kept, hex(&a.key), "the key, in hex");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(SESSION_KEY_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(dir.join(SESSION_KEY_FILE), "nope").unwrap();
        assert!(Sessions::load(&dir).key != a.key, "not a key: another made");
        let set = a.set_cookie("luke", "h", true);
        assert!(set.starts_with(&format!("{}=luke.h.", a.cookie)) && set.contains("; Path=/; Max-Age=2592000; HttpOnly; SameSite=Lax; Secure"), "{set}");
        assert_eq!(a.clear_cookie(), format!("{}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax", a.cookie));
    }
}
