//! The browser interface: an embedded web UI, recorded calls under /calls/,
//! and a WebSocket (/api/ws) carrying JSON messages both ways plus binary
//! live-audio frames `[2][u16 system][u32 call id][u32 talkgroup][i16…]`
//! (8 kHz) to connections that asked to listen.
//!
//! The messages are defined in web/src/protocol.ts and documented for
//! anyone writing their own interface in docs/api/, which the server also
//! serves: /api/docs (README.md), /api/llms.txt, /api/client.js,
//! /api/examples/…, /api/protocol.ts and /api/schema (its JSON Schema; the
//! tests in protocol_tests.rs check what is sent against it). GET
//! /api/version identifies a running instance.
//!
//! Interfaces: the built-in one at /builtin/, the user's own (folders named
//! in `server.interfaces`) at /ui/<name>/, a list of them at /ui/ and
//! /api/interfaces; / shows `server.home` (or `--ui <folder>`), else the
//! built-in one.
//!
//! A connection gets `hello`, then `plugins`; then what everyone hears
//! (`state`, `status` ~2/s, `spectrum` ~7/s per source, `log`, `concluded`,
//! …) and the answers to its own commands.
//!
//! Every request passes [`guard`]: pages from other sites are refused unless
//! listed in `server.allowedOrigins`.
//!
//! Accounts (see [`crate::auth`]): POST /api/login `{username, password}` sets
//! the session cookie, POST /api/logout ends it, GET /api/whoami says who's
//! logged in, and POST /api/setup makes the first admin (this computer only,
//! while there are no accounts). The WebSocket, /calls/, /api/interfaces and
//! /ui/ need a login. Viewers get the config without plugin settings and can
//! only `listen` and `changePassword`; admins also have `accounts`,
//! `addAccount`, `removeAccount`, `setAccountRole` and `setAccountPassword`.

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path as UrlPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use rust_embed::RustEmbed;
use serde_json::{json, Value};

use crate::auth::{LoginError, Role, Who};
use crate::config::Config;
use crate::runtime::{self, publish, Ctx, Out};
use crate::sdr;

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
#[allow_missing = true]
struct Ui;

/// The API's documentation, client and examples, served under /api/.
#[derive(RustEmbed)]
#[folder = "../../docs/api/"]
struct ApiDocs;

const PROTOCOL: &str = include_str!("../../../web/src/protocol.ts");

/// What `GET /api/version` answers — how a second launch recognises us.
pub const APP_ID: &str = "trunk-pro";

/// Serve until `quit` from a browser, Ctrl-C or SIGTERM. Recording is stopped
/// first either way, so calls in progress are written out.
pub async fn serve(ctx: Arc<Ctx>, listener: std::net::TcpListener) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    let loopback = listener.local_addr()?.ip().is_loopback();
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let app = Router::new()
        .route("/api/ws", get(ws))
        .route("/api/version", get(|| async { axum::Json(json!({ "app": APP_ID, "version": env!("CARGO_PKG_VERSION") })) }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/whoami", get(whoami))
        .route("/api/setup", post(setup))
        .route("/api/stats/export", get(stats_export))
        .route("/api/docs", get(|| async { api_file("README.md") }))
        .route("/api/schema", get(|| async { api_file("protocol.schema.json") }))
        .route("/api/protocol.ts", get(|| async { ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], PROTOCOL).into_response() }))
        .route("/api/interfaces", get(interfaces_json))
        .route("/api/{*path}", get(|UrlPath(p): UrlPath<String>| async move { api_file(&p) }))
        .route("/calls/{*path}", get(call_file))
        .route("/builtin", get(|| async { Redirect::permanent("/builtin/") }))
        .route("/builtin/", get(|| async { builtin("") }))
        .route("/builtin/{*path}", get(|UrlPath(p): UrlPath<String>| async move { builtin(&p) }))
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(ui_index))
        .route("/ui/{name}", get(|UrlPath(n): UrlPath<String>| async move { Redirect::permanent(&format!("/ui/{}/", url_segment(&n))) }))
        .route("/ui/{name}/", get(|State(ctx): State<Arc<Ctx>>, UrlPath(n): UrlPath<String>, uri: Uri| async move { ui_file(&ctx, &n, "", &uri).await }))
        .route("/ui/{name}/{*path}", get(|State(ctx): State<Arc<Ctx>>, UrlPath((n, p)): UrlPath<(String, String)>, uri: Uri| async move { ui_file(&ctx, &n, &p, &uri).await }))
        .fallback(home)
        .layer(middleware::from_fn_with_state((ctx.clone(), loopback), guard))
        .with_state(ctx.clone());
    #[cfg(unix)]
    tokio::spawn(reopen_log_on_hangup());
    let ctx2 = ctx.clone();
    let shutdown = async move {
        tokio::select! {
            _ = ctx2.quit.notified() => {}
            _ = tokio::signal::ctrl_c() => log::info!("Caught an Exit Signal..."),
            _ = terminate() => log::info!("Caught an Exit Signal..."),
        }
        let ctx3 = ctx2.clone();
        let _ = tokio::task::spawn_blocking(move || {
            stop_survey(&ctx3);
            if let Some(r) = ctx3.runner.lock().unwrap().take() {
                r.stop();
            }
        })
        .await;
        publish(&ctx2.hub, json!({ "type": "quit" }));
        // Let the sessions deliver it before the connections close.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown).await
}

/// SIGHUP: reopen the log file (logrotate moved it).
#[cfg(unix)]
async fn reopen_log_on_hangup() {
    let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) else { return };
    while s.recv().await.is_some() {
        crate::logging::reopen();
        log::info!("Received SIGHUP signal - log file reopened");
    }
}

#[cfg(unix)]
async fn terminate() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}
#[cfg(not(unix))]
async fn terminate() {
    std::future::pending::<()>().await
}

/// Who may use the interface. Any web page a browser shows can reach
/// ws://localhost:8080 too, and must not drive the recorder (change its
/// config, list its folders, quit it). What a page asks for carries its
/// `Origin`: it must be this server's own, or in `server.allowedOrigins` (a
/// custom interface served elsewhere — its requests then get CORS headers).
/// Programs that aren't browsers send no Origin and are let in. Bound to
/// localhost, the `Host` must be localhost (or an allowed origin's host) as
/// well: a site whose name was pointed at 127.0.0.1 (DNS rebinding) is
/// still another site.
async fn guard(State((ctx, loopback)): State<(Arc<Ctx>, bool)>, req: Request, next: Next) -> Response {
    // (In a block: a closure borrowing the request mustn't live across the await.)
    let (host, origin) = {
        let header = |name| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        (header(header::HOST), header(header::ORIGIN))
    };
    let allowed = ctx.config.lock().unwrap().server.allowed_origins.clone();
    match admit(loopback, host.as_deref(), origin.as_deref(), &allowed) {
        Ok(cors) => {
            let mut res = next.run(req).await;
            if let (true, Some(v)) = (cors, origin.and_then(|o| HeaderValue::from_str(&o).ok())) {
                res.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
                res.headers_mut().append(header::VARY, HeaderValue::from_static("Origin"));
            }
            res
        }
        Err(why) => {
            // Once each: a refused page may keep reconnecting.
            static TOLD: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
            let mut told = TOLD.lock().unwrap();
            if told.len() < 50 && !told.contains(&why) {
                log::warn!("Refused {why}");
                told.push(why.clone());
            }
            (StatusCode::FORBIDDEN, format!("Refused {why}\n")).into_response()
        }
    }
}

