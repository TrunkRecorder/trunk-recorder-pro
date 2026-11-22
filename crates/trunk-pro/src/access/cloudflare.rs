//! Cloudflare Access in front (`server.auth.proxy.cloudflare`): who a
//! request is, by the token Access signs and sends with each one it lets
//! through (`Cf-Access-Jwt-Assertion`). Its email header is anyone's to
//! send; the token, checked, is Cloudflare's alone: an RS256 JWT, signed by
//! one of the team's keys (`https://<team>.cloudflareaccess.com/cdn-cgi/access/certs`),
//! issued by that team, for the application's Audience tag, and not expired.
//!
//! The keys are fetched when first needed and kept ([`Keys`]), and again for
//! a token signed by one not known (Cloudflare rotates them), at most once a
//! minute, and hourly all the same. Keys that can't be had sign nobody in.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use ring::signature::{RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256};
use serde_json::Value;
use trunk_app::config::AuthCloudflare;

/// The header Access sends its token in.
pub const HEADER: &str = "cf-access-jwt-assertion";

/// How long after it expires (or before it's valid) a token is still taken: clocks differ.
const LEEWAY_S: u64 = 60;

/// How long after a fetch another may be made (for a key not known).
const REFETCH: Duration = Duration::from_secs(60);

/// How long keys are kept before they're fetched again all the same (a key Cloudflare dropped stops signing in).
const KEEP: Duration = Duration::from_secs(3600);

/// base64url, its padding there or not.
fn b64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD_INDIFFERENT.decode(s).ok()
}

/// A team's public key: RSA's modulus and exponent (big-endian).
#[derive(Clone, Debug, PartialEq)]
struct Key {
    n: Vec<u8>,
    e: Vec<u8>,
}

/// Fetches a team's keys (the certs URL's JSON) by the team's URL: the web, or a test's.
type Source = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// A team's keys, as last fetched.
#[derive(Default)]
struct Kept {
    /// The team (its URL) they're the keys of.
    team: String,
    keys: HashMap<String, Key>,
    /// When last fetched (or tried), and whether that failed (said once until one doesn't).
    fetched: Option<Instant>,
    failing: bool,
}

/// The keys Access's tokens are checked with: the recorder's, one team's at a time.
pub struct Keys {
    kept: Mutex<Kept>,
    source: Mutex<Source>,
    /// Held while fetching: requests that all want the keys at once fetch them once.
    fetching: Mutex<()>,
}

impl Default for Keys {
    fn default() -> Self {
        Keys::with(Arc::new(fetch))
    }
}

/// The certs of team `team` (its URL), from Cloudflare.
fn fetch(team: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).user_agent(concat!("trunk-pro/", env!("CARGO_PKG_VERSION"))).build().into();
    let url = format!("{team}/cdn-cgi/access/certs");
    agent.get(&url).call().and_then(|mut r| r.body_mut().read_to_string()).map_err(|e| format!("{url}: {e}"))
}

/// A token's header's key id, if it's a token at all.
fn kid(token: &str) -> Option<String> {
    let header: Value = serde_json::from_slice(&b64(token.split('.').next()?)?).ok()?;
    header["kid"].as_str().map(str::to_string)
}

