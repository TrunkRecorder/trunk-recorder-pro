//! Signing an app in by way of the browser: a program on the user's computer
//! or phone (Trunk Monitor, a script, another site's page) that can't hold a
//! passkey for this recorder, and shouldn't be handed the user's access
//! token to type in. It's OAuth 2.0's authorization code with PKCE for
//! native apps (RFC 6749, 7636, 8252), by its own names, so a library for
//! it works here; with no apps registered: where a code may go is decided by
//! what kind of place it is ([`Return`]).
//!
//! The app makes a secret of its own (the verifier) and opens the recorder's
//! page `/app/sign-in?client_id=<its name>&code_challenge=<base64url SHA-256
//! of the verifier>&redirect_uri=<where it listens>&scope=<listen or
//! control>&state=<its own>` in the browser ([`Ask`]). There the user signs in
//! as they always do and allows the app, with their role or less
//! ([`approve`]); the browser is sent to the app's `redirect_uri` with a
//! code, which the app trades with its verifier at `POST /api/app/token`
//! ([`redeem`]) for a token of its own: sent as an access token is
//! (`Authorization: Bearer`, `?token=`), good for [`super::SESSION_S`], and
//! kept by its hash beside the config ([`super::state`]), where its user and
//! an admin see it listed and remove it. It ends with what the user allowed
//! it with too (their token made again, the passkey removed, signed out
//! everywhere, the user gone, the proxy no longer naming them).
//!
//! The code is who allowed what, locked with the recorder's key
//! ([`Sessions::lock`]) for two minutes and for that challenge: whatever else
//! on the computer sees it has the code and not the verifier, gets nothing
//! for it, and reads nothing in it. Nothing is kept until it's traded, and
//! it's traded once.
//!
//! An app's token ([`Who::App`]) does what a person's sign-in does, but not
//! what makes or widens a sign-in: it allows no other app, adds no passkey,
//! signs nobody out everywhere, and changes nothing of `server.auth`
//! ([`changes_sign_in`]).

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use trunk_app::config::Auth;
use trunk_app::protocol::{Command, Secret};

use super::state::App;
use super::{holds, Sessions, Who, SESSION_S};
use crate::config::{ApiAccess, Config};
use crate::toolkit::{hex, percent_decode, percent_encode, sha256_hex};

/// How long a code is good for.
pub const CODE_S: u64 = 120;

/// What an app's token starts with (a user's access token: `trp_`).
pub const TOKEN: &str = "trpa_";

/// What [`App::credential`] starts with when the proxy in front signed the user in: the name it gave follows.
pub const PROXY: &str = "proxy:";

/// The longest name an app gives itself, in characters.
const MAX_NAME: usize = 64;

/// The longest `redirect_uri` and `state`, in bytes.
const MAX_URI: usize = 512;

/// What a code is locked for ([`Sessions::lock`]).
const CODE: &str = "app sign-in";

