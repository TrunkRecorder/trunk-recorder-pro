//! Setup → Data's tools: the store's tables as CSV files
//! (`GET /api/v1/export/{table}`, `?system=<id>` for one system's), what it
//! keeps of each system (`dataSummary`), and deleting kinds of it
//! (`deleteData`), from the store and from what the recorder holds of them
//! (the talkgroups, talker aliases, radio registry, the dashboard's history,
//! the live call list), and compacting the store (`compactData`) to give
//! the space back. Recordings on disk are never touched.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Local, TimeZone};
use serde_json::json;
use trunk_app::protocol::{DataCounts, DataKind, DataSystem, DataTable, ToClient};
use trunk_core::metrics::system_key;

use crate::access::Door;
use crate::config::ApiAccess;
use crate::db::sql::V;
use crate::db::store::{Cell, ExportPage};
use crate::runtime::{self, publish, Ctx};

/// Rows an export reads from the store at a time.
const PAGE: usize = 2000;

/// The file an export is saved as: `all` or the system's short name, the
/// table, and when (local time), e.g. `dcfd-calls-2026-11-07-143205.csv`.
pub fn file_name(system: Option<&str>, table: DataTable, at: chrono::DateTime<Local>) -> String {
    let scope: String = system.map_or("all".into(), |s| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect());
    format!("{scope}-{}-{}.csv", table.name(), at.format("%Y-%m-%d-%H%M%S"))
}

/// `GET /api/v1/export/{table}`: a table as a CSV file, every system's or
/// (`?system=<id>`) one's. Control access only, as Setup is.
pub async fn export(State(ctx): State<Arc<Ctx>>, Extension(door): Extension<Door>, Path(table): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    let fail = |code: StatusCode, msg: &str| (code, Json(json!({ "error": msg }))).into_response();
    match crate::server::access_of(&ctx, &door) {
        None => return StatusCode::NOT_FOUND.into_response(),
        Some(ApiAccess::Listen) => return fail(StatusCode::FORBIDDEN, "exporting needs control access"),
        Some(ApiAccess::Control) => {}
    }
    if ctx.db.is_disabled() {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "the call store is off");
    }
    let Ok(table) = table.trim_end_matches(".csv").parse::<DataTable>() else { return fail(StatusCode::NOT_FOUND, "no such table") };
    let system = match q.get("system").map(|v| v.trim()).filter(|v| !v.is_empty()) {
        None => None,
        Some(v) => match v.parse::<u32>() {
            Ok(id) => Some(i64::from(id)),
            Err(_) => return fail(StatusCode::BAD_REQUEST, "system must be a system's id"),
        },
    };
    let ctx2 = ctx.clone();
    let names = match tokio::task::spawn_blocking(move || ctx2.db.system_names()).await {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &e),
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let name = match system {
        None => None,
        Some(id) => match names.iter().find(|(i, _)| *i == id) {
            Some((_, n)) => Some(n.clone()),
            None => return fail(StatusCode::NOT_FOUND, "no such system in the store"),
        },
    };
    // (Read a page at a time off the async threads, and sent as it's read.)
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, String>>(4);
    let ctx2 = ctx.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = write_csv(&ctx2, table, &names, system, &mut |chunk| tx.blocking_send(Ok(chunk)).is_ok()) {
            let _ = tx.blocking_send(Err(e));
        }
    });
    // (The first chunk, before the answer's status: a table that can't be read is an error, not an empty file.)
    let first = match rx.recv().await {
        Some(Ok(c)) => c,
        Some(Err(e)) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &e),
        None => return fail(StatusCode::INTERNAL_SERVER_ERROR, "the export stopped"),
    };
    let rest = futures_util::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await? {
            Ok(chunk) => Some((Ok::<_, std::io::Error>(chunk), rx)),
            Err(e) => {
                log::warn!("Export: {e}");
                Some((Err(std::io::Error::other(e)), rx))
            }
        }
    });
    let body = Body::from_stream(futures_util::StreamExt::chain(futures_util::stream::once(async move { Ok(first) }), rest));
    let file = file_name(name.as_deref(), table, Local::now());
    ([(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}\""))], body).into_response()
}