impl Keys {
    /// Keys from `source` in place of Cloudflare's (tests).
    #[cfg(test)]
    pub fn from(source: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Keys {
        Keys::with(Arc::new(source))
    }

    fn with(source: Source) -> Keys {
        Keys { kept: Mutex::default(), source: Mutex::new(source), fetching: Mutex::new(()) }
    }

    /// Fetch from `source` from now on, nothing kept (tests).
    #[cfg(test)]
    pub fn use_source(&self, source: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) {
        *self.source.lock().unwrap() = Arc::new(source);
        *self.kept.lock().unwrap() = Kept::default();
    }

    /// Fetch `cf`'s keys if `token` [`Keys::wanted`] them (this blocks: off the
    /// async runtime), once for all who ask at once.
    pub fn ready(&self, cf: &AuthCloudflare, token: &str) {
        let _one = self.fetching.lock().unwrap();
        if self.wanted(cf, token) {
            self.refresh(cf);
        }
    }

    /// Whether `cf`'s keys should be fetched before `token` is checked: none
    /// kept for its team, not the one it was signed with (none fetched for a
    /// minute), or kept an hour. Nothing to fetch for what's no token.
    pub fn wanted(&self, cf: &AuthCloudflare, token: &str) -> bool {
        self.wanted_at(cf, token, Instant::now())
    }

    /// [`Keys::wanted`], at `now` (tests: later than now; nothing subtracts from an `Instant`).
    fn wanted_at(&self, cf: &AuthCloudflare, token: &str, now: Instant) -> bool {
        let (Some(team), Some(kid)) = (cf.issuer(), kid(token)) else { return false };
        let k = self.kept.lock().unwrap();
        let age = k.fetched.filter(|_| k.team == team).map(|t| now.saturating_duration_since(t));
        match age {
            Some(age) if age < REFETCH => false,
            Some(age) => age >= KEEP || !k.keys.contains_key(&kid),
            None => true,
        }
    }

    /// Fetch `cf`'s team's keys (this blocks: off the async runtime) and keep
    /// them; a failure is said once (until a fetch works), and the keys kept
    /// before are kept.
    pub fn refresh(&self, cf: &AuthCloudflare) {
        let Some(team) = cf.issuer() else { return };
        let source = self.source.lock().unwrap().clone();
        let got = source(&team).and_then(|body| parse_certs(&body));
        let mut k = self.kept.lock().unwrap();
        if k.team != team {
            *k = Kept { team: team.clone(), ..Default::default() };
        }
        k.fetched = Some(Instant::now());
        match got {
            Ok(keys) => {
                if k.failing {
                    log::info!("Signing in: Cloudflare Access's keys for {team} fetched");
                }
                k.keys = keys;
                k.failing = false;
            }
            Err(e) => {
                if !k.failing {
                    log::warn!("Signing in: couldn't fetch Cloudflare Access's keys ({e}): nobody is signed in by Cloudflare until they are");
                }
                k.failing = true;
            }
        }
    }

    /// Who `token` says, checked with the keys kept: its email and until when
    /// it's good (Unix s, the leeway in), or why not.
    pub fn email(&self, cf: &AuthCloudflare, token: &str) -> Result<(String, u64), Refused> {
        let team = cf.issuer().ok_or(Refused::Config)?;
        let k = self.kept.lock().unwrap();
        let keys = if k.team == team { &k.keys } else { &HashMap::new() };
        verify(token, &team, cf.aud.trim(), keys, crate::clock::unix_s() as u64)
    }
}

/// The certs URL's keys, by id: RSA keys (`kty` RSA, `n`, `e`).
fn parse_certs(body: &str) -> Result<HashMap<String, Key>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let keys: HashMap<String, Key> = v["keys"].as_array().into_iter().flatten().filter(|k| k["kty"] == "RSA").filter_map(|k| {
        Some((k["kid"].as_str()?.to_string(), Key { n: b64(k["n"].as_str()?)?, e: b64(k["e"].as_str()?)? }))
    }).collect();
    if keys.is_empty() {
        return Err("no RSA keys in it".into());
    }
    Ok(keys)
}

/// Why a token doesn't sign anyone in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Refused {
    /// Not three base64url parts of JSON, not RS256, or no `kid`.
    Malformed,
    /// Signed with a key not (yet) known.
    UnknownKey,
    BadSignature,
    /// Another team's.
    Issuer,
    /// Another application's.
    Audience,
    Expired,
    NotYet,
    /// No email (a service token's).
    NoEmail,
    /// `proxy.cloudflare` set as it can't be.
    Config,
}

impl Refused {
    pub fn says(self) -> &'static str {
        match self {
            Refused::Malformed => "isn't a token",
            Refused::UnknownKey => "is signed with a key that isn't the team's",
            Refused::BadSignature => "isn't signed by the team's key",
            Refused::Issuer => "is another team's (proxy.cloudflare.team)",
            Refused::Audience => "is for another application (proxy.cloudflare.aud: its Audience tag)",
            Refused::Expired => "has expired",
            Refused::NotYet => "isn't valid yet (is the clock right?)",
            Refused::NoEmail => "names no email (a service token's)",
            Refused::Config => "can't be checked: proxy.cloudflare's team isn't a team name",
        }
    }
}

