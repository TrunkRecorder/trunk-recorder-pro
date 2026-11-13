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
//! of the two a page is on ([`Rp::of`]), any port; a passkey made on one
//! isn't offered on the other.
//!
//! Between start and finish a ceremony waits in memory ([`Ceremonies`]):
//! for 5 minutes, and only so many, so asking to start again and again
//! (signing in needs nobody to be signed in) keeps nothing more.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use trunk_app::config::{Auth, AuthPasskey};
use webauthn_rs::prelude::{DiscoverableAuthentication, Passkey, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder};

use crate::toolkit::hex;

/// How long a ceremony waits for its finish.
pub const CEREMONY: Duration = Duration::from_secs(300);

/// How many sign-ins may wait at once (the oldest goes for a new one), and adds.
const MAX_LOGINS: usize = 64;
const MAX_ADDS: usize = 16;

/// The longest name a passkey is given, in characters.
const MAX_NAME: usize = 40;

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Where a page's passkeys are: its relying party, by the host name it's on.
#[derive(Clone, Debug, PartialEq)]
pub struct Rp {
    pub id: String,
    https: bool,
}

impl Rp {
    /// A page's on host `host` (its `Host`), `https` or not, with
    /// `server.hostname` `hostname` ([`trunk_app::config::Server::host`]):
    /// `localhost`'s, or the host name's over HTTPS. None: browsers allow it
    /// no passkeys (an IP address, another name, plain HTTP on the host name).
    pub fn of(hostname: Option<&str>, host: Option<&str>, https: bool) -> Option<Rp> {
        let name = crate::server::host_name(host?);
        if name == "localhost" || (https && hostname == Some(name.as_str())) {
            return Some(Rp { id: name, https });
        }
        None
    }

    fn webauthn(&self) -> Result<Webauthn, String> {
        let origin = Url::parse(&format!("{}://{}", if self.https { "https" } else { "http" }, self.id)).map_err(|e| e.to_string())?;
        let b = WebauthnBuilder::new(&self.id, &origin).map_err(|e| format!("Passkeys on {}: {e}", self.id))?;
        b.allow_any_port(true).rp_name("Trunk Recorder Pro").build().map_err(|e| format!("Passkeys on {}: {e}", self.id))
    }
}

/// The user handle a user's passkeys carry: from their name, so a device
/// keeps one passkey per user here (a second replaces the first) and
/// signing in can check the passkey is still theirs.
fn user_handle(name: &str) -> Uuid {
    let d = Sha256::digest(format!("trunk-pro passkey user\n{name}").as_bytes());
    Uuid::from_slice(&d[..16]).expect("16 bytes")
}

enum Step {
    /// User `user` adding a passkey called `name`.
    Add { user: String, name: String, state: PasskeyRegistration },
    SignIn(DiscoverableAuthentication),
}

struct Waiting {
    since: Instant,
    rp: Rp,
    step: Step,
}

/// Ceremonies started and not yet finished, by a random id the start answers with.
#[derive(Default)]
pub struct Ceremonies(Mutex<HashMap<String, Waiting>>);

impl Ceremonies {
    /// Keep `step` (on `rp`) until it's finished, at `now`: its id. Those
    /// waiting longer than [`CEREMONY`] are dropped; at the most of its kind,
    /// the oldest of them is.
    fn put(&self, now: Instant, rp: Rp, step: Step) -> String {
        let mut m = self.0.lock().unwrap();
        m.retain(|_, w| now.saturating_duration_since(w.since) < CEREMONY);
        let add = matches!(step, Step::Add { .. });
        let max = if add { MAX_ADDS } else { MAX_LOGINS };
        let of_kind = |w: &Waiting| matches!(w.step, Step::Add { .. }) == add;
        while m.values().filter(|w| of_kind(w)).count() >= max {
            let Some(oldest) = m.iter().filter(|(_, w)| of_kind(w)).min_by_key(|(_, w)| w.since).map(|(k, _)| k.clone()) else { break };
            m.remove(&oldest);
        }
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).expect("No random numbers for a passkey ceremony");
        let id = hex(&b);
        m.insert(id.clone(), Waiting { since: now, rp, step });
        id
    }

    /// Ceremony `id`, taken (each is finished once), if it hasn't expired at `now`.
    fn take(&self, now: Instant, id: &str) -> Option<Waiting> {
        let w = self.0.lock().unwrap().remove(id)?;
        (now.saturating_duration_since(w.since) < CEREMONY).then_some(w)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
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
    let id = ceremonies.put(Instant::now(), rp.clone(), Step::Add { user: user.to_string(), name, state });
    Ok(json!({ "id": id, "options": options }))
}

