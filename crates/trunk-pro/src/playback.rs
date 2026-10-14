//! The Playback page's search of the recordings folder: calls from any time
//! back, by system, talkgroup, radio, words in a name, and "talkgroups not in
//! the talkgroup file" (what's this new one?). Unlike `older_calls` (the Listen
//! page's last 24 hours, day folders of the default layout) it walks whatever
//! folders there are, so a system with its own filename format is found too.
//! There is no index: each query lists the folders and reads the call JSONs
//! in its time span, newest first, up to [`MAX_READ`].

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

/// Call records read at most per query (`truncated` tells the page when it stopped).
const MAX_READ: usize = 30_000;
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;
/// Folders deeper than this under the recordings folder aren't searched.
const MAX_DEPTH: usize = 8;
/// Talkgroups and radios listed beside the calls.
const MAX_TALKGROUPS: usize = 2000;
const MAX_UNITS: usize = 60;

/// What a request asked for.
struct Query {
    system: Option<String>,
    talkgroup: Option<u32>,
    unit: Option<u32>,
    /// Lower case; matches a talkgroup's number or names, or a radio's alias.
    text: String,
    new_only: bool,
    from_ms: i64,
    to_ms: i64,
    /// The next page: calls that started before this (Unix ms).
    before_ms: Option<i64>,
    limit: usize,
}

impl Query {
    fn from_json(v: &Value, now_ms: i64) -> Query {
        let some_str = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        Query {
            system: some_str("system"),
            talkgroup: v["talkgroup"].as_u64().map(|t| t as u32),
            unit: v["unit"].as_u64().map(|u| u as u32),
            text: some_str("text").unwrap_or_default().to_lowercase(),
            new_only: v["newOnly"].as_bool() == Some(true),
            from_ms: v["from"].as_i64().unwrap_or(now_ms - 24 * 3600 * 1000),
            to_ms: v["to"].as_i64().unwrap_or(now_ms + 60_000),
            before_ms: v["before"].as_i64(),
            limit: (v["limit"].as_u64().unwrap_or(DEFAULT_LIMIT as u64) as usize).clamp(1, MAX_LIMIT),
        }
    }
}

/// A call's system: its record's short name, else the folder it is in.
fn system_of(record: &Value, rel: &Path) -> String {
    record["short_name"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default())
}

/// A call file's start (Unix s) as its name has it (`<talkgroup>-<start>_<freq>`),
/// if that is a plausible time.
fn start_from_name(p: &Path) -> Option<i64> {
    let stem = p.file_stem()?.to_str()?;
    let s: i64 = stem.split_once('-')?.1.split(['_', '.', '-']).next()?.parse().ok()?;
    (1_000_000_000..4_000_000_000).contains(&s).then_some(s)
}

fn mtime_s(m: &fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64)
}

/// The call JSONs under `dir` that may be in [from_s, to_s]: (start, path).
/// A folder not touched since a day before `from_s` holds no later call.
fn list_calls(dir: &Path, system: Option<&str>, from_s: i64, to_s: i64) -> Vec<(i64, PathBuf)> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        let tops: Vec<PathBuf> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
        // The system's own folder, when there is one (a custom layout may put its calls anywhere).
        let own: Vec<&PathBuf> = tops.iter().filter(|p| system.is_some_and(|s| p.file_name().is_some_and(|n| n == s))).collect();
        let chosen: Vec<PathBuf> = if own.is_empty() { tops.clone() } else { own.into_iter().cloned().collect() };
        stack.extend(chosen.into_iter().map(|p| (p, 1)));
    }
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let old = e.metadata().is_ok_and(|m| mtime_s(&m) < from_s - 86_400);
                if depth < MAX_DEPTH && !old {
                    stack.push((p, depth + 1));
                }
            } else if p.extension().is_some_and(|x| x == "json") {
                let s = match start_from_name(&p) {
                    Some(s) => s,
                    None => e.metadata().map_or(0, |m| mtime_s(&m)),
                };
                // (An hour of slack: the name is the start, a record's time may differ.)
                if s >= from_s - 3600 && s <= to_s + 3600 {
                    out.push((s, p));
                }
            }
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out
}

#[derive(Default)]
struct TgStat {
    tag: String,
    description: String,
    group: String,
    calls: u32,
    encrypted: u32,
    seconds: f64,
    first_ms: i64,
    last_ms: i64,
}

#[derive(Default)]
struct UnitStat {
    alias: String,
    calls: u32,
    last_ms: i64,
}

/// The radios that spoke on a call, with an alias if the record has one.
fn units_of(record: &Value) -> Vec<(u32, String)> {
    record["srcList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let src = s["src"].as_u64().filter(|&u| u > 0)? as u32;
            let tag = ["tag", "tag_ota"].iter().filter_map(|k| s[*k].as_str()).find(|t| !t.is_empty()).unwrap_or("");
            Some((src, tag.to_string()))
        })
        .collect()
}