/// [`guard`]'s verdict: Ok(whether the answer needs CORS headers) or Err(why not).
fn admit(loopback: bool, host: Option<&str>, origin: Option<&str>, allowed: &[String]) -> Result<bool, String> {
    let norm = |o: &str| o.trim().trim_end_matches('/').to_ascii_lowercase();
    let allowed: Vec<String> = allowed.iter().map(|o| norm(o)).collect();
    if let Some(host) = host {
        let name = host_name(host);
        let named = allowed.iter().any(|o| o.split_once("://").is_some_and(|(_, a)| host_name(a) == name));
        if loopback && !is_loopback_name(&name) && !named {
            return Err(format!("a request for {host}: this server only answers to localhost (add the page's origin to server.allowedOrigins in the config to allow it)."));
        }
    }
    let Some(origin) = origin else { return Ok(false) };
    let o = norm(origin);
    if host.is_some_and(|h| o.split_once("://").is_some_and(|(_, a)| a.eq_ignore_ascii_case(h))) {
        return Ok(false);
    }
    if allowed.iter().any(|a| *a == o || a == "*") {
        return Ok(true);
    }
    Err(format!("a page from {origin}: add it to server.allowedOrigins in the config to let it use the interface."))
}

/// "Localhost:8080" → "localhost", "[::1]:8080" → "::1".
fn host_name(host: &str) -> String {
    let h = host.trim().to_ascii_lowercase();
    let h = match h.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or("").to_string(),
        None if h.matches(':').count() == 1 => h.split(':').next().unwrap_or("").to_string(),
        None => h,
    };
    h.trim_end_matches('.').to_string()
}

fn is_loopback_name(name: &str) -> bool {
    name == "localhost" || name.ends_with(".localhost") || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The built-in interface's file `path` (its index.html for anything else:
/// it routes with the URL's #).
fn builtin(path: &str) -> Response {
    let path = if path.is_empty() { "index.html" } else { path };
    match Ui::get(path).or_else(|| Ui::get("index.html")) {
        Some(f) => {
            let mime = mime_guess::from_path(if Ui::get(path).is_some() { path } else { "index.html" }).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], f.data).into_response()
        }
        None => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
            "<h1>Trunk Recorder Pro</h1><p>The web interface wasn't built into this binary. Run <code>npm run build</code> in <code>web/</code>, then rebuild.</p>",
        )
            .into_response(),
    }
}

/// `/`: the home interface — `--ui <folder>`, else `server.home`, else the built-in one.
async fn home(State(ctx): State<Arc<Ctx>>, uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let dir = ctx.home_dir.clone().or_else(|| {
        let home = ctx.config.lock().unwrap().server.home.clone();
        (!home.is_empty()).then(|| interface_dir(&ctx, &home).ok()).flatten()
    });
    match (dir, percent_decode(raw)) {
        (None, _) => builtin(raw),
        (Some(dir), Some(rel)) => folder_file(&dir, &rel, &uri).await,
        (Some(_), None) => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// A file of interface `name` (/ui/<name>/<path>).
async fn ui_file(ctx: &Ctx, name: &str, path: &str, uri: &Uri) -> Response {
    match interface_dir(ctx, name) {
        Ok(dir) => folder_file(&dir, path, uri).await,
        Err(e) => not_found(&e),
    }
}

/// Interface `name`'s folder (it may not exist).
fn interface_dir(ctx: &Ctx, name: &str) -> Result<PathBuf, String> {
    let cfg = ctx.config.lock().unwrap();
    let i = cfg.server.interfaces.iter().find(|i| i.name == name).ok_or_else(|| format!("There's no interface named “{name}” in the config."))?;
    Ok(interface_path(&ctx.config_path, &i.path))
}

/// An interface's folder: absolute, ~/…, or relative to the config file's folder.
fn interface_path(config_path: &Path, path: &str) -> PathBuf {
    let path = path.trim();
    if let Some(rest) = path.strip_prefix("~/") {
        return trunk_app::config::home().join(rest);
    }
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        config_path.parent().unwrap_or(Path::new(".")).join(p)
    }
}

/// File `rel` of an interface's folder, as it is on disk now (edits show on
/// reload): a folder's index.html, and the root's index.html for a path
/// that isn't a file and has no extension (a page that routes itself).
/// Nothing outside the folder, links included.
async fn folder_file(root: &Path, rel: &str, uri: &Uri) -> Response {
    let rel = Path::new(rel);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(root) = tokio::fs::canonicalize(root).await else {
        return not_found(&format!("The interface's folder {} isn't there.", root.display()));
    };
    let mut path = root.join(rel);
    if tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_dir()) {
        // Its pages' relative links need the slash.
        if !uri.path().ends_with('/') {
            return Redirect::permanent(&format!("{}/", uri.path())).into_response();
        }
        path = path.join("index.html");
    } else if rel.extension().is_none() && tokio::fs::metadata(&path).await.is_err() {
        path = root.join("index.html");
    }
    let real = match tokio::fs::canonicalize(&path).await {
        Ok(r) if r.starts_with(&root) => r,
        _ => return not_found(&format!("{} isn't in the interface's folder {}.", uri.path(), root.display())),
    };
    match tokio::fs::read(&real).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime_of(&real.to_string_lossy())), (header::CACHE_CONTROL, "no-cache".to_string())], bytes).into_response(),
        Err(e) => not_found(&format!("{}: {e}", real.display())),
    }
}

fn not_found(why: &str) -> Response {
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Not found</title><body style=\"font:15px system-ui;margin:40px\"><h1>Not found</h1><p>{}</p>\
         <p><a href=\"/builtin/\">Trunk Recorder Pro</a> · <a href=\"/ui/\">Interfaces</a></p>",
        html_escape(why)
    );
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response()
}

