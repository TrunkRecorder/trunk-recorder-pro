//! Passkeys: a user signs in with a device of theirs (a phone, a computer, a
//! password manager), nothing typed — WebAuthn, its checks webauthn-rs's.
//!
//! A user signed in (with their token, or a passkey) adds one for
//! themselves: `POST /api/passkey/register/start` and `…/finish`. Anyone
//! signs in with one: `POST /api/passkey/login/start` and `…/finish`, no
//! name asked (the passkey says whose it is: discoverable credentials), and
//! is given the session a token's sign-in is, its credential the passkey's
//! id ([`super::passkey_credential`]). Removing it from the config ends those
//! sessions.
//!
//! Browsers allow passkeys on `localhost`, or over HTTPS on a host name
//! (never an IP address): `server.hostname`. The relying party is whichever
//! of the two a page is on ([`Rp::of`]), and the one origin it takes answers
//! from is the dashboard's, from what the recorder knows (its port, HTTPS or
//! not, `server.hostname`), never what a request says: an interface's page on
//! another port of the same name can't use a passkey here. A passkey made on
//! one isn't offered on the other.
//!
//! Between start and finish, a sign-in waits with the browser: its state,
//! signed with a key of the session key's ([`super::Sessions::seal`]), for 5
//! minutes, and taken once (those that signed in are kept until then), so
//! asking to start again and again (signing in needs nobody to be signed
//! in) keeps nothing. Adding one waits in memory ([`Ceremonies`]), 2 at most
//! a user.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use trunk_app::config::{Auth, AuthPasskey, Server};
use webauthn_rs::prelude::{AuthenticationResult, DiscoverableAuthentication, Passkey, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder, WebauthnError};

use super::Sessions;
use crate::toolkit::hex;

/// How long a ceremony waits for its finish.
pub const CEREMONY: Duration = Duration::from_secs(300);

/// How many adds of one user's may wait at once (the oldest of theirs goes for a new one).
const MAX_ADDS: usize = 2;

/// The longest name a passkey is given, in characters.
const MAX_NAME: usize = 40;

/// What a sign-in waiting is signed for ([`Sessions::seal`]).
const SIGN_IN: &str = "passkey sign-in";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok()
}

/// Where a page's passkeys are: its relying party, and the one origin answers are taken from.
#[derive(Clone, Debug, PartialEq)]
pub struct Rp {
    pub id: String,
    origin: Url,
}

impl Rp {
    /// For a page asking on host `host` (its `Host`, which says only which of
    /// the two), of the recorder with `server`, its dashboard on `port`
    /// (as listened on), over HTTPS itself (`tls`) or not: `localhost`'s, at
    /// `http(s)://localhost:<port>`; or `server.hostname`'s, at
    /// `https://<hostname>:<p>`, `p` the port written in it, else `port`
    /// served over HTTPS here, else 443 (a proxy or tunnel in front, its
    /// HTTPS). None: browsers allow it no passkeys (an IP address, another
    /// name), or no port's known.
    pub fn of(server: &Server, port: u16, tls: bool, host: Option<&str>) -> Option<Rp> {
        let name = crate::server::host_name(host?);
        let origin = if port == 0 {
            return None;
        } else if name == "localhost" {
            format!("{}://localhost:{port}", if tls { "https" } else { "http" })
        } else if server.host().as_deref() == Some(name.as_str()) {
            format!("https://{name}:{}", server.host_port().unwrap_or(if tls { port } else { 443 }))
        } else {
            return None;
        };
        Some(Rp { id: name, origin: Url::parse(&origin).ok()? })
    }

    /// Its origin, as a browser writes it (no port when it's the scheme's own).
    pub fn origin(&self) -> String {
        self.origin.origin().ascii_serialization()
    }

    fn webauthn(&self) -> Result<Webauthn, String> {
        let b = WebauthnBuilder::new(&self.id, &self.origin).map_err(|e| format!("Passkeys on {}: {e}", self.id))?;
        b.rp_name("Trunk Recorder Pro").build().map_err(|e| format!("Passkeys on {}: {e}", self.id))
    }

    /// Why an answer wasn't taken, `doing` what: from another origin (another
    /// port: an SSH tunnel's, Docker's), where passkeys do work.
    fn refused(&self, doing: &str, e: WebauthnError) -> String {
        match e {
            WebauthnError::InvalidRPOrigin => format!("Passkeys work at {} alone: open the recorder there.", self.origin()),
            e => format!("{doing}: {e}"),
        }
    }
}

/// The user handle a user's passkeys carry: from their name, so a device
/// keeps one passkey per user here (a second replaces the first) and
/// signing in can check the passkey is still theirs.
fn user_handle(name: &str) -> Uuid {
    let d = Sha256::digest(format!("trunk-pro passkey user\n{name}").as_bytes());
    Uuid::from_slice(&d[..16]).expect("16 bytes")
}

