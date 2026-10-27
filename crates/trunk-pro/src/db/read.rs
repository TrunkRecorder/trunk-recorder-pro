//! Reading the call store: the queries that turn rows back into the call
//! entries the protocol carries. Paging is by whole milliseconds: a page never
//! ends between two calls that started in the same millisecond, so `before`
//! (the oldest start on the page) is a cursor that loses and repeats nothing.

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params_from_iter};
use serde_json::Value;

use super::row::entry;

/// Which calls a [`query`] wants. Every field narrows the result; the default
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

/// One page of calls, newest first.
#[derive(Debug)]
pub struct Page {
    pub entries: Vec<Value>,
    /// Calls remain that started before the oldest on this page.
    pub more: bool,
    /// Where the next page starts: the oldest start on this page, or the
    /// `before` that was asked when the page is empty.
    pub before: i64,
}

/// The columns a call entry is made from, then `id` and `start_ms` for paging.
const COLUMNS: &str = "path, record, audio, json, id, start_ms";

/// The `limit` newest calls, newest first (`start_ms`, then `id`, descending).
pub fn newest(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<Value>> {
    let (rows, _) = fetch(conn, &Where::default(), limit)?;
    Ok(rows.into_iter().map(|r| r.entry).collect())
}

/// Up to `limit` calls that started before `before_ms`, newest first, plus
/// every further call sharing the oldest one's start.
pub fn older(conn: &Connection, before_ms: i64, limit: usize) -> rusqlite::Result<Page> {
    query(
        conn,
        &CallsFilter {
            before_ms: Some(before_ms),
            limit,
            ..Default::default()
        },
    )
}

/// The calls `f` selects, paged as [`older`] pages.
pub fn query(conn: &Connection, f: &CallsFilter) -> rusqlite::Result<Page> {
    let limit = f.limit.max(1);
    let w = Where::of(f);
    let (mut rows, extra) = fetch(conn, &w, limit)?;
    let asked = f.before_ms.unwrap_or(i64::MAX);
    let Some(last) = rows.last().map(|r| (r.id, r.start_ms)) else {
        return Ok(Page {
            entries: Vec::new(),
            more: false,
            before: asked,
        });
    };
    let mut more = false;
    if extra {
        // The oldest start is shared with rows not yet taken: take them all.
        let (id, oldest) = last;
        let mut tail = w.clone();
        tail.and("start_ms = ?", Sql::Integer(oldest));
        tail.and("id < ?", Sql::Integer(id));
        rows.extend(fetch(conn, &tail, usize::MAX)?.0);
        let mut below = w.clone();
        below.and("start_ms < ?", Sql::Integer(oldest));
        more = !fetch(conn, &below, 1)?.0.is_empty();
    }
    let before = rows.last().map_or(asked, |r| r.start_ms);
    Ok(Page {
        entries: rows.into_iter().map(|r| r.entry).collect(),
        more,
        before,
    })
}

/// The call filed under `path`.
pub fn one(conn: &Connection, path: &str) -> rusqlite::Result<Option<Value>> {
    let sql = "select path, record, audio, json from calls where path = ?1";
    let mut stmt = conn.prepare_cached(sql)?;
    let mut rows = stmt.query([path])?;
    match rows.next()? {
        Some(r) => Ok(Some(entry(
            &r.get::<_, String>(0)?,
            &r.get::<_, String>(1)?,
            r.get(2)?,
            r.get(3)?,
        ))),
        None => Ok(None),
    }
}

/// A fetched call: its entry and where it sits in the order.
struct Fetched {
    entry: Value,
    id: i64,
    start_ms: i64,
}

/// The conditions and parameters of a query, built once and shared by
/// `older` and `query`.
#[derive(Clone, Default)]
struct Where {
    clauses: Vec<String>,
    params: Vec<Sql>,
}

impl Where {
    fn and(&mut self, clause: &str, param: Sql) {
        self.clauses.push(clause.to_string());
        self.params.push(param);
    }

    fn of(f: &CallsFilter) -> Where {
        let mut w = Where::default();
        if !f.systems.is_empty() {
            let marks = vec!["?"; f.systems.len()].join(",");
            w.clauses.push(format!("short_name in ({marks})"));
            w.params
                .extend(f.systems.iter().map(|s| Sql::Text(s.clone())));
        }
        if let Some(tg) = f.talkgroup {
            w.and("talkgroup = ?", Sql::Integer(i64::from(tg)));
        }
        if let Some(unit) = f.unit {
            w.and(
                "exists (select 1 from call_transmissions t where t.call_id = calls.id and t.unit = ?)",
                Sql::Integer(unit),
            );
        }
        if let Some(from) = f.from_ms {
            w.and("start_ms >= ?", Sql::Integer(from));
        }
        if let Some(to) = f.to_ms {
            w.and("start_ms < ?", Sql::Integer(to));
        }
        if let Some(before) = f.before_ms {
            w.and("start_ms < ?", Sql::Integer(before));
        }
        w
    }

    fn sql(&self) -> String {
        let cond = if self.clauses.is_empty() {
            String::new()
        } else {
            format!(" where {}", self.clauses.join(" and "))
        };
        format!("select {COLUMNS} from calls{cond} order by start_ms desc, id desc limit ?")
    }
}

/// Up to `limit` rows matching `w`, newest first, and whether one more
/// matched beyond them.
fn fetch(conn: &Connection, w: &Where, limit: usize) -> rusqlite::Result<(Vec<Fetched>, bool)> {
    let want = i64::try_from(limit).unwrap_or(i64::MAX - 1);
    let mut params = w.params.clone();
    params.push(Sql::Integer(want.saturating_add(1)));
    let mut stmt = conn.prepare_cached(&w.sql())?;
    let mut rows = stmt.query(params_from_iter(params))?;
    let mut out = Vec::new();
    let mut extra = false;
    while let Some(r) = rows.next()? {
        if out.len() as i64 == want {
            extra = true;
            break;
        }
        out.push(Fetched {
            entry: entry(
                &r.get::<_, String>(0)?,
                &r.get::<_, String>(1)?,
                r.get(2)?,
                r.get(3)?,
            ),
            id: r.get(4)?,
            start_ms: r.get(5)?,
        });
    }
    Ok((out, extra))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::row::call_row;
    use crate::db::{Db, Write};

    fn seeded(name: &str) -> Db {
        let (db, _) = Db::for_test(name);
        // 12 calls: talkgroup 2 (odd i) and 1 (even i), starts i*1000, except
        // call 6 shares call 5's start 5000 and call 12 has start 0 (clock
        // off); unit 77 talks on calls 3 (talkgroup 2) and 8 (talkgroup 1).
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
            db.send(Write::Call {
                row: call_row(&format!("s/{i}"), &r, &r.to_json(), true, true, None, 0),
                live: true,
            });
        }
        db.flush().unwrap();
        db
    }

    fn paths(v: &[Value]) -> Vec<&str> {
        v.iter().map(|e| e["path"].as_str().unwrap()).collect()
    }

    #[test]
    fn newest_is_newest_first() {
        let db = seeded("newest");
        let v = db.with_reader(|c| newest(c, 3)).unwrap();
        assert_eq!(paths(&v), ["s/11", "s/10", "s/9"]);
    }

    #[test]
    fn paging_returns_every_call_exactly_once() {
        let db = seeded("paging");
        let mut seen = Vec::new();
        let mut before = i64::MAX;
        loop {
            let p = db.with_reader(|c| older(c, before, 4)).unwrap();
            seen.extend(paths(&p.entries).into_iter().map(str::to_string));
            if !p.more {
                break;
            }
            before = p.before;
        }
        let mut want: Vec<String> = (1..=12).map(|i| format!("s/{i}")).collect();
        seen.sort();
        want.sort();
        assert_eq!(seen, want);
    }

    #[test]
    fn equal_starts_are_taken_whole() {
        let db = seeded("whole");
        let p = db.with_reader(|c| older(c, 7001, 1)).unwrap(); // limit 1: 7000 is s/7 alone
        assert_eq!(paths(&p.entries), ["s/7"], "7000 is alone");
        let p = db.with_reader(|c| older(c, 5001, 1)).unwrap();
        assert_eq!(p.entries.len(), 2, "5000 is shared, both come");
        assert_eq!(p.before, 5000);
        assert!(p.more);
    }

    #[test]
    fn a_zero_start_pages_last() {
        let db = seeded("zero");
        let p = db.with_reader(|c| older(c, 1, 10)).unwrap();
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0]["path"], "s/12");
        assert!(!p.more);
    }

    #[test]
    fn query_filters_by_talkgroup_unit_range_and_system() {
        let db = seeded("query");
        let f = |tg, unit, from, to, systems: &[&str]| CallsFilter {
            systems: systems.iter().map(|s| s.to_string()).collect(),
            talkgroup: tg,
            unit,
            from_ms: from,
            to_ms: to,
            before_ms: None,
            limit: 100,
        };
        let n = |f: &CallsFilter| db.with_reader(|c| query(c, f)).unwrap().entries.len();
        assert_eq!(n(&f(Some(1), None, None, None, &[])), 6);
        assert_eq!(n(&f(None, Some(77), None, None, &[])), 2);
        assert_eq!(n(&f(Some(2), Some(77), None, None, &[])), 1);
        assert_eq!(
            n(&f(None, None, Some(3000), Some(6000), &[])),
            4,
            "3000, 4000, 5000, 5000"
        );
        assert_eq!(n(&f(None, None, None, None, &["ohmarcs"])), 12);
        assert_eq!(n(&f(None, None, None, None, &["other"])), 0);
        let p = db
            .with_reader(|c| {
                query(
                    c,
                    &CallsFilter {
                        limit: 5,
                        ..f(None, None, None, None, &[])
                    },
                )
            })
            .unwrap();
        assert!(p.more);
        assert_eq!(p.entries.len(), 5);
    }

    #[test]
    fn one_by_path() {
        let db = seeded("one");
        assert_eq!(
            db.with_reader(|c| one(c, "s/4")).unwrap().unwrap()["record"]["talkgroup"],
            1
        );
        assert!(db.with_reader(|c| one(c, "s/99")).unwrap().is_none());
    }
}
