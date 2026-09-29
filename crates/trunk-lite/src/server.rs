//! The browser interface: an embedded web UI, recorded calls under /calls/,
//! and a WebSocket (/api/ws) carrying JSON messages both ways plus binary
//! live-audio frames.
//!
//! Server → browser: `hello` (config, devices, phase, history), `state`,
//! `status` (~2/s), `spectrum` (~7/s per source), `log`, `concluded`,
//! `devices`, `error`, and audio frames `[1][u32 call id][u32 talkgroup][i16…]`
//! (8 kHz) to connections that asked to listen.
//! `radios` (the optional USRP / Airspy drivers and their devices) comes in
//! `hello` and answers `findRadios`, which also searches for USRPs.
//! Browser → server: `setConfig`, `start`, `stop`, `devices`, `findRadios`,
//! `listen {on, talkgroup}`, `quit` (stop recording, tell every browser
//! `quit`, exit). GET /api/version identifies a running instance.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
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

/// What `GET /api/version` answers — how a second launch recognises us.
pub const APP_ID: &str = "trunk-lite";

/// Serve until `quit` from a browser, Ctrl-C or SIGTERM. Recording is stopped
/// first either way, so calls in progress are written out.
pub async fn serve(ctx: Arc<Ctx>, listener: std::net::TcpListener) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let app = Router::new()
        .route("/api/ws", get(ws))
        .route("/api/version", get(|| async { axum::Json(json!({ "app": APP_ID, "version": env!("CARGO_PKG_VERSION") })) }))
        .route("/calls/{*path}", get(call_file))
        .fallback(static_file)
        .with_state(ctx.clone());
    let ctx2 = ctx.clone();
    let shutdown = async move {
        tokio::select! {
            _ = ctx2.quit.notified() => {}
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate() => {}
        }
        let ctx3 = ctx2.clone();
        let _ = tokio::task::spawn_blocking(move || {
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

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Ui::get(path).or_else(|| Ui::get("index.html")) {
        Some(f) => {
            let mime = mime_guess::from_path(if Ui::get(path).is_some() { path } else { "index.html" }).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], f.data).into_response()
        }
        None => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
            "<h1>Trunk Recorder Lite</h1><p>The web interface wasn't built into this binary. Run <code>npm run build</code> in <code>web/</code>, then rebuild.</p>",
        )
            .into_response(),
    }
}

/// A recorded file, confined to the capture folder.
async fn call_file(State(ctx): State<Arc<Ctx>>, UrlPath(path): UrlPath<String>) -> Response {
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

async fn ws(State(ctx): State<Arc<Ctx>>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |socket| session(ctx, socket))
}

fn devices_json() -> Value {
    json!({ "type": "devices", "devices": sdr::devices() })
}

async fn session(ctx: Arc<Ctx>, mut socket: WebSocket) {
    let mut rx = ctx.hub.subscribe();
    let radios = tokio::task::spawn_blocking(|| crate::radio::radios_json(false)).await.unwrap_or(Value::Null);
    let hello = {
        let config = ctx.config.lock().unwrap().clone();
        let history: Vec<Value> = ctx.history.lock().unwrap().iter().take(300).cloned().collect();
        json!({
            "type": "hello",
            "version": env!("CARGO_PKG_VERSION"),
            "platform": std::env::consts::OS,
            "config": config,
            "configPath": ctx.config_path.display().to_string(),
            "devices": sdr::devices(),
            "phase": ctx.phase.lock().unwrap().to_json(),
            "history": history,
            "radios": radios,
        })
    };
    if socket.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    // Live audio: off until the browser asks; optionally one talkgroup only.
    let mut listen: Option<Option<u32>> = None;
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(out) => {
                    let m = match &*out {
                        Out::Text(s) => Message::Text(s.clone().into()),
                        Out::Audio { tg, frame } => match listen {
                            Some(None) => Message::Binary(frame.clone().into()),
                            Some(Some(want)) if want == *tg => Message::Binary(frame.clone().into()),
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
                    if let Some(reply) = command(&ctx, &v, &mut listen).await {
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

async fn command(ctx: &Arc<Ctx>, v: &Value, listen: &mut Option<Option<u32>>) -> Option<Value> {
    match v["type"].as_str()? {
        "setConfig" => match serde_json::from_value::<Config>(v["config"].clone()) {
            Ok(c) => {
                let saved = c.save(&ctx.config_path);
                *ctx.config.lock().unwrap() = c.clone();
                publish(&ctx.hub, json!({ "type": "config", "config": c }));
                saved.err().map(|e| json!({ "type": "error", "message": format!("Couldn't save the config: {e}") }))
            }
            Err(e) => Some(json!({ "type": "error", "message": format!("Bad config: {e}") })),
        },
        "start" => {
            let cfg = ctx.config.lock().unwrap().clone();
            let ctx2 = ctx.clone();
            let r = tokio::task::spawn_blocking(move || {
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
            ctx.quit.notify_one();
            None
        }
        "listen" => {
            *listen = if v["on"].as_bool() == Some(true) { Some(v["talkgroup"].as_u64().map(|t| t as u32)) } else { None };
            None
        }
        _ => None,
    }
}