/// User `user` adding a passkey called `name`, on `rp`, since `since`.
struct Adding {
    since: Instant,
    rp: Rp,
    user: String,
    name: String,
    state: PasskeyRegistration,
}

/// Adds started and not yet finished, by a random id the start answers
/// with; and the sign-ins that signed in, until they expire (each signs in once).
#[derive(Default)]
pub struct Ceremonies {
    adds: Mutex<HashMap<String, Adding>>,
    /// A sign-in's nonce, and until when (Unix s).
    used: Mutex<HashMap<String, u64>>,
}

impl Ceremonies {
    /// Keep `add` until it's finished, at `now`: its id. Those waiting longer
    /// than [`CEREMONY`] are dropped; at [`MAX_ADDS`] of its user's, the
    /// oldest of theirs is (another user's never are).
    fn put(&self, now: Instant, add: Adding) -> String {
        let mut m = self.adds.lock().unwrap();
        m.retain(|_, w| now.saturating_duration_since(w.since) < CEREMONY);
        while m.values().filter(|w| w.user == add.user).count() >= MAX_ADDS {
            let Some(oldest) = m.iter().filter(|(_, w)| w.user == add.user).min_by_key(|(_, w)| w.since).map(|(k, _)| k.clone()) else { break };
            m.remove(&oldest);
        }
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).expect("No random numbers for a passkey ceremony");
        let id = hex(&b);
        m.insert(id.clone(), add);
        id
    }

    /// Add `id`, taken (each is finished once), if it hasn't expired at `now`.
    fn take(&self, now: Instant, id: &str) -> Option<Adding> {
        let w = self.adds.lock().unwrap().remove(id)?;
        (now.saturating_duration_since(w.since) < CEREMONY).then_some(w)
    }

    /// Sign-in `nonce` (good until `until`) signing in, at `now`: false if it did already.
    fn sign_in_once(&self, nonce: &str, until: u64, now: u64) -> bool {
        let mut used = self.used.lock().unwrap();
        used.retain(|_, t| *t > now);
        used.insert(nonce.to_string(), until).is_none()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.adds.lock().unwrap().len() + self.used.lock().unwrap().len()
    }
}

/// A passkey's name as given (trimmed), or why not.
pub fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(format!("A passkey's name is 1 to {MAX_NAME} characters (no control characters)."));
    }
    Ok(name.to_string())
}

/// webauthn-rs's record of passkey `p`, as the config keeps it.
fn passkey_of(p: &AuthPasskey) -> Option<Passkey> {
    serde_json::from_value(p.key.clone()).ok()
}

/// Start adding a passkey called `name` for `user` (signed in, on `rp`):
/// `{id, options}`, the options for the browser's `navigator.credentials.create`
/// (`publicKey`: WebAuthn's JSON, its binary values base64url).
pub fn add_start(ceremonies: &Ceremonies, rp: &Rp, auth: &Auth, user: &str, name: &str) -> Result<Value, String> {
    let name = check_name(name)?;
    let u = auth.users.get(user).ok_or_else(|| format!("There's no user named \"{user}\"."))?;
    // (Not a second one on a device that has one of theirs already.)
    let have: Vec<_> = u.passkeys.iter().filter_map(passkey_of).map(|p| p.cred_id().clone()).collect();
    let (options, state) = rp.webauthn()?.start_passkey_registration(user_handle(user), user, user, (!have.is_empty()).then_some(have)).map_err(|e| format!("Couldn't start: {e}"))?;
    let mut options = serde_json::to_value(&options).map_err(|e| e.to_string())?;
    // A discoverable one: signing in, nobody's named first. (The browser's to do: nothing to check.)
    if let Some(sel) = options["publicKey"]["authenticatorSelection"].as_object_mut() {
        sel.insert("residentKey".into(), json!("required"));
        sel.insert("requireResidentKey".into(), json!(true));
    }
    let id = ceremonies.put(Instant::now(), Adding { since: Instant::now(), rp: rp.clone(), user: user.to_string(), name, state });
    Ok(json!({ "id": id, "options": options }))
}

/// Finish adding: the passkey the browser made, for `user` (who started it,
/// on `rp`): the config's record of it, to add ([`add`]).
pub fn add_finish(ceremonies: &Ceremonies, rp: &Rp, user: &str, id: &str, credential: Value) -> Result<AuthPasskey, String> {
    let w = ceremonies.take(Instant::now(), id).ok_or("That passkey request expired: try again.")?;
    if w.user != user || w.rp != *rp {
        return Err("That passkey request was another's.".into());
    }
    let credential: RegisterPublicKeyCredential = serde_json::from_value(credential).map_err(|e| format!("Not a passkey: {e}"))?;
    let passkey = rp.webauthn()?.finish_passkey_registration(&credential, &w.state).map_err(|e| rp.refused("The passkey wasn't taken", e))?;
    let key = serde_json::to_value(&passkey).map_err(|e| e.to_string())?;
    Ok(AuthPasskey { name: w.name, created: crate::clock::unix_s() as i64, id: b64(passkey.cred_id()), key })
}