/// A file of docs/api (built in): /api/docs is its README.md.
fn api_file(path: &str) -> Response {
    match ApiDocs::get(path) {
        Some(f) => ([(header::CONTENT_TYPE, mime_of(path))], f.data).into_response(),
        None => not_found(&format!("/api/{path}: no such file.")),
    }
}

fn mime_of(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let mime = if lower.ends_with(".md") {
        "text/markdown".to_string()
    } else if lower.ends_with(".mjs") || lower.ends_with(".js") {
        "text/javascript".to_string()
    } else {
        mime_guess::from_path(path).first_or_octet_stream().essence_str().to_string()
    };
    let text = mime.starts_with("text/") || mime.ends_with("json") || mime.ends_with("javascript");
    if text { format!("{mime}; charset=utf-8") } else { mime }
}

/// The interfaces: `GET /api/interfaces`.
async fn interfaces_json(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if who(&ctx, &headers, peer).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(interfaces_list(&ctx)).into_response()
}

fn interfaces_list(ctx: &Ctx) -> Value {
    let cfg = ctx.config.lock().unwrap().clone();
    let list: Vec<Value> = cfg
        .server
        .interfaces
        .iter()
        .map(|i| {
            let dir = interface_path(&ctx.config_path, &i.path);
            let problem = trunk_app::config::Interface::name_problem(&i.name).or_else(|| {
                if !dir.is_dir() {
                    Some(format!("The folder {} isn't there.", dir.display()))
                } else if !dir.join("index.html").is_file() {
                    Some(format!("{} has no index.html.", dir.display()))
                } else {
                    None
                }
            });
            json!({ "name": i.name, "path": i.path, "folder": dir.display().to_string(), "url": format!("/ui/{}/", url_segment(&i.name)), "problem": problem })
        })
        .collect();
    let home = match &ctx.home_dir {
        Some(d) => json!({ "folder": d.display().to_string() }),
        None if cfg.server.home.is_empty() => json!("builtin"),
        None => json!(cfg.server.home),
    };
    json!({ "home": home, "builtin": "/builtin/", "interfaces": list })
}

/// `/ui/`: the interfaces, the examples and the docs, as links.
async fn ui_index(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    // It shows folders on this computer: log in (on the built-in interface) first.
    if who(&ctx, &headers, peer).is_none() {
        return Redirect::to("/builtin/").into_response();
    }
    let v = interfaces_list(&ctx);
    let mut rows = String::from("<li><a href=\"/builtin/\">Trunk Recorder Pro</a> <span>the built-in interface</span></li>");
    for i in v["interfaces"].as_array().into_iter().flatten() {
        let (name, folder) = (i["name"].as_str().unwrap_or(""), i["folder"].as_str().unwrap_or(""));
        let note = match i["problem"].as_str() {
            Some(p) => format!("<b>{}</b>", html_escape(p)),
            None => html_escape(folder),
        };
        rows += &format!("<li><a href=\"/ui/{}/\">{}</a> <span>{note}</span></li>", url_segment(name), html_escape(name));
    }
    let mut examples = String::new();
    for f in ApiDocs::iter().filter(|f| f.starts_with("examples/") && f.ends_with(".html")) {
        examples += &format!("<li><a href=\"/api/{f}\">{}</a></li>", html_escape(f.trim_start_matches("examples/")));
    }
    let page = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>Interfaces</title>\
         <style>body{{font:15px system-ui;margin:40px auto;max-width:760px;padding:0 16px}}li{{margin:6px 0}}span{{color:#777;margin-left:8px}}</style>\
         <h1>Interfaces</h1><ul>{rows}</ul>\
         <p>Add your own in the config's <code>server.interfaces</code> (Setup → Recording → Interfaces), or run <code>trunk-pro --ui &lt;folder&gt;</code>.</p>\
         <h2>Examples</h2><ul>{examples}</ul>\
         <h2>Build your own</h2><ul><li><a href=\"/api/docs\">The API</a> (docs/api/README.md)</li><li><a href=\"/api/llms.txt\">For an LLM</a> (llms.txt)</li>\
         <li><a href=\"/api/client.js\">client.js</a></li><li><a href=\"/api/protocol.ts\">protocol.ts</a></li></ul>"
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A name as one URL path segment.
fn url_segment(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// "%20" → " "; None for bad escapes or bytes that aren't UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(s.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A recorded file, confined to the capture folder.
async fn call_file(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, UrlPath(path): UrlPath<String>) -> Response {
    if who(&ctx, &headers, peer).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let dir = PathBuf::from(&ctx.config.lock().unwrap().recording.capture_dir);
    let rel = Path::new(&path);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match tokio::fs::read(dir.join(rel)).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(rel).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn ws(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, up: WebSocketUpgrade) -> Response {
    // (Pages from other sites were refused by `guard`.)
    let Some(who) = who(&ctx, &headers, peer) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    up.on_upgrade(move |socket| session(ctx, socket, who))
}

// ─── Accounts ──────────────────────────────────────────────────────────────

const COOKIE: &str = "trpro_session";

/// Who sent `headers` from `peer` (None: nobody we let in).
fn who(ctx: &Ctx, headers: &HeaderMap, peer: SocketAddr) -> Option<Who> {
    ctx.accounts.who(peer.ip(), session_token(headers).as_deref())
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == COOKIE && !v.is_empty()).then(|| v.to_string())
    })
}

/// An Origin header, when there is one, names the host the request went to.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else { return true };
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    origin.split_once("://").map(|(_, h)| h) == Some(host)
}

/// A JSON body, refused unless it's sent as JSON (a form on another site can't).
fn json_body(headers: &HeaderMap, body: &[u8]) -> Option<Value> {
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct.starts_with("application/json") {
        return None;
    }
    serde_json::from_slice(body).ok()
}

fn who_json(ctx: &Ctx, who: Option<&Who>, local: bool) -> Value {
    json!({
        "user": who.map(|w| w.user.clone()),
        "role": who.map(|w| w.role.as_str()),
        // No accounts yet: this computer may make the first one.
        "setup": ctx.accounts.is_open(),
        "local": local,
        "problem": ctx.accounts.problem(),
    })
}

async fn whoami(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let w = who(&ctx, &headers, peer);
    let code = if w.is_some() { StatusCode::OK } else { StatusCode::UNAUTHORIZED };
    (code, axum::Json(who_json(&ctx, w.as_ref(), peer.ip().is_loopback()))).into_response()
}

fn session_cookie(token: &str, max_age: i64) -> String {
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}")
}