/// Who `token` says at `now` (Unix s), if it's an RS256 JWT signed by one of
/// `keys` (by its `kid`), issued by `iss`, for `aud`, and valid now (a
/// minute's leeway): its `email`, and until when it's good (its `exp`, and the leeway).
fn verify(token: &str, iss: &str, aud: &str, keys: &HashMap<String, Key>, now: u64) -> Result<(String, u64), Refused> {
    let mut parts = token.trim().split('.');
    let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return Err(Refused::Malformed) };
    let json = |part: &str| b64(part).and_then(|b| serde_json::from_slice::<Value>(&b).ok()).ok_or(Refused::Malformed);
    let (header, claims, sig) = (json(h)?, json(p)?, b64(s).ok_or(Refused::Malformed)?);
    let kid = header["kid"].as_str().filter(|_| header["alg"] == "RS256").ok_or(Refused::Malformed)?;
    let key = keys.get(kid).ok_or(Refused::UnknownKey)?;
    let signed = format!("{h}.{p}");
    RsaPublicKeyComponents { n: &key.n, e: &key.e }.verify(&RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &sig).map_err(|_| Refused::BadSignature)?;
    if claims["iss"].as_str() != Some(iss) {
        return Err(Refused::Issuer);
    }
    // (A list of audiences, or one.)
    let auds = match &claims["aud"] {
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        v => v.as_str().into_iter().collect::<Vec<_>>(),
    };
    if aud.is_empty() || !auds.contains(&aud) {
        return Err(Refused::Audience);
    }
    let exp = claims["exp"].as_u64().ok_or(Refused::Malformed)?;
    if now >= exp + LEEWAY_S {
        return Err(Refused::Expired);
    }
    if claims["nbf"].as_u64().is_some_and(|nbf| nbf > now + LEEWAY_S) {
        return Err(Refused::NotYet);
    }
    let email = claims["email"].as_str().map(str::trim).filter(|e| !e.is_empty()).ok_or(Refused::NoEmail)?;
    Ok((email.to_string(), exp + LEEWAY_S))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ring::signature::{KeyPair, RsaKeyPair, RSA_PKCS1_SHA256};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn enc(b: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    /// A team's test key (`a`, `b`: fixtures/cloudflare/*.pk8, RSA 2048, PKCS#8).
    pub(crate) struct TestKey(RsaKeyPair, &'static str);

    impl TestKey {
        pub(crate) fn new(n: &'static str) -> TestKey {
            let der = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/cloudflare/{n}.pk8"))).unwrap();
            TestKey(RsaKeyPair::from_pkcs8(&der).unwrap(), n)
        }

        /// Its entry in the certs URL's `keys`, its id its name.
        pub(crate) fn jwk(&self) -> Value {
            let c: RsaPublicKeyComponents<Vec<u8>> = self.0.public_key().into();
            json!({ "kid": self.1, "kty": "RSA", "alg": "RS256", "use": "sig", "e": enc(&c.e), "n": enc(&c.n) })
        }

        /// A token of `claims`, its header `header`'s (`alg` and `kid` added unless given).
        pub(crate) fn sign_with(&self, header: Value, claims: &Value) -> String {
            let mut h = json!({ "alg": "RS256", "kid": self.1, "typ": "JWT" });
            h.as_object_mut().unwrap().extend(header.as_object().unwrap().clone());
            let body = format!("{}.{}", enc(h.to_string().as_bytes()), enc(claims.to_string().as_bytes()));
            let mut sig = vec![0; self.0.public().modulus_len()];
            self.0.sign(&RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), body.as_bytes(), &mut sig).unwrap();
            format!("{body}.{}", enc(&sig))
        }

        pub(crate) fn sign(&self, claims: &Value) -> String {
            self.sign_with(json!({}), claims)
        }
    }

    pub(crate) const TEAM: &str = "https://radio.cloudflareaccess.com";
    pub(crate) const AUD: &str = "4714c1358e65fe4b408ad6d432a5f878f08194bdb4752441fd56faefa9b2b6f2";

    /// Access's claims for `email`, valid around `now`.
    pub(crate) fn claims(email: &str, now: u64) -> Value {
        json!({ "aud": [AUD], "email": email, "exp": now + 3600, "iat": now, "nbf": now, "iss": TEAM, "type": "app", "identity_nonce": "6ei69kawdKzMIAPF", "sub": "7335d417-61da-459d-899c-0a01c76a2f94", "country": "US" })
    }

    /// The certs URL's answer with `keys`.
    pub(crate) fn certs(keys: &[&TestKey]) -> String {
        json!({ "keys": keys.iter().map(|k| k.jwk()).collect::<Vec<_>>(), "public_cert": { "kid": "x", "cert": "" }, "public_certs": [] }).to_string()
    }

    /// A token is who it says only as the team's key signed it, its issuer the
    /// team's, its audience the application's, and valid now.
    #[test]
    fn a_token_is_checked() {
        let (a, b) = (TestKey::new("a"), TestKey::new("b"));
        let keys = parse_certs(&certs(&[&a])).unwrap();
        let now = 1_794_000_000;
        let at = |t: &str, now| verify(t, TEAM, AUD, &keys, now).map(|(email, _)| email);
        let good = a.sign(&claims("luke@example.com", now));
        assert_eq!(at(&good, now), Ok("luke@example.com".into()));
        assert_eq!(verify(&good, TEAM, AUD, &keys, now).map(|(_, until)| until), Ok(now + 3600 + LEEWAY_S), "good until it expires, and the leeway");
        assert_eq!(at(&good, now + 3600 + LEEWAY_S - 1), Ok("luke@example.com".into()), "a minute's leeway");
        assert_eq!(at(&good, now + 3600 + LEEWAY_S), Err(Refused::Expired));
        assert_eq!(at(&good, now - LEEWAY_S - 1), Err(Refused::NotYet));
        let with = |f: &dyn Fn(&mut Value)| {
            let mut c = claims("luke@example.com", now);
            f(&mut c);
            a.sign(&c)
        };
        assert_eq!(at(&with(&|c| c["aud"] = json!(["another"])), now), Err(Refused::Audience));
        assert_eq!(at(&with(&|c| c["aud"] = json!(AUD)), now), Ok("luke@example.com".into()), "one audience, not a list");
        assert_eq!(at(&with(&|c| c["iss"] = json!("https://evil.cloudflareaccess.com")), now), Err(Refused::Issuer));
        assert_eq!(at(&with(&|c| drop(c.as_object_mut().unwrap().remove("iss"))), now), Err(Refused::Issuer));
        assert_eq!(at(&with(&|c| drop(c.as_object_mut().unwrap().remove("exp"))), now), Err(Refused::Malformed), "no expiry");
        assert_eq!(at(&with(&|c| drop(c.as_object_mut().unwrap().remove("email"))), now), Err(Refused::NoEmail), "a service token's");
        assert_eq!(verify(&good, TEAM, "", &keys, now), Err(Refused::Audience), "no audience set: none");
        // Another key, under the team's key's id: a bad signature; under its own, unknown.
        assert_eq!(at(&b.sign_with(json!({ "kid": "a" }), &claims("luke@example.com", now)), now), Err(Refused::BadSignature));
        assert_eq!(at(&b.sign(&claims("luke@example.com", now)), now), Err(Refused::UnknownKey));
        // Changed after signing: its claims, its signature.
        let parts: Vec<&str> = good.split('.').collect();
        let forged = format!("{}.{}.{}", parts[0], enc(claims("admin@example.com", now).to_string().as_bytes()), parts[2]);
        assert_eq!(at(&forged, now), Err(Refused::BadSignature));
        assert_eq!(at(&format!("{}.{}.{}", parts[0], parts[1], enc(b"nope")), now), Err(Refused::BadSignature));
        // Not RS256 (`none`, HS256 with the public key: the old tricks), or not a token.
        let unsigned = format!("{}.{}.", enc(json!({ "alg": "none", "kid": "a" }).to_string().as_bytes()), parts[1]);
        assert_eq!(at(&unsigned, now), Err(Refused::Malformed));
        assert_eq!(at(&a.sign_with(json!({ "alg": "HS256" }), &claims("luke@example.com", now)), now), Err(Refused::Malformed));
        assert_eq!(at(&a.sign_with(json!({ "kid": null }), &claims("luke@example.com", now)), now), Err(Refused::Malformed), "no kid");
        for bad in ["", "a.b", "a.b.c.d", "!!.!!.!!", &format!("{}.{}", parts[0], parts[1]), &format!("{good}.x")] {
            assert_eq!(at(bad, now), Err(Refused::Malformed), "{bad}");
        }
        assert!(parse_certs("{}").is_err() && parse_certs("nope").is_err() && parse_certs(r#"{"keys":[{"kid":"e","kty":"EC","x":"","y":""}]}"#).is_err());
    }

    /// Keys are fetched when first needed, kept, and fetched again for a
    /// token signed by one not known — at most once a minute; a team changed,
    /// its own keys; keys that can't be had sign nobody in.
    #[test]
    fn keys_are_fetched_kept_and_refetched_for_a_new_one() {
        let (a, b) = (TestKey::new("a"), TestKey::new("b"));
        let served = Arc::new(Mutex::new(certs(&[&a])));
        let (fetches, s) = (Arc::new(AtomicUsize::new(0)), served.clone());
        let n = fetches.clone();
        let keys = Keys::from(move |team| {
            n.fetch_add(1, Ordering::SeqCst);
            assert_eq!(team, TEAM);
            let body = s.lock().unwrap().clone();
            if body.is_empty() { Err("offline".into()) } else { Ok(body) }
        });
        let cf = AuthCloudflare { team: "radio".into(), aud: AUD.into() };
        let now = crate::clock::unix_s() as u64;
        let (by_a, by_b) = (a.sign(&claims("luke@example.com", now)), b.sign(&claims("ann@example.com", now)));
        assert_eq!(keys.email(&cf, &by_a), Err(Refused::UnknownKey), "none fetched yet");
        assert!(keys.wanted(&cf, &by_a) && !keys.wanted(&cf, "not a token"));
        keys.refresh(&cf);
        assert_eq!((keys.email(&cf, &by_a).map(|(e, _)| e), fetches.load(Ordering::SeqCst)), (Ok("luke@example.com".into()), 1));
        assert!(!keys.wanted(&cf, &by_a), "kept");
        // A key not known, and a fetch a moment ago: not fetched again (anyone can send such a token).
        assert!(!keys.wanted(&cf, &by_b));
        // A minute on, Cloudflare has rotated to b: fetched once more, and b's tokens sign in.
        *served.lock().unwrap() = certs(&[&a, &b]);
        assert!(keys.wanted_at(&cf, &by_b, Instant::now() + REFETCH));
        keys.refresh(&cf);
        assert_eq!((keys.email(&cf, &by_b).map(|(e, _)| e), fetches.load(Ordering::SeqCst)), (Ok("ann@example.com".into()), 2));
        assert!(!keys.wanted(&cf, &by_a) && !keys.wanted(&cf, &by_b));
        assert!(keys.wanted_at(&cf, &by_a, Instant::now() + KEEP), "kept an hour: fetched again all the same");
        keys.refresh(&cf);
        assert!(!keys.wanted(&cf, &by_a) && fetches.load(Ordering::SeqCst) == 3);
        // Another team: not these keys.
        let other = AuthCloudflare { team: "other".into(), ..cf.clone() };
        assert_eq!(keys.email(&other, &by_a), Err(Refused::UnknownKey));
        assert!(keys.wanted(&other, &by_a));
        assert_eq!(keys.email(&AuthCloudflare { team: "no team".into(), ..cf.clone() }, &by_a), Err(Refused::Config));
        // Unreachable: nobody (and the keys had before kept).
        let down = Keys::from(|_| Err("offline".into()));
        down.refresh(&cf);
        assert_eq!(down.email(&cf, &by_a), Err(Refused::UnknownKey));
        assert!(!down.wanted(&cf, &by_a), "not tried again within the minute");
        served.lock().unwrap().clear();
        keys.refresh(&cf);
        assert_eq!(keys.email(&cf, &by_a).map(|(e, _)| e), Ok("luke@example.com".into()), "a failed fetch keeps the keys had");
    }

    /// Every change to a token Access signed is refused: each character of it
    /// replaced, taken out or one put in. And only RS256 is taken, as Access
    /// signs (not RS384, PS256, ES256, a lowercase name, or none named).
    #[test]
    fn no_change_to_a_token_is_taken() {
        let a = TestKey::new("a");
        let keys = parse_certs(&certs(&[&a])).unwrap();
        let now = 1_794_000_000;
        let good = a.sign(&claims("luke@example.com", now));
        assert!(verify(&good, TEAM, AUD, &keys, now).is_ok());
        let chars: Vec<char> = good.chars().collect();
        let mut tried = 0;
        for i in 0..=chars.len() {
            for c in ['A', 'z', '0', '-', '_', '.', '='] {
                let mut variants = vec![];
                let mut put = chars.clone();
                put.insert(i, c);
                variants.push(put);
                if i < chars.len() && chars[i] != c {
                    let mut swapped = chars.clone();
                    swapped[i] = c;
                    variants.push(swapped);
                }
                if i < chars.len() && c == 'A' {
                    let mut gone = chars.clone();
                    gone.remove(i);
                    variants.push(gone);
                }
                for v in variants {
                    let t: String = v.into_iter().collect();
                    // (Padding at the end of a part decodes as none: the same token, written another way.)
                    if t == good || t.trim_end_matches('=') == good || t.replace("=.", ".") == good {
                        continue;
                    }
                    tried += 1;
                    assert!(verify(&t, TEAM, AUD, &keys, now).is_err(), "{t}");
                }
            }
        }
        assert!(tried > 4000, "{tried}");
        for alg in ["RS384", "RS512", "PS256", "ES256", "rs256", ""] {
            assert_eq!(verify(&a.sign_with(json!({ "alg": alg }), &claims("luke@example.com", now)), TEAM, AUD, &keys, now), Err(Refused::Malformed), "{alg:?}");
        }
        assert_eq!(verify(&a.sign_with(json!({ "alg": null }), &claims("luke@example.com", now)), TEAM, AUD, &keys, now), Err(Refused::Malformed), "no alg");
        // Its claims as Access writes them, but `email` not a string, or empty: nobody.
        for email in [json!(""), json!("  "), json!(["luke@example.com"]), json!(null)] {
            let mut c = claims("x", now);
            c["email"] = email.clone();
            assert_eq!(verify(&a.sign(&c), TEAM, AUD, &keys, now), Err(Refused::NoEmail), "{email}");
        }
    }

    /// A key Cloudflare no longer lists stops signing in once the keys are
    /// fetched again (hourly at most), and a fetch that fails meanwhile keeps
    /// the keys had: what's checked is only ever a key the team served.
    #[test]
    fn a_key_cloudflare_dropped_stops_signing_in() {
        let (a, b) = (TestKey::new("a"), TestKey::new("b"));
        let served = Arc::new(Mutex::new(certs(&[&a, &b])));
        let s = served.clone();
        let keys = Keys::from(move |_| {
            let body = s.lock().unwrap().clone();
            if body.is_empty() { Err("offline".into()) } else { Ok(body) }
        });
        let cf = AuthCloudflare { team: "radio".into(), aud: AUD.into() };
        let now = crate::clock::unix_s() as u64;
        let by_a = a.sign(&claims("luke@example.com", now));
        keys.refresh(&cf);
        assert!(keys.email(&cf, &by_a).is_ok());
        *served.lock().unwrap() = certs(&[&b]);
        assert!(keys.email(&cf, &by_a).is_ok(), "until they're fetched again");
        assert!(keys.wanted_at(&cf, &by_a, Instant::now() + KEEP));
        keys.refresh(&cf);
        assert_eq!(keys.email(&cf, &by_a), Err(Refused::UnknownKey), "dropped");
        assert!(!keys.wanted(&cf, &by_a), "and not fetched again for a token by it, within the minute");
        served.lock().unwrap().clear();
        keys.refresh(&cf);
        assert_eq!(keys.email(&cf, &by_a), Err(Refused::UnknownKey), "a failed fetch brings back nothing");
        assert!(keys.email(&cf, &b.sign(&claims("ann@example.com", now))).is_ok(), "the keys had are kept");
    }
}