/// Add passkey `p` to `user` in `auth`: its name, made one of theirs alone
/// ("iPhone 2" beside "iPhone"), or why not (no such user; it's someone's already).
pub fn add(auth: &mut Auth, user: &str, mut p: AuthPasskey) -> Result<String, String> {
    if auth.users.values().any(|u| u.passkeys.iter().any(|q| q.id == p.id)) {
        return Err("That passkey is someone's here already.".into());
    }
    let u = auth.users.get_mut(user).ok_or_else(|| format!("There's no user named \"{user}\"."))?;
    let base = p.name.clone();
    let mut n = 1;
    while u.passkeys.iter().any(|q| q.name == p.name) {
        n += 1;
        p.name = format!("{base} {n}");
    }
    let name = p.name.clone();
    u.passkeys.push(p);
    Ok(name)
}

/// A sign-in waiting, as the browser keeps it: for `rp` (its id and origin),
/// until when (Unix s), its nonce, and webauthn-rs's state.
#[derive(serde::Serialize, serde::Deserialize)]
struct Waiting {
    rp: String,
    until: u64,
    nonce: String,
    state: DiscoverableAuthentication,
}

fn rp_key(rp: &Rp) -> String {
    format!("{} {}", rp.id, rp.origin())
}

/// Start signing in with a passkey, on `rp`: `{id, options}`, the options for
/// `navigator.credentials.get` (`publicKey`, and `mediation`: "conditional",
/// for the browser's autofill; left out, it asks at once). `id` is the
/// sign-in itself, signed with `sessions`' key: nothing's kept here.
pub fn sign_in_start(sessions: &Sessions, rp: &Rp) -> Result<Value, String> {
    sign_in_start_at(sessions, rp, crate::clock::unix_s() as u64)
}

fn sign_in_start_at(sessions: &Sessions, rp: &Rp, now: u64) -> Result<Value, String> {
    let (options, state) = rp.webauthn()?.start_discoverable_authentication().map_err(|e| format!("Couldn't start: {e}"))?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).expect("No random numbers for a passkey ceremony");
    let waiting = Waiting { rp: rp_key(rp), until: now + CEREMONY.as_secs(), nonce: hex(&nonce), state };
    let body = b64(&serde_json::to_vec(&waiting).map_err(|e| e.to_string())?);
    Ok(json!({ "id": sessions.seal(SIGN_IN, &body), "options": options }))
}

/// Who signed in: their name, the passkey's id, and what webauthn-rs found
/// when the config should keep the passkey's count (it went up: see [`sign_in_finish`], [`keep_count`]).
#[derive(Clone, Debug)]
pub struct SignedIn {
    pub user: String,
    pub id: String,
    pub counted: Option<AuthenticationResult>,
}

/// Finish signing in: the browser's answer to sign-in `id` (signed with
/// `sessions`' key, for `rp`, not expired, not signed in with before),
/// checked against the passkey it names (whose user it must still be, by
/// the user handle too).
///
/// Its counter: a passkey kept on one device counts its uses, and one that
/// counts less than last time may be a copy, which webauthn-rs refuses — so
/// the config keeps the count, written when it goes up. Passkeys synced
/// between devices count nothing (0, always): signing in with them writes
/// nothing, nor does a change in their backed-up flag (nothing checks it).
pub fn sign_in_finish(ceremonies: &Ceremonies, sessions: &Sessions, rp: &Rp, auth: &Auth, id: &str, credential: Value) -> Result<SignedIn, String> {
    sign_in_finish_at(ceremonies, sessions, rp, auth, id, credential, crate::clock::unix_s() as u64)
}

