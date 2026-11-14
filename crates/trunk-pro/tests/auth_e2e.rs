//! Signing in, against the real binary over real sockets: `trunk-pro`
//! started with a config of its own in a folder of its own (no radios, no
//! network), set up with `trunk-pro access …` as someone on the recorder's
//! computer would, then asked as a browser or a program would ask. What the
//! unit tests prove of each part (access.rs, server.rs, tls.rs), this proves
//! of the whole: the CLI's config is what the server reads, the cookie and
//! the session key are what a restart keeps, and nothing secret is logged or
//! saved. Passkeys need a browser: web/scripts/auth-browser.mjs.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message};

const BIN: &str = env!("CARGO_BIN_EXE_trunk-pro");

/// A folder of the test's own, emptied first and removed after.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let p = std::env::temp_dir().join(format!("trunk-pro-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }

    fn config(&self) -> PathBuf {
        self.0.join("config.json")
    }

    fn read_config(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.config()).unwrap()).unwrap()
    }

    fn write_config(&self, v: &Value) {
        std::fs::write(self.config(), serde_json::to_string_pretty(v).unwrap()).unwrap();
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A port nothing listens on (as of now).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// `trunk-pro access <args>` on the config in `dir`: whether it worked, its stdout and its stderr.
fn access(dir: &Dir, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(BIN).arg("access").args(args).arg("--config").arg(dir.config()).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A recorder running, and what it has logged (stdout and stderr).
struct Recorder {
    child: Child,
    port: u16,
    log: Arc<Mutex<String>>,
}

impl Recorder {
    /// `trunk-pro` serving the config in `dir` on `port`, once it answers
    /// (`/api/version` over plain HTTP: answered on an HTTPS port too).
    fn start(dir: &Dir, port: u16) -> Recorder {
        let mut child = Command::new(BIN).arg("--config").arg(dir.config()).args(["--port", &port.to_string(), "--no-open"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let log = Arc::new(Mutex::new(String::new()));
        for out in [Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>, Box::new(child.stderr.take().unwrap())] {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut out = out;
                let mut buf = [0u8; 4096];
                while let Ok(n) = out.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    log.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            });
        }
        let mut r = Recorder { child, port, log };
        let t = Instant::now();
        while !raw(port, "GET /api/version HTTP/1.0\r\nHost: localhost\r\n\r\n").contains("\"app\":\"trunk-pro\"") {
            if let Ok(Some(status)) = r.child.try_wait() {
                panic!("trunk-pro exited ({status}) before it answered:\n{}", r.log());
            }
            assert!(t.elapsed() < Duration::from_secs(30), "trunk-pro didn't answer in 30 s:\n{}", r.log());
            std::thread::sleep(Duration::from_millis(50));
        }
        r
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().clone()
    }

    fn url(&self, path: &str) -> String {
        format!("http://localhost:{}{path}", self.port)
    }

    fn stop(mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.log()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `request` as plain bytes to `port` on localhost: the whole answer (empty if none).
fn raw(port: u16, request: &str) -> String {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) else { return String::new() };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    if s.write_all(request.as_bytes()).is_err() {
        return String::new();
    }
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// An answer: its status, headers (lowercase names) and body.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

fn agent(tls_root: Option<&Path>) -> ureq::Agent {
    let mut c = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).timeout_global(Some(Duration::from_secs(10)));
    if let Some(root) = tls_root {
        let cert = ureq::tls::Certificate::from_pem(&std::fs::read(root).unwrap()).unwrap();
        c = c.tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::new_with_certs(&[cert])).build());
    }
    c.build().into()
}

/// `method url` with `headers` and `body`, by `agent`.
fn ask(agent: &ureq::Agent, method: &str, url: &str, headers: &[(&str, &str)], body: Option<&str>) -> Answer {
    let mut r = match method {
        "POST" => headers.iter().fold(agent.post(url), |r, (k, v)| r.header(*k, *v)).send(body.unwrap_or_default()),
        _ => headers.iter().fold(agent.get(url), |r, (k, v)| r.header(*k, *v)).call(),
    }
    .unwrap_or_else(|e| panic!("{method} {url}: {e}"));
    let headers = r.headers().iter().map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
    Answer { status: r.status().as_u16(), headers, body: r.body_mut().read_to_string().unwrap_or_default() }
}

fn get(url: &str, headers: &[(&str, &str)]) -> Answer {
    ask(&agent(None), "GET", url, headers, None)
}

fn post(url: &str, headers: &[(&str, &str)], body: &str) -> Answer {
    ask(&agent(None), "POST", url, headers, Some(body))
}

type Socket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>;

/// A WebSocket to `port` with `headers`: the messages it was greeted with up to and with its `hello`; or the refusal's status.
fn socket(port: u16, headers: &[(&str, &str)]) -> Result<(Socket, Vec<Value>), u16> {
    let mut req = format!("ws://localhost:{port}/api/ws").into_client_request().unwrap();
    for (k, v) in headers {
        req.headers_mut().insert(tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
    }
    let (mut ws, _) = match tungstenite::connect(req) {
        Ok(c) => c,
        Err(tungstenite::Error::Http(r)) => return Err(r.status().as_u16()),
        Err(e) => panic!("{e}"),
    };
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    }
    let mut greeted = vec![];
    loop {
        let v: Value = match ws.read().unwrap() {
            Message::Text(t) => serde_json::from_str(&t).unwrap(),
            _ => continue,
        };
        let hello = v["type"] == "hello";
        greeted.push(v);
        if hello {
            return Ok((ws, greeted));
        }
    }
}

/// Send `command` on `ws`, and read until a message `until` says it's the answer.
fn command(ws: &mut Socket, command: Value, until: impl Fn(&Value) -> bool) -> Value {
    ws.send(Message::text(command.to_string())).unwrap();
    loop {
        if let Message::Text(t) = ws.read().unwrap() {
            let v: Value = serde_json::from_str(&t).unwrap();
            if until(&v) {
                return v;
            }
        }
    }
}

/// The session cookie a `Set-Cookie` sets (`name=value`).
fn cookie_of(a: &Answer) -> String {
    a.header("set-cookie").unwrap_or_else(|| panic!("no cookie: {}", a.body)).split(';').next().unwrap().to_string()
}

/// From the first user made to locked out and back, as the owner does it at
/// the recorder's computer: `access on` refused with nobody to turn it off
/// again; users made, each secret printed once and only its hash kept;
/// signing in off (everything open, as ever), then on (a restart takes it);
/// the sign-in page or a 401 for whoever hasn't signed in; a token as
/// Bearer, `?token=` or a cookie, each user's role on every route and
/// socket command, and no more than a listen interface's port allows; the
/// proxy's header from its address alone; signing out everywhere (from the
/// page, and with the CLI); the way back in with `access off` whatever else
/// is wrong; and no secret in the log, the config or the session key's mode.
#[test]
fn signing_in_from_the_command_line_to_the_socket() {
    let dir = Dir::new("signin");
    let (port, wall) = (free_port(), free_port());
    std::fs::create_dir_all(dir.0.join("wall")).unwrap();
    std::fs::write(dir.0.join("wall/index.html"), "<!doctype html><title>Wall</title>").unwrap();
    dir.write_config(&json!({ "server": { "port": port, "interfaces": { "wall": { "port": wall, "access": "listen", "path": "wall" } } }, "recording": { "captureDir": dir.0.join("calls").display().to_string() } }));

    // Nobody to turn it off again: refused.
    let (ok, _, why) = access(&dir, &["on"]);
    assert!(!ok && why.contains("control"), "{why}");
    let (ok, luke, said) = access(&dir, &["add", "luke"]);
    let luke = luke.trim().to_string();
    assert!(ok && luke.starts_with("trp_") && luke.len() == 68 && said.contains("luke [control]") && said.contains("shown this once"), "{luke} {said}");
    let (ok, kiosk, _) = access(&dir, &["add", "kiosk", "--access", "listen"]);
    let kiosk = kiosk.trim().to_string();
    assert!(ok && kiosk.starts_with("trp_"));
    assert!(!access(&dir, &["add", "luke"]).0, "a name taken");
    assert!(!access(&dir, &["add", "x", "--access", "admin"]).0, "no such role");
    let saved = std::fs::read_to_string(dir.config()).unwrap();
    assert!(!saved.contains(&luke) && !saved.contains(&kiosk) && saved.contains("\"hash\""), "the config keeps hashes, never secrets: {saved}");
    let (_, list, _) = access(&dir, &["list"]);
    assert!(list.contains("signing in off") && list.contains("luke [control] token") && list.contains("kiosk [listen] token") && !list.contains("trp_"), "{list}");

    // Off: everything open on the recorder's port, as ever; a listen interface's port listens.
    let r = Recorder::start(&dir, port);
    assert_eq!(get(&r.url("/api/v1/export/calls"), &[]).status, 200);
    assert_eq!(get(&r.url("/api/session"), &[]).json()["auth"], false);
    assert_eq!(get(&format!("http://localhost:{wall}/api/v1/export/calls"), &[]).status, 403, "an interface's port: its own access");
    let (_, greeted) = socket(port, &[]).unwrap();
    assert_eq!(greeted.last().unwrap()["access"], "control");
    r.stop();

    // On: taken once the recorder restarts.
    let (ok, said, _) = access(&dir, &["on"]);
    assert!(ok && said.contains("Signing in on"), "{said}");
    let r = Recorder::start(&dir, port);
    let page = get(&r.url("/"), &[("accept", "text/html")]);
    assert!(page.status == 401 && page.body.contains("/api/login") && page.header("x-frame-options") == Some("DENY") && page.header("cache-control") == Some("no-store"), "{}", page.status);
    let refused = get(&r.url("/api/v1/calls"), &[]);
    assert_eq!((refused.status, refused.header("www-authenticate")), (401, Some("Bearer")));
    assert_eq!(get(&r.url("/api/version"), &[]).status, 200, "a second launch still finds it");
    assert_eq!(get(&r.url("/signin"), &[]).status, 200);
    for a in [&page, &refused] {
        assert_eq!((a.header("referrer-policy"), a.header("x-content-type-options")), (Some("no-referrer"), Some("nosniff")));
    }
    assert_eq!(socket(port, &[]).err(), Some(401), "no socket");

    // Tokens: Bearer, ?token=; each user's role.
    let (as_luke, as_kiosk) = (format!("Bearer {luke}"), format!("Bearer {kiosk}"));
    assert_eq!(get(&r.url("/api/v1/export/calls"), &[("authorization", &as_luke)]).status, 200);
    assert_eq!(get(&r.url("/api/v1/export/calls"), &[("authorization", &as_kiosk)]).status, 403, "exporting is an Admin's");
    assert_eq!(get(&r.url(&format!("/api/v1/calls?token={kiosk}")), &[]).status, 200);
    assert_eq!(get(&r.url("/api/v1/calls"), &[("authorization", "Bearer trp_0000")]).status, 401);
    let hash = dir.read_config()["server"]["auth"]["users"]["luke"]["token"]["hash"].as_str().unwrap().to_string();
    assert_eq!(get(&r.url("/api/v1/calls"), &[("authorization", &format!("Bearer {hash}"))]).status, 401, "its hash is no secret");
    let wall_url = |p: &str| format!("http://localhost:{wall}{p}");
    assert_eq!(get(&wall_url("/api/v1/export/calls"), &[("authorization", &as_luke)]).status, 403, "an Admin through a listen port: listen");
    assert_eq!(get(&wall_url("/api/v1/calls"), &[]).status, 401);
    assert_eq!(get(&wall_url("/signin"), &[]).status, 200);
    assert_eq!(post(&wall_url("/api/passkey/login/start"), &[], "").status, 403, "passkeys: the recorder's own port alone");

    // Signing in: the cookie, its session (never the secret), as the browser keeps it.
    assert_eq!(post(&r.url("/api/login"), &[], r#"{"token":"trp_nope"}"#).status, 401);
    let signed = post(&r.url("/api/login"), &[("content-type", "application/json")], &json!({ "token": luke }).to_string());
    let set = signed.header("set-cookie").unwrap().to_string();
    assert!(signed.status == 200 && set.starts_with("trp_") && set.contains("; Path=/; Max-Age=2592000; HttpOnly; SameSite=Lax") && !set.contains("Secure") && !set.contains(&luke), "{set}");
    assert_eq!(signed.json(), json!({ "access": "control", "user": "luke" }));
    let cookie = cookie_of(&signed);
    assert_eq!(get(&r.url("/api/session"), &[("cookie", &cookie)]).json()["user"], "luke");
    assert_eq!(get(&r.url("/api/session"), &[("cookie", &cookie), ("sec-fetch-site", "same-site")]).json()["user"], Value::Null, "another port's page: its cookie doesn't count");
    let mut forged = cookie.clone();
    forged.replace_range(forged.find("luke").unwrap()..forged.find("luke").unwrap() + 4, "kios");
    assert_eq!(get(&r.url("/api/session"), &[("cookie", &forged)]).json()["user"], Value::Null, "changed: nobody");

    // Sockets: greeted with what they may do, as whom; a View user refused what changes things.
    let (mut admin, greeted) = socket(port, &[("cookie", &cookie)]).unwrap();
    let hello = greeted.last().unwrap();
    assert!(hello["access"] == "control" && hello["user"] == "luke" && hello["configPath"].is_string(), "{hello}");
    let full = greeted.iter().find(|m| m["type"] == "config").unwrap();
    assert!(full["config"]["server"]["auth"]["users"]["luke"].is_object(), "an Admin sees the users");
    let (mut view, greeted) = socket(port, &[("authorization", &as_kiosk)]).unwrap();
    let hello = greeted.last().unwrap();
    assert!(hello["access"] == "listen" && hello["user"] == "kiosk" && hello["configPath"].is_null(), "{hello}");
    let config = greeted.iter().find(|m| m["type"] == "config").unwrap();
    assert!(config["config"]["server"].get("auth").is_none() && !config.to_string().contains(&hash), "a View user's config has no users");
    for c in [json!({ "type": "createToken", "id": 1, "name": "intruder", "access": "control" }), json!({ "type": "start" }), json!({ "type": "listDir" }), json!({ "type": "quit" })] {
        let refused = command(&mut view, c.clone(), |v| v["type"] == "error");
        assert!(refused["message"].as_str().unwrap().contains("\"kiosk\" may only listen"), "{c}: {refused}");
    }
    let answered = command(&mut view, json!({ "type": "callsQuery", "id": 7 }), |v| v["type"] == "callsResult" || v["type"] == "error");
    assert_eq!(answered["type"], "callsResult", "looking is a View user's: {answered}");
    assert!(dir.read_config()["server"]["auth"]["users"].get("intruder").is_none(), "nothing refused was done");
    let (_, greeted) = socket(wall, &[("authorization", &as_luke)]).unwrap();
    assert_eq!(greeted.last().unwrap()["access"], "listen", "an Admin through a listen port");

    // The proxy in front, set from the page: its header from its address alone.
    let full_config = full["config"].clone();
    let version = full["version"].as_u64().unwrap();
    let mut c = full_config.clone();
    c["server"]["auth"]["proxy"] = json!({ "from": ["192.0.2.1"], "users": { "luke@example.com": "luke" } });
    let saved = command(&mut admin, json!({ "type": "setConfig", "config": c, "version": version }), |v| v["type"] == "configSaved");
    assert!(saved["error"].is_null(), "{saved}");
    assert_eq!(get(&r.url("/api/session"), &[("remote-user", "luke@example.com")]).json()["user"], Value::Null, "127.0.0.1 isn't the proxy's address");
    let (mut admin, greeted) = socket(port, &[("cookie", &cookie)]).unwrap();
    let full = greeted.iter().find(|m| m["type"] == "config").unwrap();
    let mut c = full["config"].clone();
    c["server"]["auth"]["proxy"]["from"] = json!(["127.0.0.1"]);
    let saved = command(&mut admin, json!({ "type": "setConfig", "config": c.clone(), "version": full["version"] }), |v| v["type"] == "configSaved");
    assert!(saved["error"].is_null(), "{saved}");
    let by_proxy = get(&r.url("/api/session"), &[("remote-user", "luke@example.com")]).json();
    assert_eq!((&by_proxy["user"], &by_proxy["proxy"], &by_proxy["access"]), (&json!("luke"), &json!(true), &json!("control")), "{by_proxy}");
    c["server"]["auth"]["proxy"]["from"] = json!(["0.0.0.0/0"]);
    let refused = command(&mut admin, json!({ "type": "setConfig", "config": c, "version": saved["version"] }), |v| v["type"] == "configSaved");
    assert!(refused["error"].as_str().unwrap_or("").contains("every address"), "{refused}");

    // Signed out everywhere, from the page: this browser's cookie, and every other session of luke's until now.
    let other = cookie_of(&post(&r.url("/api/login"), &[], &luke));
    std::thread::sleep(Duration::from_millis(1100));
    let out = post(&r.url("/api/logout"), &[("cookie", &cookie)], r#"{"all":true}"#);
    assert!(out.status == 204 && out.header("set-cookie").unwrap().contains("Max-Age=0"), "{}", out.status);
    for c in [&cookie, &other] {
        assert_eq!(get(&r.url("/api/session"), &[("cookie", c)]).json()["user"], Value::Null, "signed out");
    }
    assert_eq!(get(&r.url("/api/v1/calls"), &[("authorization", &as_luke)]).status, 200, "a token sent each time still counts");
    assert!(dir.read_config()["server"]["auth"]["users"]["luke"]["signedOut"].is_i64());
    let kept = cookie_of(&post(&r.url("/api/login"), &[], &kiosk));
    let log = r.stop();

    // A restart keeps the session key: a cookie signed in before still counts; signed out with the CLI, it doesn't.
    let key = dir.0.join("session-key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600, "the session key: its owner's alone");
    }
    let key_hex = std::fs::read_to_string(&key).unwrap();
    assert_eq!(key_hex.trim().len(), 64);
    let r = Recorder::start(&dir, port);
    assert_eq!(get(&r.url("/api/session"), &[("cookie", &kept)]).json()["user"], "kiosk", "kept across a restart");
    assert_eq!(get(&r.url("/api/session"), &[("remote-user", "luke@example.com")]).json()["user"], "luke", "the proxy, as saved");
    let log2 = r.stop();
    std::thread::sleep(Duration::from_millis(1100));
    let (ok, said, _) = access(&dir, &["signout", "kiosk"]);
    assert!(ok && said.contains("signed out everywhere"), "{said}");
    let r = Recorder::start(&dir, port);
    assert_eq!(get(&r.url("/api/session"), &[("cookie", &kept)]).json()["user"], Value::Null, "signed out with the CLI");
    assert_eq!(get(&r.url("/api/session"), &[("authorization", &as_kiosk)]).json()["user"], "kiosk");
    let log3 = r.stop();

    // The way back in: the last Admin can't be removed while signing in is on; off, whatever else is wrong; then anything.
    let (ok, _, why) = access(&dir, &["remove", "luke"]);
    assert!(!ok && why.contains("access off"), "{why}");
    let mut c = dir.read_config();
    c["server"]["auth"]["users"]["luke"]["passkeys"] = json!([{ "name": "Mac Chrome", "created": 1_794_000_000, "id": "AQID", "key": {} }]);
    c["server"]["auth"]["proxy"]["from"] = json!(["0.0.0.0/0"]);
    dir.write_config(&c);
    // (A proxy set so by hand: the recorder says why as it starts, and its header signs nobody in.)
    let r = Recorder::start(&dir, port);
    assert_eq!(get(&r.url("/api/session"), &[("remote-user", "luke")]).json()["user"], Value::Null, "every address: nobody");
    let log4 = r.stop();
    assert!(log4.contains("every address"), "said as it starts: {log4}");
    let (_, list, _) = access(&dir, &["list"]);
    assert!(list.contains("passkey \"Mac Chrome\"") && !list.contains("AQID"), "{list}");
    let (ok, _, why) = access(&dir, &["passkey", "remove", "luke", "iPhone"]);
    assert!(!ok && why.contains("theirs: Mac Chrome"), "{why}");
    let (ok, said, why) = access(&dir, &["off"]);
    assert!(ok && said.contains("Signing in off"), "off, a proxy set as it can't be and all: {said} {why}");
    let mut c = dir.read_config();
    c["server"]["auth"]["proxy"]["from"] = json!(["127.0.0.1"]);
    dir.write_config(&c);
    let (ok, said, _) = access(&dir, &["passkey", "remove", "luke", "Mac Chrome"]);
    assert!(ok && said.contains("removed"), "{said}");
    assert!(access(&dir, &["remove", "luke"]).0, "off: anyone may go");
    assert!(!access(&dir, &["remove", "luke"]).0, "no such user now");
    assert!(!access(&dir, &["signout", "nobody"]).0);

    // Nothing secret logged, or saved.
    let saved = std::fs::read_to_string(dir.config()).unwrap();
    for (what, text) in [("the log", format!("{log}{log2}{log3}{log4}")), ("the config", saved)] {
        for secret in [&luke, &kiosk, &key_hex.trim().to_string(), &cookie, &kept] {
            let s = secret.rsplit_once('=').map_or(secret.as_str(), |(_, v)| v);
            assert!(!text.contains(s), "{what} has a secret ({}…)", &s[..12.min(s.len())]);
        }
    }
    assert!(log.contains("User luke: setConfig"), "who changed what is logged, by its kind: {log}");
}

/// Served over HTTPS (the fixtures' certificate for localhost): every port
/// is, signing in gives a `Secure` cookie, HSTS when asked for; plain HTTP to
/// the port gets the version, a redirect, or a refusal for what carried a
/// credential (sent in the clear already). A pair that isn't one stops the
/// recorder starting: never plain HTTP in its place.
#[test]
fn over_https_on_the_real_binary() {
    let dir = Dir::new("https");
    let (port, wall) = (free_port(), free_port());
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tls");
    let (cert, key, other_key) = (fixtures.join("a.crt"), fixtures.join("a.key"), fixtures.join("b.key"));
    std::fs::create_dir_all(dir.0.join("wall")).unwrap();
    std::fs::write(dir.0.join("wall/index.html"), "wall").unwrap();
    let config = |key: &Path| json!({ "server": { "port": port, "tls": { "cert": cert.display().to_string(), "key": key.display().to_string(), "hsts": true }, "interfaces": { "wall": { "port": wall, "access": "listen", "path": "wall" } } } });
    // (Not on macOS: started with no terminal, a startup failure is shown in a dialog too, which waits
    // for someone to close it. On Linux its notification goes nowhere without a desktop session.)
    if !cfg!(target_os = "macos") {
        dir.write_config(&config(&other_key));
        let out = Command::new(BIN).arg("--config").arg(dir.config()).args(["--port", &port.to_string(), "--no-open"]).env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY").env("DBUS_SESSION_BUS_ADDRESS", "").output().unwrap();
        let said = String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
        assert!(!out.status.success() && said.contains("HTTPS") && said.contains("isn't the certificate"), "a pair that isn't one: {said}");
        assert!(raw(port, "GET /api/version HTTP/1.0\r\n\r\n").is_empty(), "nothing served");
    }

    dir.write_config(&config(&key));
    let (_, luke, _) = access(&dir, &["add", "luke"]);
    let luke = luke.trim().to_string();
    assert!(access(&dir, &["on"]).0);
    let r = Recorder::start(&dir, port);
    let tls = agent(Some(&cert));
    let https = |p: u16, path: &str| format!("https://localhost:{p}{path}");
    let v = ask(&tls, "GET", &https(port, "/api/version"), &[], None);
    assert!(v.status == 200 && v.header("strict-transport-security") == Some("max-age=31536000"), "{:?}", v.headers);
    assert_eq!(ask(&tls, "GET", &https(port, "/api/v1/calls"), &[], None).status, 401);
    let signed = ask(&tls, "POST", &https(port, "/api/login"), &[], Some(&luke));
    assert!(signed.status == 200 && signed.header("set-cookie").unwrap().ends_with("; Secure"), "{:?}", signed.headers);
    let cookie = cookie_of(&signed);
    assert_eq!(ask(&tls, "GET", &https(port, "/api/session"), &[("cookie", &cookie)], None).json()["user"], "luke");
    assert_eq!(ask(&tls, "GET", &https(wall, "/api/version"), &[], None).status, 200, "an interface's port too");
    // Plain HTTP to the HTTPS port.
    let version = raw(port, "GET /api/version HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(version.starts_with("HTTP/1.1 200") && version.contains("\"app\":\"trunk-pro\"") && !version.contains("strict-transport-security"), "{version}");
    let moved = raw(port, &format!("GET /calls?x=1 HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"));
    assert!(moved.starts_with("HTTP/1.1 308") && moved.contains(&format!("Location: https://localhost:{port}/calls?x=1\r\n")), "{moved}");
    for (req, kind) in [(format!("GET /api/v1/calls HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {luke}\r\n\r\n"), "an Authorization header"),
                        (format!("GET /api/v1/calls?token={luke} HTTP/1.1\r\nHost: localhost\r\n\r\n"), "?token="),
                        (format!("GET / HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n"), "a session cookie"),
                        (format!("POST /api/login HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{luke}", luke.len()), "signing in")] {
        let res = raw(port, &req);
        assert!(res.starts_with("HTTP/1.1 400") && res.contains(kind) && !res.contains("Location"), "{kind}: {res}");
    }
    assert!(raw(port, &format!("GET / HTTP/1.1\r\nHost: localhost\r\nX-Pad: {}\r\n\r\n", "a".repeat(9000))).is_empty(), "a request longer than ours: closed, unanswered");
    let silent = TcpStream::connect(("127.0.0.1", port)).unwrap();
    assert_eq!(ask(&tls, "GET", &https(port, "/api/version"), &[], None).status, 200, "a connection that says nothing holds up nobody");
    drop(silent);
    let log = r.stop();
    assert!(log.contains("came over plain HTTP") && !log.contains(&luke), "told, and no secret in it: {log}");
}