/// Table `table` of system `system` (none: every system's; `names`: the
/// store's systems) as CSV, handed to `emit` a chunk at a time (it says
/// false once no one is reading).
fn write_csv(ctx: &Ctx, table: DataTable, names: &[(i64, String)], system: Option<i64>, emit: &mut dyn FnMut(Vec<u8>) -> bool) -> Result<(), String> {
    if matches!(table, DataTable::Radios | DataTable::HeardTalkgroups) {
        let mut out = Vec::new();
        registry_csv(ctx, table, names, system, &mut out)?;
        emit(out);
        return Ok(());
    }
    let mut after: Option<Vec<V>> = None;
    let mut first = true;
    loop {
        let ExportPage { cols, rows, next } = ctx.db.export(table, system, after.as_deref(), PAGE)?;
        let mut out = Vec::new();
        if first {
            line(&mut out, cols.iter().map(|(c, _)| c.to_string()));
            first = false;
        }
        for r in &rows {
            line(&mut out, r.iter().zip(cols.iter()).map(|(v, (_, cell))| value(v, *cell)));
        }
        if !emit(out) {
            return Ok(());
        }
        match next {
            Some(n) => after = Some(n),
            None => return Ok(()),
        }
    }
}

/// The radio registry's radios or talkgroups heard, as CSV: the config's
/// systems' from memory, those gone from it as stored.
fn registry_csv(ctx: &Ctx, table: DataTable, names: &[(i64, String)], system: Option<i64>, out: &mut Vec<u8>) -> Result<(), String> {
    let radios = table == DataTable::Radios;
    if radios {
        line(out, ["system", "unit", "name", "first_heard", "last_heard", "last_transmitted", "transmissions", "transmit_s", "affiliated_talkgroup", "affiliated_at", "registered", "talkgroups"].map(String::from));
    } else {
        line(out, ["system", "talkgroup", "name", "first_heard", "last_heard", "calls", "seconds", "encrypted_calls"].map(String::from));
    }
    let mut systems: Vec<&(i64, String)> = names.iter().filter(|(id, _)| system.is_none_or(|s| s == *id)).collect();
    systems.sort_by(|a, b| a.1.cmp(&b.1));
    for (id, name) in systems {
        // (A copy: the names are looked up with the registry's lock let go.)
        let held = ctx.shared.radio.lock().unwrap().systems.get(name).cloned();
        let Some(s) = held.map_or_else(|| ctx.db.registry_of(*id), |s| Ok(Some(s)))? else { continue };
        let book = ctx.shared.names.lock().unwrap();
        if radios {
            let mut units: Vec<_> = s.units.iter().collect();
            units.sort_by_key(|(u, _)| **u);
            for (u, r) in units {
                let tgs: Vec<String> = r.tgs.iter().map(|(tg, n)| format!("{tg}:{n}")).collect();
                let reg = r.reg.map_or("", |on| if on { "registered" } else { "deregistered" });
                line(out, [name.clone(), u.to_string(), book.unit(name, *u).unwrap_or_default(), time_s(r.first), time_s(r.last), time_s(r.last_tx), r.tx.to_string(), format!("{:.1}", r.tx_secs), r.aff.map_or(String::new(), |t| t.to_string()), time_s(r.aff_t), reg.into(), tgs.join(" ")]);
            }
        } else {
            let mut tgs: Vec<_> = s.tgs.iter().collect();
            tgs.sort_by_key(|(t, _)| **t);
            for (t, g) in tgs {
                let alpha = book.tg(name, *t).map(|i| i.alpha_tag).filter(|a| !a.is_empty()).unwrap_or_else(|| g.name.clone());
                line(out, [name.clone(), t.to_string(), alpha, time_s(g.first), time_s(g.last), g.calls.to_string(), format!("{:.1}", g.secs), g.enc.to_string()]);
            }
        }
    }
    Ok(())
}

/// One CSV line of `cells`.
fn line(out: &mut Vec<u8>, cells: impl IntoIterator<Item = String>) {
    let cells: Vec<String> = cells.into_iter().map(|c| quote(&c)).collect();
    out.extend_from_slice(cells.join(",").as_bytes());
    out.extend_from_slice(b"\r\n");
}

