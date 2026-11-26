//! Signing an app in by way of the browser: an app on the user's computer
//! or phone (Trunk Monitor) that has no way to hold a passkey for this
//! recorder, and shouldn't be handed the user's access token to type in.
//!
//! The app makes a secret of its own (the verifier) and opens the recorder's
//! page `/app/sign-in?challenge=<base64url SHA-256 of it>&name=<what it's
//! called>` in the browser. There the user signs in as they always do (their
//! token, a passkey) and allows the app; the page is answered a one-use-for
//! code ([`approve`]) and sends the browser to `trunkmonitor://signin?code=…`,
//! which the system hands the app. The app then asks
//! `POST /api/app/session {code, verifier}` ([`redeem`]) and is answered a
//! session like a browser's: the cookie to send with its requests, good for
//! [`super::SESSION_S`], ended by everything that ends the browser's (the
//! user removed or signed out everywhere, their token made again, the passkey
//! removed).
//!
//! The code is the user and their credential sealed with the recorder's key
//! ([`Sessions::seal`]) for two minutes and for that challenge: whatever else
//! on the computer sees the `trunkmonitor:` link has the code and not the
//! verifier, and gets nothing for it. Nothing is kept: within its two
//! minutes the app that made the verifier may be answered twice.
//!
//! A user the proxy in front signed in has no credential here to make a
//! session of: the app signs in with an access token, sent through the proxy
//! as every program's is (with a service token's headers, for one).

use base64::Engine;
use sha2::{Digest, Sha256};
use trunk_app::config::Auth;

use super::{Sessions, Who};
use crate::config::ApiAccess;
use crate::toolkit::{percent_decode, percent_encode};

/// Where the browser is sent with the code: the apps' link.
pub const RETURN: &str = "trunkmonitor://signin";

/// How long a code is good for.
pub const CODE_S: u64 = 120;

/// The longest name an app gives itself, in characters.
const MAX_NAME: usize = 64;

/// What a code is signed for ([`Sessions::seal`]).
const CODE: &str = "app sign-in";