/// Whether `s` is a SHA-256 as base64url, no padding: 43 of its characters.
fn is_challenge(s: &str) -> bool {
    s.len() == 43 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether `s` is a verifier as RFC 7636 has it: 43 to 128 of its characters.
fn is_verifier(s: &str) -> bool {
    (43..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

/// An app's name as the page and the log may say it: no control characters, [`MAX_NAME`] at most; "An app" for none.
pub fn name(given: &str) -> String {
    let n: String = given.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    if n.is_empty() { "An app".into() } else { n }
}

/// A query string's (or a form body's) names and values, in order.
pub fn form(s: &str) -> Vec<(String, String)> {
    let part = |p: &str| percent_decode(&p.replace('+', " "));
    s.trim_start_matches('?').split('&').filter_map(|kv| kv.split_once('=').or(Some((kv, "")))).filter_map(|(k, v)| Some((part(k)?, part(v)?))).collect()
}

/// Where the browser takes the code once the user allows the app: its
/// `redirect_uri`, taken only where a code can't be had by sending someone a
/// link (it would be the sender's own challenge, so their code to trade).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Return {
    /// Nowhere: the page shows the code, and the user copies it to the app
    /// (one with no browser beside it: over SSH, on another device).
    Paste,
    /// `http://127.0.0.1:<port>/…` (or `[::1]`, `localhost`), any port: a
    /// program listening on the computer the browser is on.
    Loopback(String),
    /// A link of the app's own kind, which the system hands the app that
    /// claimed it, on the device the browser is on: its scheme a domain
    /// name backwards (`pro.trunkrecorder.monitor:/signin`), as RFC 8252
    /// asks, so never one a browser runs itself (`javascript:`, `data:`).
    Scheme(String),
    /// A page of an origin `server.allowedOrigins` lists by name: one the
    /// owner already lets use the API.
    Web(String),
}

impl Return {
    /// `uri` as one of them, by `allowed` (`server.allowedOrigins`); or why it's none.
    pub fn of(uri: &str, allowed: &[String]) -> Result<Return, String> {
        if uri.is_empty() {
            return Ok(Return::Paste);
        }
        // (No `#…`: the code goes in the query, and RFC 6749 allows none.)
        if uri.len() > MAX_URI || uri.chars().any(|c| c.is_control() || c.is_whitespace() || c == '#') {
            return Err("The app's return link isn't one (redirect_uri).".into());
        }
        let Some((scheme, rest)) = uri.split_once(':') else { return Err("The app's return link isn't one (redirect_uri).".into()) };
        let scheme = scheme.to_ascii_lowercase();
        if scheme == "http" || scheme == "https" {
            // (Only what a host and port are made of: no `user@`, and no `\\`, which a browser reads as `/`.)
            let host = rest.strip_prefix("//").and_then(|r| r.split(['/', '?']).next()).unwrap_or("");
            if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b)) {
                return Err("The app's return link isn't one (redirect_uri).".into());
            }
            if scheme == "http" && crate::server::is_loopback_name(&crate::server::host_name(host)) {
                return Ok(Return::Loopback(uri.into()));
            }
            let origin = format!("{scheme}://{}", host.to_ascii_lowercase());
            if allowed.iter().any(|a| a.trim().trim_end_matches('/').eq_ignore_ascii_case(&origin)) {
                return Ok(Return::Web(uri.into()));
            }
            return Err(format!("The app returns to {origin}: add it to server.allowedOrigins in the config to let it sign in here."));
        }
        let named = scheme.starts_with(|c: char| c.is_ascii_lowercase()) && scheme.contains('.') && scheme.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+.-".contains(&b));
        if !named {
            return Err(format!("The app returns to a \"{scheme}:\" link: an app's own is a domain name backwards (com.example.app:/signin), or http://127.0.0.1 and a port."));
        }
        Ok(Return::Scheme(uri.into()))
    }

    /// The link, with `params` added to its query (none: [`Return::Paste`]).
    pub fn to(&self, params: &[(&str, &str)]) -> Option<String> {
        let (Return::Loopback(uri) | Return::Scheme(uri) | Return::Web(uri)) = self else { return None };
        let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", percent_encode(v))).collect();
        Some(format!("{uri}{}{}", if uri.contains('?') { '&' } else { '?' }, query.join("&")))
    }

    /// The `redirect_uri` it was made from ("": none).
    pub fn uri(&self) -> &str {
        match self {
            Return::Paste => "",
            Return::Loopback(uri) | Return::Scheme(uri) | Return::Web(uri) => uri,
        }
    }

    /// What kind of place that is, for the page: "paste", "device" (an app
    /// on the device the browser's on), or "web" (another site's page, at [`Return::host`]).
    pub fn kind(&self) -> &'static str {
        match self {
            Return::Paste => "paste",
            Return::Loopback(_) | Return::Scheme(_) => "device",
            Return::Web(_) => "web",
        }
    }

    /// The other site's host ([`Return::Web`]).
    pub fn host(&self) -> Option<&str> {
        let Return::Web(uri) = self else { return None };
        uri.split_once("://").and_then(|(_, r)| r.split(['/', '?']).next())
    }
}

/// What an app asks for: the query `/app/sign-in` was opened with.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    /// What it calls itself (`client_id`: nothing's registered, so it's a name and no more).
    pub name: String,
    /// `code_challenge`: the SHA-256 of its verifier.
    pub challenge: String,
    /// `redirect_uri`: where the code goes.
    pub back: Return,
    /// `scope`: the most it wants to do (`listen`, unless it names `control`).
    pub access: ApiAccess,
    /// `state`: its own, given back with the code as it came.
    pub state: Option<String>,
}

