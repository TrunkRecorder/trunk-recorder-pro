//! The browser interface: an embedded web UI, recorded calls under /calls/,
//! and a WebSocket (/api/ws) carrying JSON messages both ways plus binary
//! live-audio frames.
//!
//! Server → browser: `hello` (config, devices, phase, history), `state`,
//! `status` (~2/s), `spectrum` (~7/s per source), `log`, `concluded`,
//! `devices`, `error`, and audio frames `[2][u16 system][u32 call id][u32
//! talkgroup][i16…]` (8 kHz) to connections that asked to listen.
//! `radios` (the optional USRP / Airspy drivers and their devices) comes in
//! `hello` and answers `findRadios`, which also searches for USRPs.
//! Browser → server: `setConfig`, `start`, `stop`, `devices`, `findRadios`,
//! `listen {on, system, talkgroup}`, `quit` (stop recording, tell every browser
//! `quit`, exit). GET /api/version identifies a running instance.
//!
//! The first-run survey (see [`crate::survey`]): `surveyStart {source, bands,
//! findGain}`, `surveyListen {freqHz}`, `surveyRescan`, `surveyStop`; it
//! reports `survey` snapshots (`stage` "idle" when none runs) and
//! `surveySpectrum`. `hello` carries `surveyBands` and the latest snapshot.

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

/// End a running survey (frees its radio) and tell the browsers.
fn stop_survey(ctx: &Ctx) {
    let r = ctx.survey.lock().unwrap().take();
    if let Some(r) = r {
        r.stop();
        *ctx.survey_last.lock().unwrap() = None;
        publish(&ctx.hub, crate::survey::idle_json());
    }
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
            "surveyBands": trunk_app::survey::bands_json(),
            "survey": ctx.survey_last.lock().unwrap().clone().unwrap_or_else(crate::survey::idle_json),
        })
    };
    if socket.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    // Live audio: off until the browser asks; optionally one system and/or talkgroup only.
    let mut listen: Option<Listen> = None;
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(out) => {
                    let m = match &*out {
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
                // The channel file is linked with "channelFile", not here; while
                // linked, the channels are the file's (re-read: a spreadsheet may
                // have changed it).
                c.conventional.channel_file = ctx.config.lock().unwrap().conventional.channel_file.clone();
                let _ = c.load_channel_file(&ctx.config_path);
                let saved = c.save(&ctx.config_path);
                *ctx.config.lock().unwrap() = c.clone();
                publish(&ctx.hub, json!({ "type": "config", "config": c }));
                saved.err().map(|e| json!({ "type": "error", "message": format!("Couldn't save the config: {e}") }))
            }
            Err(e) => Some(json!({ "type": "error", "message": format!("Bad config: {e}") })),
        },
        // Link the conventional channels to a CSV (created from the list if
        // new), reload it (the same path again), or unlink ("").
        "channelFile" => {
            let path = v["path"].as_str().unwrap_or("").to_string();
            let mut c = ctx.config.lock().unwrap().clone();
            let r = c.link_channel_file(&ctx.config_path, &path);
            if r.is_ok() || !c.conventional.channel_file.is_empty() {
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
        "listen" => {
            *listen = (v["on"].as_bool() == Some(true))
                .then(|| Listen { system: v["system"].as_u64().map(|s| s as u16), talkgroup: v["talkgroup"].as_u64().map(|t| t as u32) });
            None
        }
        _ => None,
    }
}