/// Whether `s` is a SHA-256 as base64url, no padding: 43 of its characters.
fn is_challenge(s: &str) -> bool {
    s.len() == 43 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// An app's name as the log may say it: no control characters, [`MAX_NAME`] at most; "an app" for none.
pub fn name(given: &str) -> String {
    let n: String = given.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    if n.is_empty() { "an app".into() } else { n }
}

/// The code for `who` allowing an app that made `challenge`, at `now` (Unix
/// s): or why not (not a user signed in with a credential of this
/// recorder's, or no challenge).
pub fn approve(sessions: &Sessions, who: &Who, challenge: &str, now: u64) -> Result<String, String> {
    let Who::User { name, credential, .. } = who else {
        return Err(match who {
            Who::Proxy { .. } => "Signed in by the proxy in front: give the app an access token instead.".into(),
            _ => "Sign in first.".into(),
        });
    };
    if !is_challenge(challenge) {
        return Err("No challenge from the app: open this page from the app.".into());
    }
    Ok(sessions.seal(CODE, &format!("{}.{}.{challenge}.{}", percent_encode(name), percent_encode(credential), now + CODE_S)))
}

/// Who `code` signs in, for the app whose `verifier` it is, at `now`: the
/// user, their credential and their role as `auth` has them now. None: not a
/// code of this recorder's, expired, another app's, or the user's credential gone since.
pub fn redeem(sessions: &Sessions, auth: &Auth, code: &str, verifier: &str, now: u64) -> Option<(String, String, ApiAccess)> {
    let mut parts = sessions.unseal(CODE, code)?.split('.');
    let (user, credential, challenge, expires) = (percent_decode(parts.next()?)?, percent_decode(parts.next()?)?, parts.next()?, parts.next()?.parse::<u64>().ok()?);
    (parts.next().is_none() && now < expires).then_some(())?;
    let made = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (made == challenge).then_some(())?;
    let role = Who::User { name: user.clone(), credential: credential.clone(), until: None }.grants(auth, sessions)?;
    Some((user, credential, role))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(verifier: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    fn user(auth: &mut Auth, name: &str, access: ApiAccess) -> Who {
        let secret = super::super::add_user(auth, name, access).unwrap();
        super::super::signs_in(auth, secret.expose()).unwrap()
    }

    #[test]
    fn a_code_signs_in_the_app_that_asked_and_no_other() {
        let sessions = Sessions::fresh();
        let mut auth = Auth { enabled: true, ..Default::default() };
        let luke = user(&mut auth, "luke.b", ApiAccess::Control);
        let code = approve(&sessions, &luke, &challenge("the app's secret"), 1000).unwrap();
        let Who::User { credential, .. } = &luke else { unreachable!() };
        assert_eq!(redeem(&sessions, &auth, &code, "the app's secret", 1001), Some(("luke.b".into(), credential.clone(), ApiAccess::Control)), "a dot in the name too");
        assert_eq!(redeem(&sessions, &auth, &code, "the app's secret", 1000 + CODE_S - 1).map(|r| r.0), Some("luke.b".into()), "twice, within its time");
        assert_eq!(redeem(&sessions, &auth, &code, "another's", 1001), None, "the code alone is nothing");
        assert_eq!(redeem(&sessions, &auth, &code, "the app's secret", 1000 + CODE_S), None, "expired");
        assert_eq!(redeem(&Sessions::fresh(), &auth, &code, "the app's secret", 1001), None, "another recorder's");
        let mut longer = code.clone();
        longer.insert(code.rfind('.').unwrap() - 1, '9');
        assert_eq!(redeem(&sessions, &auth, &longer, "the app's secret", 1001), None, "its time changed");
        // A session isn't a code, nor a code a session.
        assert_eq!(redeem(&sessions, &auth, &sessions.issue("luke.b", credential), "the app's secret", 1001), None);
        assert_eq!(sessions.verify(&code), None);
    }

    #[test]
    fn a_code_is_who_the_user_is_when_its_used() {
        let sessions = Sessions::fresh();
        let mut auth = Auth { enabled: true, ..Default::default() };
        let kiosk = user(&mut auth, "kiosk", ApiAccess::Listen);
        let code = approve(&sessions, &kiosk, &challenge("v"), 1000).unwrap();
        auth.users.get_mut("kiosk").unwrap().access = ApiAccess::Control;
        assert_eq!(redeem(&sessions, &auth, &code, "v", 1001).map(|r| r.2), Some(ApiAccess::Control), "their role now");
        auth.users.get_mut("kiosk").unwrap().token = None;
        assert_eq!(redeem(&sessions, &auth, &code, "v", 1001), None, "their token gone since");
        auth.users.remove("kiosk");
        assert_eq!(redeem(&sessions, &auth, &code, "v", 1001), None, "the user gone since");
    }

    #[test]
    fn only_a_user_signed_in_here_allows_an_app() {
        let sessions = Sessions::fresh();
        let mut auth = Auth { enabled: true, ..Default::default() };
        let luke = user(&mut auth, "luke", ApiAccess::Control);
        assert!(approve(&sessions, &Who::Anonymous, &challenge("v"), 0).unwrap_err().contains("Sign in"));
        let proxy = Who::Proxy { asserted: "luke@example.com".into(), user: "luke".into(), via: super::super::Via::Header("10.0.0.1".parse().unwrap()) };
        assert!(approve(&sessions, &proxy, &challenge("v"), 0).unwrap_err().contains("proxy"));
        for bad in ["", "short", &challenge("v")[1..], &format!("{}=", &challenge("v")[1..]), &format!("{}.", &challenge("v")[1..])] {
            assert!(approve(&sessions, &luke, bad, 0).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_apps_name_is_fit_for_the_log() {
        assert_eq!(name("  Trunk Monitor on Luke's Mac "), "Trunk Monitor on Luke's Mac");
        assert_eq!(name("a\nUser root: signed in\u{7}"), "aUser root: signed in");
        assert_eq!(name(&"x".repeat(200)).chars().count(), MAX_NAME);
        assert_eq!(name(" \n"), "an app");
    }
}