impl Ask {
    /// The ask in `query`, by `allowed` (`server.allowedOrigins`); or why it's none.
    pub fn of(query: &str, allowed: &[String]) -> Result<Ask, String> {
        let pairs = form(query);
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()).unwrap_or("");
        // (Where it goes first: a refusal after this one may be sent there.)
        let back = Return::of(get("redirect_uri"), allowed)?;
        if !is_challenge(get("code_challenge")) || !matches!(get("code_challenge_method"), "" | "S256") {
            return Err("No challenge from the app (code_challenge, S256): open this page from the app.".into());
        }
        if !matches!(get("response_type"), "" | "code") {
            return Err("The app asks for something other than a code (response_type).".into());
        }
        let mut access = ApiAccess::Listen;
        for scope in get("scope").split(' ').filter(|s| !s.is_empty()) {
            let asked = ApiAccess::ALL.iter().find(|a| a.name() == scope).ok_or_else(|| format!("The app asks for \"{}\": there's listen and control (scope).", name(scope)))?;
            access = access.max(*asked);
        }
        let state = Some(get("state")).filter(|s| !s.is_empty());
        if state.is_some_and(|s| s.len() > MAX_URI) {
            return Err("The app's state is too long.".into());
        }
        Ok(Ask { name: name(get("client_id")), challenge: get("code_challenge").into(), back, access, state: state.map(str::to_string) })
    }

    /// Where the browser goes when the user says no (none: nowhere, the page stays).
    pub fn denied(&self) -> Option<String> {
        self.back.to(&self.with_state(&[("error", "access_denied")]))
    }

    fn with_state<'a>(&'a self, params: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        params.iter().copied().chain(self.state.as_deref().map(|s| ("state", s))).collect()
    }
}

/// What a code says, locked in it.
#[derive(Serialize, Deserialize)]
struct Allowed {
    user: String,
    credential: String,
    name: String,
    access: ApiAccess,
    challenge: String,
    back: Return,
    until: u64,
}

/// What the user was signed in with, as an app's kept with it: none for who allows no app.
fn credential(who: &Who) -> Option<(&str, String)> {
    match who {
        Who::User { name, credential, .. } => Some((name, credential.clone())),
        Who::Proxy { asserted, user, .. } => Some((user, format!("{PROXY}{asserted}"))),
        Who::Anonymous | Who::App { .. } => None,
    }
}

/// `who` allows the app that asked `ask` to do `access` (no more than they
/// may themselves), at `now` (Unix s): the code, and where the browser takes
/// it (none: the page shows it). Or why not: nobody signed in, or an app.
pub fn approve(sessions: &Sessions, auth: &Auth, who: &Who, ask: &Ask, access: ApiAccess, now: u64) -> Result<(String, Option<String>), String> {
    let Some((user, credential)) = credential(who) else {
        return Err(if matches!(who, Who::App { .. }) { "An app doesn't sign another in: allow it in the browser." } else { "Sign in first." }.into());
    };
    let role = who.grants(auth, sessions).ok_or("Sign in first.")?;
    let allowed = Allowed { user: user.into(), credential, name: ask.name.clone(), access: access.min(role), challenge: ask.challenge.clone(), back: ask.back.clone(), until: now + CODE_S };
    let code = sessions.lock(CODE, &serde_json::to_vec(&allowed).map_err(|e| e.to_string())?);
    let to = ask.back.to(&ask.with_state(&[("code", &code)]));
    Ok((code, to))
}

/// What user `a.user` may do now by what they allowed app `a` with: their
/// role, or what the proxy's name for them gets. None: that's gone.
fn role(auth: &Auth, a: &App) -> Option<ApiAccess> {
    match a.credential.strip_prefix(PROXY) {
        Some(asserted) => {
            auth.proxy.as_ref().filter(|p| p.problem().is_none())?;
            auth.proxied(asserted).filter(|(u, _)| *u == a.user).map(|(_, access)| access)
        }
        None => auth.users.get(&a.user).filter(|u| holds(u, &a.credential)).map(|u| u.access),
    }
}