async fn login(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let Some(v) = json_body(&headers, &body) else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    };
    let (name, password) = (v["username"].as_str().unwrap_or("").to_string(), v["password"].as_str().unwrap_or("").to_string());
    let ctx2 = ctx.clone();
    let r = tokio::task::spawn_blocking(move || ctx2.accounts.login(&name, &password, peer.ip())).await;
    match r {
        Ok(Ok((token, w))) => {
            let mut res = axum::Json(who_json(&ctx, Some(&w), peer.ip().is_loopback())).into_response();
            if let Ok(c) = session_cookie(&token, 30 * 24 * 3600).parse() {
                res.headers_mut().insert(header::SET_COOKIE, c);
            }
            res
        }
        Ok(Err(LoginError::Wait(s))) => {
            (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, s.to_string())], axum::Json(json!({ "error": format!("Too many wrong passwords. Try again in {} minutes.", s.div_ceil(60)) })))
                .into_response()
        }
        _ => (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "Wrong name or password." }))).into_response(),
    }
}

async fn logout(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if let Some(w) = who(&ctx, &headers, peer) {
        ctx.accounts.logout(&w);
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    if let Ok(c) = session_cookie("", 0).parse() {
        res.headers_mut().insert(header::SET_COOKIE, c);
    }
    res
}

/// The first admin account: only from this computer, only while there are none.
async fn setup(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let Some(v) = json_body(&headers, &body) else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    };
    if !peer.ip().is_loopback() || !ctx.accounts.is_open() || !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (name, password) = (v["username"].as_str().unwrap_or("").to_string(), v["password"].as_str().unwrap_or("").to_string());
    let ctx2 = ctx.clone();
    let r = tokio::task::spawn_blocking(move || {
        ctx2.accounts.add(&name, Role::Admin, &password)?;
        ctx2.accounts.login(&name, &password, peer.ip()).map_err(|_| "Couldn't log in".to_string())
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match r {
        Ok((token, w)) => {
            let mut res = axum::Json(who_json(&ctx, Some(&w), true)).into_response();
            if let Ok(c) = session_cookie(&token, 30 * 24 * 3600).parse() {
                res.headers_mut().insert(header::SET_COOKIE, c);
            }
            res
        }
        Err(e) => (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": e }))).into_response(),
    }
}

/// The config as a viewer sees it: no plugin settings (keys, passwords) or paths.
fn redact_config(mut c: Value) -> Value {
    if let Some(p) = c.get_mut("plugins").and_then(Value::as_object_mut) {
        for e in p.values_mut() {
            if let Some(e) = e.as_object_mut() {
                e.remove("settings");
                e.remove("path");
            }
        }
    }
    for key in ["systems", "conventional"] {
        for s in c[key].as_array_mut().into_iter().flatten() {
            if let Some(s) = s.as_object_mut() {
                s.remove("plugins");
            }
        }
    }
    if let Some(r) = c.get_mut("recording").and_then(Value::as_object_mut) {
        r.insert("captureDir".into(), json!(""));
    }
    c
}

/// A message on its way to a viewer: the config and plugin list redacted.
fn for_viewer(text: &str) -> Option<String> {
    if !text.contains(r#""type":"config""#) && !text.contains(r#""type":"plugins""#) && !text.contains(r#""type":"hello""#) {
        return None;
    }
    let mut v: Value = serde_json::from_str(text).ok()?;
    match v["type"].as_str() {
        Some("config") | Some("hello") => {
            let c = v["config"].take();
            v["config"] = redact_config(c);
            if let Some(o) = v.as_object_mut() {
                o.remove("configPath");
            }
        }
        Some("plugins") => {
            for p in v["plugins"].as_array_mut().into_iter().flatten() {
                if let Some(p) = p.as_object_mut() {
                    // Paths and problems (which quote them) are for admins.
                    p.remove("path");
                    p.remove("unlistedFrom");
                    if p.get("problem").is_some_and(|x| !x.is_null()) {
                        p.insert("problem".into(), json!("unavailable"));
                    }
                }
            }
        }
        _ => return None,
    }
    Some(v.to_string())
}

/// The `accounts` message.
fn accounts_json(ctx: &Ctx) -> Value {
    let list: Vec<Value> = ctx.accounts.list().into_iter().map(|(name, role, sessions)| json!({ "name": name, "role": role.as_str(), "sessions": sessions })).collect();
    json!({ "type": "accounts", "accounts": list })
}

/// Commands a viewer may send.
fn viewer_may(kind: &str) -> bool {
    matches!(kind, "listen" | "plugins" | "changePassword" | "stats" | "statsHistory" | "affiliations" | "affiliationLinks")
}

/// Account commands (`who` is an admin, except for `changePassword`).
async fn account_command(ctx: &Arc<Ctx>, v: &Value, who: &Who) -> Option<Value> {
    let kind = v["type"].as_str()?.to_string();
    if kind == "accounts" {
        return Some(accounts_json(ctx));
    }
    let (ctx2, v, who) = (ctx.clone(), v.clone(), who.clone());
    let r = tokio::task::spawn_blocking(move || {
        let a = &ctx2.accounts;
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        match kind.as_str() {
            "addAccount" => a.add(&s("name"), Role::parse(&s("role")).ok_or("A role is admin or viewer.")?, &s("password")),
            "removeAccount" => a.remove(&s("name")),
            "setAccountRole" => a.set_role(&s("name"), Role::parse(&s("role")).ok_or("A role is admin or viewer.")?),
            "setAccountPassword" => a.set_password(&s("name"), &s("password"), if s("name") == who.user { who.session.as_deref() } else { None }),
            "changePassword" => {
                if who.user.is_empty() {
                    return Err("There are no accounts yet.".into());
                }
                if !a.check(&who.user, &s("old")) {
                    return Err("The current password isn't right.".into());
                }
                a.set_password(&who.user, &s("password"), who.session.as_deref())
            }
            _ => Ok(()),
        }
        .map(|_| kind)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match r {
        Ok(kind) => {
            // Everyone's list changes; the first account ends this computer's open access.
            publish(&ctx.hub, accounts_json(ctx));
            Some(json!({ "type": "notice", "message": if kind == "changePassword" { "Password changed." } else { "Saved." } }))
        }
        Err(e) => Some(json!({ "type": "error", "message": e })),
    }
}

/// Statistics queries (read-only; anyone logged in).
async fn stats_command(ctx: &Arc<Ctx>, v: &Value) -> Option<Value> {
    let (ctx2, v) = (ctx.clone(), v.clone());
    let r = tokio::task::spawn_blocking(move || {
        let st = &ctx2.stats;
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        let window = Some(s("window")).filter(|w| !w.is_empty()).unwrap_or_else(|| "24h".into());
        // `systems`: one, or a multi-site system's sites; `system` names them in the answer.
        let key = s("system");
        let systems: Vec<String> = match v["systems"].as_array() {
            Some(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            None => vec![key.clone()],
        };
        match v["type"].as_str().unwrap_or("") {
            "stats" => st.summary(&key, &systems, &window).map(|mut j| {
                j["dropped"] = json!(st.dropped());
                j
            }),
            "statsHistory" => st.history(&key, &systems, &s("kind"), v["id"].as_i64().unwrap_or(0), &window),
            "affiliations" => {
                let (view, search) = (s("view"), s("search"));
                let page = crate::stats::AffiliationPage { view: &view, search: &search, id: v["id"].as_i64(), offset: v["offset"].as_i64().unwrap_or(0), limit: v["limit"].as_i64().unwrap_or(200) };
                st.affiliations(&key, &systems, &page)
            }
            "affiliationLinks" => st.links(&key, &systems, &s("view"), v["id"].as_i64().unwrap_or(0)),
            _ => Err("unknown".into()),
        }
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    Some(r.unwrap_or_else(|e| json!({ "type": "error", "message": format!("Statistics: {e}") })))
}

/// Everything known about a system's (or several sites') radios, talkgroups and affiliations, as a JSON download.
async fn stats_export(State(ctx): State<Arc<Ctx>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, uri: Uri) -> Response {
    if who(&ctx, &headers, peer).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // ?system=a, or ?system=a&system=b for a multi-site system's sites.
    let systems: Vec<String> = uri.query().unwrap_or("").split('&').filter_map(|kv| kv.strip_prefix("system=")).map(query_decode).collect();
    let name = systems.join("+");
    let ctx2 = ctx.clone();
    match tokio::task::spawn_blocking(move || ctx2.stats.export(&systems)).await {
        Ok(Ok(v)) => {
            let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
            (
                [(header::CONTENT_TYPE, "application/json".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}-affiliations.json\""))],
                v.to_string(),
            )
                .into_response()
        }
        Ok(Err(e)) => (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A query value: "%20" and "+" → " "; a bad escape is kept as it is.
fn query_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |i: usize| b.get(i).and_then(|&c| (c as char).to_digit(16));
    let mut o = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                o.push((h * 16 + l) as u8);
                i += 3;
            }
            (b'+', ..) => {
                o.push(b' ');
                i += 1;
            }
            (c, ..) => {
                o.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&o).into_owned()
}


/// End a running survey (frees its radio) and tell the browsers.
fn stop_survey(ctx: &Ctx) {
    let r = ctx.survey.lock().unwrap().take();
    if let Some(r) = r {
        r.stop();
        *ctx.survey_last.lock().unwrap() = None;
        publish(&ctx.hub, crate::survey::idle_json());
    }
}

pub(crate) fn devices_json() -> Value {
    json!({ "type": "devices", "devices": sdr::devices() })
}

/// The `dir` message, for the interface's folder picker: a folder on this
/// computer, its subfolders and its .json files (a Trunk Recorder config to
/// import); "" = the home folder. A path that doesn't exist yet lists the
/// nearest folder above it that does.
pub(crate) fn dir_json(path: &str) -> Value {
    let home = trunk_app::config::home();
    let want = if path.trim().is_empty() { home.clone() } else { PathBuf::from(path.trim()) };
    let mut at = want.as_path();
    while !at.is_dir() {
        match at.parent() {
            Some(p) if !p.as_os_str().is_empty() => at = p,
            _ => {
                at = home.as_path();
                break;
            }
        }
    }
    let at = at.canonicalize().unwrap_or_else(|_| at.to_path_buf());
    let (mut dirs, mut files, mut error) = (vec![], vec![], None);
    match std::fs::read_dir(&at) {
        Ok(rd) => {
            for e in rd.filter_map(|e| e.ok()) {
                let Ok(name) = e.file_name().into_string() else { continue };
                if name.starts_with('.') {
                    continue;
                }
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    dirs.push(name);
                } else if name.to_lowercase().ends_with(".json") {
                    files.push(name);
                }
            }
        }
        Err(e) => error = Some(e.to_string()),
    }
    dirs.sort_by_key(|n| n.to_lowercase());
    files.sort_by_key(|n| n.to_lowercase());
    json!({
        "type": "dir",
        "path": at.display().to_string(),
        "parent": at.parent().map(|p| p.display().to_string()),
        "dirs": dirs,
        "files": files,
        "home": home.display().to_string(),
        "sep": std::path::MAIN_SEPARATOR.to_string(),
        "error": error,
    })
}

/// The `trConfig` message: a Trunk Recorder config.json (the file, or a folder
/// holding one) with the talkgroup and channel files it names, read here so
/// the browser imports them in one go. A name resolves beside the config (as
/// Trunk Recorder run from its folder does), else by its file name there.
pub(crate) fn tr_config_json(path: &str) -> Value {
    let p = PathBuf::from(path.trim());
    let file = if p.is_dir() { p.join("config.json") } else { p };
    let shown = file.display().to_string();
    let fail = |e: String| json!({ "type": "trConfig", "path": shown, "error": e });
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => return fail(format!("Couldn't read {shown}: {e}")),
    };
    let j: Value = match serde_json::from_str(&text) {
        Ok(j) => j,
        Err(e) => return fail(format!("{shown} isn't a Trunk Recorder config: {e}")),
    };
    if !j["systems"].is_array() && !j["sources"].is_array() {
        return fail(format!("{shown} has no systems or sources — is it a Trunk Recorder config?"));
    }
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut files = serde_json::Map::new();
    for sys in j["systems"].as_array().into_iter().flatten() {
        for key in ["talkgroupsFile", "channelFile"] {
            let Some(name) = sys[key].as_str().filter(|n| !n.is_empty()) else { continue };
            let named = Path::new(name);
            let tries = [if named.is_absolute() { named.to_path_buf() } else { dir.join(named) }, dir.join(named.file_name().unwrap_or_default())];
            // A talkgroup file is small; anything huge is not one.
            let read = tries.iter().find_map(|t| std::fs::metadata(t).ok().filter(|m| m.is_file() && m.len() < 8 << 20).and_then(|_| std::fs::read_to_string(t).ok()));
            if let Some(t) = read {
                files.insert(name.to_string(), Value::String(t));
            }
        }
    }
    json!({ "type": "trConfig", "path": shown, "text": text, "files": files, "error": null })
}

/// The `hello` a connection starts with: everything the interface shows.
pub(crate) fn hello_json(ctx: &Ctx, radios: Value) -> Value {
    let config = ctx.config.lock().unwrap().clone();
    let history: Vec<Value> = ctx.history.lock().unwrap().iter().take(300).cloned().collect();
    // Each system's talker aliases, as saved (CSV).
    let units: serde_json::Map<String, Value> = config
        .systems
        .iter()
        .map(|s| &s.short_name)
        .chain(config.conventional.iter().map(|c| &c.short_name))
        .filter_map(|n| std::fs::read_to_string(crate::runtime::units_path(n)).ok().map(|csv| (n.clone(), Value::String(csv))))
        .collect();
    json!({
        "type": "hello",
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "config": config,
        "configPath": ctx.config_path.display().to_string(),
        "devices": sdr::devices(),
        "phase": ctx.phase.lock().unwrap().to_json(),
        "history": history,
        "units": units,
        // The codes conventional frequencies carried, as saved.
        "heard": std::fs::read_to_string(crate::runtime::heard_path(&config)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(json!({})),
        "radios": radios,
        "surveyBands": trunk_app::survey::bands_json(),
        "survey": ctx.survey_last.lock().unwrap().clone().unwrap_or_else(crate::survey::idle_json),
    })
}

async fn session(ctx: Arc<Ctx>, mut socket: WebSocket, who: Who) {
    let viewer = who.role == Role::Viewer;
    let mut rx = ctx.hub.subscribe();
    let radios = tokio::task::spawn_blocking(|| crate::radio::radios_json(false)).await.unwrap_or(Value::Null);
    let mut hello = hello_json(&ctx, radios);
    hello["access"] = json!({ "user": who.user, "role": who.role.as_str(), "accounts": !ctx.accounts.is_open() });
    let out = |v: Value| {
        let t = v.to_string();
        if viewer {
            for_viewer(&t).unwrap_or(t)
        } else {
            t
        }
    };
    if socket.send(Message::Text(out(hello).into())).await.is_err() {
        return;
    }
    if socket.send(Message::Text(out(plugins_json(&ctx).await).into())).await.is_err() {
        return;
    }
    // A session that's been logged out, removed or changed is closed.
    let mut check = tokio::time::interval(std::time::Duration::from_secs(10));
    // Live audio: off until the browser asks; optionally one system and/or talkgroup only.
    let mut listen: Option<Listen> = None;
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(out) => {
                    let m = match &*out {
                        Out::Text(s) if viewer => match for_viewer(s) {
                            Some(r) => Message::Text(r.into()),
                            None if s.contains(r#""type":"accounts""#) => continue,
                            None => Message::Text(s.clone().into()),
                        },
                        Out::Text(s) => Message::Text(s.clone().into()),
                        Out::Audio { system, tg, frame } => match listen {
                            Some(l) if l.wants(*system, *tg) => Message::Binary(frame.clone().into()),
                            _ => continue,
                        },
                    };
                    let quit = matches!(&*out, Out::Text(s) if s == r#"{"type":"quit"}"#);
                    if socket.send(m).await.is_err() || quit {
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            },
            _ = check.tick() => {
                if !ctx.accounts.still(&who) {
                    let _ = socket.send(Message::Text(json!({ "type": "loggedOut" }).to_string().into())).await;
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    let kind = v["type"].as_str().unwrap_or("");
                    if !ctx.accounts.still(&who) {
                        let _ = socket.send(Message::Text(json!({ "type": "loggedOut" }).to_string().into())).await;
                        return;
                    }
                    let reply = if viewer && !viewer_may(kind) {
                        Some(json!({ "type": "error", "message": "Your account can watch, not change things: ask an admin." }))
                    } else if matches!(kind, "accounts" | "addAccount" | "removeAccount" | "setAccountRole" | "setAccountPassword" | "changePassword") {
                        account_command(&ctx, &v, &who).await
                    } else if matches!(kind, "stats" | "statsHistory" | "affiliations" | "affiliationLinks") {
                        stats_command(&ctx, &v).await
                    } else if kind == "plugins" && viewer {
                        Some(Value::String(out(plugins_json(&ctx).await)))
                    } else {
                        command(&ctx, &v, &mut listen).await
                    };
                    if let Some(reply) = reply {
                        let text = match reply {
                            Value::String(t) => t,
                            r => r.to_string(),
                        };
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            return;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
        }
    }
}

/// Change the config on the recorder's side (a plugin added or removed):
/// saved, sent to every browser, and the plugins restarted if it changes them.
fn edit_config(ctx: &Ctx, f: impl FnOnce(&mut crate::config::Config)) -> Result<(), String> {
    let mut cfg = ctx.config.lock().unwrap();
    let old = cfg.clone();
    f(&mut cfg);
    if *cfg == old {
        return Ok(());
    }
    cfg.save(&ctx.config_path).map_err(|e| format!("Couldn't save the config: {e}"))?;
    let c = cfg.clone();
    drop(cfg);
    publish(&ctx.hub, json!({ "type": "config", "config": c }));
    if crate::plugins::changed(&old, &c) {
        ctx.plugins.reload(&c);
    }
    Ok(())
}

/// The `plugins` message (asking plugins who they are can take a moment).
async fn plugins_json(ctx: &Arc<Ctx>) -> Value {
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || {
        let cfg = ctx2.config.lock().unwrap().clone();
        ctx2.plugins.list_json(&cfg)
    })
    .await
    .unwrap_or(Value::Null)
}

/// Which live audio a connection wants: every call, or one system's
/// (65535: conventional) and/or one talkgroup's.
#[derive(Clone, Copy)]
struct Listen {
    system: Option<u16>,
    talkgroup: Option<u32>,
}

impl Listen {
    fn wants(&self, system: u16, tg: u32) -> bool {
        self.system.is_none_or(|s| s == system) && self.talkgroup.is_none_or(|t| t == tg)
    }
}

async fn command(ctx: &Arc<Ctx>, v: &Value, listen: &mut Option<Listen>) -> Option<Value> {
    match v["type"].as_str()? {
        "setConfig" => match serde_json::from_value::<Config>(v["config"].clone()) {
            Ok(mut c) => {
                // While linked, a conventional system's channels are its file's
                // (re-read: a spreadsheet may have changed it). Files are linked
                // with "channelFile".
                c.load_channel_files(&ctx.config_path);
                let saved = c.save(&ctx.config_path);
                let old = std::mem::replace(&mut *ctx.config.lock().unwrap(), c.clone());
                if old.log != c.log {
                    crate::logging::configure(&c.log, ctx.config_path.parent().unwrap_or(std::path::Path::new(".")));
                }
                publish(&ctx.hub, json!({ "type": "config", "config": c }));
                // Plugins' settings changed while recording: they restart with them.
                if crate::plugins::changed(&old, &c) {
                    let ctx2 = ctx.clone();
                    let _ = tokio::task::spawn_blocking(move || ctx2.plugins.reload(&c)).await;
                }
                saved.err().map(|e| json!({ "type": "error", "message": format!("Couldn't save the config: {e}") }))
            }
            Err(e) => Some(json!({ "type": "error", "message": format!("Bad config: {e}") })),
        },
        // Link conventional system `index`'s channels to a CSV (created from
        // its list if new), reload it (the same path again), or unlink ("").
        "channelFile" => {
            let path = v["path"].as_str().unwrap_or("").to_string();
            let k = v["index"].as_u64().unwrap_or(0) as usize;
            let mut c = ctx.config.lock().unwrap().clone();
            let r = c.link_channel_file(k, &ctx.config_path, &path);
            if r.is_ok() || c.conventional.get(k).is_some_and(|x| !x.channel_file.is_empty()) {
                let saved = c.save(&ctx.config_path);
                *ctx.config.lock().unwrap() = c.clone();
                publish(&ctx.hub, json!({ "type": "config", "config": c }));
                if let Err(e) = saved {
                    return Some(json!({ "type": "error", "message": format!("Couldn't save the config: {e}") }));
                }
            }
            r.err().map(|e| json!({ "type": "error", "message": e }))
        }
        "start" => {
            let cfg = ctx.config.lock().unwrap().clone();
            let ctx2 = ctx.clone();
            let r = tokio::task::spawn_blocking(move || {
                stop_survey(&ctx2);
                if let Some(old) = ctx2.runner.lock().unwrap().take() {
                    old.stop();
                }
                ctx2.set_phase("starting", None, false);
                match runtime::start(ctx2.clone(), cfg) {
                    Ok(r) => {
                        *ctx2.runner.lock().unwrap() = Some(r);
                        None
                    }
                    Err(e) => {
                        ctx2.set_phase("idle", Some(e.clone()), false);
                        Some(e)
                    }
                }
            })
            .await
            .ok()
            .flatten();
            r.map(|e| json!({ "type": "error", "message": e }))
        }
        "stop" => {
            let ctx2 = ctx.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Some(r) = ctx2.runner.lock().unwrap().take() {
                    r.stop();
                }
            })
            .await;
            None
        }
        "devices" => Some(devices_json()),
        "findRadios" => {
            let radios = tokio::task::spawn_blocking(|| crate::radio::radios_json(true)).await.unwrap_or(Value::Null);
            Some(json!({ "type": "radios", "radios": radios }))
        }
        "quit" => {
            let ctx2 = ctx.clone();
            let _ = tokio::task::spawn_blocking(move || stop_survey(&ctx2)).await;
            ctx.quit.notify_one();
            None
        }
        "surveyStart" => {
            let cfg = ctx.config.lock().unwrap().clone();
            let req = trunk_app::survey::Request::from_json(v);
            let ctx2 = ctx.clone();
            let r = tokio::task::spawn_blocking(move || {
                if ctx2.runner.lock().unwrap().is_some() {
                    return Some("Stop recording first — the scan needs the radio to itself.".to_string());
                }
                stop_survey(&ctx2);
                match crate::survey::start(ctx2.clone(), cfg, req) {
                    Ok(r) => {
                        *ctx2.survey.lock().unwrap() = Some(r);
                        None
                    }
                    Err(e) => Some(e),
                }
            })
            .await
            .ok()
            .flatten();
            r.map(|e| json!({ "type": "error", "message": e }))
        }
        "surveyListen" => {
            if let (Some(r), Some(f)) = (ctx.survey.lock().unwrap().as_ref(), v["freqHz"].as_f64()) {
                r.send(crate::survey::UserCmd::Listen(f));
            }
            None
        }
        "surveyRescan" => {
            if let Some(r) = ctx.survey.lock().unwrap().as_ref() {
                r.send(crate::survey::UserCmd::Rescan);
            }
            None
        }
        "surveyStop" => {
            let ctx2 = ctx.clone();
            let _ = tokio::task::spawn_blocking(move || stop_survey(&ctx2)).await;
            None
        }
        "readTrConfig" => {
            let path = v["path"].as_str().unwrap_or("").to_string();
            tokio::task::spawn_blocking(move || tr_config_json(&path)).await.ok()
        }
        "listDir" => {
            let path = v["path"].as_str().unwrap_or("").to_string();
            tokio::task::spawn_blocking(move || dir_json(&path)).await.ok()
        }
        "plugins" => Some(plugins_json(ctx).await),
        "pluginStore" => {
            let (ctx2, refresh) = (ctx.clone(), v["refresh"].as_bool() == Some(true));
            tokio::task::spawn_blocking(move || ctx2.plugins.store_json(refresh)).await.ok()
        }
        // In the background: it takes a while, and this connection should
        // hear how it goes (pluginInstall), as everyone does.
        "installPlugin" => {
            let (ctx2, v) = (ctx.clone(), v.clone());
            tokio::spawn(async move {
                let ctx3 = ctx2.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let cfg = ctx3.config.lock().unwrap().clone();
                    ctx3.plugins.install(&v, &cfg);
                    // An update of a running plugin: it restarts with the new version.
                    let cfg = ctx3.config.lock().unwrap().clone();
                    ctx3.plugins.reload(&cfg);
                })
                .await;
                publish(&ctx2.hub, plugins_json(&ctx2).await);
            });
            None
        }
        "addPlugin" | "removePlugin" => {
            let (ctx2, v) = (ctx.clone(), v.clone());
            let r = tokio::task::spawn_blocking(move || {
                let cfg = ctx2.config.lock().unwrap().clone();
                if v["type"] == "addPlugin" {
                    let (id, path, notice) = ctx2.plugins.add(&v, &cfg)?;
                    edit_config(&ctx2, |c| c.plugins.entry(id).or_default().path = path.display().to_string())?;
                    Ok(Some(notice))
                } else {
                    let id = ctx2.plugins.remove(&v, &cfg)?;
                    edit_config(&ctx2, |c| crate::plugins::forget(c, &id))?;
                    Ok(None)
                }
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            // Everyone sees the change (or the list as it still is).
            publish(&ctx.hub, plugins_json(ctx).await);
            match r {
                Ok(Some(notice)) => Some(json!({ "type": "notice", "message": notice })),
                Ok(None) => None,
                Err(e) => Some(json!({ "type": "error", "message": e })),
            }
        }
        "listen" => {
            *listen = (v["on"].as_bool() == Some(true))
                .then(|| Listen { system: v["system"].as_u64().map(|s| s as u16), talkgroup: v["talkgroup"].as_u64().map(|t| t as u32) });
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, StatusCode, Uri};
    use std::path::Path;

    async fn get(root: &Path, rel: &str, uri: &str) -> (StatusCode, String, String) {
        let r = folder_file(root, rel, &uri.parse::<Uri>().unwrap()).await;
        let status = r.status();
        let h = |n| r.headers().get(n).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        let (mime, location) = (h(header::CONTENT_TYPE), h(header::LOCATION));
        let body = String::from_utf8_lossy(&axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap()).to_string();
        (status, if location.is_empty() { mime } else { location }, body)
    }

    #[tokio::test]
    async fn an_interface_folder_is_served_and_nothing_outside_it() {
        let base = std::env::temp_dir().join(format!("trunk-pro-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("my ui");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("index.html"), "home").unwrap();
        std::fs::write(root.join("app.js"), "js").unwrap();
        std::fs::write(root.join("sub/index.html"), "sub").unwrap();
        std::fs::write(base.join("secret.txt"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(base.join("secret.txt"), root.join("link.txt")).unwrap();

        assert_eq!(get(&root, "", "/ui/x/").await, (StatusCode::OK, "text/html; charset=utf-8".into(), "home".into()));
        assert_eq!(get(&root, "app.js", "/ui/x/app.js").await, (StatusCode::OK, "text/javascript; charset=utf-8".into(), "js".into()));
        // A folder: its index.html, after the slash its links need.
        assert_eq!(get(&root, "sub", "/ui/x/sub").await.0, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(get(&root, "sub", "/ui/x/sub").await.1, "/ui/x/sub/");
        assert_eq!(get(&root, "sub", "/ui/x/sub/").await.2, "sub");
        // A page's own route: the root's index.html; a missing file: not found.
        assert_eq!(get(&root, "calls/today", "/ui/x/calls/today").await.2, "home");
        assert_eq!(get(&root, "missing.js", "/ui/x/missing.js").await.0, StatusCode::NOT_FOUND);
        // Nothing outside.
        assert_eq!(get(&root, "../secret.txt", "/ui/x/../secret.txt").await.0, StatusCode::BAD_REQUEST);
        #[cfg(unix)]
        assert_eq!(get(&root, "link.txt", "/ui/x/link.txt").await.0, StatusCode::NOT_FOUND);
        // No folder: says so.
        assert!(get(&base.join("gone"), "", "/").await.2.contains("isn't there"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn paths_and_escapes() {
        assert_eq!(percent_decode("my%20ui/a%2Bb.js").as_deref(), Some("my ui/a+b.js"));
        assert_eq!(percent_decode("bad%2"), None);
        assert_eq!(percent_decode("%FF"), None);
        let cfg = Path::new("/etc/trunk-pro/config.json");
        assert_eq!(interface_path(cfg, "ui/wall"), Path::new("/etc/trunk-pro/ui/wall"));
        assert_eq!(interface_path(cfg, "/srv/wall"), Path::new("/srv/wall"));
        assert!(interface_path(cfg, "~/wall").ends_with("wall") && !interface_path(cfg, "~/wall").starts_with("/etc"));
    }

    #[test]
    fn its_own_pages_and_programs_are_let_in() {
        assert_eq!(admit(true, Some("localhost:8080"), Some("http://localhost:8080"), &[]), Ok(false));
        assert_eq!(admit(true, Some("127.0.0.1:8080"), Some("http://127.0.0.1:8080"), &[]), Ok(false));
        assert_eq!(admit(true, Some("[::1]:8080"), Some("http://[::1]:8080"), &[]), Ok(false));
        // The Vite dev server proxies with the page's Host.
        assert_eq!(admit(true, Some("localhost:5173"), Some("http://localhost:5173"), &[]), Ok(false));
        // curl, scripts: no Origin.
        assert_eq!(admit(true, Some("localhost:8080"), None, &[]), Ok(false));
        assert_eq!(admit(true, None, None, &[]), Ok(false));
        // Bound to every address: reached by the machine's name or address.
        assert_eq!(admit(false, Some("radiobox.local:8080"), Some("http://radiobox.local:8080"), &[]), Ok(false));
        assert_eq!(admit(false, Some("192.168.1.20:8080"), Some("http://192.168.1.20:8080"), &[]), Ok(false));
    }

    #[test]
    fn other_sites_are_refused() {
        assert!(admit(true, Some("localhost:8080"), Some("https://evil.example"), &[]).is_err());
        assert!(admit(false, Some("192.168.1.20:8080"), Some("https://evil.example"), &[]).is_err());
        assert!(admit(true, Some("localhost:8080"), Some("null"), &[]).is_err());
        // DNS rebinding: evil.example resolved to 127.0.0.1, so the page is "same origin".
        assert!(admit(true, Some("evil.example:8080"), Some("http://evil.example:8080"), &[]).is_err());
        assert!(admit(true, Some("evil.example:8080"), None, &[]).is_err());
    }

    #[test]
    fn allowed_origins_get_cors() {
        let allowed = vec!["http://192.168.1.50:3000/".to_string(), "null".to_string()];
        assert_eq!(admit(true, Some("localhost:8080"), Some("http://192.168.1.50:3000"), &allowed), Ok(true));
        assert_eq!(admit(true, Some("localhost:8080"), Some("null"), &allowed), Ok(true));
        assert!(admit(true, Some("localhost:8080"), Some("http://192.168.1.51:3000"), &allowed).is_err());
        assert_eq!(admit(true, Some("localhost:8080"), Some("https://anything.example"), &["*".to_string()]), Ok(true));
        // A reverse proxy that passes its own Host, its origin listed.
        let proxied = vec!["https://radio.example.com".to_string()];
        assert_eq!(admit(true, Some("radio.example.com"), Some("https://radio.example.com"), &proxied), Ok(false));
    }

    #[test]
    fn query_values_decode() {
        assert_eq!(query_decode("clmrn-I"), "clmrn-I");
        assert_eq!(query_decode("a%20b+c%2Fd"), "a b c/d");
        assert_eq!(query_decode("bad%zz%4"), "bad%zz%4");
    }

    #[test]
    fn viewers_get_no_secrets() {
        let hello = json!({ "type": "hello", "configPath": "/x/config.json", "config": {
            "plugins": { "mqtt": { "enabled": true, "path": "/x/mqtt", "settings": { "password": "p" } } },
            "systems": [{ "shortName": "s", "plugins": { "openmhz": { "apiKey": "k" } } }],
            "conventional": [{ "shortName": "c", "plugins": { "openmhz": { "apiKey": "k2" } } }],
            "recording": { "captureDir": "/x/calls" } } });
        let out = for_viewer(&hello.to_string()).unwrap();
        for secret in ["\"p\"", "\"k\"", "k2", "/x/"] {
            assert!(!out.contains(secret), "{secret} in {out}");
        }
        assert!(out.contains("\"enabled\":true"));
        let plugins = json!({ "type": "plugins", "plugins": [{ "id": "a", "path": "/x/a", "problem": "/x/a isn't there", "unlistedFrom": "x/y" }] });
        let out = for_viewer(&plugins.to_string()).unwrap();
        assert!(!out.contains("/x/") && out.contains("unavailable"));
        assert!(for_viewer(r#"{"type":"status","x":1}"#).is_none());
    }
}
