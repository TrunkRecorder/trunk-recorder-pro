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
//! With `server.tls` set, every port is served over HTTPS ([`crate::tls`]).
//!
//! The built-in interface is served on the recorder's port (`server.port`);
//! every other interface on a port of its own ([`crate::interfaces`]), with
//! the same API beside it ([`api`]) and what its access allows
//! ([`crate::access`]): which port a request came in on is its [`Door`].
//!
//! A connection gets `hello`, then (with control access) `plugins` and
//! `interfaces`; then
//! what everyone hears (`state`, `status` ~2/s, `spectrum` ~7/s per source,
//! `log`, `concluded`, …) and the answers to its own commands.
//!
//! Every request passes [`guard`]: pages from other sites are refused unless
//! listed in `server.allowedOrigins`, and with signing in on (`server.auth`)
//! a request without a user's access token that gives it access (or the
//! say-so of a proxy in front, from its addresses, or Cloudflare Access's signed token) gets the sign-in page (or a 401). GET /signin is that page; POST /api/login and
//! /api/logout set and clear the cookie a browser signs in with, as do
//! /api/passkey/login/… with a passkey (/api/passkey/register/… adds one,
//! [`crate::access::passkey`]); GET /api/session says who this request is, and may do.

use trunk_app::protocol::{body, Body, Command, StatsQuery, StoreKind, ToClient};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Extension, Path as UrlPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use rust_embed::RustEmbed;
use serde_json::{json, Value};

use crate::access::{self, passkey, Caller, Door, Who};
use crate::config::{ApiAccess, Config};
use crate::runtime::{self, publish, Ctx, Out};

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
#[allow_missing = true]
struct Ui;

/// The API's documentation, client and examples, served under /api/.
#[derive(RustEmbed)]
#[folder = "../../docs/api/"]
struct ApiDocs;

const PROTOCOL: &str = include_str!("../../../web/src/protocol.ts");

/// What a browser that must sign in is shown, in place of the page it asked
/// for (or at /signin, asked for).
const SIGN_IN: &str = include_str!("signin.html");

/// What a request that must sign in may still ask for: who's running (a
/// second launch asks), signing in, and (see [`open`]) the API's docs.
const OPEN: &[&str] = &["/api/version", "/api/login", "/api/logout", "/api/session", "/api/passkey/login/start", "/api/passkey/login/finish", "/signin"];

/// Whether a request for `path` needs no access: [`OPEN`], and the API's
/// static files (the docs, schema, types, client.js, examples: nothing of
/// the recorder's, and a page on another origin imports client.js before it
/// has signed in). Everything else, the WebSocket, `/api/v1/` and
/// `/calls/` included, is guarded.
fn open(path: &str) -> bool {
    OPEN.contains(&path) || matches!(path, "/api/docs" | "/api/schema" | "/api/protocol.ts") || path.strip_prefix("/api/").is_some_and(|p| ApiDocs::get(p).is_some())
}

/// What `GET /api/version` answers — how a second launch recognises us.
pub const APP_ID: &str = "trunk-pro";

/// Serve until `quit` from a browser, Ctrl-C or SIGTERM. Recording is stopped
/// first either way, so calls in progress are written out.
pub async fn serve(ctx: Arc<Ctx>, listener: std::net::TcpListener) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    let loopback = listener.local_addr()?.ip().is_loopback();
    ctx.interfaces.listening_on(listener.local_addr()?.port());
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let app = builtin_app(ctx.clone(), loopback);
    let tls = ctx.tls.get().cloned();
    tokio::spawn(crate::interfaces::serve::reconcile(ctx.clone()));
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
        let _ = tokio::task::spawn_blocking(move || ctx3.quit()).await;
        publish(&ctx2.hub, ToClient::Quit);
        // Let the sessions deliver it before the connections close.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        crate::interfaces::serve::stop_all(&ctx2);
    };
    crate::tls::serve(listener, tls.as_ref(), app, shutdown).await
}

/// The built-in interface's server: its pages, the API, and the Interfaces page's screenshots.
fn builtin_app(ctx: Arc<Ctx>, loopback: bool) -> Router {
    api(Router::new())
        .route("/store/screenshots/{sha256}", get(store_screenshot))
        .route("/interfaces/{id}/screenshots/{n}", get(interface_screenshot))
        .fallback(|uri: Uri| async move { builtin(uri.path().trim_start_matches('/')) })
        .layer(middleware::from_fn_with_state((ctx.clone(), loopback, Door::Builtin), guard))
        .with_state(ctx)
}

/// What `GET /api/version` answers (plain HTTP to an HTTPS port too, [`crate::tls`]).
pub(crate) fn version() -> Value {
    json!({ "app": APP_ID, "version": env!("CARGO_PKG_VERSION"), "api": crate::interfaces::API_VERSION })
}

/// The recorder's API, served on every interface's port: the WebSocket,
/// signing in (its page at /signin too), the docs and client beside it,
/// and the recorded calls.
fn api(r: Router<Arc<Ctx>>) -> Router<Arc<Ctx>> {
    r.route("/api/ws", get(ws))
        .route("/api/version", get(|| async { axum::Json(version()) }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/session", get(signed_in))
        .route("/api/passkey/register/start", post(passkey_add_start))
        .route("/api/passkey/register/finish", post(passkey_add_finish))
        .route("/api/passkey/login/start", post(passkey_sign_in_start))
        .route("/api/passkey/login/finish", post(passkey_sign_in_finish))
        .route("/signin", get(|| async { sign_in_page(StatusCode::OK) }))
        .route("/api/docs", get(|| async { api_file("README.md") }))
        .route("/api/schema", get(|| async { api_file("protocol.schema.json") }))
        .route("/api/protocol.ts", get(|| async { ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], PROTOCOL).into_response() }))
        .merge(crate::api_v1::router())
        .route("/api/{*path}", get(|UrlPath(p): UrlPath<String>| async move { api_file(&p) }))
        .route("/calls/{*path}", get(call_file))
}

/// Interface `id`'s server: its folder, and the API with its access.
pub(crate) fn interface_app(ctx: Arc<Ctx>, id: String, stop: tokio::sync::watch::Receiver<bool>, loopback: bool) -> Router {
    let name = id.clone();
    api(Router::new())
        .fallback(move |State(ctx): State<Arc<Ctx>>, uri: Uri| {
            let id = name.clone();
            async move { interface_file(&ctx, &id, &uri).await }
        })
        .layer(middleware::from_fn_with_state((ctx.clone(), loopback, Door::Interface { id, stop }), guard))
        .with_state(ctx)
}

/// A server of `ctx` through `door`, as the recorder serves it (bound to
/// localhost, through [`guard`]; over HTTPS when `ctx.tls` is set), for tests: its address.
#[cfg(test)]
pub(crate) async fn test_server(ctx: &Arc<Ctx>, door: Door) -> std::net::SocketAddr {
    let builtin = matches!(door, Door::Builtin);
    let app = match door {
        Door::Builtin => builtin_app(ctx.clone(), true),
        Door::Interface { id, stop } => interface_app(ctx.clone(), id, stop, true),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    if builtin {
        ctx.interfaces.listening_on(addr.port());
    }
    let tls = ctx.tls.get().cloned();
    tokio::spawn(async move { crate::tls::serve(listener, tls.as_ref(), app, std::future::pending()).await });
    addr
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
/// The console window closed, Ctrl-Break, or the user logging off or
/// Windows shutting down. For a close, logoff or shutdown tokio's handler
/// holds the event (Windows waits ~5 s on a close) until we exit, so calls
/// in progress are saved as for Ctrl-C.
#[cfg(windows)]
async fn terminate() {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};
    let (Ok(mut close), Ok(mut brk), Ok(mut logoff), Ok(mut shutdown)) = (ctrl_close(), ctrl_break(), ctrl_logoff(), ctrl_shutdown()) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = close.recv() => {}
        _ = brk.recv() => {}
        _ = logoff.recv() => {}
        _ = shutdown.recv() => {}
    }
}
#[cfg(not(any(unix, windows)))]
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
///
/// Let in, the request is identified ([`access::identify`]) and handed on
/// as a [`Caller`] of `door`, which every handler asks what it may do. With
/// signing in on, one whose credential gives it nothing may only ask for
/// what's [`open`]; a browser asking for a page gets the sign-in page.
/// Every answer carries [`safe_headers`].
async fn guard(State((ctx, loopback, door)): State<(Arc<Ctx>, bool, Door)>, req: Request, next: Next) -> Response {
    let mut res = admitted(&ctx, loopback, door, req, next).await;
    safe_headers(&ctx, &mut res);
    res
}

