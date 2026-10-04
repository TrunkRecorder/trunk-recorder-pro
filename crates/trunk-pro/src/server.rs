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

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;
use serde_json::{json, Value};

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
            let mut quitting = ctx3.lifecycle.lock().unwrap();
            *quitting = true;
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
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await
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
async fn interfaces_json(State(ctx): State<Arc<Ctx>>) -> Response {
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
async fn ui_index(State(ctx): State<Arc<Ctx>>) -> Response {
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

/// A recorded file, confined to the capture folder (or the RAM spool, while
/// it waits there for the plugins). A call's .wav that was never written
/// (only uploaded) is its .m4a.
async fn call_file(State(ctx): State<Arc<Ctx>>, UrlPath(path): UrlPath<String>) -> Response {
    let mut dirs = vec![PathBuf::from(&ctx.config.lock().unwrap().recording.capture_dir)];
    dirs.extend(ctx.spool.lock().unwrap().as_ref().map(|s| s.dir.clone()));
    let rel = Path::new(&path);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut tries: Vec<PathBuf> = vec![rel.to_path_buf()];
    if rel.extension().is_some_and(|x| x == "wav") {
        tries.push(rel.with_extension("m4a"));
    }
    for r in &tries {
        for d in &dirs {
            if let Ok(bytes) = tokio::fs::read(d.join(r)).await {
                let mime = mime_guess::from_path(r).first_or_octet_stream();
                return ([(header::CONTENT_TYPE, mime.as_ref().to_string())], bytes).into_response();
            }
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn ws(State(ctx): State<Arc<Ctx>>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |socket| session(ctx, socket))
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
/// `devices` and `radios` are found off the async workers (they open USB).
pub(crate) fn hello_json(ctx: &Ctx, devices: Value, radios: Value) -> Value {
    let config = ctx.config.lock().unwrap().clone();
    let history: Vec<Value> = ctx.history.lock().unwrap().iter().take(runtime::HISTORY).cloned().collect();
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
        "devices": devices,
        "phase": ctx.phase.lock().unwrap().to_json(),
        "history": history,
        "units": units,
        // The codes conventional frequencies carried, as saved.
        "heard": std::fs::read_to_string(crate::runtime::heard_path(&config)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(json!({})),
        "radios": radios,
        "surveyBands": trunk_app::survey::bands_json(),
        "survey": ctx.survey_last.lock().unwrap().clone().unwrap_or_else(crate::survey::idle_json),
        // The dashboard: recent notable events, the computer as last sampled, every plugin's runtime.
        "events": ctx.shared.recent_events(),
        "host": ctx.host_last.lock().unwrap().clone(),
        "pluginRuntime": ctx.plugins.runtime_json(),
    })
}

/// What one connection watches ([`trunk_app::stats::Topics`]); counted in
/// the shared tally while it lasts.
struct Subs {
    ctx: Arc<Ctx>,
    topics: std::collections::BTreeSet<String>,
}

impl Subs {
    fn set(&mut self, topics: std::collections::BTreeSet<String>) {
        let add: Vec<String> = topics.difference(&self.topics).cloned().collect();
        let remove: Vec<String> = self.topics.difference(&topics).cloned().collect();
        self.ctx.retopic(&add, &remove);
        self.topics = topics;
    }
}

impl Drop for Subs {
    fn drop(&mut self) {
        let all: Vec<String> = self.topics.iter().cloned().collect();
        self.ctx.retopic(&[], &all);
    }
}

/// A query's time span: `range` ("10m", "1h", "6h", "24h", "7d") back from
/// now, or `from` / `to` (Unix s).
fn span(v: &Value, now: i64) -> (i64, i64) {
    let back = match v["range"].as_str().unwrap_or("1h") {
        "10m" => 600,
        "1h" => 3600,
        "6h" => 6 * 3600,
        "24h" => 86400,
        "7d" => 7 * 86400,
        _ => 3600,
    };
    let to = v["to"].as_i64().unwrap_or(now);
    (v["from"].as_i64().unwrap_or(to - back), to)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// `statsQuery {id, series, range | from/to, points?}`: the minute history.
pub(crate) fn stats_query(ctx: &Ctx, v: &Value) -> Value {
    let (from, to) = span(v, unix_now());
    let patterns: Vec<String> = v["series"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).take(200).map(String::from).collect();
    let points = v["points"].as_u64().unwrap_or(0) as usize;
    let h = ctx.series.lock().unwrap();
    json!({ "type": "statsResult", "id": v["id"], "from": from, "to": to, "loading": h.loading, "series": h.query(&patterns, from, to, points) })
}

/// `radioQuery`: the registry, with talkgroup names from the config and radio names as saved.
pub(crate) fn radio_query(ctx: &Ctx, v: &Value) -> Value {
    let cfg = ctx.config.lock().unwrap().clone();
    let tables = trunk_app::stats::talkgroup_tables(&cfg);
    let mut aliases: std::collections::HashMap<String, trunk_core::trunk::UnitAliases> = Default::default();
    for n in cfg.systems.iter().map(|s| &s.short_name).chain(cfg.conventional.iter().map(|c| &c.short_name)) {
        if let Ok(csv) = std::fs::read_to_string(crate::runtime::units_path(n)) {
            aliases.insert(n.clone(), trunk_core::trunk::UnitAliases::parse_csv(&csv));
        }
    }
    let tg = |sys: &str, t: u32| trunk_app::stats::tg_info(&tables, sys, t);
    let unit = |sys: &str, u: u32| aliases.get(sys).and_then(|a| a.get(u)).map(str::to_string);
    let names = trunk_app::stats::Names { tg: &tg, unit: &unit };
    let mut out = ctx.shared.radio.lock().unwrap().query(v, unix_now(), &names);
    out["type"] = json!("radioResult");
    out["id"] = v["id"].clone();
    out
}

async fn session(ctx: Arc<Ctx>, mut socket: WebSocket) {
    let mut rx = ctx.hub.subscribe();
    let found = tokio::task::spawn_blocking(|| (json!(sdr::devices()), crate::radio::radios_json(false))).await;
    let (devices, radios) = found.unwrap_or((Value::Null, Value::Null));
    let hello = hello_json(&ctx, devices, radios);
    if socket.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    let plugins = plugins_json(&ctx).await;
    if socket.send(Message::Text(plugins.to_string().into())).await.is_err() {
        return;
    }
    // Live audio: off until the browser asks; optionally one system and/or talkgroup only.
    let mut listen: Option<Listen> = None;
    let mut subs = Subs { ctx: ctx.clone(), topics: Default::default() };
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(out) => {
                    let m = match &*out {
                        Out::Text(s) => Message::Text(s.clone().into()),
                        Out::Topic { topic, text } if subs.topics.contains(topic) => Message::Text(text.clone().into()),
                        Out::Topic { .. } => continue,
                        Out::Audio { short_name, tg, frame } => match &listen {
                            Some(l) if l.wants(short_name, *tg) => Message::Binary(frame.clone().into()),
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
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    if let Some(reply) = command(&ctx, &v, &mut listen, &mut subs).await {
                        if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
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
/// What live audio a connection asked for: one system's (by short name)
/// and / or one talkgroup's, or everything.
struct Listen {
    system: Option<String>,
    talkgroup: Option<u32>,
}

impl Listen {
    fn from_json(v: &Value) -> Listen {
        Listen { system: v["system"].as_str().map(String::from), talkgroup: v["talkgroup"].as_u64().map(|t| t as u32) }
    }
    fn wants(&self, short_name: &str, tg: u32) -> bool {
        self.system.as_deref().is_none_or(|s| s == short_name) && self.talkgroup.is_none_or(|t| t == tg)
    }
}

async fn command(ctx: &Arc<Ctx>, v: &Value, listen: &mut Option<Listen>, subs: &mut Subs) -> Option<Value> {
    match v["type"].as_str()? {
        // What this connection watches: costly messages (waterfalls, the
        // control channel log, detail views) only go to those that ask.
        "subscribe" => {
            let topics: std::collections::BTreeSet<String> = v["topics"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).take(64).map(String::from).collect();
            subs.set(topics);
            Some(json!({ "type": "subscribed", "topics": subs.topics }))
        }
        "statsQuery" => {
            let (ctx2, v) = (ctx.clone(), v.clone());
            tokio::task::spawn_blocking(move || stats_query(&ctx2, &v)).await.ok()
        }
        "radioQuery" => {
            let (ctx2, v) = (ctx.clone(), v.clone());
            tokio::task::spawn_blocking(move || radio_query(&ctx2, &v)).await.ok()
        }
        "setConfig" => match serde_json::from_value::<Config>(v["config"].clone()) {
            Ok(mut c) => {
                // While linked, a conventional system's channels are its file's
                // (re-read: a spreadsheet may have changed it). Files are linked
                // with "channelFile".
                c.load_channel_files(&ctx.config_path);
                let saved = c.save(&ctx.config_path);
                let old = std::mem::replace(&mut *ctx.config.lock().unwrap(), c.clone());
                // A talkgroup file changed while recording (an Ignore flag set): the session takes it now.
                if ctx.runner.lock().unwrap().is_some() {
                    let mut cmds = ctx.engine_cmds.lock().unwrap();
                    for s in &c.systems {
                        if old.systems.iter().find(|o| o.short_name == s.short_name).is_some_and(|o| o.talkgroups_csv != s.talkgroups_csv) {
                            cmds.push(crate::runtime::EngineCmd::Talkgroups { short_name: s.short_name.clone(), csv: s.talkgroups_csv.clone() });
                        }
                    }
                }
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
                let quitting = ctx2.lifecycle.lock().unwrap();
                if *quitting {
                    return None;
                }
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
                let _held = ctx2.lifecycle.lock().unwrap();
                if let Some(r) = ctx2.runner.lock().unwrap().take() {
                    r.stop();
                }
            })
            .await;
            None
        }
        "devices" => tokio::task::spawn_blocking(devices_json).await.ok(),
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
                let quitting = ctx2.lifecycle.lock().unwrap();
                if *quitting {
                    return None;
                }
                {
                    let mut runner = ctx2.runner.lock().unwrap();
                    match runner.take() {
                        // (Its capture files ended: nothing is recording.)
                        Some(r) if r.finished() => r.stop(),
                        Some(r) => {
                            *runner = Some(r);
                            return Some("Stop recording first — the scan needs the radio to itself.".to_string());
                        }
                        None => {}
                    }
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
        // A moment with the radio alone (nothing may start meanwhile); the answer is this connection's.
        "profileSource" => {
            let cfg = ctx.config.lock().unwrap().clone();
            let req = trunk_app::profile::Request::from_json(v);
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let quitting = ctx2.lifecycle.lock().unwrap();
                if *quitting {
                    return None;
                }
                {
                    let mut runner = ctx2.runner.lock().unwrap();
                    match runner.take() {
                        Some(r) if r.finished() => r.stop(),
                        Some(r) => {
                            *runner = Some(r);
                            return Some(trunk_app::profile::error_json(req.source, "Stop recording first — profiling needs the radio to itself."));
                        }
                        None => {}
                    }
                }
                stop_survey(&ctx2);
                Some(crate::profile::run(&cfg, &req))
            })
            .await
            .ok()
            .flatten()
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
            *listen = (v["on"].as_bool() == Some(true)).then(|| Listen::from_json(v));
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{admit, folder_file, interface_path, percent_decode, Listen};
    use serde_json::json;
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

    /// Live audio is asked for by a system's short name.
    #[test]
    fn listening_by_short_name() {
        let l = Listen::from_json(&json!({ "on": true, "system": "dcfd", "talkgroup": null }));
        assert!(l.wants("dcfd", 101) && !l.wants("wmata", 101));
        let l = Listen::from_json(&json!({ "on": true, "system": "wmata", "talkgroup": 101 }));
        assert!(l.wants("wmata", 101) && !l.wants("wmata", 102) && !l.wants("dcfd", 101));
        assert!(Listen::from_json(&json!({ "system": null, "talkgroup": null })).wants("conv", 7));
    }
}