fn sign_in_finish_at(ceremonies: &Ceremonies, sessions: &Sessions, rp: &Rp, auth: &Auth, id: &str, credential: Value, now: u64) -> Result<SignedIn, String> {
    const EXPIRED: &str = "That sign-in expired: try again.";
    let body = sessions.unseal(SIGN_IN, id).ok_or(EXPIRED)?;
    let w: Waiting = unb64(body).and_then(|b| serde_json::from_slice(&b).ok()).ok_or(EXPIRED)?;
    if now >= w.until {
        return Err(EXPIRED.into());
    }
    if w.rp != rp_key(rp) {
        return Err("That sign-in was started elsewhere.".into());
    }
    let credential: PublicKeyCredential = serde_json::from_value(credential).map_err(|e| format!("Not a passkey's answer: {e}"))?;
    let webauthn = rp.webauthn()?;
    let (handle, cred_id) = webauthn.identify_discoverable_authentication(&credential).map_err(|_| "Not a passkey of this recorder's.")?;
    let cred_id = b64(cred_id);
    let found = auth.users.iter().find_map(|(name, u)| u.passkeys.iter().find(|p| p.id == cred_id).map(|p| (name, p)));
    let Some((user, p)) = found.filter(|(name, _)| user_handle(name) == handle) else { return Err("Not a passkey of this recorder's (removed?).".into()) };
    let passkey = passkey_of(p).ok_or("That passkey's record in the config can't be read.")?;
    let result = webauthn.finish_discoverable_authentication_v2(&credential, w.state, &passkey).map_err(|e| rp.refused("That passkey didn't sign in", e))?;
    if !ceremonies.sign_in_once(&w.nonce, w.until, now) {
        return Err("That sign-in was used already: try again.".into());
    }
    let counted = (result.needs_update() && result.counter() > 0).then_some(result);
    Ok(SignedIn { user: user.clone(), id: cred_id, counted })
}

