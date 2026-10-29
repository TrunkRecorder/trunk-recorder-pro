//! Reading the call store, the same for every backend: which calls a query
//! wants, the paging over a backend's rows, and the live list merged in.
//! Paging is by whole milliseconds: a page never ends between two calls that
//! started in the same millisecond, so `before` (the oldest start on the
//! page) is a cursor that loses and repeats nothing.

use serde_json::Value;

pub use trunk_app::protocol::CallsPage as Page;

/// Which calls a query wants. Every field narrows the result; the default
/// is every call.
#[derive(Default, Clone, Debug)]
pub struct CallsFilter {
    /// Systems (`short_name`) to take calls from; empty means any.
    pub systems: Vec<String>,
    pub talkgroup: Option<u32>,
    /// Only calls this radio transmitted in.
    pub unit: Option<i64>,
    /// Calls starting at or after this time.
    pub from_ms: Option<i64>,
    /// Calls starting before this time.
    pub to_ms: Option<i64>,
    /// Calls starting before this time: the cursor from the previous page.
    pub before_ms: Option<i64>,
    pub limit: usize,
}

/// The columns a call entry is made from, then `id` and `start_ms` for paging.
pub const COLUMNS: &str = "path, record, audio, json, id, start_ms";

/// A parameter of a query, in either dialect.
#[derive(Clone, Debug)]
pub enum Param {
    Int(i64),
    Text(String),
}

/// A query's conditions and their parameters. Clauses mark each parameter
/// with `?`; [`Where::select`] numbers them `$1, $2…` for PostgreSQL.
#[derive(Clone, Default)]
pub struct Where {
    clauses: Vec<String>,
    pub params: Vec<Param>,
}

impl Where {
    fn and(&mut self, clause: &str, param: Param) {
        self.clauses.push(clause.to_string());
        self.params.push(param);
    }

    pub fn of(f: &CallsFilter) -> Where {
        let mut w = Where::default();
        if !f.systems.is_empty() {
            let marks = vec!["?"; f.systems.len()].join(",");
            w.clauses.push(format!("short_name in ({marks})"));
            w.params.extend(f.systems.iter().map(|s| Param::Text(s.clone())));
        }
        if let Some(tg) = f.talkgroup {
            w.and("talkgroup = ?", Param::Int(i64::from(tg)));
        }
        if let Some(unit) = f.unit {
            w.and("id in (select call_id from call_transmissions where unit = ?)", Param::Int(unit));
        }
        if let Some(from) = f.from_ms {
            w.and("start_ms >= ?", Param::Int(from));
        }
        if let Some(to) = f.to_ms {
            w.and("start_ms < ?", Param::Int(to));
        }
        if let Some(before) = f.before_ms {
            w.and("start_ms < ?", Param::Int(before));
        }
        w
    }

    /// The select of a page: `columns` of the calls wanted, newest first,
    /// and a last parameter for the limit. `dollar`: PostgreSQL's numbered
    /// placeholders.
    pub fn select(&self, columns: &str, dollar: bool) -> String {
        let cond = if self.clauses.is_empty() { String::new() } else { format!(" where {}", self.clauses.join(" and ")) };
        let sql = format!("select {columns} from calls{cond} order by start_ms desc, id desc limit ?");
        if dollar { numbered(&sql) } else { sql }
    }
}