/// How the user who allowed app `a` was signed in then, in words for the log ([`Who::how`]).
pub fn how(auth: &Auth, a: &App) -> String {
    match a.credential.strip_prefix(PROXY) {
        Some(_) => "signed in by the proxy".into(),
        None => Who::User { name: a.user.clone(), credential: a.credential.clone(), until: None }.how(auth),
    }
}

/// What app `a` may do at `now`: what it was allowed and no more than its
/// user may. None: past its time, its user signed out everywhere since it
/// was allowed, or what they allowed it with gone.
pub fn live(auth: &Auth, sessions: &Sessions, a: &App, now: u64) -> Option<ApiAccess> {
    if now >= a.until || sessions.state.signed_out(&a.user).is_some_and(|at| a.created as i64 <= at) {
        return None;
    }
    role(auth, a).map(|r| r.min(a.access))
}

/// An app signed in: its token, which is had this once, and what's kept of it, by id.
#[derive(Debug)]
pub struct Granted {
    pub token: Secret,
    pub id: String,
    pub app: App,
}

/// Trade `code` for a token, for the app whose `verifier` it was allowed
/// for, at `now`; `back`: the `redirect_uri` it says it was sent to, if it
/// says. Kept ([`super::state::State`]) before it's answered. Or why not, in
/// OAuth's word for it and ours. Blocking: it writes a file.
pub fn redeem(sessions: &Sessions, auth: &Auth, code: &str, verifier: &str, back: Option<&str>, now: u64) -> Result<Granted, (&'static str, String)> {
    let refused = || ("invalid_grant", "Not a code for this app (or too old, or used): sign in again from the app.".to_string());
    let allowed: Allowed = sessions.unlock(CODE, code).and_then(|b| serde_json::from_slice(&b).ok()).ok_or_else(refused)?;
    let made = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if now >= allowed.until || !is_verifier(verifier) || made != allowed.challenge || back.is_some_and(|b| b != allowed.back.uri()) {
        return Err(refused());
    }
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| ("server_error", format!("No random numbers to make a token with: {e}")))?;
    let token = format!("{TOKEN}{}", hex(&secret));
    let app = App { user: allowed.user, name: allowed.name, access: allowed.access, credential: allowed.credential, created: now, until: now + SESSION_S, hash: sha256_hex(token.as_bytes()) };
    // (The user's token made again, their passkey removed, or signed out everywhere, in those two minutes.)
    let since = (allowed.until - CODE_S) as i64;
    if sessions.state.signed_out(&app.user).is_some_and(|at| since <= at) {
        return Err(refused());
    }
    live(auth, sessions, &app, now).ok_or_else(refused)?;
    // One code, one app: its id is the code's, so a code traded again finds it taken.
    let id = sha256_hex(code.as_bytes())[..16].to_string();
    match sessions.state.add_app(auth, &id, app.clone()) {
        Ok(true) => Ok(Granted { token: Secret::new(token), id, app }),
        Ok(false) => Err(refused()),
        Err(e) => Err(("server_error", format!("Couldn't keep the app's sign-in: {e}"))),
    }
}

/// Who `secret` signs in: the app whose token it is ([`Who::App`]).
pub fn signs_in(sessions: &Sessions, secret: &str) -> Option<Who> {
    let secret = secret.trim();
    secret.starts_with(TOKEN).then_some(())?;
    let (id, a) = sessions.state.app_of(&sha256_hex(secret.as_bytes()))?;
    Some(Who::App { user: a.user, id, name: a.name, until: a.until })
}

