//! The call store over HTTP: `GET /api/v1/calls`, `/api/v1/calls/{path}` and
//! `/api/v1/store`, for plugins and interfaces on other machines. Answers come
//! from the committed rows, as `callsQuery` does. Every door that serves the
//! API serves these, with the access the WebSocket there has.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::access::Door;
use crate::db::read::{self, CallsFilter};
use crate::runtime::{self, Ctx};

/// The routes, to be mounted ahead of the `/api/{*path}` docs catch-all.
pub fn router() -> Router<Arc<Ctx>> {
    Router::new().nest(
        "/api/v1",
        Router::new()
            .route("/calls", get(calls))
            .route("/calls/{*path}", get(call))
            .route("/store", get(store)),
    )
}

/// A refusal: its status and `{ "error": message }`.
struct Fail(StatusCode, String);

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn error(code: StatusCode, msg: &str) -> Fail {
    Fail(code, msg.to_string())
}

/// Why a request may not go on: its door is gone or off, or the store is.
fn refused(ctx: &Ctx, door: &Door) -> Option<Response> {
    if crate::server::access_of(ctx, door).is_none() {
        return Some(StatusCode::NOT_FOUND.into_response());
    }
    ctx.db
        .is_disabled()
        .then(|| error(StatusCode::SERVICE_UNAVAILABLE, "the call store is off").into_response())
}

/// Parameter `name`, trimmed; one that is empty is as if not given.
fn param<'a>(q: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    q.get(name).map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// Parameter `name` as a number, if given.
fn number<T: std::str::FromStr>(
    q: &HashMap<String, String>,
    name: &str,
) -> Result<Option<T>, Fail> {
    match param(q, name) {
        None => Ok(None),
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(|_| error(StatusCode::BAD_REQUEST, &format!("{name} must be a number"))),
    }
}

fn filter(ctx: &Ctx, q: &HashMap<String, String>) -> Result<CallsFilter, Fail> {
    let systems = match param(q, "system") {
        Some(name) => runtime::systems_for(&ctx.config.lock().unwrap(), name),
        None => Vec::new(),
    };
    Ok(CallsFilter {
        systems,
        talkgroup: number(q, "talkgroup")?,
        unit: number(q, "unit")?,
        from_ms: number(q, "from")?,
        to_ms: number(q, "to")?,
        before_ms: number(q, "before")?,
        limit: number::<usize>(q, "limit")?.unwrap_or(100).clamp(1, 500),
    })
}

