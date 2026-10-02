//! The browser interface: an embedded web UI, recorded calls under /calls/,
//! and a WebSocket (/api/ws) carrying JSON messages both ways plus binary
//! live-audio frames.
//!
//! Server → browser: `hello` (config, devices, phase, history), `state`,
//! `status` (~2/s), `spectrum` (~7/s per source), `log`, `concluded`,
//! `devices`, `error`, and audio frames `[2][u16 system][u32 call id][u32
//! talkgroup][i16…]` (8 kHz) to connections that asked to listen.
//! `radios` (the optional USRP / Airspy / SoapySDR drivers, SoapySDR's modules
//! and the devices) comes in `hello` and answers `findRadios`, which also
//! searches for USRPs and SoapySDR devices.
//! Browser → server: `setConfig`, `start`, `stop`, `devices`, `findRadios`,
//! `listen {on, system, talkgroup}`, `quit` (stop recording, tell every browser
//! `quit`, exit). GET /api/version identifies a running instance.
//!
//! The first-run survey (see [`crate::survey`]): `surveyStart {source, bands,
//! findGain}`, `surveyListen {freqHz}`, `surveyRescan`, `surveyStop`; it
//! reports `survey` snapshots (`stage` "idle" when none runs) and
//! `surveySpectrum`. `hello` carries `surveyBands` and the latest snapshot.
//!
//! Plugins (see [`crate::plugins::manage`]): `plugins` follows `hello`, and
//! answers `plugins`, `setPlugin`, `addPlugin`, `removePlugin` and
//! `setPluginAudio`; `pluginRuntime` says how each is doing.

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
pub const APP_ID: &str = "trunk-pro";

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
            "<h1>Trunk Recorder Pro</h1><p>The web interface wasn't built into this binary. Run <code>npm run build</code> in <code>web/</code>, then rebuild.</p>",
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

/// The `dir` message, for the interface's folder picker: a folder on this
/// computer, its subfolders and its .json files (a Trunk Recorder config to
/// import); "" = the home folder. A path that doesn't exist yet lists the
/// nearest folder above it that does.
fn dir_json(path: &str) -> Value {
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
fn tr_config_json(path: &str) -> Value {
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

async fn session(ctx: Arc<Ctx>, mut socket: WebSocket) {
    let mut rx = ctx.hub.subscribe();
    let radios = tokio::task::spawn_blocking(|| crate::radio::radios_json(false)).await.unwrap_or(Value::Null);
    let hello = {
        let config = ctx.config.lock().unwrap().clone();
        let history: Vec<Value> = ctx.history.lock().unwrap().iter().take(300).cloned().collect();
        // Each system's talker aliases, as saved (CSV).
        let units: serde_json::Map<String, Value> = config
            .systems
            .iter()
            .map(|s| &s.short_name)
            .chain([&config.conventional.short_name])
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
    };
    if socket.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    let plugins = plugins_json(&ctx).await;
    if socket.send(Message::Text(plugins.to_string().into())).await.is_err() {
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
                let _ = tokio::task::spawn_blocking(move || ctx3.plugins.install(&v)).await;
                publish(&ctx2.hub, plugins_json(&ctx2).await);
            });
            None
        }
        "setPlugin" | "addPlugin" | "removePlugin" | "setPluginAudio" => {
            let (ctx2, v) = (ctx.clone(), v.clone());
            let r = tokio::task::spawn_blocking(move || {
                let p = &ctx2.plugins;
                match v["type"].as_str() {
                    Some("setPlugin") => p.set(&v).map(|_| None),
                    Some("addPlugin") => p.add(&v).map(Some),
                    Some("removePlugin") => p.remove(&v).map(|_| None),
                    _ => p.set_audio(&v).map(|_| None),
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