/// Whether command `c` would change who signs in, by `now` (the config as it
/// is): a token made, or `server.auth` edited. An app's token does neither,
/// whatever it was allowed ([`Who::person`]).
pub fn changes_sign_in(now: &Config, c: &Command) -> bool {
    match c {
        Command::CreateToken { .. } => true,
        Command::SetConfig { config, .. } => Config::from_value(config.clone()).is_ok_and(|next| next.server.auth != now.server.auth),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(verifier: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    const V: &str = "the-apps-own-secret-43-characters-or-more-0";
    const BACK: &str = "pro.trunkrecorder.monitor:/signin";

    fn ask(verifier: &str, more: &str) -> Ask {
        Ask::of(&format!("client_id=Trunk+Monitor&code_challenge={}&redirect_uri={}{more}", challenge(verifier), percent_encode(BACK)), &[]).unwrap()
    }

    fn user(auth: &mut Auth, name: &str, access: ApiAccess) -> Who {
        let secret = super::super::add_user(auth, name, access).unwrap();
        super::super::signs_in(auth, secret.expose()).unwrap()
    }

    fn code_of(to: &str) -> String {
        form(to.split_once('?').unwrap().1).into_iter().find(|(k, _)| k == "code").unwrap().1
    }

    #[test]
    fn a_code_signs_in_the_app_that_asked_and_no_other_once() {
        let sessions = Sessions::fresh();
        // (The clock's own time: what's kept is dropped once it's past.)
        let t = crate::clock::unix_s() as u64;
        let mut auth = Auth { enabled: true, ..Default::default() };
        let luke = user(&mut auth, "luke.b", ApiAccess::Control);
        let (code, to) = approve(&sessions, &auth, &luke, &ask(V, "&state=a%26b"), ApiAccess::Control, t).unwrap();
        let to = to.unwrap();
        assert!(to.starts_with(&format!("{BACK}?code=")) && to.ends_with("&state=a%26b"), "{to}");
        assert_eq!(code_of(&to), code);
        assert!(!code.contains("luke") && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "nothing to read in it, nothing a URL changes");
        let no = |r: Result<Granted, (&'static str, String)>| r.unwrap_err().0;
        assert_eq!(no(redeem(&sessions, &auth, &code, "another-apps-secret-43-characters-or-more-0", None, t + 1)), "invalid_grant", "the code alone is nothing");
        assert_eq!(no(redeem(&sessions, &auth, &code, V, None, t + CODE_S)), "invalid_grant", "expired");
        assert_eq!(no(redeem(&sessions, &auth, &code, V, Some("pro.trunkrecorder.other:/signin"), t + 1)), "invalid_grant", "not where it was sent");
        assert_eq!(no(redeem(&Sessions::fresh(), &auth, &code, V, None, t + 1)), "invalid_grant", "another recorder's");
        let mut changed = code.clone().into_bytes();
        changed[20] = if changed[20] == b'A' { b'B' } else { b'A' };
        assert_eq!(no(redeem(&sessions, &auth, &String::from_utf8(changed).unwrap(), V, None, t + 1)), "invalid_grant", "changed");
        let got = redeem(&sessions, &auth, &code, V, Some(BACK), t + 1).unwrap();
        assert_eq!((got.app.user.as_str(), got.app.name.as_str(), got.app.access, got.app.until), ("luke.b", "Trunk Monitor", ApiAccess::Control, t + 1 + SESSION_S));
        assert!(got.token.expose().starts_with(TOKEN) && got.app.hash == sha256_hex(got.token.expose().as_bytes()));
        assert_eq!(no(redeem(&sessions, &auth, &code, V, None, t + 2)), "invalid_grant", "once");
        // Its token says who: the app, as its user.
        let who = signs_in(&sessions, got.token.expose()).unwrap();
        assert_eq!(who, Who::App { user: "luke.b".into(), id: got.id.clone(), name: "Trunk Monitor".into(), until: t + 1 + SESSION_S });
        assert_eq!(signs_in(&sessions, &format!("{TOKEN}{}", "0".repeat(64))), None);
        assert_eq!(super::super::signs_in(&auth, got.token.expose()), None, "no user's access token");
        assert_eq!(sessions.verify(&code), None, "a code is no session");
    }

    #[test]
    fn an_app_does_what_it_was_allowed_while_its_user_may() {
        let dir = crate::db::TestDir::new("app-live");
        let sessions = Sessions::fresh_in(&dir);
        let mut auth = Auth { enabled: true, ..Default::default() };
        let luke = user(&mut auth, "luke", ApiAccess::Control);
        let kiosk = user(&mut auth, "kiosk", ApiAccess::Listen);
        let now = crate::clock::unix_s() as u64;
        let app = |who: &Who, want: ApiAccess| {
            let v = format!("{V}{}", sessions.state.apps().len());
            let (code, _) = approve(&sessions, &auth, who, &ask(&v, ""), want, now).unwrap();
            let got = redeem(&sessions, &auth, &code, &v, None, now).unwrap();
            signs_in(&sessions, got.token.expose()).unwrap()
        };
        // What was allowed, and no more than the user's role: then, and now.
        let (viewer, admin, wide) = (app(&luke, ApiAccess::Listen), app(&luke, ApiAccess::Control), app(&kiosk, ApiAccess::Control));
        assert_eq!((viewer.grants(&auth, &sessions), admin.grants(&auth, &sessions), wide.grants(&auth, &sessions)), (Some(ApiAccess::Listen), Some(ApiAccess::Control), Some(ApiAccess::Listen)));
        auth.users.get_mut("kiosk").unwrap().access = ApiAccess::Control;
        assert_eq!(wide.grants(&auth, &sessions), Some(ApiAccess::Listen), "allowed while a View user: View");
        auth.users.get_mut("luke").unwrap().access = ApiAccess::Listen;
        assert_eq!(admin.grants(&auth, &sessions), Some(ApiAccess::Listen), "its user's role now");
        // An app allows no other.
        assert!(approve(&sessions, &auth, &admin, &ask(V, ""), ApiAccess::Listen, now).unwrap_err().contains("An app"));
        assert!(approve(&sessions, &auth, &Who::Anonymous, &ask(V, ""), ApiAccess::Listen, now).unwrap_err().contains("Sign in"));
        // Removed: its token is nobody's. The other stays.
        let Who::App { id, .. } = &viewer else { unreachable!() };
        assert_eq!(sessions.state.remove_app(&auth, id).unwrap().map(|a| a.name), Some("Trunk Monitor".into()));
        assert_eq!((viewer.grants(&auth, &sessions), admin.grants(&auth, &sessions)), (None, Some(ApiAccess::Listen)));
        // Ended by what ends a browser's session: signed out everywhere, the token it was allowed with made again.
        sessions.sign_out(&auth, "luke").unwrap();
        assert_eq!(admin.grants(&auth, &sessions), None, "signed out everywhere");
        assert_eq!(wide.grants(&auth, &sessions), Some(ApiAccess::Listen), "another user's");
        auth.users.get_mut("kiosk").unwrap().token = None;
        assert_eq!(wide.grants(&auth, &sessions), None, "what they allowed it with is gone");
    }

    #[test]
    fn the_proxys_user_allows_an_app_while_the_proxy_names_them() {
        let dir = crate::db::TestDir::new("app-proxy");
        let sessions = Sessions::fresh_in(&dir);
        let mut auth = Auth { enabled: true, ..Default::default() };
        user(&mut auth, "luke", ApiAccess::Control);
        auth.proxy = Some(trunk_app::config::AuthProxy { header: "Remote-User".into(), from: vec!["10.0.0.1".into()], users: [("luke@example.com".to_string(), "luke".to_string())].into(), ..Default::default() });
        let proxy = Who::Proxy { asserted: "luke@example.com".into(), user: "luke".into(), via: super::super::Via::Header("10.0.0.1".parse().unwrap()) };
        let now = crate::clock::unix_s() as u64;
        let (code, _) = approve(&sessions, &auth, &proxy, &ask(V, ""), ApiAccess::Control, now).unwrap();
        let got = redeem(&sessions, &auth, &code, V, None, now).unwrap();
        let app = signs_in(&sessions, got.token.expose()).unwrap();
        assert_eq!((got.app.credential.as_str(), app.grants(&auth, &sessions)), ("proxy:luke@example.com", Some(ApiAccess::Control)));
        auth.proxy.as_mut().unwrap().users.clear();
        assert_eq!(app.grants(&auth, &sessions), None, "no longer named by the proxy");
    }

    #[test]
    fn a_code_goes_only_where_a_link_sent_to_someone_gets_nothing() {
        let allowed = ["https://app.example.com".to_string(), "*".to_string()];
        let of = |uri: &str| Return::of(uri, &allowed);
        assert_eq!(of(""), Ok(Return::Paste));
        for ok in ["http://127.0.0.1:53412/callback", "http://[::1]:8000/", "http://localhost:9/x?y=1", "HTTP://127.0.0.1"] {
            assert!(matches!(of(ok), Ok(Return::Loopback(_))), "{ok}");
        }
        for ok in ["pro.trunkrecorder.monitor:/signin", "com.example.app://signin?x=1"] {
            assert!(matches!(of(ok), Ok(Return::Scheme(_))), "{ok}");
        }
        assert!(matches!(of("https://App.Example.com/back"), Ok(Return::Web(_))));
        for bad in [
            "https://evil.example/back",
            "http://app.example.com/back",
            "https://app.example.com.evil.example/",
            "https://app.example.com@evil.example/",
            "http://127.0.0.1@evil.example/",
            "http://127.0.0.1\\@evil.example/",
            "http://127.0.0.1.evil.example/",
            "https://127.0.0.1/",
            "http:///x",
            "javascript:alert(1)",
            "javascript://%0aalert(1)",
            "data:text/html,x",
            "trunkmonitor://signin",
            "intent://x#Intent;end",
            "com.example.app:/x#y",
            "com.example.app:/x y",
            "Com.Example\n.app:/x",
            "nothing",
        ] {
            assert!(of(bad).is_err(), "{bad}");
        }
        assert!(of("https://other.example/").unwrap_err().contains("allowedOrigins"), "every origin let in is no origin named");
        assert_eq!(of("http://127.0.0.1:1/cb?x=1").unwrap().to(&[("code", "a b"), ("state", "s")]).unwrap(), "http://127.0.0.1:1/cb?x=1&code=a%20b&state=s");
        let web = of("https://app.example.com/back").unwrap();
        assert_eq!((of("").unwrap().kind(), of(BACK).unwrap().kind(), web.kind(), web.host(), of(BACK).unwrap().host()), ("paste", "device", "web", Some("app.example.com"), None));
    }

    #[test]
    fn an_ask_is_read_by_oauths_names() {
        let c = challenge(V);
        let a = Ask::of(&format!("?response_type=code&client_id=Trunk%20Monitor+on+Luke%27s+Mac&redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Fcb&code_challenge={c}&code_challenge_method=S256&scope=listen+control&state=xyz"), &[]).unwrap();
        assert_eq!(a, Ask { name: "Trunk Monitor on Luke's Mac".into(), challenge: c.clone(), back: Return::Loopback("http://127.0.0.1:5000/cb".into()), access: ApiAccess::Control, state: Some("xyz".into()) });
        assert_eq!(a.denied().unwrap(), "http://127.0.0.1:5000/cb?error=access_denied&state=xyz");
        let bare = Ask::of(&format!("code_challenge={c}"), &[]).unwrap();
        assert_eq!((bare.name.as_str(), &bare.back, bare.access, bare.denied()), ("An app", &Return::Paste, ApiAccess::Listen, None));
        for bad in [String::new(), "code_challenge=short".into(), format!("code_challenge={c}&code_challenge_method=plain"), format!("code_challenge={c}&response_type=token"), format!("code_challenge={c}&scope=everything"), format!("code_challenge={c}&redirect_uri=javascript:alert(1)"), format!("code_challenge={c}&state={}", "s".repeat(600))] {
            assert!(Ask::of(&bad, &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_apps_name_is_fit_for_the_page_and_the_log() {
        assert_eq!(name("  Trunk Monitor on Luke's Mac "), "Trunk Monitor on Luke's Mac");
        assert_eq!(name("a\nUser root: signed in\u{7}"), "aUser root: signed in");
        assert_eq!(name(&"x".repeat(200)).chars().count(), MAX_NAME);
        assert_eq!(name(" \n"), "An app");
    }

    #[test]
    fn an_app_changes_nothing_of_who_signs_in() {
        let now = Config::default();
        let mut next = now.clone();
        let set = |c: &Config| Command::SetConfig { config: serde_json::to_value(c).unwrap(), version: 1 };
        assert!(!changes_sign_in(&now, &set(&next)));
        next.server.auth.anonymous = Some(ApiAccess::Listen);
        assert!(changes_sign_in(&now, &set(&next)));
        assert!(changes_sign_in(&now, &Command::CreateToken { id: 1, name: "x".into(), access: ApiAccess::Listen }));
        assert!(!changes_sign_in(&now, &Command::parse(r#"{"type":"start"}"#).unwrap()));
    }
}