/// Keep `signed`'s count in `auth` (see [`sign_in_finish`]): the passkey's
/// record as it is now, its counter raised to it, never lowered (two
/// sign-ins at once: the higher stays). Whether it changed.
pub fn keep_count(auth: &mut Auth, signed: &SignedIn) -> bool {
    let Some(result) = &signed.counted else { return false };
    let Some(p) = auth.users.get_mut(&signed.user).and_then(|u| u.passkeys.iter_mut().find(|p| p.id == signed.id)) else { return false };
    let Some(mut passkey) = passkey_of(p) else { return false };
    if passkey.update_credential(result) != Some(true) {
        return false;
    }
    match serde_json::to_value(&passkey) {
        Ok(key) => {
            p.key = key;
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{add_user, holds, passkey_credential, Sessions, Who};
    use crate::config::ApiAccess;

    fn server(hostname: &str) -> Server {
        Server { hostname: hostname.into(), ..Default::default() }
    }

    fn localhost() -> Rp {
        Rp::of(&Server::default(), 8080, false, Some("localhost:8080")).unwrap()
    }

    /// Passkeys on localhost, or the host name; nowhere else. The origin is
    /// the dashboard's, from what the recorder knows: its port (whatever the
    /// Host says), https on the host name always (the port written in it,
    /// else this one over HTTPS here, else 443: a proxy's in front).
    #[test]
    fn passkeys_are_for_localhost_or_the_host_name_at_the_dashboard_s_origin() {
        let at = |s: &Server, port: u16, tls: bool, host: &str| Rp::of(s, port, tls, Some(host)).map(|r| (r.id.clone(), r.origin()));
        let none = Server::default();
        assert_eq!(at(&none, 8080, false, "localhost:8080"), Some(("localhost".into(), "http://localhost:8080".into())));
        assert_eq!(at(&none, 8080, true, "LOCALHOST"), Some(("localhost".into(), "https://localhost:8080".into())));
        assert_eq!(at(&none, 8080, false, "localhost:9999"), Some(("localhost".into(), "http://localhost:8080".into())), "the Host's port is no say");
        let named = server("radio.ts.net");
        for (s, host) in [(&none, "127.0.0.1:8080"), (&none, "[::1]:8080"), (&none, "radio.ts.net"), (&named, "192.168.1.5:8443"), (&named, "other.ts.net")] {
            assert_eq!(at(s, 8080, true, host), None, "{host}");
        }
        assert_eq!(at(&named, 8080, false, "Radio.ts.net.:8080"), Some(("radio.ts.net".into(), "https://radio.ts.net".into())), "no HTTPS here: a proxy's in front, at 443");
        assert_eq!(at(&named, 8443, true, "radio.ts.net:8443"), Some(("radio.ts.net".into(), "https://radio.ts.net:8443".into())), "HTTPS here: this port");
        assert_eq!(at(&server("radio.ts.net:9443"), 8080, false, "radio.ts.net"), Some(("radio.ts.net".into(), "https://radio.ts.net:9443".into())), "the port written in it");
        assert_eq!(at(&server("radio.ts.net:443"), 8443, true, "radio.ts.net"), Some(("radio.ts.net".into(), "https://radio.ts.net".into())), "443 written: as a browser writes it");
        assert_eq!(Rp::of(&named, 8080, true, None), None, "no Host");
        assert_eq!(Rp::of(&none, 0, false, Some("localhost")), None, "no port known");
        // Each makes its options.
        localhost().webauthn().unwrap();
        Rp::of(&named, 8443, true, Some("radio.ts.net")).unwrap().webauthn().unwrap();
    }

    /// An add is taken once, within 5 minutes; 2 of a user's wait at most,
    /// the oldest of theirs going first (never another user's).
    #[test]
    fn adds_expire_and_are_bounded_by_user() {
        let c = Ceremonies::default();
        let rp = localhost();
        let t0 = Instant::now();
        let add = |user: &str, at: Instant| {
            let (_, state) = rp.webauthn().unwrap().start_passkey_registration(user_handle(user), user, user, None).unwrap();
            c.put(at, Adding { since: at, rp: rp.clone(), user: user.into(), name: "Mac".into(), state })
        };
        let a = add("luke", t0);
        assert!(c.take(t0, &a).is_some() && c.take(t0, &a).is_none(), "once");
        let b = add("luke", t0);
        assert!(c.take(t0 + CEREMONY, &b).is_none(), "expired");
        let ann = add("ann", t0);
        let lukes: Vec<String> = (0..10).map(|i| add("luke", t0 + Duration::from_millis(i))).collect();
        assert_eq!(c.len(), MAX_ADDS + 1, "luke's 2, and ann's");
        assert!(c.take(t0, &lukes[0]).is_none() && c.take(t0, lukes.last().unwrap()).is_some(), "luke's oldest went");
        assert!(c.take(t0, &ann).is_some(), "luke's adds don't push out ann's");
        add("ann", t0 + CEREMONY + Duration::from_secs(1));
        assert_eq!(c.len(), 1, "the expired dropped");
    }

    #[test]
    fn names_and_adding() {
        assert_eq!(check_name("  iPhone "), Ok("iPhone".into()));
        for bad in ["", "  ", "a\nb", &"x".repeat(MAX_NAME + 1)] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        let mut auth = Auth::default();
        add_user(&mut auth, "luke", ApiAccess::Control).unwrap();
        add_user(&mut auth, "ann", ApiAccess::Listen).unwrap();
        let p = |id: &str| AuthPasskey { name: "iPhone".into(), created: 1, id: id.into(), key: json!({}) };
        assert_eq!(add(&mut auth, "luke", p("AA")), Ok("iPhone".into()));
        assert_eq!(add(&mut auth, "luke", p("BB")), Ok("iPhone 2".into()), "one name each");
        assert!(add(&mut auth, "ann", p("AA")).unwrap_err().contains("someone's"));
        assert!(add(&mut auth, "nobody", p("CC")).is_err());
        assert_eq!(add(&mut auth, "ann", p("CC")), Ok("iPhone".into()), "another user's names are theirs");
    }

    /// A session signed in with a passkey lasts as long as the passkey does.
    #[test]
    fn a_passkey_session_ends_with_the_passkey() {
        let mut auth = Auth { enabled: true, ..Default::default() };
        add_user(&mut auth, "luke", ApiAccess::Control).unwrap();
        add(&mut auth, "luke", AuthPasskey { name: "Mac".into(), created: 1, id: "AbC-_9".into(), key: json!({}) }).unwrap();
        let sessions = Sessions::fresh();
        let who = sessions.verify(&sessions.issue("luke", &passkey_credential("AbC-_9"))).unwrap();
        assert!(matches!(&who, Who::User { name, credential, until: Some(_) } if name == "luke" && credential == "pk:AbC-_9"), "{who:?}");
        assert_eq!(who.grants(&auth), Some(ApiAccess::Control));
        assert!(!holds(&auth.users["luke"], "pk:other") && !holds(&auth.users["luke"], "AbC-_9"), "another passkey's, or the id as a token's hash");
        // The token taken away: the passkey's session stays.
        auth.users.get_mut("luke").unwrap().token = None;
        assert_eq!(who.grants(&auth), Some(ApiAccess::Control));
        auth.users.get_mut("luke").unwrap().passkeys.clear();
        assert_eq!(who.grants(&auth), None, "removed: signed out");
    }

    /// Bogus answers are refused; so is a finish of another's add, or a
    /// sign-in that isn't one this key signed, for this relying party, in time.
    #[test]
    fn bogus_finishes_are_refused() {
        let c = Ceremonies::default();
        let sessions = Sessions::fresh();
        let rp = localhost();
        let mut auth = Auth::default();
        add_user(&mut auth, "luke", ApiAccess::Control).unwrap();
        add_user(&mut auth, "ann", ApiAccess::Control).unwrap();
        assert!(sign_in_finish(&c, &sessions, &rp, &auth, "nope", json!({})).unwrap_err().contains("expired"));
        let id = sign_in_start(&sessions, &rp).unwrap()["id"].as_str().unwrap().to_string();
        assert!(sign_in_finish(&c, &sessions, &rp, &auth, &id, json!({ "id": "x" })).unwrap_err().contains("Not a passkey"));
        assert!(sign_in_finish(&c, &Sessions::fresh(), &rp, &auth, &id, json!({})).unwrap_err().contains("expired"), "another key's");
        let other = Rp::of(&Server::default(), 9090, false, Some("localhost")).unwrap();
        assert!(sign_in_finish(&c, &sessions, &other, &auth, &id, json!({})).unwrap_err().contains("elsewhere"), "another origin's");
        // Tampered: its state another, or its expiry later.
        let (body, mac) = id.rsplit_once('.').unwrap();
        let mut w: Value = serde_json::from_slice(&unb64(body).unwrap()).unwrap();
        w["until"] = json!(w["until"].as_u64().unwrap() + 3600);
        let later = format!("{}.{mac}", b64(&serde_json::to_vec(&w).unwrap()));
        assert!(sign_in_finish(&c, &sessions, &rp, &auth, &later, json!({})).unwrap_err().contains("expired"), "tampered");
        assert!(sign_in_finish(&c, &sessions, &rp, &auth, &format!("{body}.{}", "0".repeat(64)), json!({})).unwrap_err().contains("expired"), "another mac");
        assert!(sessions.verify(&id).is_none() && sessions.unseal(SIGN_IN, &sessions.issue("luke", "h")).is_none(), "a sign-in is no session, a session no sign-in");
        let started = add_start(&c, &rp, &auth, "luke", "Mac").unwrap();
        assert_eq!(started["options"]["publicKey"]["authenticatorSelection"]["residentKey"], "required");
        assert_eq!(started["options"]["publicKey"]["rp"]["id"], "localhost");
        let id = started["id"].as_str().unwrap();
        assert!(add_finish(&c, &rp, "ann", id, json!({})).unwrap_err().contains("another's"));
        let id = add_start(&c, &rp, &auth, "luke", "Mac").unwrap()["id"].as_str().unwrap().to_string();
        assert!(sign_in_finish(&c, &sessions, &rp, &auth, &id, json!({})).unwrap_err().contains("expired"), "an add's id is no sign-in");
        let id = add_start(&c, &rp, &auth, "luke", "Mac").unwrap()["id"].as_str().unwrap().to_string();
        let fake = json!({ "id": "AAAA", "rawId": "AAAA", "type": "public-key", "response": { "attestationObject": "AAAA", "clientDataJSON": "AAAA" } });
        assert!(add_finish(&c, &rp, "luke", &id, fake).unwrap_err().contains("wasn't taken"));
        assert!(add_start(&c, &rp, &auth, "luke", "").is_err(), "a name first");
    }

    /// Starting sign-ins keeps nothing (they wait with the browser), however many.
    #[test]
    fn starting_sign_ins_keeps_nothing() {
        let (c, sessions, rp) = (Ceremonies::default(), Sessions::fresh(), localhost());
        for _ in 0..1000 {
            sign_in_start(&sessions, &rp).unwrap();
        }
        assert_eq!(c.len(), 0);
    }

    /// The whole of it, with a software authenticator (an ES256 key, in this
    /// test): added, signed in with, counted, and refused once removed or
    /// counted back (a copy), or answered for another site or another port;
    /// a sign-in once, and within its 5 minutes.
    #[test]
    fn add_and_sign_in_with_a_passkey() {
        let c = Ceremonies::default();
        let sessions = Sessions::fresh();
        let rp = localhost();
        let origin = "http://localhost:8080";
        let mut auth = Auth { enabled: true, ..Default::default() };
        add_user(&mut auth, "luke", ApiAccess::Control).unwrap();
        let key = super::soft::Key::new();
        // Made on another port (an interface's page): refused.
        let started = add_start(&c, &rp, &auth, "luke", "Test key").unwrap();
        let made = key.create(&started["options"]["publicKey"], "http://localhost:8081", 1);
        let why = add_finish(&c, &rp, "luke", started["id"].as_str().unwrap(), made).unwrap_err();
        assert!(why.contains("work at http://localhost:8080 alone"), "{why}");
        // Added.
        let started = add_start(&c, &rp, &auth, "luke", "Test key").unwrap();
        let made = key.create(&started["options"]["publicKey"], origin, 1);
        let p = add_finish(&c, &rp, "luke", started["id"].as_str().unwrap(), made).unwrap();
        assert_eq!((p.name.as_str(), p.id.as_str()), ("Test key", b64(&key.id).as_str()));
        add(&mut auth, "luke", p).unwrap();
        let again = add_start(&c, &rp, &auth, "luke", "Again").unwrap();
        assert_eq!(again["options"]["publicKey"]["excludeCredentials"][0]["id"], b64(&key.id), "not twice on one device");
        let sign_in = |counter: u32, origin: &str, user: &str, auth: &Auth| {
            let started = sign_in_start(&sessions, &rp).unwrap();
            let answer = key.get(&started["options"]["publicKey"], origin, counter, user);
            (started["id"].as_str().unwrap().to_string(), answer.clone(), sign_in_finish(&c, &sessions, &rp, auth, started["id"].as_str().unwrap(), answer))
        };
        // Signed in: the counter went up, so the record's to keep.
        let started = sign_in_start(&sessions, &rp).unwrap();
        assert_eq!(started["options"]["mediation"], "conditional");
        let (id, answer, signed) = sign_in(5, origin, "luke", &auth);
        let signed = signed.unwrap();
        assert_eq!((signed.user.as_str(), signed.id.as_str()), ("luke", b64(&key.id).as_str()));
        assert!(signed.counted.is_some(), "counted: kept");
        // The same answer again (before its count's kept, or for a passkey that counts nothing): once.
        assert!(sign_in_finish(&c, &sessions, &rp, &auth, &id, answer).unwrap_err().contains("used already"), "replayed");
        assert!(keep_count(&mut auth, &signed));
        assert_eq!(auth.users["luke"].passkeys[0].key["cred"]["counter"], 5);
        // Counted back: a copy, refused.
        assert!(sign_in(3, origin, "luke", &auth).2.unwrap_err().contains("compromised"));
        // Another site's page answering, another port's (an interface's page), or another user's handle: refused.
        assert!(sign_in(6, "http://evil.example", "luke", &auth).2.unwrap_err().contains("work at http://localhost:8080"));
        let why = sign_in(6, "http://localhost:8081", "luke", &auth).2.unwrap_err();
        assert!(why.contains("Passkeys work at http://localhost:8080 alone"), "{why}");
        assert!(sign_in(7, origin, "ann", &auth).2.unwrap_err().contains("removed?"));
        // Two at once, from the count of 5: the higher stays, whichever's kept last.
        let (_, _, seven) = sign_in(7, origin, "luke", &auth);
        let (_, _, nine) = sign_in(9, origin, "luke", &auth);
        assert!(keep_count(&mut auth, &nine.unwrap()));
        assert!(!keep_count(&mut auth, &seven.unwrap()), "lower: not kept");
        assert_eq!(auth.users["luke"].passkeys[0].key["cred"]["counter"], 9, "never lowered");
        // Expired: 5 minutes on.
        let started = sign_in_start_at(&sessions, &rp, crate::clock::unix_s() as u64 - CEREMONY.as_secs()).unwrap();
        let answer = key.get(&started["options"]["publicKey"], origin, 10, "luke");
        assert!(sign_in_finish(&c, &sessions, &rp, &auth, started["id"].as_str().unwrap(), answer).unwrap_err().contains("expired"));
        // A synced passkey counts nothing: nothing to keep.
        let synced = super::soft::Key::new();
        let adding = add_start(&c, &rp, &auth, "luke", "Phone").unwrap();
        let made = synced.create(&adding["options"]["publicKey"], origin, 0);
        add(&mut auth, "luke", add_finish(&c, &rp, "luke", adding["id"].as_str().unwrap(), made).unwrap()).unwrap();
        let started = sign_in_start(&sessions, &rp).unwrap();
        let answer = synced.get(&started["options"]["publicKey"], origin, 0, "luke");
        assert!(sign_in_finish(&c, &sessions, &rp, &auth, started["id"].as_str().unwrap(), answer).unwrap().counted.is_none());
        // Only sign-ins that signed in are kept (until they expire).
        assert_eq!(c.len(), 4 + 1, "luke's 4 sign-ins, and the add still waiting");
        // Removed: not a passkey here any more.
        auth.users.get_mut("luke").unwrap().passkeys.clear();
        assert!(sign_in(11, origin, "luke", &auth).2.unwrap_err().contains("removed?"));
    }

    /// The host name's relying party: answers from its https origin alone,
    /// the port as the config has it (none written: 443).
    #[test]
    fn on_the_host_name_the_origin_s_port_is_the_dashboard_s() {
        let c = Ceremonies::default();
        let sessions = Sessions::fresh();
        let mut auth = Auth { enabled: true, ..Default::default() };
        add_user(&mut auth, "luke", ApiAccess::Control).unwrap();
        let key = super::soft::Key::new();
        let rp = Rp::of(&server("radio.example.com"), 8080, false, Some("radio.example.com")).unwrap();
        let started = add_start(&c, &rp, &auth, "luke", "Phone").unwrap();
        let made = key.create(&started["options"]["publicKey"], "https://radio.example.com", 1);
        add(&mut auth, "luke", add_finish(&c, &rp, "luke", started["id"].as_str().unwrap(), made).unwrap()).unwrap();
        let answer_from = |rp: &Rp, origin: &str, n: u32| {
            let started = sign_in_start(&sessions, rp).unwrap();
            let answer = key.get(&started["options"]["publicKey"], origin, n, "luke");
            sign_in_finish(&c, &sessions, rp, &auth, started["id"].as_str().unwrap(), answer).map(|s| s.user)
        };
        assert_eq!(answer_from(&rp, "https://radio.example.com", 2), Ok("luke".into()));
        assert_eq!(answer_from(&rp, "https://radio.example.com:443", 3), Ok("luke".into()), "443 is https's own");
        for other in ["https://radio.example.com:8081", "http://radio.example.com", "https://radio.example.com:8080"] {
            assert!(answer_from(&rp, other, 4).unwrap_err().contains("work at https://radio.example.com alone"), "{other}");
        }
        let at_8443 = Rp::of(&server("radio.example.com:8443"), 8080, false, Some("radio.example.com")).unwrap();
        assert_eq!(answer_from(&at_8443, "https://radio.example.com:8443", 5), Ok("luke".into()), "the port written in server.hostname");
        assert!(answer_from(&at_8443, "https://radio.example.com", 6).is_err());
    }
}

/// A software authenticator for the tests: one ES256 key, answering as a
/// browser would (base64url JSON), attestation "none".
#[cfg(test)]
pub(crate) mod soft {
    use super::{b64, user_handle};
    use p256::ecdsa::{signature::Signer, Signature, SigningKey};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};

    pub struct Key {
        pub id: Vec<u8>,
        sk: SigningKey,
    }

    fn unb64(v: &Value) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(v.as_str().unwrap()).unwrap()
    }

    fn client_data(ty: &str, options: &Value, origin: &str) -> Vec<u8> {
        json!({ "type": ty, "challenge": options["challenge"], "origin": origin, "crossOrigin": false }).to_string().into_bytes()
    }

    /// rpIdHash, flags (user present and verified, + attested data), counter.
    fn auth_data(rp: &str, flags: u8, counter: u32) -> Vec<u8> {
        let mut d = Sha256::digest(rp.as_bytes()).to_vec();
        d.push(flags);
        d.extend(counter.to_be_bytes());
        d
    }

    impl Key {
        pub fn new() -> Key {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).unwrap();
            seed[0] = 1;
            let mut id = vec![0u8; 16];
            getrandom::fill(&mut id).unwrap();
            Key { id, sk: SigningKey::from_slice(&seed).unwrap() }
        }

        /// `navigator.credentials.create`'s answer to `options`, from `origin`.
        pub fn create(&self, options: &Value, origin: &str, counter: u32) -> Value {
            use serde_cbor_2::Value as C;
            let point = self.sk.verifying_key().to_sec1_point(false);
            let cose = C::Map([(C::Integer(1), C::Integer(2)), (C::Integer(3), C::Integer(-7)), (C::Integer(-1), C::Integer(1)), (C::Integer(-2), C::Bytes(point.x().unwrap().to_vec())), (C::Integer(-3), C::Bytes(point.y().unwrap().to_vec()))].into_iter().collect());
            let mut data = auth_data(options["rp"]["id"].as_str().unwrap(), 0x01 | 0x04 | 0x40, counter);
            data.extend([0u8; 16]);
            data.extend((self.id.len() as u16).to_be_bytes());
            data.extend(&self.id);
            data.extend(serde_cbor_2::to_vec(&cose).unwrap());
            let att = C::Map([(C::Text("fmt".into()), C::Text("none".into())), (C::Text("attStmt".into()), C::Map(Default::default())), (C::Text("authData".into()), C::Bytes(data))].into_iter().collect());
            let id = b64(&self.id);
            json!({ "id": id, "rawId": id, "type": "public-key", "extensions": {},
                    "response": { "attestationObject": b64(&serde_cbor_2::to_vec(&att).unwrap()), "clientDataJSON": b64(&client_data("webauthn.create", options, origin)) } })
        }

        /// `navigator.credentials.get`'s answer to `options`, from `origin`, as `user`'s.
        pub fn get(&self, options: &Value, origin: &str, counter: u32, user: &str) -> Value {
            assert!(options["challenge"].is_string() && unb64(&options["challenge"]).len() >= 16);
            let rp = options["rpId"].as_str().unwrap();
            let data = auth_data(rp, 0x01 | 0x04, counter);
            let cd = client_data("webauthn.get", options, origin);
            let mut signed = data.clone();
            signed.extend(Sha256::digest(&cd));
            let sig: Signature = self.sk.sign(&signed);
            let id = b64(&self.id);
            json!({ "id": id, "rawId": id, "type": "public-key", "extensions": {},
                    "response": { "authenticatorData": b64(&data), "clientDataJSON": b64(&cd), "signature": b64(sig.to_der().as_bytes()), "userHandle": b64(user_handle(user).as_bytes()) } })
        }
    }
}