/// The `playbackResult` for `req` over the recordings in `dir`. `known(system,
/// talkgroup)`: whether the system's talkgroup file lists it.
pub fn query(dir: &Path, known: &dyn Fn(&str, u32) -> bool, req: &Value, now_ms: i64) -> Value {
    let q = Query::from_json(req, now_ms);
    let found = list_calls(dir, q.system.as_deref(), q.from_ms.div_euclid(1000), q.to_ms.div_euclid(1000));
    let first_page = q.before_ms.is_none();
    let mut tgs: HashMap<(String, u32), TgStat> = HashMap::new();
    let mut units: HashMap<(String, u32), UnitStat> = HashMap::new();
    let mut systems: std::collections::BTreeSet<String> = Default::default();
    let mut page: Vec<Value> = Vec::new();
    let (mut total, mut more, mut read, mut matched_ms) = (0usize, false, 0usize, (i64::MAX, 0i64));
    for (_, p) in &found {
        if read >= MAX_READ {
            break;
        }
        read += 1;
        let Some(record) = fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
        let start = record["start_time_ms"].as_i64().or_else(|| record["start_time"].as_i64().map(|s| s * 1000)).unwrap_or(0);
        if start < q.from_ms || start >= q.to_ms {
            continue;
        }
        let Ok(rel) = p.strip_prefix(dir) else { continue };
        let system = system_of(&record, rel);
        systems.insert(system.clone());
        if q.system.as_deref().is_some_and(|s| s != system) {
            continue;
        }
        let tg = record["talkgroup"].as_u64().unwrap_or(0) as u32;
        let radios = units_of(&record);
        // Words match the talkgroup's number or names, or a radio's alias.
        if !q.text.is_empty() {
            let hay = ["talkgroup_tag", "talkgroup_description", "talkgroup_group", "talkgroup_group_tag"].iter().filter_map(|k| record[*k].as_str()).collect::<Vec<_>>().join(" ").to_lowercase();
            let in_alias = radios.iter().any(|(_, a)| a.to_lowercase().contains(&q.text));
            if !(hay.contains(&q.text) || tg.to_string() == q.text || in_alias) {
                continue;
            }
        }
        let is_new = !known(&system, tg);
        let by_tg = q.talkgroup.is_none_or(|t| t == tg) && (!q.new_only || is_new);
        let by_unit = q.unit.is_none_or(|u| radios.iter().any(|(r, _)| *r == u));
        let encrypted = record["encrypted"].as_i64().unwrap_or(0) != 0;
        // Beside the calls: the talkgroups (as filtered by all but the talkgroup) and the radios (all but the radio).
        if first_page && by_unit && (tgs.len() < MAX_TALKGROUPS || tgs.contains_key(&(system.clone(), tg))) {
            let t = tgs.entry((system.clone(), tg)).or_default();
            if t.calls == 0 {
                t.last_ms = start;
                t.tag = record["talkgroup_tag"].as_str().unwrap_or("").to_string();
                t.description = record["talkgroup_description"].as_str().unwrap_or("").to_string();
                t.group = record["talkgroup_group"].as_str().unwrap_or("").to_string();
            }
            t.calls += 1;
            t.encrypted += encrypted as u32;
            t.seconds += record["call_length_ms"].as_f64().unwrap_or(0.0) / 1000.0;
            t.first_ms = if t.first_ms == 0 { start } else { t.first_ms.min(start) };
        }
        if first_page && by_tg {
            for (r, alias) in &radios {
                if units.len() < 4 * MAX_UNITS || units.contains_key(&(system.clone(), *r)) {
                    let u = units.entry((system.clone(), *r)).or_default();
                    if u.calls == 0 {
                        u.last_ms = start;
                    }
                    if u.alias.is_empty() {
                        u.alias = alias.clone();
                    }
                    u.calls += 1;
                }
            }
        }
        if !(by_tg && by_unit) {
            continue;
        }
        total += 1;
        matched_ms = (matched_ms.0.min(start), matched_ms.1.max(start));
        if q.before_ms.is_some_and(|b| start >= b) {
            continue;
        }
        if page.len() < q.limit {
            let audio = ["wav", "m4a"].iter().any(|x| p.with_extension(x).exists());
            page.push(json!({ "path": rel.with_extension("").to_string_lossy().replace('\\', "/"), "record": record, "audio": audio, "json": true }));
        } else {
            more = true;
        }
    }
    // (Files are read newest first by their names, records sorted by their own start: they agree but for a second.)
    page.sort_by_key(|e| std::cmp::Reverse(e["record"]["start_time_ms"].as_i64().unwrap_or(0)));
    let mut talkgroups: Vec<Value> = tgs
        .into_iter()
        .map(|((system, tg), t)| {
            json!({
                "system": system, "talkgroup": tg, "alphaTag": t.tag, "description": t.description, "group": t.group,
                "calls": t.calls, "encrypted": t.encrypted, "seconds": t.seconds.round(), "firstMs": t.first_ms, "lastMs": t.last_ms,
                "known": known(&system, tg),
            })
        })
        .collect();
    talkgroups.sort_by(|a, b| b["calls"].as_u64().cmp(&a["calls"].as_u64()));
    let mut radios: Vec<Value> = units.into_iter().map(|((system, unit), u)| json!({ "system": system, "unit": unit, "alias": u.alias, "calls": u.calls, "lastMs": u.last_ms })).collect();
    radios.sort_by(|a, b| b["calls"].as_u64().cmp(&a["calls"].as_u64()));
    radios.truncate(MAX_UNITS);
    json!({
        "type": "playbackResult",
        "id": req["id"],
        "from": q.from_ms,
        "to": q.to_ms,
        "before": q.before_ms,
        "entries": page,
        "more": more,
        "total": total,
        "truncated": read >= MAX_READ && found.len() > read,
        "systems": systems,
        "talkgroups": if first_page { json!(talkgroups) } else { Value::Null },
        "units": if first_page { json!(radios) } else { Value::Null },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, system: &str, name: &str, tg: u32, start_s: i64, units: &[u32], tag: &str) {
        let d = dir.join(system).join("2026/10/14");
        fs::create_dir_all(&d).unwrap();
        let src: Vec<Value> = units.iter().map(|u| json!({ "src": u, "tag": format!("Unit {u}") })).collect();
        let rec = json!({
            "short_name": system, "talkgroup": tg, "talkgroup_tag": tag, "talkgroup_description": "", "talkgroup_group": "", "talkgroup_group_tag": "",
            "start_time": start_s, "start_time_ms": start_s * 1000, "call_length_ms": 4000, "encrypted": 0, "srcList": src,
        });
        fs::write(d.join(format!("{name}.json")), rec.to_string()).unwrap();
        fs::write(d.join(format!("{name}.m4a")), b"x").unwrap();
    }

    #[test]
    fn finds_by_system_talkgroup_radio_and_newness() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-playback-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let t0 = 1_791_000_000;
        write(&dir, "metro", &format!("101-{}_851", t0), 101, t0, &[7], "Dispatch");
        write(&dir, "metro", &format!("101-{}_851", t0 + 60), 101, t0 + 60, &[8], "Dispatch");
        write(&dir, "metro", &format!("999-{}_852", t0 + 120), 999, t0 + 120, &[8, 9], "");
        write(&dir, "county", &format!("5-{}_853", t0 + 30), 5, t0 + 30, &[7], "Fire");
        let known = |s: &str, t: u32| !(s == "metro" && t == 999);
        let ask = |v: Value| query(&dir, &known, &v, (t0 + 600) * 1000);
        let base = json!({ "id": 1, "from": (t0 - 100) * 1000 });
        let with = |extra: Value| {
            let mut v = base.clone();
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            v
        };

        let all = ask(base.clone());
        assert_eq!(all["total"], 4);
        assert_eq!(all["entries"].as_array().unwrap().len(), 4);
        // Newest first; the path is the call without its extension.
        assert_eq!(all["entries"][0]["record"]["talkgroup"], 999);
        assert!(all["entries"][0]["path"].as_str().unwrap().starts_with("metro/2026/10/14/999-"));
        assert_eq!(all["entries"][0]["audio"], true);
        assert_eq!(all["systems"], json!(["county", "metro"]));
        assert_eq!(all["talkgroups"].as_array().unwrap().len(), 3);

        assert_eq!(ask(with(json!({ "system": "county" })))["total"], 1);
        assert_eq!(ask(with(json!({ "system": "metro", "talkgroup": 101 })))["total"], 2);
        assert_eq!(ask(with(json!({ "unit": 8 })))["total"], 2);
        assert_eq!(ask(with(json!({ "unit": 7 })))["total"], 2);
        assert_eq!(ask(with(json!({ "text": "fire" })))["total"], 1);
        assert_eq!(ask(with(json!({ "text": "unit 9" })))["total"], 1);

        // What's new: only talkgroups the file lacks; the list beside still shows its radios.
        let new = ask(with(json!({ "newOnly": true })));
        assert_eq!(new["total"], 1);
        assert_eq!(new["entries"][0]["record"]["talkgroup"], 999);
        assert_eq!(new["units"].as_array().unwrap().len(), 2);
        let flagged: Vec<bool> = all["talkgroups"].as_array().unwrap().iter().filter(|t| t["talkgroup"] == 999).map(|t| t["known"].as_bool().unwrap()).collect();
        assert_eq!(flagged, [false]);

        // The talkgroup list isn't narrowed by the talkgroup chosen (so you can pick another).
        assert_eq!(ask(with(json!({ "system": "metro", "talkgroup": 101 })))["talkgroups"].as_array().unwrap().len(), 2);

        // Time span, and pages.
        assert_eq!(ask(with(json!({ "from": (t0 + 50) * 1000 })))["total"], 2);
        let p1 = ask(with(json!({ "limit": 3 })));
        assert_eq!((p1["entries"].as_array().unwrap().len(), p1["more"].clone()), (3, json!(true)));
        let oldest = p1["entries"][2]["record"]["start_time_ms"].as_i64().unwrap();
        let p2 = ask(with(json!({ "limit": 3, "before": oldest })));
        assert_eq!((p2["entries"].as_array().unwrap().len(), p2["more"].clone(), p2["talkgroups"].clone()), (1, json!(false), Value::Null));
        let _ = fs::remove_dir_all(&dir);
    }
}