/// A CSV cell: quoted when it holds a comma, a quote or a line break.
fn quote(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// A stored value as its cell says to write it.
fn value(v: &V, cell: Cell) -> String {
    match (v, cell) {
        (V::Null, _) => String::new(),
        (V::Int(t), Cell::Ms) => Local.timestamp_millis_opt(*t).single().map_or(String::new(), |d| d.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
        (V::Int(t), Cell::S) => time_s(*t),
        (V::Int(b), Cell::Bool) => (*b != 0).to_string(),
        (V::Bool(b), _) => b.to_string(),
        (V::Int(i), _) => i.to_string(),
        (V::Real(r), _) => r.to_string(),
        (V::Text(t), _) => t.clone(),
    }
}

/// Unix s as a local time (0: never, empty).
fn time_s(t: i64) -> String {
    if t <= 0 {
        return String::new();
    }
    Local.timestamp_opt(t, 0).single().map_or(String::new(), |d| d.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// The config's systems: id → short name (trunked and conventional).
fn config_systems(ctx: &Ctx) -> Vec<(i64, String)> {
    let cfg = ctx.config.lock().unwrap();
    cfg.systems.iter().map(|s| (i64::from(s.id), s.short_name.clone())).chain(cfg.conventional.iter().map(|c| (i64::from(c.id), c.short_name.clone()))).collect()
}

/// `dataSummary`: every system the store has and how much of each it keeps.
pub fn summary(ctx: &Ctx, id: u64) -> ToClient {
    let rows = match ctx.db.data_systems() {
        Ok(r) => r,
        Err(e) => return ToClient::DataSummary { id, systems: Vec::new(), error: Some(e) },
    };
    let config = config_systems(ctx);
    let systems = rows
        .into_iter()
        .map(|(sid, short_name, mut counts)| {
            let in_config = config.iter().any(|(i, _)| *i == sid);
            let held = ctx.shared.radio.lock().unwrap().systems.get(&short_name).map(|s| (s.units.len(), s.tgs.len()));
            // (One gone from the config: its registry as stored, not in memory.)
            let (units, tgs) = held.or_else(|| (!in_config).then(|| ctx.db.registry_of(sid).ok().flatten().map(|s| (s.units.len(), s.tgs.len()))).flatten()).unwrap_or_default();
            (counts.radios, counts.heard_talkgroups) = (units as i64, tgs as i64);
            DataSystem { id: sid as u32, short_name, in_config, counts }
        })
        .collect();
    ToClient::DataSummary { id, systems, error: None }
}

/// `deleteData`: `kinds` of system `system`'s data (none: every system's),
/// from what the recorder holds (first, so none of it is saved again), then
/// the store, then the live list and what the interfaces were told. A system gone from the config that has all of its data
/// deleted is forgotten by the store too.
pub fn delete(ctx: &Ctx, id: u64, kinds: &[DataKind], system: Option<u32>) -> ToClient {
    let answer = |deleted: DataCounts, error: Option<String>| ToClient::DataDeleted { id, deleted, error };
    if ctx.db.is_disabled() {
        return answer(DataCounts::default(), Some("the call store is off".into()));
    }
    let names = match ctx.db.system_names() {
        Ok(n) => n,
        Err(e) => return answer(DataCounts::default(), Some(e)),
    };
    let system_id = system.map(i64::from);
    let name = match system_id {
        None => None,
        Some(sid) => match names.iter().find(|(i, _)| *i == sid) {
            Some((_, n)) => Some(n.clone()),
            None => return answer(DataCounts::default(), Some(format!("no system {sid} in the store"))),
        },
    };
    let config = config_systems(ctx);
    let in_config = system_id.is_some_and(|sid| config.iter().any(|(i, _)| *i == sid));
    let mine = |n: &str| name.as_deref().is_none_or(|s| s == n);
    let has = |k: DataKind| kinds.contains(&k);
    let mut n = DataCounts::default();

    if has(DataKind::TalkerAliases) || has(DataKind::Registry) {
        // (A system gone from the config: its registry isn't in memory, so counted as stored.)
        if let (true, false, Some(sid)) = (has(DataKind::Registry), in_config, system_id) {
            if let Ok(Some(s)) = ctx.db.registry_of(sid) {
                (n.radios, n.heard_talkgroups) = (s.units.len() as i64, s.tgs.len() as i64);
            }
        }
        let (units, tgs) = runtime::forget(ctx, name.as_deref(), has(DataKind::TalkerAliases), has(DataKind::Registry));
        n.radios += units as i64;
        n.heard_talkgroups += tgs as i64;
    }
    if has(DataKind::DashboardHistory) {
        let patterns = match &name {
            Some(s) => vec![system_key(s), format!("{}/*", system_key(s))],
            None => vec!["*".to_string()],
        };
        ctx.series.lock().unwrap().forget(&patterns);
    }
    let forget = system_id.is_some() && !in_config && DataKind::ALL.iter().all(|k| has(*k));
    let stored = ctx.db.delete(kinds, system_id, forget, crate::clock::unix_ms() as i64);
    // (People make talkgroups, the recording never: told to the recording and the interfaces once the store's are gone.)
    if has(DataKind::Talkgroups) {
        let held: Vec<String> = ctx.talkgroups.lock().unwrap().keys().filter(|k| mine(k)).cloned().collect();
        for s in held {
            if let Err(e) = runtime::set_talkgroups(ctx, &s, Vec::new()) {
                log::warn!("Deleting {s}'s talkgroups: {e}");
            }
        }
    }
    if has(DataKind::Calls) {
        let cfg = ctx.config.lock().unwrap().clone();
        let filed: Vec<String> = name.as_deref().map(|s| runtime::systems_for(&cfg, s)).unwrap_or_default();
        ctx.recent.lock().unwrap().retain(|c| name.is_some() && !filed.contains(&c.record.short_name));
        publish(&ctx.hub, runtime::history(ctx));
    }
    if has(DataKind::TalkerAliases) {
        for (_, s) in config.iter().filter(|(_, s)| mine(s)) {
            publish(&ctx.hub, ToClient::Units { system: s.clone(), csv: String::new() });
        }
    }
    let what = kinds.iter().map(|k| format!("{k:?}")).collect::<Vec<_>>().join(", ");
    log::info!("Deleted from the store ({}): {what}", name.as_deref().unwrap_or("every system"));
    match stored {
        Ok(d) => {
            (n.calls, n.talkgroups, n.talker_aliases, n.history_series) = (d.calls, d.talkgroups, d.talker_aliases, d.history_series);
            answer(n, None)
        }
        Err(e) => answer(n, Some(format!("The store couldn't delete it all: {e}"))),
    }
}

/// `compactData`: the store given back the space deleted rows left.
pub fn compact(ctx: &Ctx, id: u64) -> ToClient {
    let t = std::time::Instant::now();
    match ctx.db.compact() {
        Ok((before_bytes, after_bytes)) => {
            log::info!("Call store: compacted from {} to {} MB in {:.1} s", before_bytes / 1_000_000, after_bytes / 1_000_000, t.elapsed().as_secs_f64());
            ToClient::DataCompacted { id, before_bytes, after_bytes, error: None }
        }
        Err(e) => {
            log::warn!("Call store: compacting failed: {e}");
            ToClient::DataCompacted { id, before_bytes: 0, after_bytes: 0, error: Some(e) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TestDir;

    /// `GET path` on a server of `ctx` as `door`: the status, the file it's saved as, and the body.
    async fn fetch(ctx: &Arc<Ctx>, door: Door, path: &str) -> (u16, String, String) {
        let app = crate::server::api(axum::Router::new()).layer(Extension(door)).with_state(ctx.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}{path}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
            let mut r = agent.get(&url).call().unwrap();
            let file = r.headers().get("content-disposition").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            (r.status().as_u16(), file, r.body_mut().read_to_string().unwrap())
        })
        .await
        .unwrap()
    }

    /// A system's tables export as CSV files named for it, and deleting its
    /// data clears the store and what the recorder holds (the live calls,
    /// talkgroups, registry, aliases); every system's gone, the summary has
    /// nothing left to count.
    #[tokio::test]
    async fn a_systems_data_exports_and_deletes() {
        let dir = TestDir::new("data-tools-ctx");
        let ctx = runtime::test_ctx(&dir.join("cap"));
        let sid = ctx.db.system_id("ohmarcs").unwrap();
        for i in 0..3 {
            runtime::index_call(&ctx, &format!("ohmarcs/{i}"), &crate::db::row::tests::record(9179, 1_700_000_000_000 + i * 1000), true, true);
        }
        runtime::set_talkgroups(&ctx, "ohmarcs", trunk_core::trunk::parse_rows("Decimal,Alpha Tag\n9179,\"FD, Disp\"\n")).unwrap();
        let mut heard = trunk_app::stats::radio::SystemRadio::default();
        heard.units.insert(9001644, trunk_app::stats::radio::Unit { first: 1_700_000_000, last: 1_700_000_100, tx: 3, tgs: vec![(9179, 3)], ..Default::default() });
        ctx.shared.radio.lock().unwrap().put("ohmarcs", heard);
        ctx.db.flush().unwrap();

        let (code, file, body) = fetch(&ctx, Door::Builtin, "/api/v1/export/calls").await;
        assert_eq!(code, 200, "{body}");
        assert!(file.starts_with("attachment; filename=\"all-calls-") && file.ends_with(".csv\""), "{file}");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 4, "a header and each call: {body}");
        assert!(lines[0].starts_with("system,start,stop,length_s,talkgroup,talkgroup_name") && lines[1].contains(",\"FD, Disp\","), "{body}");
        let (code, file, body) = fetch(&ctx, Door::Builtin, &format!("/api/v1/export/radios?system={sid}")).await;
        assert_eq!(code, 200);
        assert!(file.contains("filename=\"ohmarcs-radios-"), "{file}");
        assert!(body.lines().nth(1).is_some_and(|l| l.starts_with("ohmarcs,9001644,") && l.ends_with(",9179:3")), "{body}");
        assert_eq!(fetch(&ctx, Door::Builtin, "/api/v1/export/nope").await.0, 404);
        assert_eq!(fetch(&ctx, Door::Builtin, "/api/v1/export/calls?system=999").await.0, 404);
        assert_eq!(fetch(&ctx, Door::Builtin, "/api/v1/export/calls?system=x").await.0, 400);

        let ToClient::DataSummary { systems, error: None, .. } = summary(&ctx, 1) else { panic!() };
        assert_eq!(systems.iter().map(|s| (s.short_name.as_str(), s.in_config, s.counts.calls, s.counts.talkgroups, s.counts.radios)).collect::<Vec<_>>(), [("ohmarcs", true, 3, 1, 1)]);
        let ToClient::DataDeleted { deleted, error, .. } = delete(&ctx, 2, DataKind::ALL, Some(sid as u32)) else { panic!() };
        assert_eq!((deleted.calls, deleted.talkgroups, deleted.radios, error), (3, 1, 1, None));
        assert!(ctx.recent.lock().unwrap().is_empty() && ctx.talkgroups.lock().unwrap().is_empty() && ctx.shared.radio.lock().unwrap().systems.is_empty());
        let ToClient::DataSummary { systems, .. } = summary(&ctx, 3) else { panic!() };
        assert_eq!(systems.iter().map(|s| (s.in_config, s.counts)).collect::<Vec<_>>(), [(true, DataCounts::default())], "still the config's, with nothing kept");
        let (_, _, body) = fetch(&ctx, Door::Builtin, "/api/v1/export/calls").await;
        assert_eq!(body.lines().count(), 1, "the header alone: {body}");
    }

    #[test]
    fn file_names_say_what_and_when() {
        let at = Local.with_ymd_and_hms(2026, 11, 7, 14, 32, 5).unwrap();
        assert_eq!(file_name(None, DataTable::Calls, at), "all-calls-2026-11-07-143205.csv");
        assert_eq!(file_name(Some("dcfd"), DataTable::HeardTalkgroups, at), "dcfd-heard-talkgroups-2026-11-07-143205.csv");
        assert_eq!(file_name(Some("a b/c"), DataTable::TalkerAliases, at), "a_b_c-talker-aliases-2026-11-07-143205.csv");
    }

    #[test]
    fn cells_are_quoted_when_they_must_be() {
        let mut out = Vec::new();
        line(&mut out, ["a".to_string(), "b,c".into(), "say \"hi\"".into(), "".into()]);
        assert_eq!(String::from_utf8(out).unwrap(), "a,\"b,c\",\"say \"\"hi\"\"\",\r\n");
        assert_eq!(value(&V::Int(1), Cell::Bool), "true");
        assert_eq!(value(&V::Bool(false), Cell::Bool), "false");
        assert_eq!(value(&V::Null, Cell::Ms), "");
        assert_eq!(time_s(0), "");
    }
}