/// Run a store read off the async threads: its answer, or a 500.
async fn read<T: Send + 'static>(
    ctx: &Arc<Ctx>,
    f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T, Fail> {
    let ctx = ctx.clone();
    match tokio::task::spawn_blocking(move || ctx.db.with_reader(f)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(error(StatusCode::INTERNAL_SERVER_ERROR, &e)),
        Err(e) => Err(error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn calls(
    State(ctx): State<Arc<Ctx>>,
    Extension(door): Extension<Door>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Some(r) = refused(&ctx, &door) {
        return r;
    }
    let f = match filter(&ctx, &q) {
        Ok(f) => f,
        Err(r) => return r.into_response(),
    };
    match read(&ctx, move |c| read::query(c, &f)).await {
        Ok(page) => {
            Json(json!({ "entries": page.entries, "more": page.more, "before": page.before }))
                .into_response()
        }
        Err(r) => r.into_response(),
    }
}

async fn call(
    State(ctx): State<Arc<Ctx>>,
    Extension(door): Extension<Door>,
    Path(path): Path<String>,
) -> Response {
    if let Some(r) = refused(&ctx, &door) {
        return r;
    }
    let path = path.replace('\\', "/");
    match read(&ctx, move |c| read::one(c, &path)).await {
        Ok(Some(entry)) => Json(entry).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such call").into_response(),
        Err(r) => r.into_response(),
    }
}

async fn store(State(ctx): State<Arc<Ctx>>, Extension(door): Extension<Door>) -> Response {
    if let Some(r) = refused(&ctx, &door) {
        return r;
    }
    let (retention_days, batch_seconds) = {
        let cfg = ctx.config.lock().unwrap();
        (cfg.store.retention_days, cfg.store.batch_seconds)
    };
    let stats = read(&ctx, |c| {
        c.query_row(
            "select count(*), min(start_ms), max(start_ms) from calls",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
    })
    .await;
    match stats {
        Ok((calls, oldest, newest)) => {
            let path: Value = ctx.db.path().map(|p| p.display().to_string()).into();
            Json(json!({
                "path": path,
                "schemaVersion": crate::db::schema::SCHEMA_VERSION,
                "calls": calls,
                "oldestMs": oldest,
                "newestMs": newest,
                "retentionDays": retention_days,
                "batchSeconds": batch_seconds,
            }))
            .into_response()
        }
        Err(r) => r.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TestDir;

    /// `GET path` on a server of `ctx` as `door`: the status and body.
    async fn get_as(ctx: &Arc<Ctx>, door: Door, path: &str) -> (u16, String) {
        let app = crate::server::api(Router::new())
            .layer(Extension(door))
            .with_state(ctx.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}{path}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let mut r = agent.get(&url).call().unwrap();
            (r.status().as_u16(), r.body_mut().read_to_string().unwrap())
        })
        .await
        .unwrap()
    }

    async fn get(ctx: &Arc<Ctx>, path: &str) -> (u16, String) {
        get_as(ctx, Door::Builtin, path).await
    }

    fn json_of(body: &str) -> Value {
        serde_json::from_str(body).unwrap()
    }

    #[tokio::test]
    async fn calls_routes_answer_from_the_store() {
        let dir = TestDir::new("api-v1");
        let ctx = runtime::test_ctx(&dir.join("api-v1-cap"));
        let store = get(&ctx, "/api/v1/store").await;
        assert_eq!(store.0, 200, "{}", store.1);
        let v = json_of(&store.1);
        assert_eq!(
            (
                v["calls"].as_u64(),
                v["oldestMs"].is_null(),
                v["newestMs"].is_null()
            ),
            (Some(0), true, true)
        );
        let r = crate::db::row::tests::record(9179, 1_700_000_000_000);
        runtime::index_call(&ctx, "ohmarcs/a", &r, &r.to_json(), true, true);
        ctx.db.flush().unwrap();
        let (code, body) = get(&ctx, "/api/v1/calls?talkgroup=9179").await;
        assert_eq!(code, 200, "not the docs 404: {body}");
        let v = json_of(&body);
        assert_eq!(v["entries"].as_array().unwrap().len(), 1);
        assert_eq!(v["more"], false);
        assert_eq!(
            json_of(&get(&ctx, "/api/v1/calls?talkgroup=1").await.1)["entries"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        let every = "system=ohmarcs&from=0&to=1700000001000&before=1700000001000&limit=5";
        let (code, body) = get(&ctx, &format!("/api/v1/calls?{every}")).await;
        assert_eq!(code, 200, "{body}");
        assert_eq!(json_of(&body)["entries"].as_array().unwrap().len(), 1);
        let (code, body) = get(&ctx, "/api/v1/calls/ohmarcs/a").await;
        assert_eq!(code, 200);
        assert_eq!(json_of(&body)["record"]["talkgroup"], 9179);
        assert_eq!(
            get(&ctx, "/api/v1/calls/ohmarcs%5Ca").await.0,
            200,
            "a backslash is a slash"
        );
        let (code, body) = get(&ctx, "/api/v1/calls/ohmarcs/none").await;
        assert_eq!(
            (code, json_of(&body)["error"].as_str()),
            (404, Some("no such call"))
        );
        let (code, body) = get(&ctx, "/api/v1/calls?limit=x").await;
        assert_eq!(
            (code, json_of(&body)["error"].as_str()),
            (400, Some("limit must be a number"))
        );
        assert_eq!(get(&ctx, "/api/v1/calls?talkgroup=-1").await.0, 400);
        let (code, body) = get(&ctx, "/api/v1/store").await;
        assert_eq!(code, 200);
        let v = json_of(&body);
        assert_eq!(
            (v["schemaVersion"].as_u64(), v["calls"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(
            (v["oldestMs"].as_i64(), v["newestMs"].as_i64()),
            (Some(1_700_000_000_000), Some(1_700_000_000_000))
        );
        assert_eq!(
            (v["retentionDays"].as_u64(), v["batchSeconds"].as_u64()),
            (Some(120), Some(60))
        );
        assert!(v["path"].as_str().unwrap().ends_with("trunk-pro.sqlite"));
    }

    #[tokio::test]
    async fn an_empty_parameter_is_absent() {
        let dir = TestDir::new("api-v1-empty");
        let ctx = runtime::test_ctx(&dir.join("api-v1-empty-cap"));
        let r = crate::db::row::tests::record(9179, 1_700_000_000_000);
        runtime::index_call(&ctx, "ohmarcs/a", &r, &r.to_json(), true, true);
        ctx.db.flush().unwrap();
        for q in [
            "talkgroup=",
            "system=",
            "system=%20",
            "unit=",
            "from=&to=",
            "before=%20",
            "limit=",
            "talkgroup=&system=&unit=&from=&to=&before=&limit=",
        ] {
            let (code, body) = get(&ctx, &format!("/api/v1/calls?{q}")).await;
            assert_eq!(code, 200, "{q}: {body}");
            assert_eq!(
                json_of(&body)["entries"].as_array().unwrap().len(),
                1,
                "{q}"
            );
        }
    }

    #[tokio::test]
    async fn an_interface_door_reads_with_listen_and_is_404_when_disabled() {
        use trunk_app::config::{ApiAccess, Interface};
        let dir = TestDir::new("api-v1-iface");
        let ctx = runtime::test_ctx(&dir.join("api-v1-iface-cap"));
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let door = Door::Interface {
            id: "wall".into(),
            stop,
        };
        let set = |enabled| {
            ctx.config.lock().unwrap().server.interfaces.insert(
                "wall".into(),
                Interface {
                    port: 8081,
                    enabled,
                    access: ApiAccess::Listen,
                    ..Default::default()
                },
            )
        };
        set(true);
        for p in ["/api/v1/calls", "/api/v1/store"] {
            let (code, body) = get_as(&ctx, door.clone(), p).await;
            assert_eq!(code, 200, "{p}: {body}");
        }
        set(false);
        for p in ["/api/v1/calls", "/api/v1/calls/ohmarcs/a", "/api/v1/store"] {
            let (code, body) = get_as(&ctx, door.clone(), p).await;
            assert_eq!((code, body.as_str()), (404, ""), "{p}");
        }
    }

    #[tokio::test]
    async fn a_store_that_is_off_answers_503() {
        let dir = TestDir::new("api-v1-off");
        let store = dir.join("trunk-pro.sqlite");
        crate::db::seed_newer(&store);
        let ctx = runtime::test_ctx_with_store(&dir.join("cap"), &store);
        for p in ["/api/v1/calls", "/api/v1/calls/ohmarcs/a", "/api/v1/store"] {
            let (code, body) = get(&ctx, p).await;
            assert_eq!(
                (code, json_of(&body)["error"].as_str()),
                (503, Some("the call store is off")),
                "{p}"
            );
        }
    }

    #[tokio::test]
    async fn a_door_that_is_off_answers_404() {
        let dir = TestDir::new("api-v1-door");
        let ctx = runtime::test_ctx(&dir.join("api-v1-door-cap"));
        let (_tx, stop) = tokio::sync::watch::channel(false);
        let door = Door::Interface {
            id: "gone".into(),
            stop,
        };
        for p in ["/api/v1/calls", "/api/v1/calls/ohmarcs/a", "/api/v1/store"] {
            assert_eq!(get_as(&ctx, door.clone(), p).await.0, 404, "{p}");
        }
    }
}