/// What every answer [`guard`] gives carries: no `Referer` sent on from its
/// pages (a `?token=` in their URL stays there), no type sniffed; and, over
/// HTTPS here with `server.tls.hsts` set, browsers told to keep to HTTPS
/// for a year (every port of the host name: why it's off unless set).
fn safe_headers(ctx: &Ctx, res: &mut Response) {
    let h = res.headers_mut();
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if ctx.tls.get().is_some() && ctx.config.lock().unwrap().server.tls.as_ref().is_some_and(|t| t.hsts) {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
}

async fn admitted(ctx: &Arc<Ctx>, loopback: bool, door: Door, mut req: Request, next: Next) -> Response {
    // (In a block: a closure borrowing the request mustn't live across the await.)
    let (host, origin) = {
        let header = |name| req.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        (header(header::HOST), header(header::ORIGIN))
    };
    let (allowed, auth, hostname) = {
        let cfg = ctx.config.lock().unwrap();
        (cfg.server.allowed_origins.clone(), cfg.server.auth.clone(), cfg.server.host())
    };
    // (Bound to localhost, `server.hostname` is a name it answers to as well: a proxy in front, `tailscale serve`.)
    let named = hostname.is_some_and(|n| host.as_deref().is_some_and(|h| host_name(h) == n));
    match admit(loopback && !named, host.as_deref(), origin.as_deref(), &allowed) {
        Ok(cors) => {
            // Cookies don't keep to a port: without this, a listen interface's page on :8081 (or
            // any page `allowedOrigins: "*"` lets in) could use a signed-in browser's cookie on
            // the recorder's own port. A link followed to here (trunkrecorder.pro's `#import=…`,
            // an interface's link to the dashboard) still arrives signed in.
            let cookie_ok = !cors && cookies_count(req.headers());
            let peer = req.extensions().get::<ConnectInfo<crate::tls::Peer>>().map(|c| c.0 .0.ip());
            // Behind Cloudflare Access: its team's keys, fetched first if its token's isn't kept (off the runtime: it blocks).
            let cf = auth.proxy.as_ref().and_then(|p| p.cloudflare.clone()).filter(|_| auth.enabled && cookie_ok);
            let token = req.headers().get(access::cloudflare::HEADER).and_then(|v| v.to_str().ok());
            if let (Some(cf), Some(token)) = (cf, token) {
                if ctx.cloudflare.wanted(&cf, token) {
                    let ctx2 = ctx.clone();
                    let token = token.to_string();
                    let _ = tokio::task::spawn_blocking(move || ctx2.cloudflare.ready(&cf, &token)).await;
                }
            }
            let who = access::identify(&auth, &ctx.sessions, &ctx.cloudflare, req.headers(), req.uri().query(), cookie_ok, peer);
            // An allowed origin's page sending `Authorization` asks first, with no credentials:
            // answered here, token or not. (No Allow-Credentials: its cookies stay unusable.)
            let mut res = if cors && req.method() == Method::OPTIONS && req.headers().contains_key(header::ACCESS_CONTROL_REQUEST_METHOD) {
                let allow = [(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST"), (header::ACCESS_CONTROL_ALLOW_HEADERS, "Authorization, Content-Type"), (header::ACCESS_CONTROL_MAX_AGE, "600")];
                (StatusCode::NO_CONTENT, allow).into_response()
            } else if auth.enabled && who.grants(&auth).is_none() && !open(req.uri().path()) {
                sign_in(&req)
            } else {
                req.extensions_mut().insert(Caller { door, who });
                next.run(req).await
            };
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

/// Whether a request's cookies are its own: from a page of this origin, typed
/// in, or a link followed to a page here (`Sec-Fetch-*`; a program that sends
/// none is no page of another site's).
fn cookies_count(headers: &HeaderMap) -> bool {
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or("");
    matches!(h("sec-fetch-site"), "" | "same-origin" | "none") || (h("sec-fetch-mode") == "navigate" && h("sec-fetch-dest") == "document")
}

/// The answer to a request that must sign in first: a browser asking for a
/// page gets the sign-in page in its place (no redirect: the URL, its `#…`
/// with it, stays as it was); anything else, a 401.
fn sign_in(req: &Request) -> Response {
    let accept = req.headers().get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("");
    if req.method() == Method::GET && accept.contains("text/html") && !req.uri().path().starts_with("/api/") {
        return sign_in_page(StatusCode::UNAUTHORIZED);
    }
    (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], "Sign in first: an access token as Authorization: Bearer, ?token=, or POST /api/login.\n").into_response()
}

/// The sign-in page, as `status`: 401 in place of a page asked for, 200 at
/// /signin. Never in another page's frame (nothing here frames it): typed
/// into, or clicked, under a page made to look like something else.
fn sign_in_page(status: StatusCode) -> Response {
    (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store"), (header::X_FRAME_OPTIONS, "DENY")], SIGN_IN).into_response()
}

/// `POST /api/login`: `{"token": secret}` (or the secret alone), a user's
/// access token — the browser keeps a session signed in with it in this
/// recorder's cookie ([`access::Sessions`]) for 30 days, and the answer says
/// who signed in: `{access, user}` (the user's role and name, whether
/// signing in is on or not).
async fn login(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, body: String) -> Response {
    let secret = serde_json::from_str::<Value>(&body).ok().and_then(|v| v["token"].as_str().map(str::to_string)).unwrap_or(body);
    let secret = secret.trim();
    let found = access::user_of(&ctx.config.lock().unwrap().server.auth, secret).and_then(|(name, u)| Some((name.clone(), u.access, u.token.as_ref()?.hash.clone())));
    let Some((user, role, hash)) = found else { return (StatusCode::UNAUTHORIZED, "That isn't an access token of this recorder.\n").into_response() };
    signed_in_as(&ctx, &headers, &user, role, &hash)
}

/// A browser signed in as `user` (with `role`) with `credential`: its session's cookie, and `{access, user}`.
fn signed_in_as(ctx: &Ctx, headers: &HeaderMap, user: &str, role: ApiAccess, credential: &str) -> Response {
    let cookie = ctx.sessions.set_cookie(user, credential, https(ctx, headers));
    match HeaderValue::from_str(&cookie) {
        Ok(v) => ([(header::SET_COOKIE, v)], axum::Json(json!({ "access": role, "user": user }))).into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// Whether a request came over HTTPS: served so here, or by a proxy in front
/// that says so (`X-Forwarded-Proto`, or the page's `Origin`). It decides one
/// thing, the session cookie's `Secure`, so it's believed from anyone (a
/// Cloudflare Tunnel or Docker in front needs no `auth.proxy`): a forged
/// one only has the forger's own browser drop its cookie over plain HTTP.
/// Passkeys' origin never comes from it ([`passkey::Rp::of`]).
fn https(ctx: &Ctx, headers: &HeaderMap) -> bool {
    let h = |n: header::HeaderName| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    ctx.tls.get().is_some() || h(header::ORIGIN).starts_with("https://") || h(header::HeaderName::from_static("x-forwarded-proto")) == "https"
}

/// Where passkeys are for a request, if a browser allows them on the host it's
/// for (its `Host` picks localhost's or the host name's, no more), at the
/// built-in interface's origin ([`passkey::Rp::of`]).
fn passkey_rp(ctx: &Ctx, headers: &HeaderMap) -> Option<passkey::Rp> {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    passkey::Rp::of(&ctx.config.lock().unwrap().server, ctx.interfaces.builtin_port(), ctx.tls.get().is_some(), host)
}

/// A passkey request's relying party: on the built-in interface's port alone
/// (an interface's page could otherwise have a passkey answer for it, its
/// session good on every port), from a page of this origin's (or no page:
/// none of another's, even one `allowedOrigins` lets in), on a host passkeys
/// work on. Else the answer to it.
fn passkey_request(ctx: &Ctx, caller: &Caller, headers: &HeaderMap) -> Result<passkey::Rp, Response> {
    if let Door::Interface { .. } = caller.door {
        let at = passkey_rp(ctx, headers).map(|rp| format!(": {}/signin", rp.origin())).unwrap_or_default();
        return Err((StatusCode::FORBIDDEN, format!("Passkeys are on the recorder's own page{at}\n")).into_response());
    }
    let h = |n| headers.get(n).and_then(|v| v.to_str().ok());
    if admit(false, h(header::HOST), h(header::ORIGIN), &[]).is_err() {
        return Err((StatusCode::FORBIDDEN, "Passkeys: from this recorder's own pages only.\n").into_response());
    }
    passkey_rp(ctx, headers).ok_or_else(|| (StatusCode::BAD_REQUEST, "Passkeys work on localhost, or on the host name in server.hostname over HTTPS.\n").into_response())
}

/// A passkey route's answer: its JSON, or why not (400).
fn passkey_answer<T: serde::Serialize>(r: Result<T, String>) -> Response {
    match r {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e}\n")).into_response(),
    }
}

/// Who may add a passkey: a user signed in (they add one for themselves, on
/// their device; by the proxy in front too), on a port with control access — a listen interface's page
/// could otherwise give its own key to whoever's signed in there. Their name, or the answer.
fn passkey_adder(ctx: &Ctx, caller: &Caller) -> Result<String, Response> {
    let Some(name) = caller.who.user() else { return Err((StatusCode::UNAUTHORIZED, "Sign in first (signing in on), then add a passkey.\n").into_response()) };
    let port = caller.port_access(ctx, &ctx.config.lock().unwrap());
    if port != Some(ApiAccess::Control) || caller.access(ctx).is_none() {
        return Err((StatusCode::FORBIDDEN, "Passkeys are added on the recorder's own page.\n").into_response());
    }
    Ok(name.to_string())
}

/// `POST /api/passkey/register/start`: `{name}` (what to call it: "iPhone"),
/// for the user signed in: `{id, options}` ([`passkey::add_start`]).
async fn passkey_add_start(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, headers: HeaderMap, body: String) -> Response {
    let rp = match passkey_request(&ctx, &caller, &headers) {
        Ok(rp) => rp,
        Err(r) => return r,
    };
    let user = match passkey_adder(&ctx, &caller) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let name = serde_json::from_str::<Value>(&body).ok().and_then(|v| v["name"].as_str().map(str::to_string)).unwrap_or_default();
    let auth = ctx.config.lock().unwrap().server.auth.clone();
    passkey_answer(passkey::add_start(&ctx.passkeys, &rp, &auth, &user, &name))
}

/// `POST /api/passkey/register/finish`: `{id, credential}` (what
/// `navigator.credentials.create` made, as JSON, binary values base64url):
/// added to the user's passkeys and saved; `{name, id}` (its name, made theirs alone; its id).
async fn passkey_add_finish(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, headers: HeaderMap, body: String) -> Response {
    let rp = match passkey_request(&ctx, &caller, &headers) {
        Ok(rp) => rp,
        Err(r) => return r,
    };
    let user = match passkey_adder(&ctx, &caller) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let v = serde_json::from_str::<Value>(&body).unwrap_or_default();
    let p = match passkey::add_finish(&ctx.passkeys, &rp, &user, v["id"].as_str().unwrap_or(""), v["credential"].clone()) {
        Ok(p) => p,
        Err(e) => return passkey_answer::<()>(Err(e)),
    };
    let id = p.id.clone();
    let ctx2 = ctx.clone();
    let added = tokio::task::spawn_blocking(move || {
        let mut added = Err(String::new());
        edit_config(&ctx2, |c| added = passkey::add(&mut c.server.auth, &user, p))?;
        added
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    passkey_answer(added.map(|name| json!({ "name": name, "id": id })))
}

/// `POST /api/passkey/login/start`: `{id, options}` ([`passkey::sign_in_start`]). Open: it signs in.
async fn passkey_sign_in_start(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, headers: HeaderMap) -> Response {
    match passkey_request(&ctx, &caller, &headers) {
        Ok(rp) => passkey_answer(passkey::sign_in_start(&ctx.sessions, &rp)),
        Err(r) => r,
    }
}

/// `POST /api/passkey/login/finish`: `{id, credential}` (what
/// `navigator.credentials.get` answered): signed in as the passkey's user, as
/// `/api/login` signs in — the cookie, and `{access, user}`.
async fn passkey_sign_in_finish(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, headers: HeaderMap, body: String) -> Response {
    let rp = match passkey_request(&ctx, &caller, &headers) {
        Ok(rp) => rp,
        Err(r) => return r,
    };
    let v = serde_json::from_str::<Value>(&body).unwrap_or_default();
    let auth = ctx.config.lock().unwrap().server.auth.clone();
    let signed = match passkey::sign_in_finish(&ctx.passkeys, &ctx.sessions, &rp, &auth, v["id"].as_str().unwrap_or(""), v["credential"].clone()) {
        Ok(s) => s,
        Err(e) => return (StatusCode::UNAUTHORIZED, format!("{e}\n")).into_response(),
    };
    if signed.counted.is_some() {
        // (Its count, kept against the record as it is then: see passkey::keep_count. Not kept, a
        // copy of the passkey could count from the old number: no sign-in.)
        let (ctx2, signed2) = (ctx.clone(), signed.clone());
        let kept = tokio::task::spawn_blocking(move || edit_config(&ctx2, |c| void(passkey::keep_count(&mut c.server.auth, &signed2)))).await.unwrap_or_else(|e| Err(e.to_string()));
        if let Err(e) = kept {
            log::warn!("Passkey {} of {}: its counter couldn't be kept ({e}): not signed in", signed.id, signed.user);
            return (StatusCode::SERVICE_UNAVAILABLE, format!("Not signed in: the passkey's count couldn't be saved ({e}).\n")).into_response();
        }
    }
    let Some(role) = ctx.config.lock().unwrap().server.auth.users.get(&signed.user).map(|u| u.access) else { return (StatusCode::UNAUTHORIZED, "Not a passkey of this recorder's (removed?).\n").into_response() };
    signed_in_as(&ctx, &headers, &signed.user, role, &access::passkey_credential(&signed.id))
}

/// `POST /api/logout`: this recorder's cookie, expired (other recorders' on
/// the same host name are theirs). `{"all": true}`: signed out everywhere
/// too, every browser's session of the user's signed in until now
/// (`signedOut`; their access token's still good: made again, it isn't), on
/// the built-in interface's port; `{"all": true, "user": name}` another
/// user, by a user with control access.
async fn logout(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, body: String) -> Response {
    let v = serde_json::from_str::<Value>(&body).unwrap_or_default();
    let signed_out = |clear: bool| {
        let mut res = StatusCode::NO_CONTENT.into_response();
        if let (true, Ok(v)) = (clear, HeaderValue::from_str(&ctx.sessions.clear_cookie())) {
            res.headers_mut().insert(header::SET_COOKIE, v);
        }
        res
    };
    if v["all"] != true {
        return signed_out(true);
    }
    if !matches!(caller.door, Door::Builtin) {
        return (StatusCode::FORBIDDEN, "Signing out everywhere is on the recorder's own page.\n").into_response();
    }
    let me = match &caller.who {
        Who::User { name, .. } if caller.who.grants(&ctx.config.lock().unwrap().server.auth).is_some() => Some(name.clone()),
        _ => None,
    };
    let user = match v["user"].as_str() {
        Some(u) if me.as_deref() != Some(u) && caller.access(&ctx) != Some(ApiAccess::Control) => return (StatusCode::FORBIDDEN, "Another user is signed out everywhere by a user with control access.\n").into_response(),
        Some(u) => u.to_string(),
        None => match me.clone() {
            Some(u) => u,
            None if matches!(caller.who, Who::Proxy { .. }) => return (StatusCode::FORBIDDEN, "Signed in by the proxy in front: signing out is the proxy's.\n").into_response(),
            None => return (StatusCode::UNAUTHORIZED, "Sign in first.\n").into_response(),
        },
    };
    let (ctx2, who) = (ctx.clone(), user.clone());
    let done = tokio::task::spawn_blocking(move || {
        let mut found = false;
        edit_config(&ctx2, |c| {
            if let Some(u) = c.server.auth.users.get_mut(&who) {
                u.signed_out = Some(crate::clock::unix_s() as i64);
                found = true;
            }
        })?;
        found.then_some(()).ok_or_else(|| format!("There's no user named \"{who}\"."))
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match done {
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e}\n")).into_response(),
        Ok(()) => {
            let by = caller.who.user().filter(|u| *u != user).map(|u| format!(" by {u}")).unwrap_or_default();
            log::info!("User {user}: signed out everywhere{by}");
            signed_out(me.as_deref() == Some(&user))
        }
    }
}

/// `GET /api/session`: whether signing in is on, what this request may do
/// on this port as whom (a page's WebSocket can't see a 401: this is how it
/// learns it must sign in), whether the proxy in front signed it in (then
/// it's the proxy's to sign out of), whether passkeys work here (the built-in
/// interface's port, on a host they work on) and the one origin they work
/// at (`passkeyOrigin`, from any port: a page elsewhere links there), and
/// the user's own passkeys (`ownPasskeys`: `{id, name, created}` each, never
/// their keys; left out when none, or not signed in as a user).
async fn signed_in(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, headers: HeaderMap) -> Response {
    let access = caller.access(&ctx);
    let (enabled, own) = {
        let c = ctx.config.lock().unwrap();
        let user = access.and(caller.who.user()).and_then(|u| c.server.auth.users.get(u));
        let own: Vec<Value> = user.map(|u| u.passkeys.iter().map(|p| json!({ "id": p.id, "name": p.name, "created": p.created })).collect()).unwrap_or_default();
        (c.server.auth.enabled, own)
    };
    let proxy = matches!(caller.who, Who::Proxy { .. });
    let rp = passkey_rp(&ctx, &headers);
    let builtin = matches!(caller.door, Door::Builtin);
    let mut answer = json!({ "auth": enabled, "access": access, "user": caller.who.user(), "proxy": proxy, "passkeys": builtin && rp.is_some() });
    if let Some(rp) = rp {
        answer["passkeyOrigin"] = rp.origin().into();
    }
    if !own.is_empty() {
        answer["ownPasskeys"] = Value::Array(own);
    }
    axum::Json(answer).into_response()
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
pub(crate) fn host_name(host: &str) -> String {
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
/// it routes with the URL's #). Only on its own port. Its page is never in
/// another page's frame, as the sign-in page isn't ([`sign_in_page`]).
fn builtin(path: &str) -> Response {
    let path = if path.is_empty() { "index.html" } else { path };
    match Ui::get(path).or_else(|| Ui::get("index.html")) {
        Some(f) => {
            let mime = mime_guess::from_path(if Ui::get(path).is_some() { path } else { "index.html" }).first_or_octet_stream();
            let mut res = ([(header::CONTENT_TYPE, mime.as_ref().to_string())], f.data).into_response();
            if mime.essence_str() == "text/html" {
                res.headers_mut().insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
            }
            res
        }
        None => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()), (header::X_FRAME_OPTIONS, "DENY".to_string())],
            "<h1>Trunk Recorder Pro</h1><p>The web interface wasn't built into this binary. Run <code>npm run build</code> in <code>web/</code>, then rebuild.</p>",
        )
            .into_response(),
    }
}

/// A screenshot of an interface in the store (the Interfaces page shows
/// them), fetched and checked by the recorder, so the page needn't reach
/// GitHub. Built-in interface's port only, control access.
async fn store_screenshot(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, UrlPath(sha): UrlPath<String>) -> Response {
    if caller.access(&ctx) != Some(ApiAccess::Control) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match tokio::task::spawn_blocking(move || ctx.interfaces.store_screenshot(&sha)).await {
        Ok(Ok((data, mime))) => ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "max-age=86400, immutable")], data).into_response(),
        Ok(Err(e)) => (StatusCode::NOT_FOUND, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Screenshot `n` of an installed interface (its interface.json's), from its folder. Control access.
async fn interface_screenshot(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, UrlPath((id, n)): UrlPath<(String, usize)>) -> Response {
    if caller.access(&ctx) != Some(ApiAccess::Control) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let found = {
        let cfg = ctx.config.lock().unwrap();
        ctx.interfaces.screenshot(&cfg, &ctx.config_path, &id, n)
    };
    let Some((file, mime)) = found else { return StatusCode::NOT_FOUND.into_response() };
    match tokio::fs::read(&file).await {
        Ok(data) => ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "no-cache")], data).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A file of interface `id`, on its port.
async fn interface_file(ctx: &Ctx, id: &str, uri: &Uri) -> Response {
    let dir = {
        let cfg = ctx.config.lock().unwrap();
        ctx.interfaces.all(&cfg, &ctx.config_path).remove(id).map(|(_, dir)| dir)
    };
    match (dir, crate::toolkit::percent_decode(uri.path().trim_start_matches('/'))) {
        (Some(dir), Some(rel)) => folder_file(&dir, &rel, uri).await,
        (None, _) => not_found(&format!("There's no interface {id} any more.")),
        (_, None) => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// File `rel` of an interface's folder, as it is on disk now (edits show on
/// reload): a folder's index.html, and the root's index.html for a path
/// that isn't a file and has no extension (a page that routes itself).
/// Nothing outside the folder, links included, and no hidden files (its
/// `.install.json`, a `.git`).
async fn folder_file(root: &Path, rel: &str, uri: &Uri) -> Response {
    let rel = Path::new(rel);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if rel.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.')) {
        return not_found(&format!("{} is hidden.", uri.path()));
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
        Ok(bytes) => {
            let headers = [(header::CONTENT_TYPE, mime_of(&real.to_string_lossy())), (header::CACHE_CONTROL, "no-cache".to_string()), (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string())];
            (headers, bytes).into_response()
        }
        Err(e) => not_found(&format!("{}: {e}", real.display())),
    }
}

fn not_found(why: &str) -> Response {
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Not found</title><body style=\"font:15px system-ui;margin:40px\"><h1>Not found</h1><p>{}</p>\
         <p><a href=\"/\">Home</a> · <a href=\"/api/docs\">The API</a></p>",
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A recorded file, confined to the capture folder (or the RAM spool, while
/// it waits there for the plugins). A call's .wav that was never written
/// (only uploaded) is its .m4a, and an .m4a not (yet) encoded its .wav.
/// Byte ranges are served: Safari plays audio only from a server that does.
async fn call_file(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, UrlPath(path): UrlPath<String>, req: Request) -> Response {
    if caller.access(&ctx).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut dirs = vec![PathBuf::from(&ctx.config.lock().unwrap().recording.capture_dir)];
    dirs.extend(ctx.spool.lock().unwrap().as_ref().map(|s| s.dir.clone()));
    let rel = Path::new(&path);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut tries: Vec<PathBuf> = vec![rel.to_path_buf()];
    match rel.extension().and_then(|x| x.to_str()) {
        Some("wav") => tries.push(rel.with_extension("m4a")),
        Some("m4a") => tries.push(rel.with_extension("wav")),
        _ => {}
    }
    let range = req.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("bytes=")).map(String::from);
    for r in &tries {
        for d in &dirs {
            if let Ok(bytes) = tokio::fs::read(d.join(r)).await {
                let mime = mime_guess::from_path(r).first_or_octet_stream().as_ref().to_string();
                let len = bytes.len();
                let Some(spec) = range.as_deref() else {
                    return ([(header::CONTENT_TYPE, mime), (header::ACCEPT_RANGES, "bytes".into())], bytes).into_response();
                };
                // One range: `a-b`, `a-` or `-n` (the last n bytes).
                let (a, b) = spec.split(',').next().unwrap_or("").split_once('-').unwrap_or(("", ""));
                let (start, end) = match (a.trim().parse::<usize>().ok(), b.trim().parse::<usize>().ok()) {
                    (Some(s), Some(e)) => (s, e.min(len.saturating_sub(1))),
                    (Some(s), None) => (s, len.saturating_sub(1)),
                    (None, Some(n)) => (len.saturating_sub(n), len.saturating_sub(1)),
                    (None, None) => (1, 0),
                };
                if start > end || start >= len {
                    return (StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{len}"))]).into_response();
                }
                let headers = [(header::CONTENT_TYPE, mime), (header::ACCEPT_RANGES, "bytes".into()), (header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))];
                return (StatusCode::PARTIAL_CONTENT, headers, bytes[start..=end].to_vec()).into_response();
            }
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn ws(State(ctx): State<Arc<Ctx>>, Extension(caller): Extension<Caller>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |socket| session(ctx, socket, caller))
}

pub(crate) fn devices() -> ToClient {
    ToClient::Devices { devices: json!(crate::radio::rtl::devices()) }
}

/// The `dir` message, for the interface's folder picker: a folder on this
/// computer, its subfolders and its .json files (a Trunk Recorder config to
/// import); "" = the home folder. A path that doesn't exist yet lists the
/// nearest folder above it that does.
pub(crate) fn dir(path: &str) -> ToClient {
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
    ToClient::Dir(body(json!({
        "path": at.display().to_string(),
        "parent": at.parent().map(|p| p.display().to_string()),
        "dirs": dirs,
        "files": files,
        "home": home.display().to_string(),
        "sep": std::path::MAIN_SEPARATOR.to_string(),
        "error": error,
    })))
}

/// The `trConfig` message: a Trunk Recorder config.json (the file, or a folder
/// holding one) with the talkgroup and channel files it names, read here so
/// the browser imports them in one go. A name resolves beside the config (as
/// Trunk Recorder run from its folder does), else by its file name there.
pub(crate) fn tr_config(path: &str) -> ToClient {
    let p = PathBuf::from(path.trim());
    let file = if p.is_dir() { p.join("config.json") } else { p };
    let shown = file.display().to_string();
    let fail = |e: String| ToClient::TrConfig(body(json!({ "path": shown, "error": e })));
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
    ToClient::TrConfig(body(json!({ "path": shown, "text": text, "files": files, "error": null })))
}

/// What a connection is told first, as ordinary messages: how the recorder
/// is now, then `hello` (it's caught up), with its `access` and `user` (who
/// it signed in as). Control access gets what only it does
/// (devices, key containers, plugins, interfaces); a listen-only connection
/// gets the config without secrets ([`Out::of`]).
pub(crate) async fn greeting(ctx: &Arc<Ctx>, access: ApiAccess, user: Option<String>) -> Vec<ToClient> {
    let control = access == ApiAccess::Control;
    let ctx2 = ctx.clone();
    let found = tokio::task::spawn_blocking(move || (control.then(|| json!(crate::radio::rtl::devices())), crate::radio::radios_json(false), runtime::history(&ctx2), kept_now(&ctx2))).await;
    let (devices, radios, history, kept) = found.unwrap_or((None, Value::Null, ToClient::History { entries: Vec::new(), next: None }, Vec::new()));
    let mut m = greeting_now(ctx, devices, radios, history, kept);
    if control {
        m.push(plugins_list(ctx).await);
        let cfg = ctx.config.lock().unwrap().clone();
        m.push(ctx.interfaces.list(&cfg, &ctx.config_path));
    }
    // (A store that couldn't open: said to everyone who connects, until it's fixed.)
    if let Some(why) = ctx.db.failure() {
        m.push(ToClient::error(format!("The store couldn't be opened ({why}). Nothing is kept until it opens: talkgroups, aliases, the radio registry, the call index and the dashboard's history. Recording goes on.")));
    }
    m.push(ToClient::Hello {
        version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        api: crate::interfaces::API_VERSION,
        config_path: control.then(|| ctx.config_path.display().to_string()),
        survey_bands: trunk_app::survey::bands_json(),
        capabilities: capabilities(ctx),
        access,
        user,
    });
    m
}

/// What `hello` says the recorder can do: everything, but `calls` and
/// `history` only while the call store is open.
fn capabilities(ctx: &Ctx) -> Vec<trunk_app::protocol::Capability> {
    use trunk_app::protocol::Capability;
    Capability::ALL.iter().copied().filter(|c| match c {
        Capability::Calls => ctx.db.indexes_calls(),
        Capability::History => !ctx.db.is_disabled(),
        _ => true,
    }).collect()
}

/// The greeting's messages from what's kept between runs (the store, or
/// files: so it blocks): each system's talker aliases, and the codes
/// conventional frequencies carried as saved (if not told since).
fn kept_now(ctx: &Ctx) -> Vec<ToClient> {
    use crate::kept;
    let config = ctx.config.lock().unwrap().clone();
    let mut m: Vec<ToClient> = config.systems.iter().map(|s| &s.short_name).chain(config.conventional.iter().map(|c| &c.short_name)).filter_map(|n| Some(ToClient::Units { system: n.clone(), csv: kept::read_units(&ctx.db, n)? })).collect();
    if ctx.heard_last.lock().unwrap().is_none() {
        let saved = kept::read(&ctx.db, trunk_app::saving::Kind::Heard, "").and_then(|t| serde_json::from_str::<Value>(&t).ok());
        m.push(ToClient::Heard { heard: saved.unwrap_or(json!({})) });
    }
    m
}

/// The greeting's messages that need nothing found or listed: `devices` for
/// control access only; `kept` from [`kept_now`].
pub(crate) fn greeting_now(ctx: &Ctx, devices: Option<Value>, radios: Value, history: ToClient, kept: Vec<ToClient>) -> Vec<ToClient> {
    let config = ctx.config.lock().unwrap().clone();
    let mut m = vec![ctx.phase.lock().unwrap().message(), ToClient::Config { config: Box::new(config.clone()), version: ctx.config_version.load(Ordering::SeqCst) }];
    for (system, n) in ctx.names.lock().unwrap().iter() {
        m.push(ToClient::Talkgroups { system: system.clone(), talkgroups: n.talkgroups.clone() });
        m.push(ToClient::UnitNames { system: system.clone(), unit_names: n.unit_names.clone() });
    }
    // Each system's talker aliases; the codes conventional frequencies carried: as last told, else as saved.
    let (heard, units): (Vec<ToClient>, Vec<ToClient>) = kept.into_iter().partition(|k| matches!(k, ToClient::Heard { .. }));
    m.extend(units);
    // Each system's MDC1200 radios, as the registry has them (their IDs shown in hex).
    {
        let radio = ctx.shared.radio.lock().unwrap();
        for system in radio.systems.keys() {
            let units = radio.mdc_units(system);
            if !units.is_empty() {
                m.push(ToClient::MdcUnits { system: system.clone(), units });
            }
        }
    }
    m.extend(ctx.heard_last.lock().unwrap().clone().or_else(|| heard.into_iter().next()));
    m.push(history);
    m.push(ToClient::Radios { radios });
    m.push(ctx.survey_last.lock().unwrap().clone().unwrap_or_else(crate::survey::idle));
    // The dashboard: recent notable events (oldest first), the computer as last sampled.
    m.extend(ctx.shared.recent_events().into_iter().map(|e| ToClient::MonitorEvent(body(e))));
    m.extend(ctx.host_last.lock().unwrap().clone());
    if let Some(devices) = devices {
        m.push(ToClient::Devices { devices });
        // Key containers waiting for a password, and which are unlocked.
        m.push(crate::unlock::status(ctx));
    }
    m
}

/// A message as a connection with (or without) control access hears it, if it may.
pub(crate) fn text_for(out: &Out, control: bool) -> Option<String> {
    match out {
        Out::Text(s) => Some(s.clone()),
        Out::Control(s) => control.then(|| s.clone()),
        Out::Config { full, listen } => Some(if control { full } else { listen }.clone()),
        Out::Topic { .. } | Out::Audio { .. } | Out::Quit => None,
    }
}

/// What one connection watches ([`trunk_app::stats::Topics`]); counted in
/// the shared tally while it lasts.
struct Subs {
    ctx: Arc<Ctx>,
    topics: std::collections::BTreeSet<String>,
}

impl Subs {
    fn new(ctx: &Arc<Ctx>, topics: std::collections::BTreeSet<String>) -> Subs {
        let mut s = Subs { ctx: ctx.clone(), topics: Default::default() };
        s.set(topics);
        s
    }

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


/// `statsQuery`: the minute history.
/// `statsQuery`: within what memory keeps (a week with no store; with one,
/// a day and a bit), from memory; further back, the store's hours before the
/// last day and memory's minutes since, added into one grid of whole hours:
/// each answer a few hundred points a series, whatever the span. Blocks: run
/// it under `spawn_blocking`.
pub(crate) fn stats_query(ctx: &Ctx, q: &StatsQuery) -> ToClient {
    use trunk_app::stats::{Bucket, Buckets, Span};
    let now = crate::clock::unix_s() as i64;
    let (from, to) = q.span(now);
    let h = ctx.series.lock().unwrap();
    let kept = h.cap_minutes() as i64 * 60;
    if ctx.db.is_disabled() || from >= now - kept + 60 {
        return q.answer(&h, now, h.loading);
    }
    let (patterns, loading) = (q.patterns(), h.loading);
    let span = Span::of(from, to, q.points, 60);
    // (On the hour, at least two hours inside what memory keeps.)
    let split = (now - kept + 2 * 3600).div_euclid(3600) * 3600 + 3600;
    let mut b = Buckets::new();
    h.fill(&patterns, &span, split.div_euclid(60), &mut b);
    drop(h);
    let hours = crate::db::store::HoursQuery { patterns: &patterns, span, from: span.m0 * 60, before: split };
    match ctx.db.stats_hours(&hours) {
        Ok(rows) => {
            for (k, i, h) in rows {
                if let Some(i) = usize::try_from(i).ok().filter(|i| *i < span.n) {
                    b.entry(k).or_insert_with(|| vec![Bucket::default(); span.n])[i].add(h.sum, h.n, h.lo, h.hi);
                }
            }
        }
        Err(e) => log::warn!("Call store: the dashboard's history not read ({e})"),
    }
    q.result(from, to, loading, span.answer(b))
}

/// `radioQuery`: the registry, with talkgroup names from the config and radio names as saved.
pub(crate) fn radio_query(ctx: &Ctx, q: &Body) -> ToClient {
    ctx.shared.radio_query(&Value::Object(q.clone()), crate::clock::unix_s() as i64)
}

/// How long a connection may take to take one message. One that doesn't
/// read (a tab frozen in the background) is closed rather than waited on, and
/// gets everything afresh when it reconnects.
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// A plugin's button waits this long for its answer (the browser's `ask` gives up at 20 s).
const PLUGIN_ACTION_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// One message to a connection, within [`SEND_TIMEOUT`]: false when it's
/// gone or isn't reading, and is to be dropped.
async fn send(socket: &mut WebSocket, m: Message) -> bool {
    matches!(tokio::time::timeout(SEND_TIMEOUT, socket.send(m)).await, Ok(Ok(())))
}

/// One WebSocket, with the access `caller` had when it connected. Once that
/// changes (the config's interfaces or users did), or what it signed in
/// with expires (a session's cookie, Cloudflare's token: [`Who::until`]),
/// it's closed: it reconnects, and is greeted afresh with what it has now.
async fn session(ctx: Arc<Ctx>, mut socket: WebSocket, caller: Caller) {
    let mut rx = ctx.hub.subscribe();
    let Some(access) = caller.access(&ctx) else {
        send(&mut socket, Message::Close(None)).await;
        return;
    };
    let control = access == ApiAccess::Control;
    for m in greeting(&ctx, access, caller.who.user().map(str::to_string)).await {
        let Some(text) = text_for(&Out::of(&m), control) else { continue };
        if !send(&mut socket, Message::Text(text.into())).await {
            return;
        }
    }
    // An interface's connections close when it's no longer served on its port.
    let mut stop = match &caller.door {
        Door::Interface { stop, .. } => Some(stop.clone()),
        Door::Builtin => None,
    };
    // Live audio: off until the browser asks; optionally one system and/or talkgroup only.
    let mut listen: Option<Listen> = None;
    let mut subs = Subs::new(&ctx, Default::default());
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(out) => {
                    let m = match &*out {
                        Out::Text(s) => Message::Text(s.clone().into()),
                        Out::Control(s) if control => Message::Text(s.clone().into()),
                        Out::Control(_) => continue,
                        // The config changed: so may this connection's access.
                        Out::Config { full, listen } => {
                            if caller.access(&ctx) != Some(access) {
                                send(&mut socket, Message::Close(None)).await;
                                return;
                            }
                            Message::Text(if control { full } else { listen }.clone().into())
                        }
                        Out::Topic { topic, text } if subs.topics.contains(topic) => Message::Text(text.clone().into()),
                        Out::Topic { .. } => continue,
                        Out::Audio { short_name, tg, frame, voice } => match &listen {
                            Some(l) if l.wants(short_name, *tg) => {
                                if let (Some(_), Some(v)) = (&l.voice, voice)
                                    && !send(&mut socket, Message::Text(v.clone().into())).await
                                {
                                    send(&mut socket, Message::Close(None)).await;
                                    return;
                                }
                                if !l.audio {
                                    continue;
                                }
                                Message::Binary(frame.clone().into())
                            }
                            _ => continue,
                        },
                        Out::Quit => Message::Text(ToClient::Quit.to_json().into()),
                    };
                    let quit = matches!(&*out, Out::Quit);
                    if !send(&mut socket, m).await || quit {
                        send(&mut socket, Message::Close(None)).await;
                        return;
                    }
                }
                // It fell behind and missed messages: what it shows is no
                // longer so. Closed, it reconnects and is greeted afresh.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    send(&mut socket, Message::Close(None)).await;
                    return;
                }
                Err(_) => return,
            },
            _ = async {
                match stop.as_mut() {
                    Some(s) => drop(s.wait_for(|stopped| *stopped).await),
                    None => std::future::pending().await,
                }
            } => {
                send(&mut socket, Message::Close(None)).await;
                return;
            },
            _ = expiry(caller.who.until()) => {
                send(&mut socket, Message::Close(None)).await;
                return;
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    // As it is now, not as the last config message this connection saw.
                    if caller.access(&ctx) != Some(access) {
                        send(&mut socket, Message::Close(None)).await;
                        return;
                    }
                    let reply = match Command::parse(&t) {
                        Ok(c) if access >= c.needs() => {
                            // (Who changed what: by its kind alone, never what it carries.)
                            match (&caller.who, c.needs()) {
                                (Who::User { name, .. }, ApiAccess::Control) => log::info!("User {name}: {}", c.kind()),
                                (Who::Proxy { user, .. }, ApiAccess::Control) => log::info!("User {user} (by the proxy): {}", c.kind()),
                                _ => {}
                            }
                            command(&ctx, c, &mut listen, &mut subs).await
                        }
                        Ok(c) => Some(ToClient::error(access::refusal(&ctx, &caller, c.kind()))),
                        Err(e) => Some(ToClient::error(e)),
                    };
                    if let Some(reply) = reply {
                        if !send(&mut socket, Message::Text(reply.to_json().into())).await {
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

/// Until `until` (Unix s) has passed; None: never.
async fn expiry(until: Option<u64>) {
    match until {
        Some(t) => tokio::time::sleep(std::time::Duration::from_secs(t.saturating_sub(crate::clock::unix_s() as u64))).await,
        None => std::future::pending().await,
    }
}

/// Change the config on the recorder's side (a plugin added or removed):
/// saved, sent to every browser, and the plugins restarted if it changes them.
pub(crate) fn edit_config(ctx: &Ctx, f: impl FnOnce(&mut crate::config::Config)) -> Result<(), String> {
    let mut cfg = ctx.config.lock().unwrap();
    let old = cfg.clone();
    f(&mut cfg);
    if *cfg == old {
        return Ok(());
    }
    cfg.save(&ctx.config_path).map_err(|e| format!("Couldn't save the config: {e}"))?;
    ctx.config_changed(&cfg);
    let c = cfg.clone();
    drop(cfg);
    ctx.plugins.set_delays(&c);
    ctx.plugins.apply(&old, &c);
    Ok(())
}

fn void<T>(_: T) {}

/// The `plugins` message (asking plugins who they are can take a moment).
async fn plugins_list(ctx: &Arc<Ctx>) -> ToClient {
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || {
        let cfg = ctx2.config.lock().unwrap().clone();
        ctx2.plugins.list(&cfg)
    })
    .await
    .unwrap_or_else(|e| ToClient::error(e.to_string()))
}

/// What live audio a connection asked for: one system's (by short name;
/// whichever of its sites a call plays from) and / or one talkgroup's, or
/// everything.
struct Listen {
    system: Option<String>,
    talkgroup: Option<u32>,
    /// The PCM (type 2).
    audio: bool,
    /// The vocoder frames (`voice` messages): watched as topic `voice`, so
    /// they're built while it lasts.
    voice: Option<Subs>,
}

impl Listen {
    fn wants(&self, short_name: &str, tg: u32) -> bool {
        self.system.as_deref().is_none_or(|s| s == short_name) && self.talkgroup.is_none_or(|t| t == tg)
    }
}

/// `msg` as an error message, if there is one.
fn failed(msg: Result<(), String>) -> Option<ToClient> {
    msg.err().map(ToClient::error)
}

/// Do what a connection asked (its access allows it); its answer, if any, is for it alone.
async fn command(ctx: &Arc<Ctx>, cmd: Command, listen: &mut Option<Listen>, subs: &mut Subs) -> Option<ToClient> {
    match cmd {
        // What this connection watches: costly messages (waterfalls, the
        // control channel log, detail views) only go to those that ask.
        Command::Subscribe { topics } => {
            subs.set(topics.into_iter().take(64).collect());
            Some(ToClient::Subscribed { topics: subs.topics.iter().cloned().collect() })
        }
        Command::StatsQuery(q) => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || stats_query(&ctx2, &q)).await.ok()
        }
        // (Answered to this connection alone, once the plugin says how it went.)
        Command::PluginAction { id, plugin, action, value } => {
            let host = ctx.plugins.host();
            let answer = tokio::task::spawn_blocking(move || host.ok_or("It isn't running: plugins run while recording".to_string())?.action(&plugin, action, value, PLUGIN_ACTION_WAIT))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let (ok, message) = answer.unwrap_or_else(|e| (false, e));
            Some(ToClient::PluginActionResult { id, ok, message })
        }
        Command::RadioQuery(q) => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || radio_query(&ctx2, &q)).await.ok()
        }
        Command::CallsQuery { id, query } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || runtime::calls_query(&ctx2, id, &query)).await.ok()
        }
        Command::SetConfig { config, version } => match Config::from_value(config) {
            Ok(c) if crate::iq_rules::check_all(&c.diagnostics).is_err() => {
                let error = crate::iq_rules::check_all(&c.diagnostics).err();
                Some(ToClient::ConfigSaved { version: ctx.config_version.load(Ordering::SeqCst), error })
            }
            Ok(c) if c.server.auth.problem().is_some() => Some(ToClient::ConfigSaved { version: ctx.config_version.load(Ordering::SeqCst), error: c.server.auth.problem() }),
            Ok(c) if crate::tls::problem(&c, &ctx.config_path).is_some() => Some(ToClient::ConfigSaved { version: ctx.config_version.load(Ordering::SeqCst), error: crate::tls::problem(&c, &ctx.config_path) }),
            Ok(mut c) => {
                // While linked, a conventional system's channels are its file's
                // (re-read: a spreadsheet may have changed it). Files are linked
                // with "channelFile".
                c.load_channel_files(&ctx.config_path);
                // Only an edit of the config as it is now: another browser's change since isn't lost.
                let (old, saved, now) = {
                    let mut cur = ctx.config.lock().unwrap();
                    let at = ctx.config_version.load(Ordering::SeqCst);
                    if version != at {
                        let error = "The config was changed elsewhere meanwhile, so this edit wasn't saved: here it is as it is now.".to_string();
                        return Some(ToClient::ConfigSaved { version: at, error: Some(error) });
                    }
                    let saved = c.save(&ctx.config_path);
                    let old = std::mem::replace(&mut *cur, c.clone());
                    (old, saved, ctx.config_changed(&cur))
                };
                if old.log != c.log {
                    crate::logging::configure(&c.log, ctx.config_path.parent().unwrap_or(std::path::Path::new(".")));
                }
                if old.keys != c.keys || old.systems != c.systems || old.conventional != c.conventional {
                    crate::unlock::config_changed(ctx, &c);
                }
                // Their delays apply at once, to what's waiting too (no restart).
                ctx.plugins.set_delays(&c);
                // Sharing switched on or off (or what it uses changed, like licenses): at once too.
                for sys in &c.systems {
                    if old.systems.iter().find(|o| o.short_name == sys.short_name) != Some(sys) {
                        ctx.tell_engine(crate::runtime::EngineCmd::ShareSystem(sys.clone()));
                    }
                }
                // A radio's gain changed while recording: it takes it now (the ones that can), the
                // rest at the next start, which the log says; the banner says whether a restart waits.
                for (i, g) in ctx.live_sources(&old, &c) {
                    if let Some(g) = g {
                        log::info!("{}: gain changed while recording, applied now: {g}", c.sources[i].title(i));
                    } else {
                        log::info!("{}: changed while recording; the change applies when recording starts next (Stop, then Start)", c.sources[i].title(i));
                    }
                }
                ctx.restart_check(&c);
                // Interfaces turned on or off, moved, added: served so at once.
                if old.server.interfaces != c.server.interfaces {
                    tokio::spawn(crate::interfaces::serve::reconcile(ctx.clone()));
                }
                // Plugins' settings changed while recording: sent to those that take them as
                // they run, else they restart with them.
                let ctx2 = ctx.clone();
                let _ = tokio::task::spawn_blocking(move || ctx2.plugins.apply(&old, &c)).await;
                Some(ToClient::ConfigSaved { version: now, error: saved.err().map(|e| format!("Couldn't save the config: {e}")) })
            }
            Err(e) => Some(ToClient::ConfigSaved { version: ctx.config_version.load(Ordering::SeqCst), error: Some(format!("Bad config: {e}")) }),
        },
        // (Written beside the config; a recording takes it at once.)
        // (Off the async workers: the store's queue can be full, and the edit holds the talkgroups' lock while it hands over.)
        Command::SetTalkgroups { system, talkgroups, from } => {
            let ctx2 = ctx.clone();
            failed(tokio::task::spawn_blocking(move || runtime::set_names(&ctx2, &system, talkgroups, from)).await.unwrap_or_else(|e| Err(e.to_string())))
        }
        Command::SetUnitNames { system, unit_names, from } => {
            let ctx2 = ctx.clone();
            failed(tokio::task::spawn_blocking(move || runtime::set_names(&ctx2, &system, unit_names, from)).await.unwrap_or_else(|e| Err(e.to_string())))
        }
        Command::SetUnitName { system, unit, name } => {
            let ctx2 = ctx.clone();
            failed(tokio::task::spawn_blocking(move || runtime::set_unit_name(&ctx2, &system, unit, name)).await.unwrap_or_else(|e| Err(e.to_string())))
        }
        // (Captures read and write the disk: off the async workers.)
        Command::IqCaptures { id } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || crate::iq_capture::listing(&ctx2, id, None)).await.ok()
        }
        Command::IqCaptureStart { id, seconds, note, what, problem } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let error = crate::iq_capture::start(&ctx2, what, seconds, &note, problem).err();
                crate::iq_capture::listing(&ctx2, id, error)
            })
            .await
            .ok()
        }
        Command::IqCaptureReview { id, name } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || crate::iq_capture::review(&ctx2, id, &name)).await.ok()
        }
        Command::IqCaptureSend { id, name } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let error = crate::iq_capture::send(&ctx2, &name).err();
                crate::iq_capture::listing(&ctx2, id, error)
            })
            .await
            .ok()
        }
        Command::IqCaptureWithdraw { id, name } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let error = crate::iq_capture::withdraw(&ctx2, &name).err();
                crate::iq_capture::listing(&ctx2, id, error)
            })
            .await
            .ok()
        }
        Command::IqRuleCheck { id, rule } => Some(ToClient::IqRuleChecked {
            id,
            error: serde_json::from_value::<trunk_app::config::CaptureRule>(rule).map_err(|e| e.to_string()).and_then(|r| crate::iq_rules::check(&r)).err(),
        }),
        Command::IqCaptureCancel { id, key } => {
            crate::iq_capture::cancel(&ctx, key);
            Some(crate::iq_capture::listing(&ctx, id, None))
        }
        Command::IqCaptureDelete { id, name } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let error = crate::iq_capture::delete(&ctx2, &name).err();
                crate::iq_capture::listing(&ctx2, id, error)
            })
            .await
            .ok()
        }
        // (Counting and deleting read and write the store: off the async workers.)
        Command::DataSummary { id } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || crate::data::summary(&ctx2, id)).await.ok()
        }
        Command::DeleteData { id, kinds, system } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || crate::data::delete(&ctx2, id, &kinds, system)).await.ok()
        }
        Command::CompactData { id } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || crate::data::compact(&ctx2, id)).await.ok()
        }
        Command::SetTalkgroup { system, talkgroup, edit } => {
            let ctx2 = ctx.clone();
            failed(tokio::task::spawn_blocking(move || runtime::set_talkgroup(&ctx2, &system, talkgroup, edit)).await.unwrap_or_else(|e| Err(e.to_string())))
        }
        // A key container with no passwordFile, unlocked with a password typed in, or locked again
        // (docs/configuration/systems.md). The password is never kept.
        Command::UnlockKeys { source, password } => {
            let (ctx2, password) = (ctx.clone(), zeroize::Zeroizing::new(password));
            let r = tokio::task::spawn_blocking(move || crate::unlock::unlock(&ctx2, &source, &password)).await.unwrap_or_else(|e| Err(e.to_string()));
            failed(r)
        }
        Command::LockKeys { source } => {
            crate::unlock::lock(ctx, &source);
            None
        }
        // Link conventional system `index`'s channels to a CSV (created from
        // its list if new), reload it (the same path again), or unlink ("").
        Command::ChannelFile { index, path } => {
            let mut r = Ok(());
            // (A failed link that left the channels unlinked is no edit.)
            let saved = edit_config(&ctx, |c| {
                let before = c.clone();
                r = c.link_channel_file(index, &ctx.config_path, &path);
                if r.is_err() && c.conventional.get(index).is_none_or(|x| x.channel_file.is_empty()) {
                    *c = before;
                }
            });
            if let Err(e) = saved {
                return Some(ToClient::error(e));
            }
            failed(r)
        }
        Command::Start => {
            let cfg = ctx.config.lock().unwrap().clone();
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || ctx2.begin_recording(cfg)).await.ok().and_then(Result::err).map(ToClient::error)
        }
        Command::Stop => {
            let ctx2 = ctx.clone();
            let _ = tokio::task::spawn_blocking(move || ctx2.stop_recording()).await;
            None
        }
        Command::Devices => tokio::task::spawn_blocking(devices).await.ok(),
        Command::FindRadios => {
            let radios = tokio::task::spawn_blocking(|| crate::radio::radios_json(true)).await.unwrap_or(Value::Null);
            Some(ToClient::Radios { radios })
        }
        Command::Quit => {
            let ctx2 = ctx.clone();
            let _ = tokio::task::spawn_blocking(move || ctx2.stop_survey()).await;
            ctx.quit.notify_one();
            None
        }
        Command::SurveyStart(b) => {
            let cfg = ctx.config.lock().unwrap().clone();
            let req = trunk_app::survey::Request::from_json(&Value::Object(b));
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || ctx2.begin_survey(cfg, req)).await.ok().and_then(Result::err).map(ToClient::error)
        }
        // (Behind the lifecycle lock, which a start or stop may hold for seconds.)
        Command::SurveyListen { freq_hz } => {
            let ctx2 = ctx.clone();
            let _ = tokio::task::spawn_blocking(move || ctx2.survey_send(crate::survey::UserCmd::Listen(freq_hz))).await;
            None
        }
        Command::SurveyRescan => {
            let ctx2 = ctx.clone();
            let _ = tokio::task::spawn_blocking(move || ctx2.survey_send(crate::survey::UserCmd::Rescan)).await;
            None
        }
        // (The request takes up to 15 s: off the connection's task.)
        Command::LicenseQuery { id, lat, lon, radius_m, signals } => {
            let api_base = ctx.config.lock().unwrap().server.trp_api_base().to_string();
            let answer = tokio::task::spawn_blocking(move || trunk_app::licenses::request(lat, lon, radius_m, &signals).and_then(|req| crate::licenses::identify(&api_base, &req)))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            Some(match answer {
                Ok(v) => ToClient::LicenseResult { id, results: v["results"].as_array().cloned().unwrap_or_default(), error: None },
                Err(e) => ToClient::LicenseResult { id, results: vec![], error: Some(e) },
            })
        }
        Command::CallSignQuery { id, call_signs } => {
            let api_base = ctx.config.lock().unwrap().server.trp_api_base().to_string();
            let mut calls: Vec<String> = call_signs.iter().map(|c| c.trim().to_uppercase()).filter(|c| crate::config::plausible_call_sign(c)).collect();
            calls.sort();
            calls.dedup();
            calls.truncate(64);
            if calls.is_empty() {
                return Some(ToClient::CallSignResult { id, results: vec![], error: Some("No call signs to look up.".into()) });
            }
            let answer = tokio::task::spawn_blocking(move || crate::licenses::call_signs(&api_base, &calls)).await.unwrap_or_else(|e| Err(e.to_string()));
            Some(match answer {
                Ok(v) => ToClient::CallSignResult { id, results: v["results"].as_array().cloned().unwrap_or_default(), error: None },
                Err(e) => ToClient::CallSignResult { id, results: vec![], error: Some(e) },
            })
        }
        Command::SurveyStop => {
            let ctx2 = ctx.clone();
            let _ = tokio::task::spawn_blocking(move || ctx2.stop_survey()).await;
            None
        }
        // A moment with the radio alone (nothing may start meanwhile); the answer is this connection's.
        Command::ProfileSource(b) => {
            let cfg = ctx.config.lock().unwrap().clone();
            let req = trunk_app::profile::Request::from_json(&Value::Object(b));
            let ctx2 = ctx.clone();
            let answer = tokio::task::spawn_blocking(move || {
                let source = req.source;
                ctx2.with_radios("profiling", || crate::profile::run(&cfg, &req)).map(|r| r.unwrap_or_else(|e| trunk_app::profile::error_json(source, &e)))
            })
            .await
            .ok()
            .flatten();
            answer.map(|v| ToClient::SourceProfile(body(v)))
        }
        Command::ReadTrConfig { path } => tokio::task::spawn_blocking(move || tr_config(&path)).await.ok(),
        Command::ListDir { path } => tokio::task::spawn_blocking(move || dir(&path)).await.ok(),
        Command::Plugins => Some(plugins_list(ctx).await),
        Command::Store { kind, refresh } => {
            let ctx2 = ctx.clone();
            tokio::task::spawn_blocking(move || match kind {
                StoreKind::Plugin => ctx2.plugins.store(refresh),
                StoreKind::Interface => ctx2.interfaces.store(refresh),
            })
            .await
            .ok()
        }
        // In the background: it takes a while, and this connection should
        // hear how it goes (installProgress), as everyone does. An update of
        // a running plugin restarts it with the new version; a new interface
        // is served at once, on a port of its own, with the access its
        // listing asks.
        Command::Install { kind: StoreKind::Plugin, what } => {
            let ctx2 = ctx.clone();
            tokio::spawn(async move {
                let ctx3 = ctx2.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let cfg = ctx3.config.lock().unwrap().clone();
                    ctx3.plugins.install(&what, &cfg, &crate::store::announce(&ctx3.hub, StoreKind::Plugin));
                    let cfg = ctx3.config.lock().unwrap().clone();
                    ctx3.plugins.reload(&cfg);
                })
                .await;
                publish(&ctx2.hub, plugins_list(&ctx2).await);
            });
            None
        }
        Command::Install { kind: StoreKind::Interface, what } => {
            let ctx2 = ctx.clone();
            tokio::spawn(async move {
                let ctx3 = ctx2.clone();
                let r = tokio::task::spawn_blocking(move || {
                    let cfg = ctx3.config.lock().unwrap().clone();
                    let done = ctx3.interfaces.install(&what, &cfg, &crate::store::announce(&ctx3.hub, StoreKind::Interface))?;
                    let taken = ctx3.interfaces.taken();
                    edit_config(&ctx3, |c| done.apply(c, &taken)).err()
                })
                .await;
                if let Ok(Some(e)) = r {
                    publish(&ctx2.hub, ToClient::error(e));
                }
                crate::interfaces::serve::reconcile(ctx2).await;
            });
            None
        }
        Command::AddPlugin { path } => {
            let ctx2 = ctx.clone();
            let r = tokio::task::spawn_blocking(move || {
                let cfg = ctx2.config.lock().unwrap().clone();
                let (id, path, notice) = ctx2.plugins.add(&path, &cfg)?;
                // A rebuilt copy of one that's on (the config doesn't change): it restarts with it.
                if cfg.plugins.get(&id).is_some_and(|p| p.enabled && Path::new(&p.path) == path) {
                    ctx2.plugins.reload(&cfg);
                }
                edit_config(&ctx2, |c| c.plugins.entry(id).or_default().path = path.display().to_string())?;
                Ok(notice)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            // Everyone sees the change (or the list as it still is).
            publish(&ctx.hub, plugins_list(ctx).await);
            Some(r.map_or_else(ToClient::error, ToClient::notice))
        }
        Command::RemovePlugin { id } => {
            let ctx2 = ctx.clone();
            let r = tokio::task::spawn_blocking(move || {
                let cfg = ctx2.config.lock().unwrap().clone();
                let id = ctx2.plugins.remove(&id, &cfg)?;
                edit_config(&ctx2, |c| crate::plugins::forget(c, &id))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            publish(&ctx.hub, plugins_list(ctx).await);
            failed(r)
        }
        Command::Interfaces => {
            let cfg = ctx.config.lock().unwrap().clone();
            Some(ctx.interfaces.list(&cfg, &ctx.config_path))
        }
        Command::AddInterface { path } => {
            let ctx2 = ctx.clone();
            let r = tokio::task::spawn_blocking(move || {
                let cfg = ctx2.config.lock().unwrap().clone();
                let (id, entry) = ctx2.interfaces.add(&path, &cfg, &ctx2.config_path)?;
                let notice = format!("Added {id}, on port {}.", entry.port);
                edit_config(&ctx2, |c| void(c.server.interfaces.insert(id, entry)))?;
                Ok(notice)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            crate::interfaces::serve::reconcile(ctx.clone()).await;
            Some(r.map_or_else(ToClient::error, ToClient::notice))
        }
        Command::RemoveInterface { id } => {
            let ctx2 = ctx.clone();
            let r = tokio::task::spawn_blocking(move || {
                let cfg = ctx2.config.lock().unwrap().clone();
                ctx2.interfaces.remove(&id, &cfg)?;
                edit_config(&ctx2, |c| void(c.server.interfaces.remove(&id)))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            crate::interfaces::serve::reconcile(ctx.clone()).await;
            failed(r)
        }
        // (Works with signing in off too: that's how the first user is made, before it's turned on.)
        Command::CreateToken { id, name, access } => {
            let (ctx2, asked) = (ctx.clone(), name.clone());
            let made = tokio::task::spawn_blocking(move || {
                let mut made = Err(String::new());
                edit_config(&ctx2, |c| made = crate::access::add_user(&mut c.server.auth, &asked, access))?;
                made
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            Some(match made {
                Ok(secret) => ToClient::TokenCreated { id, name: name.trim().to_string(), secret: Some(secret), error: None },
                Err(e) => ToClient::TokenCreated { id, name, secret: None, error: Some(e) },
            })
        }
        Command::Listen { on, system, talkgroup, voice, audio } => {
            *listen = on.then(|| Listen { system, talkgroup, audio, voice: voice.then(|| Subs::new(ctx, ["voice".to_string()].into())) });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{admit, folder_file, Listen};
    use crate::toolkit::percent_decode;
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

    /// A chart further back than memory keeps (a day and a bit, with the
    /// store): the store's hours and memory's minutes in one grid of whole
    /// hours; within it, memory alone.
    #[test]
    fn a_long_chart_is_the_store_s_hours_and_memory_s_minutes() {
        use trunk_app::stats::{HourRow, Rollup};
        let dir = crate::db::TestDir::new("stats-query");
        let ctx = crate::runtime::test_ctx_with_store(&dir.join("cap"), &dir.join("trunk-pro.sqlite"));
        let now = crate::clock::unix_s() as i64;
        let old = (now - 30 * 86_400).div_euclid(3600) * 3600;
        ctx.db.send(crate::db::Change::StatsHours(vec![HourRow { series: "a".into(), t: old, avg: 10.0, lo: 5.0, hi: 20.0, n: 60 }]));
        ctx.db.flush().unwrap();
        ctx.series.lock().unwrap().insert(&Rollup { t: now.div_euclid(60) * 60 - 3600, values: [("a".to_string(), [1.0, 0.0, 2.0])].into() });
        let ask = |range: &str| {
            let q = trunk_app::protocol::StatsQuery { series: vec!["a".into()], range: Some(range.into()), points: 90, ..Default::default() };
            match super::stats_query(&ctx, &q) {
                trunk_app::protocol::ToClient::StatsResult(b) => serde_json::Value::Object(b),
                m => panic!("{m:?}"),
            }
        };
        let r = ask("90d");
        let a = &r["series"]["a"];
        assert_eq!(a["stepS"], 24 * 3600, "90 days in about 90 points: a day each");
        let n: Vec<u64> = a["n"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        assert_eq!((n.iter().sum::<u64>(), n.iter().filter(|v| **v > 0).count()), (61, 2), "the stored hour and the minute in memory");
        assert_eq!(a["hi"].as_array().unwrap().iter().filter_map(|v| v.as_f64()).fold(0.0, f64::max), 20.0);
        let day = ask("24h");
        assert_eq!(day["series"]["a"]["n"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).sum::<u64>(), 1, "a day: memory's");
        assert_eq!(day["series"]["a"]["stepS"], 17 * 60, "by the minute: memory's grid (a day in 90 points)");
        assert_eq!(ctx.series.lock().unwrap().cap_minutes(), crate::statstore::STORE_MEMORY_MINUTES, "a day and a bit in memory");
        let week = ask("7d");
        assert_eq!(week["series"]["a"]["stepS"], 2 * 3600, "a week: whole hours, as the store has them (two each, in 90 points)");
        assert_eq!(week["series"]["a"]["n"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).sum::<u64>(), 1, "memory's minute in it");
    }

    /// `hello` lists `calls` and `history` while the call store is open, and not when it
    /// could not be opened.
    #[tokio::test]
    async fn hello_lists_calls_only_while_the_store_is_open() {
        use trunk_app::protocol::{Capability, ToClient};
        async fn capabilities(ctx: &std::sync::Arc<crate::runtime::Ctx>) -> Vec<Capability> {
            match super::greeting(ctx, crate::config::ApiAccess::Listen, None).await.pop() {
                Some(ToClient::Hello { capabilities, .. }) => capabilities,
                _ => panic!("hello comes last"),
            }
        }
        let dir = crate::db::TestDir::new("hello-calls");
        let open = crate::runtime::test_ctx(&dir.join("open"));
        assert_eq!(capabilities(&open).await, Capability::ALL);
        let store = dir.join("trunk-pro.sqlite");
        crate::db::seed_unopenable(&store);
        let off = crate::runtime::test_ctx_with_store(&dir.join("off"), &store);
        let caps = capabilities(&off).await;
        assert!(!caps.contains(&Capability::Calls) && !caps.contains(&Capability::History), "{caps:?}");
        assert_eq!(caps.len(), Capability::ALL.len() - 2);
    }

    /// An edit of the config as it was before someone else's change is
    /// refused (and told the version now); one of the config as it is, saved.
    #[tokio::test]
    async fn a_config_edit_names_the_version_it_edits() {
        use super::{command, Subs};
        use trunk_app::protocol::{Command, ToClient};
        let dir = std::env::temp_dir().join(format!("trunk-pro-cfgver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::protocol_tests::ctx(crate::protocol_tests::conventional_config(), &dir);
        let (mut listen, mut subs) = (None, Subs { ctx: ctx.clone(), topics: Default::default() });
        let mut edited = serde_json::to_value(crate::protocol_tests::conventional_config()).unwrap();
        edited["recording"]["minCallS"] = 3.into();
        let set = |version| Command::SetConfig { config: edited.clone(), version };
        match command(&ctx, set(1), &mut listen, &mut subs).await {
            Some(ToClient::ConfigSaved { version: 2, error: None }) => {}
            r => panic!("not saved: {:?}", r.map(|m| m.to_value())),
        }
        assert_eq!(ctx.config.lock().unwrap().recording.min_call_s, 3.0);
        match command(&ctx, set(1), &mut listen, &mut subs).await {
            Some(ToClient::ConfigSaved { version: 2, error: Some(_) }) => {}
            r => panic!("not refused: {:?}", r.map(|m| m.to_value())),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A connection that falls behind (a tab frozen in the background) is
    /// closed, not carried on with gaps as if what it shows were live: it
    /// reconnects and is greeted afresh (#54).
    #[tokio::test]
    async fn a_connection_that_falls_behind_is_closed_to_start_afresh() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as Ws;
        let dir = std::env::temp_dir().join(format!("trunk-pro-lag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::protocol_tests::ctx(crate::protocol_tests::conventional_config(), &dir);
        let url = format!("ws://{}/api/ws", super::test_server(&ctx, super::Door::Builtin).await);
        // Connected and greeted: everything up to hello.
        let greeted = || async {
            let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            while let Some(Ok(m)) = ws.next().await {
                if m.to_text().is_ok_and(|t| t.contains(r#""type":"hello""#)) {
                    return ws;
                }
            }
            panic!("no hello");
        };
        let mut ws = greeted().await;
        // More than the hub holds, published before its session runs (this test's runtime is one thread).
        for i in 0..100 {
            crate::runtime::publish(&ctx.hub, trunk_app::protocol::ToClient::notice(format!("n{i}")));
        }
        let mut stale = 0;
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    Some(Ok(Ws::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(m)) if m.to_text().is_ok_and(|t| t.contains(r#""type":"notice""#)) => stale += 1,
                    _ => {}
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "closed, not carried on");
        assert_eq!(stale, 0, "none of what it fell behind on");
        greeted().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `method path` to a test server at `addr` with `headers` (and a body): its status, headers and body.
    async fn http(addr: std::net::SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&str>) -> (u16, axum::http::HeaderMap, String) {
        let (url, method, body) = (format!("http://{addr}{path}"), method.to_string(), body.map(str::to_string));
        let headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
            let mut r = match (method.as_str(), body) {
                ("POST", body) => headers.iter().fold(agent.post(&url), |r, (k, v)| r.header(k, v)).send(body.unwrap_or_default()),
                ("OPTIONS", _) => headers.iter().fold(agent.options(&url), |r, (k, v)| r.header(k, v)).call(),
                _ => headers.iter().fold(agent.get(&url), |r, (k, v)| r.header(k, v)).call(),
            }
            .unwrap();
            (r.status().as_u16(), r.headers().clone(), r.body_mut().read_to_string().unwrap_or_default())
        })
        .await
        .unwrap()
    }

    /// A recorder with signing in on and no anonymous access, and two users: control `luke`'s token's secret, listen `kiosk`'s.
    fn signed_in_ctx(dir: &crate::db::TestDir, name: &str) -> (std::sync::Arc<crate::runtime::Ctx>, String, String) {
        use crate::config::ApiAccess;
        let ctx = crate::runtime::test_ctx(&dir.join(&format!("{name}-cap")));
        let mut c = ctx.config.lock().unwrap();
        let luke = crate::access::add_user(&mut c.server.auth, "luke", ApiAccess::Control).unwrap().expose().to_string();
        let kiosk = crate::access::add_user(&mut c.server.auth, "kiosk", ApiAccess::Listen).unwrap().expose().to_string();
        c.server.auth.enabled = true;
        drop(c);
        (ctx, luke, kiosk)
    }

    /// With signing in on, a request without a token gets nothing but the
    /// version, signing in, who it is and the API's static files (a browser
    /// asking for a page, the sign-in page); one with a token gets what its
    /// user's role allows; a session cookie counts only from this origin's
    /// pages, or a link followed here.
    #[tokio::test]
    async fn signing_in_guards_every_route() {
        let dir = crate::db::TestDir::new("signin-http");
        let (ctx, luke, kiosk) = signed_in_ctx(&dir, "signin-http");
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let get = |path: &'static str, headers: Vec<(&'static str, String)>| async move {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            http(addr, "GET", path, &h, None).await
        };
        let bearer = |s: &str| vec![("authorization", format!("Bearer {s}"))];
        for p in ["/api/v1/calls", "/calls/x.wav", "/api/ws", "/api/v1/export/calls", "/api/v1/storage", "/api/nothing.js", "/store/screenshots/00"] {
            let (code, h, _) = get(p, vec![]).await;
            assert_eq!((code, h.get("www-authenticate").map(|v| v.to_str().unwrap())), (401, Some("Bearer")), "{p}");
        }
        // The API's static files hold nothing of the recorder's: a page on another origin imports client.js before signing in.
        for p in ["/api/client.js", "/api/docs", "/api/schema", "/api/protocol.ts", "/api/llms.txt", "/api/calls.openapi.yaml", "/api/examples/wall.html"] {
            assert_eq!(get(p, vec![]).await.0, 200, "{p}");
        }
        let (code, h, page) = get("/", vec![("accept", "text/html,*/*".into())]).await;
        assert!(code == 401 && page.contains("/api/login") && h["content-type"].to_str().unwrap().starts_with("text/html"), "{code} {page}");
        let (code, h, page) = get("/signin", vec![]).await;
        assert!(code == 200 && page.contains("/api/login"), "/signin: the page, asked for");
        // Every answer: no Referer from its pages, no sniffing; the sign-in page in no frame; HSTS never over plain HTTP.
        ctx.config.lock().unwrap().server.tls = Some(trunk_app::config::Tls { cert: "c.pem".into(), key: "k.pem".into(), hsts: true });
        let hv = |h: &axum::http::HeaderMap, n: &str| h.get(n).map(|v| v.to_str().unwrap().to_string());
        assert_eq!((hv(&h, "referrer-policy"), hv(&h, "x-content-type-options"), hv(&h, "x-frame-options")), (Some("no-referrer".into()), Some("nosniff".into()), Some("DENY".into())));
        for (p, headers) in [("/api/v1/calls", vec![]), ("/api/version", vec![]), ("/api/session", vec![("origin", "https://evil.example".to_string())])] {
            let (code, h, _) = get(p, headers).await;
            assert_eq!((hv(&h, "referrer-policy"), hv(&h, "x-content-type-options"), hv(&h, "x-frame-options"), hv(&h, "strict-transport-security")), (Some("no-referrer".into()), Some("nosniff".into()), None, None), "{p} ({code})");
        }
        ctx.config.lock().unwrap().server.tls = None;
        assert_eq!(get("/signin", bearer(&luke)).await.0, 200, "signed in too");
        assert_eq!(get("/api/version", vec![]).await.0, 200, "a second launch still finds it");
        let (code, _, who) = get("/api/session", vec![]).await;
        assert_eq!((code, serde_json::from_str::<serde_json::Value>(&who).unwrap()), (200, serde_json::json!({ "auth": true, "access": null, "user": null, "proxy": false, "passkeys": false })));
        // A token: what its user's role allows.
        assert_eq!(get("/api/v1/calls", bearer(&luke)).await.0, 200);
        assert_eq!(get("/calls/x.wav", bearer(&luke)).await.0, 404, "no such file, but let in");
        assert_eq!(get("/api/v1/calls", vec![("authorization", "Bearer trp_00".into())]).await.0, 401, "not a token");
        assert_eq!(get("/api/v1/calls", bearer(&kiosk)).await.0, 200);
        assert_eq!(get("/api/v1/export/calls", bearer(&kiosk)).await.0, 403, "exporting is control's");
        assert_eq!(get("/store/screenshots/00", bearer(&kiosk)).await.0, 403);
        assert_eq!(get("/api/v1/calls?token=", vec![]).await.0, 401);
        let q = format!("/api/v1/calls?token={kiosk}");
        let (code, _, _) = http(addr, "GET", &q, &[], None).await;
        assert_eq!(code, 200, "?token=");
        // Signing in: this recorder's cookie, holding a session (not the secret), and who signed in.
        let (code, h, who) = http(addr, "POST", "/api/login", &[("content-type", "application/json")], Some(&format!(r#"{{"token":"{luke}"}}"#))).await;
        let set = h.get("set-cookie").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        let (name, value) = set.split_once(';').and_then(|(c, _)| c.split_once('=')).map(|(n, v)| (n.to_string(), v.to_string())).unwrap_or_default();
        assert_eq!((code, serde_json::from_str::<serde_json::Value>(&who).unwrap()), (200, serde_json::json!({ "access": "control", "user": "luke" })));
        assert!(name.starts_with("trp_") && value.starts_with("luke.") && !set.contains(&luke), "{set}");
        assert!(set.contains("; Path=/; Max-Age=2592000; HttpOnly; SameSite=Lax") && !set.contains("Secure"), "{set}");
        assert!(matches!(ctx.sessions.verify(&value), Some(super::Who::User { name, credential, until: Some(_) }) if name == "luke" && credential == crate::toolkit::sha256_hex(luke.as_bytes())));
        let (_, h, _) = http(addr, "POST", "/api/login", &[("x-forwarded-proto", "https")], Some(&luke)).await;
        assert!(h["set-cookie"].to_str().unwrap().ends_with("; Secure"), "behind https; and the secret alone is a body too");
        let (_, _, who) = http(addr, "POST", "/api/login", &[], Some(&kiosk)).await;
        assert_eq!(serde_json::from_str::<serde_json::Value>(&who).unwrap(), serde_json::json!({ "access": "listen", "user": "kiosk" }));
        assert_eq!(http(addr, "POST", "/api/login", &[], Some(r#"{"token":"trp_nope"}"#)).await.0, 401);
        let cookie = format!("{name}={value}");
        let session = |extra: Vec<(&'static str, String)>| {
            let mut h = vec![("cookie", cookie.clone())];
            h.extend(extra);
            async move { serde_json::from_str::<serde_json::Value>(&get("/api/session", h).await.2).unwrap() }
        };
        assert_eq!(session(vec![]).await, serde_json::json!({ "auth": true, "access": "control", "user": "luke", "proxy": false, "passkeys": false }), "127.0.0.1: no passkeys");
        assert_eq!(session(vec![("sec-fetch-site", "same-origin".into())]).await["access"], "control");
        // The user's own passkeys, by name and when added: never another's, never a key.
        {
            let mut c = ctx.config.lock().unwrap();
            let pk = |name: &str, id: &str| crate::config::AuthPasskey { name: name.into(), created: 1_763_200_000, id: id.into(), key: serde_json::json!({ "secret": "public key" }) };
            c.server.auth.users.get_mut("luke").unwrap().passkeys = vec![pk("Mac Safari", "BAUG")];
            c.server.auth.users.get_mut("kiosk").unwrap().passkeys = vec![pk("Wall tablet", "AQID")];
        }
        let own = session(vec![]).await;
        assert_eq!(own["ownPasskeys"], serde_json::json!([{ "id": "BAUG", "name": "Mac Safari", "created": 1_763_200_000 }]));
        assert!(!own.to_string().contains("Wall tablet") && !own.to_string().contains("public key"), "{own}");
        let anon = serde_json::from_str::<serde_json::Value>(&get("/api/session", vec![]).await.2).unwrap();
        assert!(anon.get("ownPasskeys").is_none(), "not signed in: none");
        let (_, _, viewer) = get("/api/session", bearer(&kiosk)).await;
        assert_eq!(serde_json::from_str::<serde_json::Value>(&viewer).unwrap()["ownPasskeys"][0]["name"], "Wall tablet", "a View user: theirs");
        for u in ["luke", "kiosk"] {
            ctx.config.lock().unwrap().server.auth.users.get_mut(u).unwrap().passkeys.clear();
        }
        assert_eq!(session(vec![("sec-fetch-site", "same-site".into())]).await["access"], serde_json::Value::Null, "another port's page: its cookie doesn't count");
        let link = vec![("sec-fetch-site", "cross-site".into()), ("sec-fetch-mode", "navigate".into()), ("sec-fetch-dest", "document".into())];
        assert_eq!(session(link).await["user"], "luke", "a link followed here arrives signed in");
        assert_eq!(get("/api/v1/calls", vec![("cookie", format!("{name}={luke}"))]).await.0, 401, "a secret in the cookie is no session");
        // Signing out: this recorder's cookie alone (another recorder on this host name keeps its own).
        let (code, h, _) = http(addr, "POST", "/api/logout", &[("cookie", &format!("other=1; trp_0badc0de=x; {cookie}"))], None).await;
        let cleared: Vec<&str> = h.get_all("set-cookie").iter().map(|v| v.to_str().unwrap()).collect();
        assert_eq!((code, cleared), (204, vec![format!("{name}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax").as_str()]));
        // The user's token made again: the session no longer signs in.
        let remade = ctx.config.lock().unwrap().server.auth.users["luke"].clone();
        ctx.config.lock().unwrap().server.auth.users.get_mut("luke").unwrap().token.as_mut().unwrap().hash = crate::toolkit::sha256_hex(b"another");
        assert_eq!(session(vec![]).await["user"], serde_json::Value::Null, "remade: signed out");
        ctx.config.lock().unwrap().server.auth.users.insert("luke".into(), remade);
        // An allowed origin's page: its preflight is answered without a token, then its Bearer
        // token counts and its cookie doesn't; another origin's preflight is refused as ever.
        ctx.config.lock().unwrap().server.allowed_origins = vec!["https://app.example".into()];
        let ask = |origin: &str| vec![("origin", origin.to_string()), ("access-control-request-method", "GET".to_string()), ("access-control-request-headers", "authorization".to_string())];
        let options = |headers: Vec<(&'static str, String)>| async move {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            http(addr, "OPTIONS", "/api/v1/calls", &h, None).await
        };
        let (code, h, _) = options(ask("https://app.example")).await;
        let hv = |n: &str| h.get(n).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        assert_eq!((code, hv("access-control-allow-origin"), hv("access-control-allow-methods"), hv("access-control-allow-headers"), hv("access-control-max-age"), hv("vary")),
                   (204, "https://app.example".into(), "GET, POST".into(), "Authorization, Content-Type".into(), "600".into(), "Origin".into()));
        assert!(h.get("access-control-allow-credentials").is_none(), "its cookies stay unusable");
        let from_app = |mut extra: Vec<(&'static str, String)>| {
            extra.push(("origin", "https://app.example".into()));
            extra
        };
        let (code, h, _) = get("/api/v1/calls", from_app(bearer(&luke))).await;
        assert_eq!((code, h.get("access-control-allow-origin").map(|v| v.to_str().unwrap())), (200, Some("https://app.example")));
        assert_eq!(get("/api/v1/calls", from_app(vec![("cookie", cookie.clone())])).await.0, 401, "another origin's page: its cookie doesn't count");
        assert_eq!(options(ask("https://evil.example")).await.0, 403);
        ctx.config.lock().unwrap().server.allowed_origins = vec![];
        // Anonymous listen: reading, but not exporting.
        ctx.config.lock().unwrap().server.auth.anonymous = Some(crate::config::ApiAccess::Listen);
        assert_eq!(get("/api/v1/calls", vec![]).await.0, 200);
        assert_eq!(get("/api/v1/export/calls", vec![]).await.0, 403);
        // Off: the port's access, as ever; signing in still says who (the user's role, not the port's).
        ctx.config.lock().unwrap().server.auth.enabled = false;
        assert_eq!(get("/api/v1/export/calls", vec![]).await.0, 200);
        let (code, _, who) = http(addr, "POST", "/api/login", &[], Some(&kiosk)).await;
        assert_eq!((code, serde_json::from_str::<serde_json::Value>(&who).unwrap()), (200, serde_json::json!({ "access": "listen", "user": "kiosk" })));
        assert_eq!(get("/signin", vec![]).await.0, 200);
    }

    /// Passkeys: usable on localhost or the host name, on the built-in
    /// interface's port alone (the session says, and where they are); signing
    /// in with one is open, adding one is for a user signed in; an
    /// interface's port refuses both, another origin's page may do neither; a
    /// bogus answer signs nobody in. Then the whole of it over HTTP, with a
    /// software key: added (saved), signed in with (the cookie), an answer
    /// made on the interface's page refused, and signed out once removed.
    #[tokio::test]
    async fn passkeys_add_and_sign_in() {
        let dir = crate::db::TestDir::new("passkey-http");
        let (ctx, luke, kiosk) = signed_in_ctx(&dir, "passkey-http");
        ctx.config.lock().unwrap().server.interfaces.insert("wall".into(), trunk_app::config::Interface { port: 8081, ..Default::default() });
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let wall = super::test_server(&ctx, super::Door::Interface { id: "wall".into(), stop }).await;
        let (local, port) = (format!("localhost:{}", addr.port()), addr.port());
        let json = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or_default();
        let session = |headers: Vec<(&'static str, String)>| async move {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            serde_json::from_str::<serde_json::Value>(&http(addr, "GET", "/api/session", &h, None).await.2).unwrap()
        };
        assert_eq!(session(vec![]).await["passkeys"], false, "127.0.0.1");
        assert!(session(vec![]).await.get("passkeyOrigin").is_none());
        let here = session(vec![("host", local.clone())]).await;
        assert_eq!((&here["passkeys"], &here["passkeyOrigin"]), (&true.into(), &format!("http://localhost:{port}").into()), "localhost, at this port");
        let on_wall = |extra: Vec<(&'static str, String)>| {
            let mut h = vec![("host", format!("localhost:{}", wall.port()))];
            h.extend(extra);
            h
        };
        let h: Vec<(String, String)> = on_wall(vec![]).into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let hr: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let there = json(&http(wall, "GET", "/api/session", &hr, None).await.2);
        assert_eq!((&there["passkeys"], &there["passkeyOrigin"]), (&false.into(), &format!("http://localhost:{port}").into()), "an interface's port: none there, but where they are");
        let named = vec![("host", format!("radio.ts.net:{port}")), ("x-forwarded-proto", "https".to_string())];
        assert_eq!(http(addr, "GET", "/api/session", &[("host", "radio.ts.net")], None).await.0, 403, "bound to localhost, another name is refused");
        ctx.config.lock().unwrap().server.hostname = "radio.ts.net".into();
        let at_name = session(named.clone()).await;
        assert_eq!((&at_name["passkeys"], &at_name["passkeyOrigin"]), (&true.into(), &"https://radio.ts.net".into()), "the host name: https, at 443 (no HTTPS here: a proxy's in front)");
        let plain = session(vec![("host", format!("radio.ts.net:{port}"))]).await;
        assert_eq!(plain["passkeyOrigin"], "https://radio.ts.net", "what the request says is no say");
        ctx.config.lock().unwrap().server.hostname = format!("radio.ts.net:{port}");
        assert_eq!(session(named.clone()).await["passkeyOrigin"], format!("https://radio.ts.net:{port}"), "the port written in server.hostname");
        ctx.config.lock().unwrap().server.hostname = "radio.ts.net".into();
        // Signing in starts for anyone, on a host passkeys work on.
        let post = |path: &'static str, headers: Vec<(&'static str, String)>, body: Option<String>| async move {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            http(addr, "POST", path, &h, body.as_deref()).await
        };
        let host = vec![("host", local.clone())];
        let (code, _, started) = post("/api/passkey/login/start", host.clone(), None).await;
        let started = json(&started);
        assert!(code == 200 && started["id"].is_string() && started["options"]["publicKey"]["challenge"].is_string(), "{code} {started}");
        assert_eq!(post("/api/passkey/login/start", vec![], None).await.0, 400, "127.0.0.1: no passkeys");
        let (code, _, why) = post("/api/passkey/login/finish", host.clone(), Some(format!(r#"{{"id":{},"credential":{{"id":"x"}}}}"#, started["id"]))).await;
        assert!(code == 401 && why.contains("Not a passkey"), "{code} {why}");
        // Adding: signed in, on a control port.
        let name = Some(r#"{"name":"Test key"}"#.to_string());
        assert_eq!(post("/api/passkey/register/start", host.clone(), name.clone()).await.0, 401, "nobody signed in");
        let as_ = |secret: &str| vec![("host", local.clone()), ("authorization", format!("Bearer {secret}"))];
        assert_eq!(post("/api/passkey/register/start", as_(&kiosk), name.clone()).await.0, 200, "a View user adds their own");
        // An interface's port: none of it (its page could have an answer made for the built-in port's sign-in).
        let wall_post = |path: &'static str, extra: Vec<(&'static str, String)>, body: Option<String>| {
            let h = on_wall(extra);
            async move {
                let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
                http(wall, "POST", path, &h, body.as_deref()).await
            }
        };
        assert_eq!(wall_post("/api/passkey/register/start", vec![("authorization", format!("Bearer {luke}"))], name.clone()).await.0, 403, "not through an interface's port");
        let (code, _, why) = wall_post("/api/passkey/login/start", vec![], None).await;
        assert!(code == 403 && why.contains(&format!("http://localhost:{port}/signin")), "{code} {why}");
        assert_eq!(wall_post("/api/passkey/login/finish", vec![], Some(format!(r#"{{"id":{},"credential":{{}}}}"#, started["id"]))).await.0, 403);
        // Another origin's page: not even one allowedOrigins lets in.
        ctx.config.lock().unwrap().server.allowed_origins = vec!["https://app.example".into()];
        let mut from_app = as_(&luke);
        from_app.push(("origin", "https://app.example".into()));
        assert_eq!(post("/api/passkey/register/start", from_app, name.clone()).await.0, 403);
        assert_eq!(post("/api/passkey/login/start", vec![("host", local.clone()), ("origin", "https://app.example".into())], None).await.0, 403);
        let mut same = as_(&luke);
        same.push(("origin", format!("http://{local}")));
        // The whole of it, from this recorder's own page.
        let key = crate::access::passkey::soft::Key::new();
        let (code, _, started) = post("/api/passkey/register/start", same.clone(), name.clone()).await;
        let started = json(&started);
        assert_eq!(code, 200, "{started}");
        let made = key.create(&started["options"]["publicKey"], &format!("http://{local}"), 0);
        let (code, _, added) = post("/api/passkey/register/finish", same.clone(), Some(serde_json::json!({ "id": started["id"], "credential": made }).to_string())).await;
        assert_eq!((code, json(&added)["name"].clone()), (200, "Test key".into()), "{added}");
        let saved = ctx.config.lock().unwrap().server.auth.users["luke"].passkeys.clone();
        assert!(saved.len() == 1 && std::fs::read_to_string(&ctx.config_path).unwrap().contains(&saved[0].id), "saved");
        // The interface's page, its server having started a sign-in on the built-in port: its answer's origin is the interface's. Refused.
        let (_, _, started) = post("/api/passkey/login/start", host.clone(), None).await;
        let started = json(&started);
        let relayed = key.get(&started["options"]["publicKey"], &format!("http://localhost:{}", wall.port()), 0, "luke");
        let (code, h, why) = post("/api/passkey/login/finish", host.clone(), Some(serde_json::json!({ "id": started["id"], "credential": relayed }).to_string())).await;
        assert!(code == 401 && h.get("set-cookie").is_none() && why.contains(&format!("work at http://localhost:{port} alone")), "{code} {why}");
        let (_, _, started) = post("/api/passkey/login/start", host.clone(), None).await;
        let started = json(&started);
        let answer = key.get(&started["options"]["publicKey"], &format!("http://{local}"), 0, "luke");
        let finish = Some(serde_json::json!({ "id": started["id"], "credential": answer }).to_string());
        let (code, h, who) = post("/api/passkey/login/finish", host.clone(), finish.clone()).await;
        assert_eq!((code, json(&who)), (200, serde_json::json!({ "access": "control", "user": "luke" })));
        let cookie = h["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
        assert_eq!(ctx.sessions.verify(cookie.split_once('=').unwrap().1).unwrap().grants(&ctx.config.lock().unwrap().server.auth), Some(crate::config::ApiAccess::Control));
        let signed = session(vec![("cookie", cookie.clone())]).await;
        assert_eq!((&signed["user"], &signed["access"]), (&"luke".into(), &"control".into()));
        let (code, _, why) = post("/api/passkey/login/finish", host.clone(), finish).await;
        assert!(code == 401 && why.contains("used already"), "the same answer again: {code} {why}");
        // Removed (a config edit, as Setup makes it): signed out.
        ctx.config.lock().unwrap().server.auth.users.get_mut("luke").unwrap().passkeys.clear();
        assert_eq!(session(vec![("cookie", cookie)]).await["user"], serde_json::Value::Null);
    }

    /// A passkey that counts its uses: its count kept as it goes up; when it
    /// can't be (the config can't be written), no sign-in.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_passkey_s_count_unsaved_signs_nobody_in() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::db::TestDir::new("passkey-count");
        let (ctx, luke, _) = signed_in_ctx(&dir, "passkey-count");
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let local = format!("localhost:{}", addr.port());
        let origin = format!("http://{local}");
        let json = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or_default();
        let post = |path: &'static str, extra: Vec<(&'static str, String)>, body: Option<String>| {
            let mut h = vec![("host", local.clone()), ("origin", origin.clone())];
            h.extend(extra);
            async move {
                let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
                http(addr, "POST", path, &h, body.as_deref()).await
            }
        };
        let key = crate::access::passkey::soft::Key::new();
        let as_luke = vec![("authorization", format!("Bearer {luke}"))];
        let started = json(&post("/api/passkey/register/start", as_luke.clone(), Some(r#"{"name":"Key"}"#.into())).await.2);
        let made = key.create(&started["options"]["publicKey"], &origin, 1);
        assert_eq!(post("/api/passkey/register/finish", as_luke, Some(serde_json::json!({ "id": started["id"], "credential": made }).to_string())).await.0, 200);
        let sign_in = |n: u32| {
            let post = post.clone();
            let key = &key;
            let origin = origin.clone();
            async move {
                let started = json(&post("/api/passkey/login/start", vec![], None).await.2);
                let answer = key.get(&started["options"]["publicKey"], &origin, n, "luke");
                post("/api/passkey/login/finish", vec![], Some(serde_json::json!({ "id": started["id"], "credential": answer }).to_string())).await
            }
        };
        let (code, h, _) = sign_in(5).await;
        assert!(code == 200 && h.get("set-cookie").is_some());
        assert_eq!(ctx.config.lock().unwrap().server.auth.users["luke"].passkeys[0].key["cred"]["counter"], 5, "kept");
        let folder = ctx.config_path.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
        let (code, h, why) = sign_in(6).await;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(code == 503 && h.get("set-cookie").is_none() && why.contains("count couldn't be saved"), "{code} {why}");
    }

    /// A WebSocket is greeted with what it may do and as whom; a listen
    /// user can't change anything; through a listen interface's port, a
    /// control user only listens; and a user removed closes their sockets.
    /// The sign-in page is on every port.
    #[tokio::test]
    async fn a_socket_has_its_users_access_until_it_changes() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as Ws};
        let dir = crate::db::TestDir::new("signin-ws");
        let (ctx, luke, kiosk) = signed_in_ctx(&dir, "signin-ws");
        ctx.config.lock().unwrap().server.interfaces.insert("wall".into(), trunk_app::config::Interface { port: 8081, ..Default::default() });
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let builtin = super::test_server(&ctx, super::Door::Builtin).await;
        let wall = super::test_server(&ctx, super::Door::Interface { id: "wall".into(), stop }).await;
        // Connected with `secret` and greeted: the socket and its hello.
        let greeted = |addr: std::net::SocketAddr, secret: String| async move {
            let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
            req.headers_mut().insert("authorization", format!("Bearer {secret}").parse().unwrap());
            let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
            while let Some(Ok(m)) = ws.next().await {
                let v: serde_json::Value = m.to_text().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
                if v["type"] == "hello" {
                    return (ws, v);
                }
            }
            panic!("no hello");
        };
        assert!(tokio_tungstenite::connect_async(format!("ws://{builtin}/api/ws")).await.is_err(), "no token: no socket");
        let (_, hello) = greeted(builtin, luke.clone()).await;
        assert_eq!((&hello["access"], &hello["user"]), (&"control".into(), &"luke".into()));
        assert!(hello["configPath"].is_string());
        let (_, hello) = greeted(wall, luke.clone()).await;
        assert_eq!(hello["access"], "listen", "the lower of the port's and the user's");
        let (code, h, page) = http(wall, "GET", "/signin", &[], None).await;
        assert!(code == 200 && page.contains("/api/login"), "an interface's port has it too, before its own folder");
        assert_eq!((h["referrer-policy"].to_str().unwrap(), h["x-content-type-options"].to_str().unwrap()), ("no-referrer", "nosniff"), "an interface's port's answers too");
        let (mut ws, hello) = greeted(builtin, kiosk.clone()).await;
        assert_eq!((&hello["access"], &hello["user"]), (&"listen".into(), &"kiosk".into()));
        ws.send(Ws::text(r#"{"type":"start"}"#)).await.unwrap();
        let refused = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(Ok(m)) = ws.next().await {
                let v: serde_json::Value = m.to_text().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
                if v["type"] == "error" {
                    return v["message"].as_str().unwrap_or("").to_string();
                }
            }
            String::new()
        })
        .await
        .unwrap();
        assert!(refused.contains("\"kiosk\" may only listen") && refused.contains("\"start\""), "{refused}");
        // Removed: their socket is closed (it would be greeted afresh, but can't sign in now).
        let ctx2 = ctx.clone();
        tokio::task::spawn_blocking(move || super::edit_config(&ctx2, |c| drop(c.server.auth.users.remove("kiosk")))).await.unwrap().unwrap();
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    Some(Ok(Ws::Close(_))) | None | Some(Err(_)) => return,
                    _ => {}
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "closed once their user went");
    }

    /// Behind a proxy that signs people in: its header is who it says from
    /// its addresses alone (the test server's peer is 127.0.0.1), a token
    /// says who first, a name mapped is that user, one that's no user gets
    /// `other`'s; a socket signed in so is greeted as them, and closed once
    /// they're removed; a proxy set as it can't be isn't saved.
    #[tokio::test]
    async fn a_proxy_in_front_signs_people_in() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as Ws};
        use trunk_app::config::AuthProxy;
        let dir = crate::db::TestDir::new("signin-proxy");
        let (ctx, _, kiosk) = signed_in_ctx(&dir, "signin-proxy");
        let proxy = |from: &str| {
            let mut p = AuthProxy { from: vec![from.into()], ..Default::default() };
            p.users.insert("luke@example.com".into(), "luke".into());
            p
        };
        ctx.config.lock().unwrap().server.auth.proxy = Some(proxy("127.0.0.0/8"));
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let session = |headers: Vec<(&'static str, String)>| async move {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            serde_json::from_str::<serde_json::Value>(&http(addr, "GET", "/api/session", &h, None).await.2).unwrap()
        };
        let as_ = |name: &str| vec![("remote-user", name.to_string())];
        assert_eq!(session(as_("luke@example.com")).await, serde_json::json!({ "auth": true, "access": "control", "user": "luke", "proxy": true, "passkeys": false }), "mapped");
        assert_eq!((session(as_("kiosk")).await["user"].clone(), session(as_("kiosk")).await["access"].clone()), ("kiosk".into(), "listen".into()), "the same name");
        assert_eq!(http(addr, "GET", "/api/v1/export/calls", &[("remote-user", "luke@example.com")], None).await.0, 200, "control's, let in");
        let mut both = as_("luke@example.com");
        both.push(("authorization", format!("Bearer {kiosk}")));
        assert_eq!((session(both.clone()).await["user"].clone(), session(both).await["proxy"].clone()), ("kiosk".into(), false.into()), "a token says who first");
        assert_eq!(session(as_("stranger")).await["access"], serde_json::Value::Null, "no user, other none: not signed in");
        assert_eq!(http(addr, "GET", "/api/v1/calls", &[("remote-user", "stranger")], None).await.0, 401);
        ctx.config.lock().unwrap().server.auth.proxy.as_mut().unwrap().other = Some(crate::config::ApiAccess::Listen);
        assert_eq!(session(as_("stranger")).await["access"], "listen", "other's");
        let mut cross = as_("luke@example.com");
        cross.push(("sec-fetch-site", "cross-site".into()));
        assert_eq!(session(cross).await["access"], serde_json::Value::Null, "another site's page: the header counts where the cookie would, so it's anonymous");
        // Not from the proxy's addresses: ignored.
        ctx.config.lock().unwrap().server.auth.proxy = Some(proxy("10.0.0.1"));
        assert_eq!(session(as_("luke@example.com")).await["user"], serde_json::Value::Null, "127.0.0.1 isn't the proxy now");
        ctx.config.lock().unwrap().server.auth.proxy = Some(proxy("127.0.0.1"));
        // A socket: greeted as them; closed once they're removed.
        let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
        req.headers_mut().insert("remote-user", "luke@example.com".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let hello = loop {
            let v: serde_json::Value = ws.next().await.unwrap().unwrap().to_text().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
            if v["type"] == "hello" {
                break v;
            }
        };
        assert_eq!((&hello["access"], &hello["user"]), (&"control".into(), &"luke".into()));
        let ctx2 = ctx.clone();
        tokio::task::spawn_blocking(move || super::edit_config(&ctx2, |c| drop(c.server.auth.users.remove("luke")))).await.unwrap().unwrap();
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    Some(Ok(Ws::Close(_))) | None | Some(Err(_)) => return,
                    _ => {}
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "closed once luke went");
        // A proxy set as it can't be: not saved, and said why.
        use super::{command, Subs};
        use trunk_app::protocol::{Command, ToClient};
        let (mut listen, mut subs) = (None, Subs { ctx: ctx.clone(), topics: Default::default() });
        let version = ctx.config_version.load(std::sync::atomic::Ordering::SeqCst);
        let mut c = serde_json::to_value(ctx.config.lock().unwrap().clone()).unwrap();
        c["server"]["auth"]["proxy"]["from"] = serde_json::json!(["127.0.0.1", "10.0.0.0/40"]);
        match command(&ctx, Command::SetConfig { config: c, version }, &mut listen, &mut subs).await {
            Some(ToClient::ConfigSaved { error: Some(e), .. }) => assert!(e.contains("10.0.0.0/40"), "{e}"),
            r => panic!("not refused: {:?}", r.map(|m| m.to_value())),
        }
        assert_eq!(ctx.config.lock().unwrap().server.auth.proxy.as_ref().unwrap().from, vec!["127.0.0.1".to_string()]);
    }

    /// Behind Cloudflare Tunnel (cloudflared on this computer, plain HTTP to
    /// localhost, the browser on https://radio.example.com): let in once
    /// `server.hostname` names it, signed in with a `Secure` cookie, and
    /// passkeys there (its relying party the host name, its origin https with
    /// no port). With Access in front too (`proxy.cloudflare`): who its token
    /// says, from any address; its email header alone nobody; a token first;
    /// another site's page nobody; a socket as them.
    #[tokio::test]
    async fn behind_cloudflare_tunnel_and_access() {
        use crate::access::cloudflare::tests::{certs, claims, TestKey, AUD};
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let dir = crate::db::TestDir::new("signin-cloudflare");
        let (ctx, luke, kiosk) = signed_in_ctx(&dir, "signin-cloudflare");
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let json = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or_default();
        let call = |method: &'static str, path: &'static str, headers: Vec<(&'static str, String)>, body: Option<String>| async move {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            http(addr, method, path, &h, body.as_deref()).await
        };
        // What cloudflared passes on: the browser's Host and Origin, and that it came over https.
        let tunnel = |mut extra: Vec<(&'static str, String)>| {
            extra.extend([("host", "radio.example.com".to_string()), ("origin", "https://radio.example.com".to_string()), ("x-forwarded-proto", "https".to_string()), ("cf-connecting-ip", "203.0.113.9".to_string())]);
            extra
        };
        let (code, _, why) = call("GET", "/api/session", tunnel(vec![]), None).await;
        assert!(code == 403 && why.contains("only answers to localhost"), "no server.hostname: refused ({code} {why})");
        ctx.config.lock().unwrap().server.hostname = "radio.example.com".into();
        let (code, _, who) = call("GET", "/api/session", tunnel(vec![]), None).await;
        assert_eq!((code, json(&who)), (200, serde_json::json!({ "auth": true, "access": null, "user": null, "proxy": false, "passkeys": true, "passkeyOrigin": "https://radio.example.com" })), "named: let in, passkeys there");
        let (code, h, _) = call("POST", "/api/login", tunnel(vec![]), Some(luke.clone())).await;
        assert!(code == 200 && h["set-cookie"].to_str().unwrap().ends_with("; Secure"), "{code} {h:?}");
        // Passkeys over the tunnel: added and signed in with, from https://radio.example.com (no port).
        let key = crate::access::passkey::soft::Key::new();
        let name = Some(r#"{"name":"Phone"}"#.to_string());
        let (code, _, started) = call("POST", "/api/passkey/register/start", tunnel(vec![("authorization", format!("Bearer {luke}"))]), name).await;
        let started = json(&started);
        assert_eq!((code, &started["options"]["publicKey"]["rp"]["id"]), (200, &"radio.example.com".into()), "{started}");
        let made = key.create(&started["options"]["publicKey"], "https://radio.example.com", 0);
        let (code, _, added) = call("POST", "/api/passkey/register/finish", tunnel(vec![("authorization", format!("Bearer {luke}"))]), Some(serde_json::json!({ "id": started["id"], "credential": made }).to_string())).await;
        assert_eq!(code, 200, "{added}");
        let (_, _, started) = call("POST", "/api/passkey/login/start", tunnel(vec![]), None).await;
        let started = json(&started);
        assert_eq!(started["options"]["publicKey"]["rpId"], "radio.example.com", "{started}");
        let plain = key.get(&started["options"]["publicKey"], "http://radio.example.com", 1, "luke");
        let (code, _, _) = call("POST", "/api/passkey/login/finish", tunnel(vec![]), Some(serde_json::json!({ "id": started["id"], "credential": plain }).to_string())).await;
        assert_eq!(code, 401, "an http:// origin isn't the page's");
        let (_, _, started) = call("POST", "/api/passkey/login/start", tunnel(vec![]), None).await;
        let started = json(&started);
        let answer = key.get(&started["options"]["publicKey"], "https://radio.example.com", 2, "luke");
        let (code, h, who) = call("POST", "/api/passkey/login/finish", tunnel(vec![]), Some(serde_json::json!({ "id": started["id"], "credential": answer }).to_string())).await;
        assert_eq!((code, json(&who)), (200, serde_json::json!({ "access": "control", "user": "luke" })));
        assert!(h["set-cookie"].to_str().unwrap().ends_with("; Secure"));
        // Access in front: its token, checked against the team's keys (served here, not fetched), from an address not listed.
        let a = TestKey::new("a");
        let served = certs(&[&a]);
        ctx.cloudflare.use_source(move |team| if team == "https://radio.cloudflareaccess.com" { Ok(served.clone()) } else { Err("another team".into()) });
        let mut proxy = trunk_app::config::AuthProxy { from: vec!["10.0.0.1".into()], cloudflare: Some(trunk_app::config::AuthCloudflare { team: "radio".into(), aud: AUD.into() }), ..Default::default() };
        proxy.users.insert("luke@example.com".into(), "luke".into());
        ctx.config.lock().unwrap().server.auth.proxy = Some(proxy);
        let now = crate::clock::unix_s() as u64;
        let token = a.sign(&claims("luke@example.com", now));
        let session = |extra: Vec<(&'static str, String)>| async move { json(&call("GET", "/api/session", tunnel(extra), None).await.2) };
        let mapped = session(vec![("cf-access-jwt-assertion", token.clone())]).await;
        assert_eq!((&mapped["access"], &mapped["user"], &mapped["proxy"], &mapped["passkeys"]), (&"control".into(), &"luke".into(), &true.into(), &true.into()), "mapped, from 127.0.0.1 (not proxy.from): {mapped}");
        assert_eq!(call("GET", "/api/v1/export/calls", tunnel(vec![("cf-access-jwt-assertion", token.clone())]), None).await.0, 200, "control's, let in");
        assert_eq!(session(vec![("cf-access-authenticated-user-email", "luke@example.com".into())]).await["user"], serde_json::Value::Null, "its email header alone: anyone's to send");
        assert_eq!(session(vec![("remote-user", "luke@example.com".into())]).await["user"], serde_json::Value::Null, "nor the proxy's header");
        let forged = TestKey::new("b").sign_with(serde_json::json!({ "kid": "a" }), &claims("luke@example.com", now));
        assert_eq!(session(vec![("cf-access-jwt-assertion", forged)]).await["user"], serde_json::Value::Null, "not the team's key");
        let both = vec![("cf-access-jwt-assertion", token.clone()), ("authorization", format!("Bearer {kiosk}"))];
        assert_eq!((session(both.clone()).await["user"].clone(), session(both).await["proxy"].clone()), ("kiosk".into(), false.into()), "a token says who first");
        assert_eq!(session(vec![("cf-access-jwt-assertion", token.clone()), ("sec-fetch-site", "cross-site".into())]).await["user"], serde_json::Value::Null, "another site's page");
        // A socket over the tunnel: greeted as them.
        let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
        for (k, v) in tunnel(vec![("cf-access-jwt-assertion", token.clone())]) {
            req.headers_mut().insert(k, v.parse().unwrap());
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let hello = loop {
            let v: serde_json::Value = ws.next().await.unwrap().unwrap().to_text().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
            if v["type"] == "hello" {
                break v;
            }
        };
        assert_eq!((&hello["access"], &hello["user"]), (&"control".into(), &"luke".into()));
        // One whose token is about to expire (its minute's leeway nearly gone): closed when it does.
        let mut c = claims("luke@example.com", now);
        c["exp"] = serde_json::json!(crate::clock::unix_s() as u64 + 2 - 60);
        let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
        for (k, v) in tunnel(vec![("cf-access-jwt-assertion", a.sign(&c))]) {
            req.headers_mut().insert(k, v.parse().unwrap());
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let closed = tokio::time::timeout(std::time::Duration::from_secs(6), async {
            let mut greeted = false;
            loop {
                match ws.next().await {
                    Some(Ok(m)) if !m.is_close() => greeted |= m.to_text().is_ok_and(|t| t.contains(r#""type":"hello""#)),
                    _ => return greeted,
                }
            }
        })
        .await;
        assert_eq!(closed, Ok(true), "greeted, then closed as its token expired");
        // Keys that can't be had: nobody.
        ctx.cloudflare.use_source(|_| Err("offline".into()));
        assert_eq!(session(vec![("cf-access-jwt-assertion", token)]).await["user"], serde_json::Value::Null, "no keys: no fallback");
    }

    /// A session ends when its cookie expires (a socket it opened is closed
    /// then), and when its user is signed out everywhere: by themselves (on
    /// the built-in interface's port), or by a user with control; sessions
    /// signed in later, and access tokens sent with each request, still count.
    #[tokio::test]
    async fn sessions_end_when_they_expire_or_are_signed_out_everywhere() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as Ws};
        let dir = crate::db::TestDir::new("signin-expiry");
        let (ctx, luke, kiosk) = signed_in_ctx(&dir, "signin-expiry");
        ctx.config.lock().unwrap().server.interfaces.insert("wall".into(), trunk_app::config::Interface { port: 8081, ..Default::default() });
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let wall = super::test_server(&ctx, super::Door::Interface { id: "wall".into(), stop }).await;
        let json = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or_default();
        let hash = |u: &str| ctx.config.lock().unwrap().server.auth.users[u].token.as_ref().unwrap().hash.clone();
        let name = ctx.sessions.set_cookie("x", "y", false).split('=').next().unwrap().to_string();
        let cookie = |u: &str, expires: u64| format!("{name}={}", ctx.sessions.issue_until(u, &hash(u), expires));
        let user = |headers: Vec<(&'static str, String)>| async move {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            json(&http(addr, "GET", "/api/session", &h, None).await.2)["user"].clone()
        };
        let now = crate::clock::unix_s() as u64;
        assert_eq!(user(vec![("cookie", cookie("luke", now + 60))]).await, "luke");
        assert_eq!(user(vec![("cookie", cookie("luke", now - 1))]).await, serde_json::Value::Null, "expired");
        // A socket opened with a session: closed when it expires.
        let socket = |cookie: String| async move {
            let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
            req.headers_mut().insert("cookie", cookie.parse().unwrap());
            let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
            loop {
                let v: serde_json::Value = ws.next().await.unwrap().unwrap().to_text().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
                if v["type"] == "hello" {
                    assert_eq!(v["user"], "luke");
                    return ws;
                }
            }
        };
        let closes = |mut ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, within: u64| async move {
            tokio::time::timeout(std::time::Duration::from_secs(within), async {
                loop {
                    match ws.next().await {
                        Some(Ok(Ws::Close(_))) | None | Some(Err(_)) => return,
                        _ => {}
                    }
                }
            })
            .await
            .is_ok()
        };
        let t = std::time::Instant::now();
        let ws = socket(cookie("luke", crate::clock::unix_s() as u64 + 2)).await;
        assert!(closes(ws, 6).await && t.elapsed() >= std::time::Duration::from_millis(900), "closed as it expired");
        // Signed out everywhere, by luke: this browser's cookie cleared, every session of theirs until now ended (an open socket too).
        let (_, h, _) = http(addr, "POST", "/api/login", &[], Some(&luke)).await;
        let signed = h["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
        let ws = socket(signed.clone()).await;
        let all = Some(r#"{"all":true}"#);
        assert_eq!(http(wall, "POST", "/api/logout", &[("cookie", &signed)], all).await.0, 403, "not through an interface's port");
        assert_eq!(http(addr, "POST", "/api/logout", &[], all).await.0, 401, "nobody signed in");
        let (code, h, _) = http(addr, "POST", "/api/logout", &[("cookie", &signed)], all).await;
        assert!(code == 204 && h["set-cookie"].to_str().unwrap().contains("Max-Age=0"), "{code}");
        let at = ctx.config.lock().unwrap().server.auth.users["luke"].signed_out.unwrap();
        assert!(at.abs_diff(crate::clock::unix_s() as i64) <= 2 && std::fs::read_to_string(&ctx.config_path).unwrap().contains("\"signedOut\""), "saved");
        assert_eq!(user(vec![("cookie", signed.clone())]).await, serde_json::Value::Null, "signed out");
        assert_eq!(user(vec![("cookie", cookie("luke", now + 60))]).await, serde_json::Value::Null, "every session signed in before");
        assert!(closes(ws, 5).await, "its socket closed");
        assert_eq!(user(vec![("cookie", cookie("luke", crate::clock::unix_s() as u64 + crate::access::SESSION_S + 2))]).await, "luke", "one signed in since");
        assert_eq!(user(vec![("authorization", format!("Bearer {luke}"))]).await, "luke", "the access token: made again, it wouldn't be");
        // Another user: by an Admin alone.
        let bearer = |s: &str| format!("Bearer {s}");
        let kiosk_cookie = cookie("kiosk", now + 60);
        assert_eq!(http(addr, "POST", "/api/logout", &[("authorization", &bearer(&kiosk))], Some(r#"{"all":true,"user":"luke"}"#)).await.0, 403, "a View user: themselves alone");
        let (code, h, _) = http(addr, "POST", "/api/logout", &[("authorization", &bearer(&luke))], Some(r#"{"all":true,"user":"kiosk"}"#)).await;
        assert!(code == 204 && h.get("set-cookie").is_none(), "an Admin's own cookie stays");
        assert_eq!(user(vec![("cookie", kiosk_cookie)]).await, serde_json::Value::Null);
        assert_eq!(http(addr, "POST", "/api/logout", &[("authorization", &bearer(&luke))], Some(r#"{"all":true,"user":"nobody"}"#)).await.0, 400);
        // Plain sign out: as ever.
        assert_eq!(http(addr, "POST", "/api/logout", &[], None).await.0, 204);
    }

    /// `createToken` makes a user with a token and answers with its secret
    /// (once: the config keeps its hash), with signing in off too; a name
    /// taken is refused. A config that would turn signing in on with no
    /// control user isn't taken.
    #[tokio::test]
    async fn a_user_is_made_and_signing_in_needs_one_with_control() {
        use super::{command, Subs};
        use crate::config::ApiAccess;
        use trunk_app::protocol::{Command, ToClient};
        let dir = crate::db::TestDir::new("signin-token");
        let ctx = crate::protocol_tests::ctx(crate::protocol_tests::conventional_config(), &dir.join("x"));
        std::fs::create_dir_all(dir.join("x")).unwrap();
        let (mut listen, mut subs) = (None, Subs { ctx: ctx.clone(), topics: Default::default() });
        let create = |name: &str| Command::CreateToken { id: 7, name: name.into(), access: ApiAccess::Listen };
        let made = command(&ctx, create(" wall "), &mut listen, &mut subs).await.unwrap();
        assert!(!format!("{made:?}").contains("trp_"), "no secret in its Debug: {made:?}");
        let ToClient::TokenCreated { id: 7, name, secret: Some(secret), error: None } = made else { panic!("{:?}", made.to_value()) };
        let hash = ctx.config.lock().unwrap().server.auth.users["wall"].token.as_ref().unwrap().hash.clone();
        assert_eq!((name.as_str(), crate::toolkit::sha256_hex(secret.expose().as_bytes())), ("wall", hash.clone()));
        assert!(std::fs::read_to_string(dir.join("x/config.json")).unwrap().contains(&format!("\"hash\": \"{hash}\"")), "saved");
        match command(&ctx, create("wall"), &mut listen, &mut subs).await {
            Some(ToClient::TokenCreated { secret: None, error: Some(e), .. }) => assert!(e.contains("already"), "{e}"),
            r => panic!("not refused: {:?}", r.map(|m| m.to_value())),
        }
        let version = ctx.config_version.load(std::sync::atomic::Ordering::SeqCst);
        let mut on = serde_json::to_value(ctx.config.lock().unwrap().clone()).unwrap();
        on["server"]["auth"]["enabled"] = true.into();
        match command(&ctx, Command::SetConfig { config: on.clone(), version }, &mut listen, &mut subs).await {
            Some(ToClient::ConfigSaved { error: Some(e), .. }) => assert!(e.contains("control"), "{e}"),
            r => panic!("not refused: {:?}", r.map(|m| m.to_value())),
        }
        assert!(!ctx.config.lock().unwrap().server.auth.enabled);
        on["server"]["auth"]["users"]["wall"]["access"] = "control".into();
        assert!(matches!(command(&ctx, Command::SetConfig { config: on, version }, &mut listen, &mut subs).await, Some(ToClient::ConfigSaved { error: None, .. })));
        assert!(ctx.config.lock().unwrap().server.auth.enabled);
    }

    /// Served over HTTPS, every port is (the API, signing in with a `Secure`
    /// cookie, an interface's); a config whose certificate won't do isn't saved.
    #[tokio::test]
    async fn over_https_signing_in_is_secure() {
        use super::{command, Subs};
        use crate::tls::tests::{config_of, fixture, http, https};
        use std::sync::atomic::Ordering;
        use trunk_app::protocol::{Command, ToClient};
        let dir = crate::db::TestDir::new("signin-https");
        let (ctx, luke, _) = signed_in_ctx(&dir, "signin-https");
        let (cert, key) = fixture("a");
        assert!(ctx.tls.set(crate::tls::Tls::new(&config_of(&cert, &key), &ctx.config_path).unwrap()).is_ok());
        assert_eq!(ctx.scheme(), "https");
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let (res, _) = https(addr, &format!("POST /api/login HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{luke}", luke.len())).await;
        let set = res.lines().find_map(|l| l.strip_prefix("set-cookie: ")).unwrap_or_default();
        assert!(res.starts_with("HTTP/1.1 200") && set.ends_with("; HttpOnly; SameSite=Lax; Secure"), "{res}");
        let (res, _) = https(addr, "GET /api/v1/calls HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await;
        assert!(res.starts_with("HTTP/1.1 401"), "guarded as ever: {res}");
        // The socket over TLS (wss://): signed in with a token, greeted as them; without one, refused.
        let wss = |secret: Option<String>| async move {
            use futures_util::StreamExt;
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let tls = crate::tls::tests::client().connect("localhost".try_into().unwrap(), tcp).await.unwrap();
            let mut req = format!("wss://localhost:{}/api/ws", addr.port()).into_client_request().unwrap();
            if let Some(s) = secret {
                req.headers_mut().insert("authorization", format!("Bearer {s}").parse().unwrap());
            }
            let (mut ws, _) = tokio_tungstenite::client_async(req, tls).await.ok()?;
            while let Some(Ok(m)) = ws.next().await {
                let v: serde_json::Value = m.to_text().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
                if v["type"] == "hello" {
                    return Some(v);
                }
            }
            None
        };
        let hello = wss(Some(luke.clone())).await.expect("greeted over wss://");
        assert_eq!((&hello["access"], &hello["user"]), (&"control".into(), &"luke".into()));
        assert!(wss(None).await.is_none(), "no token: no socket");
        // HSTS: told only when server.tls.hsts is set (it binds every port of the name).
        assert!(!res.contains("strict-transport-security"), "off unless set: {res}");
        ctx.config.lock().unwrap().server.tls = Some(trunk_app::config::Tls { hsts: true, ..config_of(&cert, &key) });
        for path in ["/api/v1/calls", "/signin", "/api/version"] {
            let (res, _) = https(addr, &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")).await;
            assert!(res.contains("\r\nstrict-transport-security: max-age=31536000\r\n"), "{path}: {res}");
        }
        assert!(!http(addr, "GET /api/version HTTP/1.0\r\nHost: localhost\r\n\r\n").await.contains("strict-transport-security"), "never over plain HTTP");
        ctx.config.lock().unwrap().server.tls = None;
        let (res, _) = https(addr, &format!("GET /api/v1/calls HTTP/1.1\r\nHost: localhost:{0}\r\nAuthorization: Bearer {luke}\r\nOrigin: https://localhost:{0}\r\nConnection: close\r\n\r\n", addr.port())).await;
        assert!(res.starts_with("HTTP/1.1 200"), "its own https:// page: {res}");
        assert!(http(addr, "GET /api/version HTTP/1.0\r\nHost: localhost\r\n\r\n").await.contains("\"app\":\"trunk-pro\""), "a second launch still finds it");
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let iface = super::test_server(&ctx, super::Door::Interface { id: "wall".into(), stop: stopped }).await;
        assert!(https(iface, "GET /api/version HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.0.starts_with("HTTP/1.1 200"), "an interface's port too");
        // A proxy in front: over HTTPS too, the connection's address is known.
        ctx.config.lock().unwrap().server.auth.proxy = Some(trunk_app::config::AuthProxy { from: vec!["127.0.0.1".into()], ..Default::default() });
        let (res, _) = https(addr, "GET /api/session HTTP/1.1\r\nHost: localhost\r\nRemote-User: luke\r\nConnection: close\r\n\r\n").await;
        assert!(res.contains(r#""user":"luke""#) && res.contains(r#""proxy":true"#), "{res}");
        ctx.config.lock().unwrap().server.auth.proxy = None;
        // A certificate that won't do: not saved.
        let (mut listen, mut subs) = (None, Subs { ctx: ctx.clone(), topics: Default::default() });
        let version = ctx.config_version.load(Ordering::SeqCst);
        let mut c = serde_json::to_value(ctx.config.lock().unwrap().clone()).unwrap();
        c["server"]["tls"] = serde_json::json!({ "cert": cert, "key": fixture("b").1 });
        match command(&ctx, Command::SetConfig { config: c, version }, &mut listen, &mut subs).await {
            Some(ToClient::ConfigSaved { error: Some(e), .. }) => assert!(e.starts_with("HTTPS: the key") && e.contains("isn't the certificate"), "{e}"),
            r => panic!("not refused: {:?}", r.map(|m| m.to_value())),
        }
        assert!(ctx.config.lock().unwrap().server.tls.is_none());
    }

    /// A WebSocket to `addr` with `headers`, greeted: it, and its `hello`.
    async fn socket(addr: std::net::SocketAddr, headers: &[(&str, &str)]) -> (tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, serde_json::Value) {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderName};
        let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
        for (k, v) in headers {
            req.headers_mut().insert(HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        while let Some(Ok(m)) = ws.next().await {
            let v: serde_json::Value = m.to_text().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
            if v["type"] == "hello" {
                return (ws, v);
            }
        }
        panic!("no hello from {addr}");
    }

    /// Every command, as an interface sends it, and the access it takes:
    /// listen for looking, control for everything else. A command that's new
    /// fails [`every_command_has_a_decided_role`] until it has its row here.
    fn commands() -> Vec<(&'static str, crate::config::ApiAccess)> {
        use crate::config::ApiAccess::{Control as C, Listen as L};
        vec![
            (r#"{"type":"setConfig","config":{},"version":1}"#, C),
            (r#"{"type":"setTalkgroups","system":"x","talkgroups":[]}"#, C),
            (r#"{"type":"setUnitNames","system":"x","unitNames":[]}"#, C),
            (r#"{"type":"setUnitName","system":"x","unit":1,"name":"a"}"#, C),
            (r#"{"type":"setTalkgroup","system":"x","talkgroup":1,"edit":{}}"#, C),
            (r#"{"type":"start"}"#, C),
            (r#"{"type":"stop"}"#, C),
            (r#"{"type":"devices"}"#, C),
            (r#"{"type":"findRadios"}"#, C),
            (r#"{"type":"listDir"}"#, C),
            (r#"{"type":"readTrConfig","path":"/"}"#, C),
            (r#"{"type":"listen","on":true}"#, L),
            (r#"{"type":"unlockKeys","source":"a","password":"b"}"#, C),
            (r#"{"type":"lockKeys","source":"a"}"#, C),
            (r#"{"type":"channelFile","index":0,"path":""}"#, C),
            (r#"{"type":"surveyStart"}"#, C),
            (r#"{"type":"surveyListen","freqHz":1.0}"#, C),
            (r#"{"type":"surveyRescan"}"#, C),
            (r#"{"type":"surveyStop"}"#, C),
            (r#"{"type":"licenseQuery","id":1,"lat":0,"lon":0,"radiusM":1,"signals":[]}"#, C),
            (r#"{"type":"callSignQuery","id":1,"callSigns":[]}"#, C),
            (r#"{"type":"profileSource"}"#, C),
            (r#"{"type":"plugins"}"#, C),
            (r#"{"type":"addPlugin","path":"/x"}"#, C),
            (r#"{"type":"removePlugin","id":"x"}"#, C),
            (r#"{"type":"interfaces"}"#, C),
            (r#"{"type":"store","kind":"plugin"}"#, C),
            (r#"{"type":"install","kind":"plugin","id":"x"}"#, C),
            (r#"{"type":"addInterface","path":"/x"}"#, C),
            (r#"{"type":"removeInterface","id":"x"}"#, C),
            (r#"{"type":"subscribe","topics":["log"]}"#, L),
            (r#"{"type":"statsQuery","id":1,"series":["a"]}"#, L),
            (r#"{"type":"radioQuery"}"#, L),
            (r#"{"type":"callsQuery","id":1}"#, L),
            (r#"{"type":"pluginAction","id":1,"plugin":"x","action":"y"}"#, C),
            (r#"{"type":"iqCaptures","id":1}"#, C),
            (r#"{"type":"iqCaptureStart","id":1,"seconds":1}"#, C),
            (r#"{"type":"iqCaptureCancel","id":1,"key":1}"#, C),
            (r#"{"type":"iqCaptureReview","id":1,"name":"x"}"#, C),
            (r#"{"type":"iqCaptureSend","id":1,"name":"x"}"#, C),
            (r#"{"type":"iqCaptureWithdraw","id":1,"name":"x"}"#, C),
            (r#"{"type":"iqRuleCheck","id":1,"rule":{}}"#, C),
            (r#"{"type":"iqCaptureDelete","id":1,"name":"x"}"#, C),
            (r#"{"type":"dataSummary","id":1}"#, C),
            (r#"{"type":"deleteData","id":1,"kinds":[]}"#, C),
            (r#"{"type":"compactData","id":1}"#, C),
            (r#"{"type":"createToken","id":1,"name":"intruder","access":"control"}"#, C),
            (r#"{"type":"quit"}"#, C),
        ]
    }

    /// Every command the recorder takes is in [`commands`], with the access
    /// `Command::needs` gives it: a new one can't be sent by a View user
    /// unnoticed (it needs control until it's listed as looking, and this
    /// test fails until it's decided).
    #[test]
    fn every_command_has_a_decided_role() {
        use trunk_app::protocol::Command;
        let listed: std::collections::BTreeSet<&str> = commands().iter().map(|(j, _)| Command::parse(j).unwrap_or_else(|e| panic!("{j}: {e}")).kind()).collect();
        let all: std::collections::BTreeSet<&str> = Command::TYPES.iter().copied().collect();
        assert_eq!(listed, all, "every command, once: a new one needs its row in commands()");
        for (j, needs) in commands() {
            assert_eq!(Command::parse(j).unwrap().needs(), needs, "{j}");
        }
    }

    /// Whoever may only listen (a View user; nobody signed in, anonymous
    /// View; an Admin through a listen interface's port; anyone there with
    /// signing in off) is refused every command but looking, over a real
    /// socket, and told why; what it may send is answered. Nothing refused
    /// was done (no user made).
    #[tokio::test]
    async fn a_listen_socket_is_refused_every_command_but_looking() {
        use crate::config::ApiAccess;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as Ws;
        let dir = crate::db::TestDir::new("ws-matrix");
        let (ctx, luke, kiosk) = signed_in_ctx(&dir, "ws-matrix");
        ctx.config.lock().unwrap().server.interfaces.insert("wall".into(), trunk_app::config::Interface { port: 8081, ..Default::default() });
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let builtin = super::test_server(&ctx, super::Door::Builtin).await;
        let wall = super::test_server(&ctx, super::Door::Interface { id: "wall".into(), stop }).await;
        let (looking, changing): (Vec<_>, Vec<_>) = commands().into_iter().partition(|(_, a)| *a == ApiAccess::Listen);
        let kind = |j: &str| trunk_app::protocol::Command::parse(j).unwrap().kind();
        let want: std::collections::BTreeSet<&str> = changing.iter().map(|(j, _)| kind(j)).collect();
        let (bearer_kiosk, bearer_luke) = (format!("Bearer {kiosk}"), format!("Bearer {luke}"));
        let cases: [(&str, std::net::SocketAddr, Vec<(&str, &str)>, bool, Option<ApiAccess>, &str); 4] = [
            ("a View user", builtin, vec![("authorization", &bearer_kiosk)], true, None, "The user \"kiosk\" may only listen"),
            ("anonymous View", builtin, vec![], true, Some(ApiAccess::Listen), "Not signed in, this page may only listen"),
            ("an Admin on a listen port", wall, vec![("authorization", &bearer_luke)], true, None, "This interface may only listen"),
            ("signing in off, a listen port", wall, vec![], false, None, "This interface may only listen"),
        ];
        for (who, addr, headers, enabled, anonymous, says) in cases {
            {
                let mut c = ctx.config.lock().unwrap();
                c.server.auth.enabled = enabled;
                c.server.auth.anonymous = anonymous;
            }
            let (mut ws, hello) = socket(addr, &headers).await;
            assert_eq!(hello["access"], "listen", "{who}");
            for (j, _) in looking.iter().chain(&changing) {
                ws.send(Ws::text(*j)).await.unwrap();
            }
            let (mut refused, mut answered) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
            let heard = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                while refused.len() < want.len() {
                    let Some(Ok(m)) = ws.next().await else { return false };
                    let v: serde_json::Value = m.to_text().ok().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
                    let message = v["message"].as_str().unwrap_or("");
                    if v["type"] == "error" && message.contains("may only listen") {
                        assert!(message.starts_with(says), "{who}: {message}");
                        refused.insert(message.split("send \"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("").to_string());
                    } else if let Some(t) = v["type"].as_str() {
                        answered.insert(t.to_string());
                    }
                }
                true
            })
            .await;
            assert_eq!(heard, Ok(true), "{who}: refused {refused:?}");
            assert_eq!(refused.iter().map(String::as_str).collect::<std::collections::BTreeSet<_>>(), want, "{who}: every command but looking, and no more");
            for t in ["subscribed", "statsResult", "radioResult", "callsResult"] {
                assert!(answered.contains(t), "{who}: {t} answered ({answered:?})");
            }
        }
        let users: Vec<String> = ctx.config.lock().unwrap().server.auth.users.keys().cloned().collect();
        assert_eq!(users, ["kiosk", "luke"], "nothing refused was done");
        assert_eq!(ctx.phase.lock().unwrap().message().kind(), "state");
    }

    /// Every route the recorder serves, on both kinds of port (the built-in
    /// interface's; a listen interface's), for each kind of caller: signing
    /// in off; on, and nobody signed in; on, anonymous View; a View user's
    /// token; an Admin's. "g" is the guard's 401 (sign in first); anything
    /// else, the status the route answers with. The routes are read from
    /// the source, so a route that's new fails this test until it's decided.
    #[tokio::test]
    async fn every_route_is_decided_for_every_caller_on_both_ports() {
        use crate::config::ApiAccess;
        let dir = crate::db::TestDir::new("route-matrix");
        let (ctx, luke, kiosk) = signed_in_ctx(&dir, "route-matrix");
        let ui = dir.join("wall-ui");
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::write(ui.join("index.html"), "wall").unwrap();
        let cap = std::path::PathBuf::from(&ctx.config.lock().unwrap().recording.capture_dir);
        std::fs::create_dir_all(&cap).unwrap();
        std::fs::write(cap.join("x.wav"), b"RIFF").unwrap();
        ctx.config.lock().unwrap().server.interfaces.insert("wall".into(), trunk_app::config::Interface { port: 8081, path: ui.display().to_string(), ..Default::default() });
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let builtin = super::test_server(&ctx, super::Door::Builtin).await;
        let wall = super::test_server(&ctx, super::Door::Interface { id: "wall".into(), stop }).await;
        // (route as the source writes it, method, path asked, body, extra header; then by caller: off, anonymous, anonymous View, View, Admin.)
        type Row = (&'static str, &'static str, &'static str, Option<&'static str>, Option<(&'static str, &'static str)>, [&'static str; 5], Option<[&'static str; 5]>);
        const ALL: fn(&'static str) -> [&'static str; 5] = |s| [s; 5];
        let rows: Vec<Row> = vec![
            ("/api/ws", "GET", "/api/ws", None, None, ["400", "g", "400", "400", "400"], Some(["400", "g", "400", "400", "400"])),
            ("/api/version", "GET", "/api/version", None, None, ALL("200"), Some(ALL("200"))),
            ("/api/login", "POST", "/api/login", Some(""), None, ALL("401"), Some(ALL("401"))),
            ("/api/logout", "POST", "/api/logout", Some(""), None, ALL("204"), Some(ALL("204"))),
            ("/api/session", "GET", "/api/session", None, None, ALL("200"), Some(ALL("200"))),
            ("/api/passkey/register/start", "POST", "/api/passkey/register/start", Some(r#"{"name":"Key"}"#), None, ["401", "g", "401", "200", "200"], Some(["403", "g", "403", "403", "403"])),
            ("/api/passkey/register/finish", "POST", "/api/passkey/register/finish", Some("{}"), None, ["401", "g", "401", "400", "400"], Some(["403", "g", "403", "403", "403"])),
            ("/api/passkey/login/start", "POST", "/api/passkey/login/start", Some(""), None, ALL("200"), Some(ALL("403"))),
            ("/api/passkey/login/finish", "POST", "/api/passkey/login/finish", Some("{}"), None, ALL("401"), Some(ALL("403"))),
            ("/signin", "GET", "/signin", None, None, ALL("200"), Some(ALL("200"))),
            ("/api/docs", "GET", "/api/docs", None, None, ALL("200"), Some(ALL("200"))),
            ("/api/schema", "GET", "/api/schema", None, None, ALL("200"), Some(ALL("200"))),
            ("/api/protocol.ts", "GET", "/api/protocol.ts", None, None, ALL("200"), Some(ALL("200"))),
            ("/api/{*path}", "GET", "/api/client.js", None, None, ALL("200"), Some(ALL("200"))),
            ("/api/{*path}", "GET", "/api/nothing.js", None, None, ["404", "g", "404", "404", "404"], Some(["404", "g", "404", "404", "404"])),
            ("/api/v1/calls", "GET", "/api/v1/calls", None, None, ["200", "g", "200", "200", "200"], Some(["200", "g", "200", "200", "200"])),
            ("/api/v1/calls/{*path}", "GET", "/api/v1/calls/nope.json", None, None, ["404", "g", "404", "404", "404"], Some(["404", "g", "404", "404", "404"])),
            ("/api/v1/storage", "GET", "/api/v1/storage", None, None, ["200", "g", "200", "200", "200"], Some(["200", "g", "200", "200", "200"])),
            ("/api/v1/export/{table}", "GET", "/api/v1/export/calls", None, None, ["200", "g", "403", "403", "200"], Some(["403", "g", "403", "403", "403"])),
            ("/calls/{*path}", "GET", "/calls/x.wav", None, None, ["200", "g", "200", "200", "200"], Some(["200", "g", "200", "200", "200"])),
            ("/store/screenshots/{sha256}", "GET", "/store/screenshots/00", None, None, ["404", "g", "403", "403", "404"], None),
            ("/interfaces/{id}/screenshots/{n}", "GET", "/interfaces/wall/screenshots/0", None, None, ["404", "g", "403", "403", "404"], None),
            ("fallback", "GET", "/", None, None, ["200", "g", "200", "200", "200"], Some(["200", "g", "200", "200", "200"])),
            ("fallback", "GET", "/", None, Some(("accept", "text/html")), ["200", "401", "200", "200", "200"], Some(["200", "401", "200", "200", "200"])),
        ];
        // Every route the source has: the API's (the call store's under /api/v1), the built-in interface's.
        let routes = |src: &str, prefix: &str| src.split(".route(\"").skip(1).map(|r| format!("{prefix}{}", r.split('"').next().unwrap())).collect::<Vec<_>>();
        let mut served: std::collections::BTreeSet<String> = routes(include_str!("server.rs"), "").into_iter().chain(routes(include_str!("api_v1.rs"), "/api/v1")).collect();
        served.insert("fallback".into());
        assert_eq!(rows.iter().map(|r| r.0.to_string()).collect::<std::collections::BTreeSet<_>>(), served, "every route, decided: a new one needs its row here");
        let callers: [(&str, bool, Option<ApiAccess>, Option<String>); 5] =
            [("off", false, None, None), ("anonymous", true, None, None), ("anonymous View", true, Some(ApiAccess::Listen), None), ("View", true, None, Some(kiosk.clone())), ("Admin", true, None, Some(luke.clone()))];
        let mut wrong = vec![];
        for (i, (caller, enabled, anonymous, secret)) in callers.iter().enumerate() {
            {
                let mut c = ctx.config.lock().unwrap();
                c.server.auth.enabled = *enabled;
                c.server.auth.anonymous = *anonymous;
            }
            for (route, method, path, body, extra, on_builtin, on_wall) in &rows {
                for (door, addr, expect) in [("built-in", builtin, Some(on_builtin)), ("wall", wall, on_wall.as_ref())] {
                    let Some(expect) = expect else { continue };
                    let host = format!("localhost:{}", addr.port());
                    let bearer = secret.as_ref().map(|s| format!("Bearer {s}"));
                    let mut h: Vec<(&str, &str)> = vec![("host", &host)];
                    h.extend(bearer.as_deref().map(|b| ("authorization", b)));
                    h.extend(*extra);
                    let (code, headers, _) = http(addr, method, path, &h, *body).await;
                    let got = if code == 401 && headers.get("www-authenticate").is_some() { "g".to_string() } else { code.to_string() };
                    if got != expect[i] {
                        wrong.push(format!("{method} {path} ({route}) on the {door} port, {caller}: {got}, not {}", expect[i]));
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }

    /// Nothing secret reaches a View user: not in what a socket is greeted
    /// with, nor a config sent later, nor the session, nor the call store's
    /// routes. The secrets: plugins' settings (the recorder's and a
    /// system's), key sources (files and passwords' files), the database's
    /// connection string, the users (their tokens' hashes, passkeys' records),
    /// the proxy and Cloudflare's settings. (Paths that are no credential, the
    /// HTTPS files' among them, are the config as a View user sees it.)
    /// An Admin is sent them: they were there to find.
    #[tokio::test]
    async fn a_view_user_is_sent_no_secret() {
        use crate::config::ApiAccess;
        let dir = crate::db::TestDir::new("redaction");
        let (ctx, luke, kiosk) = signed_in_ctx(&dir, "redaction");
        {
            let mut c = ctx.config.lock().unwrap();
            let mut v = serde_json::to_value(&*c).unwrap();
            v["plugins"] = serde_json::json!({ "openmhz": { "enabled": false, "settings": { "apiKey": "SECRET-1" } } });
            v["keys"] = serde_json::json!({ "radio": { "file": "SECRET-2.json", "passwordFile": "SECRET-3.txt" } });
            v["systems"][0]["keys"] = "SECRET-4".into();
            v["systems"][0]["plugins"] = serde_json::json!({ "openmhz": { "apiKey": "SECRET-5" } });
            v["conventional"] = serde_json::json!([{ "shortName": "conv", "plugins": { "openmhz": { "apiKey": "SECRET-6" } } }]);
            v["storage"]["url"] = "postgres://trp:SECRET-7@db/trunk".into();
            v["server"]["auth"]["proxy"] = serde_json::json!({ "cloudflare": { "team": "secret8team", "aud": "SECRET9aud" }, "users": { "SECRET-10@example.com": "luke" } });
            v["server"]["auth"]["users"]["luke"]["passkeys"] = serde_json::json!([{ "name": "SECRET-11", "created": 1, "id": "SECRET-12", "key": { "cred": "SECRET-13" } }]);
            *c = crate::config::Config::from_value(v).unwrap();
        }
        let hashes: Vec<String> = ctx.config.lock().unwrap().server.auth.users.values().filter_map(|u| u.token.as_ref().map(|t| t.hash.clone())).collect();
        let secret_in = |text: &str| text.contains("SECRET") || text.to_ascii_lowercase().contains("secret8team") || hashes.iter().any(|h| text.contains(h.as_str())) || text.contains(&luke) || text.contains(&kiosk);
        let sent = |access: ApiAccess| {
            let ctx = ctx.clone();
            async move {
                let m = super::greeting(&ctx, access, Some("kiosk".into())).await;
                m.iter().filter_map(|m| super::text_for(&crate::runtime::Out::of(m), access == ApiAccess::Control)).collect::<Vec<_>>().join("\n")
            }
        };
        let view = sent(ApiAccess::Listen).await;
        assert!(view.contains(r#""type":"config""#) && view.contains(r#""shortName":"conv""#), "the config, without its secrets");
        assert!(!secret_in(&view), "{view}");
        let admin = sent(ApiAccess::Control).await;
        for n in 1..=13 {
            assert!(admin.contains(&format!("SECRET-{n}")) || (n == 8 || n == 9), "SECRET-{n} was there to find");
        }
        // A config sent later, as everyone hears it.
        let config = crate::runtime::Out::of(&trunk_app::protocol::ToClient::Config { config: Box::new(ctx.config.lock().unwrap().clone()), version: 2 });
        let later = super::text_for(&config, false).unwrap();
        assert!(later.contains(r#""shortName":"conv""#) && !secret_in(&later), "{later}");
        // Over the wire: the greeting, the session, the call store.
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let bearer = format!("Bearer {kiosk}");
        let (mut ws, hello) = socket(addr, &[("authorization", &bearer)]).await;
        assert!(hello["configPath"].is_null() && !secret_in(&hello.to_string()), "{hello}");
        drop(ws.close(None).await);
        for path in ["/api/session", "/api/v1/storage", "/api/v1/calls"] {
            let (code, _, body) = http(addr, "GET", path, &[("authorization", &bearer)], None).await;
            assert!(code == 200 && !secret_in(&body), "{path}: {code} {body}");
        }
    }

    /// Which requests' cookies (and so the proxy's header) count: a page of
    /// this origin, one typed in or a bookmark, a program (no `Sec-Fetch-*`),
    /// a link followed to here; never another site's, another port's
    /// (same-site), or another site's frame or form.
    #[test]
    fn whose_cookies_count() {
        let h = |pairs: &[(&str, &str)]| {
            let mut m = HeaderMap::new();
            for (n, v) in pairs {
                m.insert(axum::http::HeaderName::from_bytes(n.as_bytes()).unwrap(), v.parse().unwrap());
            }
            super::cookies_count(&m)
        };
        use axum::http::HeaderMap;
        let site = |s: &str, mode: &str, dest: &str| h(&[("sec-fetch-site", s), ("sec-fetch-mode", mode), ("sec-fetch-dest", dest)]);
        assert!(h(&[]) && h(&[("sec-fetch-site", "same-origin")]) && h(&[("sec-fetch-site", "none")]));
        assert!(site("same-origin", "cors", "empty") && site("none", "navigate", "document"));
        assert!(site("cross-site", "navigate", "document") && site("same-site", "navigate", "document"), "a link followed here");
        for (s, mode, dest) in [("same-site", "cors", "empty"), ("cross-site", "cors", "empty"), ("cross-site", "no-cors", "image"), ("cross-site", "navigate", "iframe"), ("same-site", "websocket", "websocket"), ("cross-site", "navigate", "frame"), ("cross-site", "no-cors", "script"), ("CROSS-SITE", "cors", "empty")] {
            assert!(!site(s, mode, dest), "{s} {mode} {dest}");
        }
        assert!(!h(&[("sec-fetch-site", "garbage")]), "a value that's none of them");
    }

    /// What CORS lets in (`allowedOrigins`, `*` too) signs in with a token
    /// alone: never the browser's cookie, nor the proxy's header; its socket
    /// too. A page from an origin not allowed is refused, token or not.
    #[tokio::test]
    async fn what_cors_lets_in_signs_in_with_a_token_alone() {
        let dir = crate::db::TestDir::new("cors-token");
        let (ctx, luke, _) = signed_in_ctx(&dir, "cors-token");
        ctx.config.lock().unwrap().server.auth.proxy = Some(trunk_app::config::AuthProxy { from: vec!["127.0.0.1".into()], ..Default::default() });
        let addr = super::test_server(&ctx, super::Door::Builtin).await;
        let (_, h, _) = http(addr, "POST", "/api/login", &[], Some(&luke)).await;
        let cookie = h["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
        let bearer = format!("Bearer {luke}");
        let user = |headers: Vec<(&'static str, String)>| async move {
            let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let (code, h, body) = http(addr, "GET", "/api/session", &h, None).await;
            (code, h.get("access-control-allow-origin").map(|v| v.to_str().unwrap().to_string()), serde_json::from_str::<serde_json::Value>(&body).unwrap_or_default()["user"].clone())
        };
        let evil = || ("origin", "https://evil.example".to_string());
        assert_eq!(user(vec![("cookie", cookie.clone())]).await.2, "luke", "its own page");
        assert_eq!(user(vec![("remote-user", "luke".into())]).await.2, "luke", "the proxy, from its address");
        for allowed in [vec!["https://evil.example".to_string()], vec!["*".to_string()]] {
            ctx.config.lock().unwrap().server.allowed_origins = allowed.clone();
            assert_eq!(user(vec![evil(), ("cookie", cookie.clone())]).await, (200, Some("https://evil.example".into()), serde_json::Value::Null), "{allowed:?}: its cookie doesn't count");
            assert_eq!(user(vec![evil(), ("remote-user", "luke".into())]).await.2, serde_json::Value::Null, "{allowed:?}: nor the proxy's header");
            assert_eq!(user(vec![evil(), ("authorization", bearer.clone())]).await.2, "luke", "{allowed:?}: a token does");
            assert_eq!(http(addr, "GET", "/api/v1/calls", &[("origin", "https://evil.example"), ("cookie", &cookie)], None).await.0, 401);
            let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!("ws://{addr}/api/ws")).unwrap();
            req.headers_mut().insert("origin", "https://evil.example".parse().unwrap());
            req.headers_mut().insert("cookie", cookie.parse().unwrap());
            assert!(tokio_tungstenite::connect_async(req).await.is_err(), "{allowed:?}: no socket on its cookie");
            let (_, hello) = socket(addr, &[("origin", "https://evil.example"), ("authorization", &bearer)]).await;
            assert_eq!(hello["user"], "luke", "{allowed:?}: a socket on its token");
        }
        ctx.config.lock().unwrap().server.allowed_origins = vec![];
        assert_eq!(user(vec![evil(), ("authorization", bearer.clone())]).await.0, 403, "not allowed: refused, token or not");
        let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!("ws://{addr}/api/ws")).unwrap();
        req.headers_mut().insert("origin", "https://evil.example".parse().unwrap());
        req.headers_mut().insert("authorization", bearer.parse().unwrap());
        assert!(tokio_tungstenite::connect_async(req).await.is_err(), "nor its socket");
        // A proxy's user signs out at the proxy; signing out everywhere is refused them.
        let (code, _, why) = http(addr, "POST", "/api/logout", &[("remote-user", "luke")], Some(r#"{"all":true}"#)).await;
        assert!(code == 403 && why.contains("proxy"), "{code} {why}");
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
        std::fs::write(root.join(".install.json"), "{}").unwrap();
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
        assert_eq!(get(&root, ".install.json", "/.install.json").await.0, StatusCode::NOT_FOUND);
        // No folder: says so.
        assert!(get(&base.join("gone"), "", "/").await.2.contains("isn't there"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn paths_and_escapes() {
        assert_eq!(percent_decode("my%20ui/a%2Bb.js").as_deref(), Some("my ui/a+b.js"));
        assert_eq!(percent_decode("bad%2"), None);
        assert_eq!(percent_decode("%FF"), None);
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
        let l = Listen { system: Some("dcfd".into()), talkgroup: None, audio: true, voice: None };
        assert!(l.wants("dcfd", 101) && !l.wants("wmata", 101));
        let l = Listen { system: Some("wmata".into()), talkgroup: Some(101), audio: true, voice: None };
        assert!(l.wants("wmata", 101) && !l.wants("wmata", 102) && !l.wants("dcfd", 101));
        assert!(Listen { system: None, talkgroup: None, audio: true, voice: None }.wants("conv", 7));
    }
}