/// `sql` with each `?` numbered `$1, $2…`, as PostgreSQL has its parameters.
pub fn numbered(sql: &str) -> String {
    let mut n = 0;
    sql.chars()
        .map(|c| {
            if c == '?' {
                n += 1;
                format!("${n}")
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// A fetched call: its entry and where it sits in the order.
pub struct Fetched {
    pub entry: Value,
    pub id: i64,
    pub start_ms: i64,
}

/// A backend's rows for a [`Where`]: up to `limit`, newest first, and
/// whether one more matched beyond them.
pub type Fetch<'a> = dyn FnMut(&Where, usize) -> Result<(Vec<Fetched>, bool), String> + 'a;

/// The page `f` asks for, over a backend's `fetch`: the `limit` newest,
/// plus every further call sharing the oldest one's start.
pub fn paged(f: &CallsFilter, fetch: &mut Fetch) -> Result<Page, String> {
    let limit = f.limit.max(1);
    let w = Where::of(f);
    let (mut rows, extra) = fetch(&w, limit)?;
    let asked = f.before_ms.unwrap_or(0);
    let Some((id, oldest)) = rows.last().map(|r| (r.id, r.start_ms)) else {
        return Ok(Page { entries: Vec::new(), more: false, before: asked });
    };
    let mut more = false;
    if extra {
        // The oldest start is shared with rows not yet taken: take them all.
        let mut tail = w.clone();
        tail.and("start_ms = ?", Param::Int(oldest));
        tail.and("id < ?", Param::Int(id));
        rows.extend(fetch(&tail, usize::MAX / 2)?.0);
        let mut below = w.clone();
        below.and("start_ms < ?", Param::Int(oldest));
        more = !fetch(&below, 1)?.0.is_empty();
    }
    let before = rows.last().map_or(asked, |r| r.start_ms);
    Ok(Page { entries: rows.into_iter().map(|r| r.entry).collect(), more, before })
}

/// Whether call entry `e` is one `f` wants (its time, system, talkgroup,
/// radio): the same filter as [`Where::of`]'s, for calls held in memory.
pub fn matches(e: &Value, f: &CallsFilter) -> bool {
    let r = &e["record"];
    let start = start_of(e);
    let system = r["short_name"].as_str().unwrap_or_default();
    (f.systems.is_empty() || f.systems.iter().any(|s| s == system))
        && f.talkgroup.is_none_or(|tg| r["talkgroup"].as_u64() == Some(u64::from(tg)))
        && f.unit.is_none_or(|u| r["srcList"].as_array().is_some_and(|l| l.iter().any(|t| t["src"].as_i64() == Some(u))))
        && f.from_ms.is_none_or(|t| start >= t)
        && f.to_ms.is_none_or(|t| start < t)
        && f.before_ms.is_none_or(|t| start < t)
}

fn start_of(e: &Value) -> i64 {
    e["record"]["start_time_ms"].as_i64().unwrap_or(0)
}

/// Newest first, then by path: the order every page is in.
fn newest_first(v: &mut [Value]) {
    v.sort_by_key(|e| std::cmp::Reverse((start_of(e), e["path"].as_str().unwrap_or_default().to_string())));
}

/// A page of `f` from calls held in memory (the live list, with the store
/// off): paged as [`paged`] pages, equal starts taken whole.
pub fn page_of(calls: &[Value], f: &CallsFilter) -> Page {
    let mut v: Vec<Value> = calls.iter().filter(|e| matches(e, f)).cloned().collect();
    newest_first(&mut v);
    let limit = f.limit.max(1);
    let mut take = v.len().min(limit);
    while take > 0 && take < v.len() && start_of(&v[take]) == start_of(&v[take - 1]) {
        take += 1;
    }
    let more = take < v.len();
    v.truncate(take);
    let before = v.last().map_or(f.before_ms.unwrap_or(0), start_of);
    Page { entries: v, more, before }
}

/// `stored` (a page from the store) with the live list's calls that belong
/// on it merged in: those `f` wants that the store hasn't committed yet,
/// within the page's span (from its oldest start when more remain). A call
/// in both is the live copy. The cursor is the stored page's, so paging on
/// loses and repeats nothing.
pub fn with_live(stored: Page, live: &[Value], f: &CallsFilter) -> Page {
    let floor = if stored.more { Some(stored.before) } else { None };
    let mut seen = std::collections::HashSet::new();
    let mut v: Vec<Value> = live
        .iter()
        .filter(|e| matches(e, f) && floor.is_none_or(|b| start_of(e) >= b))
        .cloned()
        .chain(stored.entries)
        .filter(|e| seen.insert(e["path"].as_str().unwrap_or_default().to_string()))
        .collect();
    newest_first(&mut v);
    let before = if stored.more { stored.before } else { v.last().map_or(stored.before, start_of) };
    Page { entries: v, more: stored.more, before }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::row::{call_row, entry};
    use crate::db::{Db, Write};
    use std::time::Duration;

    /// Each backend with 12 calls: talkgroup 2 (odd i) and 1 (even i),
    /// starts i*1000, except call 6 shares call 5's start 5000 and call 12
    /// has start 0 (clock off); unit 77 talks on calls 3 (talkgroup 2) and 8
    /// (talkgroup 1).
    fn seeded(name: &str) -> Vec<Db> {
        let dbs = Db::for_each_backend(name, Duration::from_secs(1));
        for db in &dbs {
            for i in 1..=12u32 {
                let start = match i {
                    6 => 5000,
                    12 => 0,
                    _ => i as i64 * 1000,
                };
                let mut r = crate::db::row::tests::record(1 + i % 2, start);
                if i == 3 || i == 8 {
                    r.src_list[0].src = 77;
                }
                db.send(Write::Call { row: call_row(&format!("s/{i}"), &r, &r.to_json(), true, true, None, 0), live: true });
            }
            db.flush().unwrap();
        }
        dbs
    }

    fn paths(v: &[Value]) -> Vec<&str> {
        v.iter().map(|e| e["path"].as_str().unwrap()).collect()
    }

    fn page(db: &Db, f: CallsFilter) -> Page {
        db.page(&f).unwrap()
    }

    fn newest(limit: usize) -> CallsFilter {
        CallsFilter { limit, ..Default::default() }
    }

    fn older(before: i64, limit: usize) -> CallsFilter {
        CallsFilter { before_ms: Some(before), limit, ..Default::default() }
    }

    #[test]
    fn newest_is_newest_first_and_takes_equal_starts_whole() {
        for db in seeded("newest") {
            assert_eq!(paths(&page(&db, newest(3)).entries), ["s/11", "s/10", "s/9"], "{}", db.kind());
            // The sixth newest is s/6 at 5000, which s/5 shares.
            assert_eq!(paths(&page(&db, newest(6)).entries), ["s/11", "s/10", "s/9", "s/8", "s/7", "s/6", "s/5"], "{}", db.kind());
        }
    }

    /// Every page of `f` from the start, the paths in the order they came.
    fn every_page(db: &Db, f: CallsFilter) -> Vec<String> {
        let mut seen = Vec::new();
        let mut f = f;
        loop {
            let p = page(db, f.clone());
            assert!(!p.entries.is_empty() || !p.more, "an empty page says no more");
            seen.extend(paths(&p.entries).into_iter().map(str::to_string));
            if !p.more {
                break;
            }
            f.before_ms = Some(p.before);
        }
        seen
    }

    #[test]
    fn paging_returns_every_call_exactly_once() {
        for db in seeded("paging") {
            let mut want: Vec<String> = (1..=12).map(|i| format!("s/{i}")).collect();
            want.sort();
            for limit in 1..=12 {
                let mut seen = every_page(&db, newest(limit));
                seen.sort();
                assert_eq!(seen, want, "limit {limit} ({})", db.kind());
            }
            // Talkgroup 2 (the odd calls), two to a page.
            let mut seen = every_page(&db, CallsFilter { talkgroup: Some(2), limit: 2, ..Default::default() });
            seen.sort();
            let mut want: Vec<String> = (1..=12).filter(|i| i % 2 == 1).map(|i| format!("s/{i}")).collect();
            want.sort();
            assert_eq!(seen, want);
        }
    }

    #[test]
    fn an_empty_page_says_the_before_asked_or_0() {
        for db in Db::for_each_backend("empty-before", Duration::from_secs(1)) {
            let p = page(&db, CallsFilter::default());
            assert_eq!((p.entries.len(), p.more, p.before), (0, false, 0));
            assert_eq!(page(&db, older(1234, 10)).before, 1234, "the before asked");
        }
    }

    #[test]
    fn equal_starts_are_taken_whole_and_a_zero_start_pages_last() {
        for db in seeded("whole-ms") {
            assert_eq!(paths(&page(&db, older(7001, 1)).entries), ["s/7"], "7000 is alone");
            let p = page(&db, older(5001, 1));
            assert_eq!((p.entries.len(), p.before, p.more), (2, 5000, true), "5000 is shared, both come ({})", db.kind());
            let p = page(&db, older(1, 10));
            assert_eq!((paths(&p.entries), p.more), (vec!["s/12"], false));
        }
    }

    #[test]
    fn query_filters_by_talkgroup_unit_range_and_system() {
        for db in seeded("query") {
            let f = |tg, unit, from, to, systems: &[&str]| CallsFilter { systems: systems.iter().map(|s| s.to_string()).collect(), talkgroup: tg, unit, from_ms: from, to_ms: to, before_ms: None, limit: 100 };
            let n = |f: CallsFilter| page(&db, f).entries.len();
            assert_eq!(n(f(Some(1), None, None, None, &[])), 6, "{}", db.kind());
            assert_eq!(n(f(None, Some(77), None, None, &[])), 2);
            assert_eq!(n(f(Some(2), Some(77), None, None, &[])), 1);
            assert_eq!(n(f(None, None, Some(3000), Some(6000), &[])), 4, "3000, 4000, 5000, 5000");
            assert_eq!(n(f(None, None, None, None, &["ohmarcs"])), 12);
            assert_eq!(n(f(None, None, None, None, &["other"])), 0);
            let p = page(&db, CallsFilter { limit: 5, ..f(None, None, None, None, &[]) });
            assert_eq!((p.more, p.entries.len()), (true, 5));
        }
    }

    /// The live list's calls not committed yet join the page they belong
    /// on, and paging on from it still loses and repeats nothing.
    #[test]
    fn live_calls_join_the_page_they_belong_on() {
        for db in seeded("live") {
            let live_entry = |path: &str, tg: u32, start: i64| {
                let r = crate::db::row::tests::record(tg, start);
                entry(path, &r.to_json(), true, true)
            };
            // A call newer than all, one in the middle (a long call finished late), one stored already.
            let live = [live_entry("live/new", 1, 20_000), live_entry("live/mid", 2, 6500), live_entry("s/11", 2, 11_000)];
            let mut seen = Vec::new();
            let mut f = CallsFilter { limit: 4, ..Default::default() };
            loop {
                let p = with_live(page(&db, f.clone()), &live, &f);
                seen.extend(paths(&p.entries).into_iter().map(str::to_string));
                if !p.more {
                    break;
                }
                f.before_ms = Some(p.before);
            }
            let mut want: Vec<String> = (1..=12).map(|i| format!("s/{i}")).chain(["live/new".into(), "live/mid".into()]).collect();
            let mut got = seen.clone();
            got.sort();
            want.sort();
            assert_eq!(got, want, "every call once: {seen:?} ({})", db.kind());
            assert_eq!(seen[0], "live/new", "newest first");
            // The live list alone (the store off), paged the same way, by talkgroup.
            let p = page_of(&live, &CallsFilter { talkgroup: Some(2), limit: 1, ..Default::default() });
            assert_eq!((paths(&p.entries), p.more, p.before), (vec!["s/11"], true, 11_000));
        }
    }

    #[test]
    fn one_by_path_and_known() {
        for db in seeded("one") {
            assert_eq!(db.one("s/4").unwrap().unwrap()["record"]["talkgroup"], 1, "{}", db.kind());
            assert!(db.one("s/99").unwrap().is_none());
            assert!(db.known("s/4").unwrap() && !db.known("s/99").unwrap());
        }
    }

    #[test]
    fn placeholders_are_numbered_for_postgres() {
        let w = Where::of(&CallsFilter { systems: vec!["a".into(), "b".into()], talkgroup: Some(1), ..Default::default() });
        let sql = w.select("path", true);
        assert!(sql.contains("short_name in ($1,$2)") && sql.contains("talkgroup = $3") && sql.ends_with("limit $4"), "{sql}");
        assert!(w.select("path", false).ends_with("limit ?"));
    }
}