/// Finish adding: the passkey the browser made, for `user` (who started it,
/// on `rp`): the config's record of it, to add ([`add`]).
pub fn add_finish(ceremonies: &Ceremonies, rp: &Rp, user: &str, id: &str, credential: Value) -> Result<AuthPasskey, String> {
    let w = ceremonies.take(Instant::now(), id).ok_or("That passkey request expired: try again.")?;
    let Step::Add { user: who, name, state } = w.step else { return Err("That isn't a request to add a passkey.".into()) };
    if who != user || w.rp != *rp {
        return Err("That passkey request was another's.".into());
    }
    let credential: RegisterPublicKeyCredential = serde_json::from_value(credential).map_err(|e| format!("Not a passkey: {e}"))?;
    let passkey = rp.webauthn()?.finish_passkey_registration(&credential, &state).map_err(|e| format!("The passkey wasn't taken: {e}"))?;
    let key = serde_json::to_value(&passkey).map_err(|e| e.to_string())?;
    Ok(AuthPasskey { name, created: crate::clock::unix_s() as i64, id: b64(passkey.cred_id()), key })
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

/// Start signing in with a passkey, on `rp`: `{id, options}`, the options for
/// `navigator.credentials.get` (`publicKey`, and `mediation`: "conditional",
/// for the browser's autofill; left out, it asks at once).
pub fn sign_in_start(ceremonies: &Ceremonies, rp: &Rp) -> Result<Value, String> {
    let (options, state) = rp.webauthn()?.start_discoverable_authentication().map_err(|e| format!("Couldn't start: {e}"))?;
    let id = ceremonies.put(Instant::now(), rp.clone(), Step::SignIn(state));
    Ok(json!({ "id": id, "options": options }))
}

/// Who signed in: their name, the passkey's id, and its record as it is now
/// when the config should keep that (its counter went up: see [`sign_in_finish`]).
#[derive(Debug)]
pub struct SignedIn {
    pub user: String,
    pub id: String,
    pub updated: Option<Value>,
}

/// Finish signing in: the browser's answer, checked against the passkey it
/// names (whose user it must still be, by the user handle too).
///
/// Its counter: a passkey kept on one device counts its uses, and one that
/// counts less than last time may be a copy, which webauthn-rs refuses — so
/// the config keeps the count, written when it goes up. Passkeys synced
/// between devices count nothing (0, always): signing in with them writes
/// nothing, nor does a change in their backed-up flag (nothing checks it).
pub fn sign_in_finish(ceremonies: &Ceremonies, rp: &Rp, auth: &Auth, id: &str, credential: Value) -> Result<SignedIn, String> {
    let w = ceremonies.take(Instant::now(), id).ok_or("That sign-in expired: try again.")?;
    let Step::SignIn(state) = w.step else { return Err("That isn't a sign-in.".into()) };
    if w.rp != *rp {
        return Err("That sign-in was started elsewhere.".into());
    }
    let credential: PublicKeyCredential = serde_json::from_value(credential).map_err(|e| format!("Not a passkey's answer: {e}"))?;
    let webauthn = rp.webauthn()?;
    let (handle, cred_id) = webauthn.identify_discoverable_authentication(&credential).map_err(|_| "Not a passkey of this recorder's.")?;
    let cred_id = b64(cred_id);
    let found = auth.users.iter().find_map(|(name, u)| u.passkeys.iter().find(|p| p.id == cred_id).map(|p| (name, p)));
    let Some((user, p)) = found.filter(|(name, _)| user_handle(name) == handle) else { return Err("Not a passkey of this recorder's (removed?).".into()) };
    let mut passkey = passkey_of(p).ok_or("That passkey's record in the config can't be read.")?;
    let result = webauthn.finish_discoverable_authentication_v2(&credential, state, &passkey).map_err(|e| format!("That passkey didn't sign in: {e}"))?;
    let updated = if result.needs_update() && result.counter() > 0 && passkey.update_credential(&result) == Some(true) { serde_json::to_value(&passkey).ok() } else { None };
    Ok(SignedIn { user: user.clone(), id: cred_id, updated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{add_user, holds, passkey_credential, Sessions, Who};
    use crate::config::ApiAccess;

    fn localhost() -> Rp {
        Rp::of(None, Some("localhost:8080"), false).unwrap()
    }

    /// Passkeys on localhost (http or https, any port), or the host name over HTTPS; nowhere else.
    #[test]
    fn passkeys_are_for_localhost_or_the_host_name() {
        assert_eq!(Rp::of(None, Some("localhost:8080"), false).map(|r| r.id), Some("localhost".into()));
        assert_eq!(Rp::of(None, Some("LOCALHOST"), true).map(|r| r.id), Some("localhost".into()));
        for (hostname, host, https) in [(None, "127.0.0.1:8080", false), (None, "[::1]:8080", true), (None, "radio.ts.net", true), (Some("radio.ts.net"), "radio.ts.net", false), (Some("radio.ts.net"), "192.168.1.5:8443", true), (Some("radio.ts.net"), "other.ts.net", true)] {
            assert_eq!(Rp::of(hostname, Some(host), https), None, "{hostname:?} {host} {https}");
        }
        assert_eq!(Rp::of(Some("radio.ts.net"), Some("Radio.ts.net.:8443"), true).map(|r| r.id), Some("radio.ts.net".into()));
        assert_eq!(Rp::of(Some("radio.ts.net"), None, true), None, "no Host");
        // Each makes its options.
        localhost().webauthn().unwrap();
        Rp::of(Some("radio.ts.net"), Some("radio.ts.net"), true).unwrap().webauthn().unwrap();
    }

    /// A ceremony is taken once, within 5 minutes; only so many wait, the
    /// oldest of its kind going first (sign-ins never push out adds).
    #[test]
    fn ceremonies_expire_and_are_bounded() {
        let c = Ceremonies::default();
        let rp = localhost();
        let t0 = Instant::now();
        let start = || rp.webauthn().unwrap().start_discoverable_authentication().unwrap().1;
        let a = c.put(t0, rp.clone(), Step::SignIn(start()));
        assert!(c.take(t0, &a).is_some() && c.take(t0, &a).is_none(), "once");
        let b = c.put(t0, rp.clone(), Step::SignIn(start()));
        assert!(c.take(t0 + CEREMONY, &b).is_none(), "expired");
        let mut auth = Auth::default();
        add_user(&mut auth, "luke", ApiAccess::Control).unwrap();
        let (_, reg) = rp.webauthn().unwrap().start_passkey_registration(user_handle("luke"), "luke", "luke", None).unwrap();
        let add = c.put(t0, rp.clone(), Step::Add { user: "luke".into(), name: "Mac".into(), state: reg });
        let ids: Vec<String> = (0..MAX_LOGINS + 10).map(|i| c.put(t0 + Duration::from_millis(i as u64), rp.clone(), Step::SignIn(start()))).collect();
        assert_eq!(c.len(), MAX_LOGINS + 1, "the sign-ins, at most, and the add");
        assert!(c.take(t0, &ids[0]).is_none() && c.take(t0, ids.last().unwrap()).is_some(), "the oldest went");
        assert!(c.take(t0, &add).is_some(), "sign-ins don't push out an add");
        c.put(t0 + CEREMONY + Duration::from_secs(1), rp.clone(), Step::SignIn(start()));
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
        assert_eq!(who, Who::User { name: "luke".into(), credential: "pk:AbC-_9".into() });
        assert_eq!(who.grants(&auth), Some(ApiAccess::Control));
        assert!(!holds(&auth.users["luke"], "pk:other") && !holds(&auth.users["luke"], "AbC-_9"), "another passkey's, or the id as a token's hash");
        // The token taken away: the passkey's session stays.
        auth.users.get_mut("luke").unwrap().token = None;
        assert_eq!(who.grants(&auth), Some(ApiAccess::Control));
        auth.users.get_mut("luke").unwrap().passkeys.clear();
        assert_eq!(who.grants(&auth), None, "removed: signed out");
    }

    /// Bogus answers are refused; so is a finish of another's, or another kind's, ceremony.
    #[test]
    fn bogus_finishes_are_refused() {
        let c = Ceremonies::default();
        let rp = localhost();
        let mut auth = Auth::default();
        add_user(&mut auth, "luke", ApiAccess::Control).unwrap();
        add_user(&mut auth, "ann", ApiAccess::Control).unwrap();
        assert!(sign_in_finish(&c, &rp, &auth, "nope", json!({})).unwrap_err().contains("expired"));
        let id = sign_in_start(&c, &rp).unwrap()["id"].as_str().unwrap().to_string();
        assert!(sign_in_finish(&c, &rp, &auth, &id, json!({ "id": "x" })).unwrap_err().contains("Not a passkey"));
        assert!(sign_in_finish(&c, &rp, &auth, &id, json!({})).unwrap_err().contains("expired"), "taken once, even refused");
        let started = add_start(&c, &rp, &auth, "luke", "Mac").unwrap();
        assert_eq!(started["options"]["publicKey"]["authenticatorSelection"]["residentKey"], "required");
        assert_eq!(started["options"]["publicKey"]["rp"]["id"], "localhost");
        let id = started["id"].as_str().unwrap();
        assert!(add_finish(&c, &rp, "ann", id, json!({})).unwrap_err().contains("another's"));
        let id = add_start(&c, &rp, &auth, "luke", "Mac").unwrap()["id"].as_str().unwrap().to_string();
        assert!(sign_in_finish(&c, &rp, &auth, &id, json!({})).unwrap_err().contains("isn't a sign-in"));
        let id = add_start(&c, &rp, &auth, "luke", "Mac").unwrap()["id"].as_str().unwrap().to_string();
        let fake = json!({ "id": "AAAA", "rawId": "AAAA", "type": "public-key", "response": { "attestationObject": "AAAA", "clientDataJSON": "AAAA" } });
        assert!(add_finish(&c, &rp, "luke", &id, fake).unwrap_err().contains("wasn't taken"));
        assert!(add_start(&c, &rp, &auth, "luke", "").is_err(), "a name first");
    }

    /// The whole of it, with a software authenticator (an ES256 key, in this
    /// test): added, signed in with, counted, and refused once removed or
    /// counted back (a copy), or answered for another site.
    #[test]
    fn add_and_sign_in_with_a_passkey() {
        let c = Ceremonies::default();
        let rp = localhost();
        let origin = "http://localhost:8080";
        let mut auth = Auth { enabled: true, ..Default::default() };
        add_user(&mut auth, "luke", ApiAccess::Control).unwrap();
        let key = super::soft::Key::new();
        // Added.
        let started = add_start(&c, &rp, &auth, "luke", "Test key").unwrap();
        let made = key.create(&started["options"]["publicKey"], origin, 1);
        let p = add_finish(&c, &rp, "luke", started["id"].as_str().unwrap(), made).unwrap();
        assert_eq!((p.name.as_str(), p.id.as_str()), ("Test key", b64(&key.id).as_str()));
        add(&mut auth, "luke", p).unwrap();
        let again = add_start(&c, &rp, &auth, "luke", "Again").unwrap();
        assert_eq!(again["options"]["publicKey"]["excludeCredentials"][0]["id"], b64(&key.id), "not twice on one device");
        // Signed in: the counter went up, so the record's to keep.
        let started = sign_in_start(&c, &rp).unwrap();
        assert_eq!(started["options"]["mediation"], "conditional");
        let answer = key.get(&started["options"]["publicKey"], origin, 5, "luke");
        let signed = sign_in_finish(&c, &rp, &auth, started["id"].as_str().unwrap(), answer).unwrap();
        assert_eq!((signed.user.as_str(), signed.id.as_str()), ("luke", b64(&key.id).as_str()));
        let updated = signed.updated.expect("counted: kept");
        assert_eq!(updated["cred"]["counter"], 5);
        auth.users.get_mut("luke").unwrap().passkeys[0].key = updated;
        // Counted back: a copy, refused.
        let started = sign_in_start(&c, &rp).unwrap();
        let answer = key.get(&started["options"]["publicKey"], origin, 3, "luke");
        assert!(sign_in_finish(&c, &rp, &auth, started["id"].as_str().unwrap(), answer).unwrap_err().contains("compromised"));
        // Another site's page answering, or another user's handle: refused.
        let started = sign_in_start(&c, &rp).unwrap();
        let answer = key.get(&started["options"]["publicKey"], "http://evil.example", 6, "luke");
        assert!(sign_in_finish(&c, &rp, &auth, started["id"].as_str().unwrap(), answer).unwrap_err().contains("origin"));
        let started = sign_in_start(&c, &rp).unwrap();
        let answer = key.get(&started["options"]["publicKey"], origin, 7, "ann");
        assert!(sign_in_finish(&c, &rp, &auth, started["id"].as_str().unwrap(), answer).unwrap_err().contains("removed?"));
        // A synced passkey counts nothing: nothing to keep.
        let started = sign_in_start(&c, &rp).unwrap();
        let synced = super::soft::Key::new();
        let adding = add_start(&c, &rp, &auth, "luke", "Phone").unwrap();
        let made = synced.create(&adding["options"]["publicKey"], origin, 0);
        add(&mut auth, "luke", add_finish(&c, &rp, "luke", adding["id"].as_str().unwrap(), made).unwrap()).unwrap();
        let answer = synced.get(&started["options"]["publicKey"], origin, 0, "luke");
        assert!(sign_in_finish(&c, &rp, &auth, started["id"].as_str().unwrap(), answer).unwrap().updated.is_none());
        // Removed: not a passkey here any more.
        auth.users.get_mut("luke").unwrap().passkeys.clear();
        let started = sign_in_start(&c, &rp).unwrap();
        let answer = key.get(&started["options"]["publicKey"], origin, 9, "luke");
        assert!(sign_in_finish(&c, &rp, &auth, started["id"].as_str().unwrap(), answer).unwrap_err().contains("removed?"));
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
