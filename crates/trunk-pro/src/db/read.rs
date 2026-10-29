//! A page of calls from the store, the same for every backend: the query a
//! [`CallsQuery`] makes. Calls are in one order everywhere, newest first by
//! start then path ([`trunk_app::calls`], which pages calls held in memory
//! the same way), and a page is one query: up to its limit and one more
//! (whether more remain), after the cursor its query carries.

use trunk_app::calls::{CallsQuery, Cursor};

use super::row::CALL_COLS;
use super::sql::{Params, V};

/// The columns a page reads: each call's id and system's short name, then
/// [`CALL_COLS`] (`row::stored_call` reads them from 2).
pub fn columns() -> String {
    std::iter::once("c.id".to_string()).chain(std::iter::once("s.short_name".to_string())).chain(CALL_COLS.iter().map(|c| format!("c.{c}"))).collect::<Vec<_>>().join(", ")
}

/// The calls a query wants, as SQL. Every page walks an index in order and
/// stops at its limit, whatever it asks for:
/// - several systems: a query each, put together (`union all`) and cut to
///   the limit again, rather than one that sorts all their calls;
/// - a talkgroup: `calls_system_tg_start` (talkgroups are a system's own);
/// - a radio: `transmissions_radio`, each radio's first transmission in a
///   call by system and the call's start, joined to the call.
///
/// Equal starts are ordered by path (a sort only within one millisecond).
#[derive(Clone, Default)]
pub struct Where {
    systems: Vec<i64>,
    talkgroup: Option<u32>,
    unit: Option<i64>,
    from: Option<i64>,
    to: Option<i64>,
    after: Option<Cursor>,
}

impl Where {
    /// `q` over `systems` (its system's id, or each system's; none: any).
    pub fn of(q: &CallsQuery, systems: &[i64]) -> Where {
        Where { systems: systems.to_vec(), talkgroup: q.talkgroup, unit: q.unit, from: q.from, to: q.to, after: q.cursor.as_deref().and_then(Cursor::decode) }
    }

    /// One system's (or, with `system` none, every system's) part: the
    /// newest `limit` of `columns` (of `calls c` and `systems s`).
    fn arm(&self, system: Option<i64>, columns: &str, limit: i64, ps: &mut Params) -> String {
        // (The table the index walk is on: a radio's transmissions, else calls.)
        let (from, start, sys) = match self.unit {
            Some(_) => ("call_transmissions u join calls c on c.id = u.call_id join systems s on s.id = c.system_id", "u.call_start_ms", "u.system_id"),
            None => ("calls c join systems s on s.id = c.system_id", "c.start_ms", "c.system_id"),
        };
        let mut w: Vec<String> = Vec::new();
        if let Some(id) = system {
            w.push(format!("{sys} = {}", ps.add(id)));
        }
        if let Some(u) = self.unit {
            w.push(format!("u.first and u.unit = {}", ps.add(u)));
        }
        if let Some(tg) = self.talkgroup {
            w.push(format!("c.talkgroup = {}", ps.add(tg)));
        }
        if let Some(t) = self.from {
            w.push(format!("{start} >= {}", ps.add(t)));
        }
        if let Some(t) = self.to {
            w.push(format!("{start} < {}", ps.add(t)));
        }
        if let Some(a) = &self.after {
            // (The index's range first, then the path within the millisecond.)
            let (t, p) = (ps.add(a.start_ms), ps.add(a.path.clone()));
            w.push(format!("{start} <= {t} and ({start} < {t} or c.path < {p})"));
        }
        let cond = if w.is_empty() { String::new() } else { format!(" where {}", w.join(" and ")) };
        format!("select {columns} from {from}{cond} order by {start} desc, c.path desc limit {}", ps.add(limit))
    }

    /// The query of a page: up to `limit` of `columns`, in order, and its
    /// parameters.
    pub fn query(&self, columns: &str, limit: i64) -> (String, Vec<V>) {
        let mut ps = Params::default();
        let sql = match self.systems.as_slice() {
            [] => self.arm(None, columns, limit, &mut ps),
            [one] => self.arm(Some(*one), columns, limit, &mut ps),
            many => {
                let arms: Vec<String> = many.iter().enumerate().map(|(i, s)| format!("select * from ({}) a{i}", self.arm(Some(*s), columns, limit, &mut ps))).collect();
                let lim = ps.add(limit);
                format!("{} order by start_ms desc, path desc limit {lim}", arms.join(" union all "))
            }
        };
        (sql, ps.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::row::call_row;
    use crate::db::{Change, Db};
    use std::time::Duration;
    use trunk_app::calls::StoredCall;
    use trunk_recorder_plugin::{ErrorInterval, SrcEntry, ToneEntry};

    /// Each backend with 12 calls: talkgroup 2 (odd i) and 1 (even i),
    /// starts i*1000, except call 6 shares call 5's start 5000 and call 12
    /// has start 0 (clock off); unit 77 talks on calls 3 (talkgroup 2) and 8
    /// (talkgroup 1), twice on 8.
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
                if i == 8 {
                    r.src_list[1].src = 77;
                }
                db.send(Change::Call { row: call_row(&format!("s/{i}"), &r, true, true, None, 0), live: true });
            }
            db.flush().unwrap();
        }
        dbs
    }

    fn paths(v: &[StoredCall]) -> Vec<&str> {
        v.iter().map(|e| e.path.as_str()).collect()
    }

    /// Every page of `q` from the start, the paths in the order they came.
    fn every_page(db: &Db, q: CallsQuery, systems: &[i64]) -> Vec<String> {
        let mut seen = Vec::new();
        let mut q = q;
        loop {
            let p = db.calls(&q, systems).unwrap();
            seen.extend(paths(&p.entries).into_iter().map(str::to_string));
            match p.next {
                Some(n) => q.cursor = Some(n),
                None => break,
            }
        }
        seen
    }

    #[test]
    fn newest_first_then_by_path() {
        for db in seeded("newest") {
            let q = |limit| CallsQuery { limit: Some(limit), ..Default::default() };
            assert_eq!(paths(&db.calls(&q(3), &[]).unwrap().entries), ["s/11", "s/10", "s/9"], "{}", db.kind());
            // s/6 and s/5 share 5000: by path, s/6 first; a page may end between them.
            let p = db.calls(&q(6), &[]).unwrap();
            assert_eq!(paths(&p.entries), ["s/11", "s/10", "s/9", "s/8", "s/7", "s/6"]);
            assert_eq!(p.next.as_deref(), Some("5000:s/6"));
        }
    }

    #[test]
    fn paging_returns_every_call_exactly_once_in_order() {
        for db in seeded("paging") {
            let want = ["s/11", "s/10", "s/9", "s/8", "s/7", "s/6", "s/5", "s/4", "s/3", "s/2", "s/1", "s/12"];
            for limit in 1..=12 {
                assert_eq!(every_page(&db, CallsQuery { limit: Some(limit), ..Default::default() }, &[]), want, "limit {limit} ({})", db.kind());
            }
            let a = db.system_id("ohmarcs").unwrap();
            assert_eq!(every_page(&db, CallsQuery { talkgroup: Some(2), limit: Some(2), ..Default::default() }, &[a]), ["s/11", "s/9", "s/7", "s/5", "s/3", "s/1"]);
            assert_eq!(every_page(&db, CallsQuery { unit: Some(77), limit: Some(1), ..Default::default() }, &[a]), ["s/8", "s/3"], "each call once, though 77 talked twice on s/8");
        }
    }

    #[test]
    fn query_filters_by_talkgroup_unit_range_and_system() {
        for db in seeded("query") {
            let a = db.system_id("ohmarcs").unwrap();
            let n = |q: CallsQuery, systems: &[i64]| db.calls(&q, systems).unwrap().entries.len();
            assert_eq!(n(CallsQuery { talkgroup: Some(1), ..Default::default() }, &[a]), 6, "{}", db.kind());
            assert_eq!(n(CallsQuery { unit: Some(77), ..Default::default() }, &[a]), 2);
            assert_eq!(n(CallsQuery { talkgroup: Some(2), unit: Some(77), ..Default::default() }, &[a]), 1);
            assert_eq!(n(CallsQuery { from: Some(3000), to: Some(6000), ..Default::default() }, &[]), 4, "3000, 4000, 5000, 5000");
            assert_eq!(n(CallsQuery::default(), &[a]), 12);
            assert_eq!(n(CallsQuery::default(), &[a, 999]), 12, "two systems' arms");
            assert_eq!(n(CallsQuery::default(), &[999]), 0);
        }
    }

    /// The store mirrors a call once and gives back the record that went in:
    /// every field, its radios, and the lists in `detail`.
    #[test]
    fn a_call_comes_back_as_it_went_in() {
        for db in Db::for_each_backend("mirror", Duration::from_secs(1)) {
            let mut r = crate::db::row::tests::record(9179, 1_700_000_000_123);
            r.call_num = 42;
            r.transmission_id = "t-1".into();
            r.freq_error = -12;
            (r.signal, r.noise, r.snr, r.clean_voice_pct) = (Some(-60.25), Some(-110.5), None, Some(97.5));
            (r.tdma_slot, r.phase2_tdma, r.emergency, r.priority, r.encrypted, r.algid, r.key_id, r.decrypted) = (1, true, true, 3, true, 0x84, 7, true);
            (r.talkgroup_ota, r.talkgroup_tag, r.talkgroup_group) = (Some(9180), "FD Disp".into(), "Fire".into());
            (r.color_code, r.ran, r.tone_mode, r.tone_confidence, r.audio_type, r.system_type, r.voice_mode) = (-1, 3, "ctcss".into(), 0.875, "digital".into(), "p25".into(), "phase2".into());
            r.src_list[1] = SrcEntry { src: 8344139, time: 1_700_000_002, pos: 2.5, tag: "E1".into(), tag_ota: "ENGINE 1".into(), lat: Some(38.9), lon: Some(-77.03), ..Default::default() };
            r.src_list[1].extra.insert("future".into(), serde_json::json!(1));
            r.error_list = vec![ErrorInterval { pos: 1.0, len: 0.5, frames: 3, error_count: 7, bad_frames: 1, max_frame_errors: 4 }];
            r.tones = vec![ToneEntry { kind: "ctcss".into(), value: "123.0".into(), pos: 0.0, len: 4.0, src: 9001644, origin: "audio".into(), ..Default::default() }];
            r.signals = vec!["Page".into()];
            r.patched_talkgroups = vec![9181];
            r.extra.insert("newField".into(), serde_json::json!({ "a": [1, 2] }));
            db.send(Change::Call { row: call_row("ohmarcs/x", &r, true, false, None, 0), live: true });
            db.flush().unwrap();
            let back = db.one("ohmarcs/x").unwrap().unwrap();
            assert_eq!(serde_json::to_value(&back.record).unwrap(), serde_json::to_value(&r).unwrap(), "{}", db.kind());
            assert_eq!((back.path.as_str(), back.audio, back.json), ("ohmarcs/x", true, false));
            assert!(db.one("s/99").unwrap().is_none());
            assert!(db.known("ohmarcs/x").unwrap() && !db.known("s/99").unwrap());
        }
    }

    /// Every page walks an index on PostgreSQL too (SQLite's plans:
    /// `schema::tests::every_page_walks_an_index_without_sorting`), with 50,000
    /// calls analysed: no table read whole, no sort but of what's cut to the
    /// limit.
    #[test]
    fn every_page_walks_an_index_on_postgres() {
        for db in seeded("pg-plans").into_iter().filter(|db| db.kind() == "postgres") {
            let store = db.store().unwrap();
            // (Copies of call s/1 a second apart, one in ten system a's, the rest
            // another's; a's over 40 talkgroups, each with one of 500 radios.)
            let (a, b) = (db.system_id("ohmarcs").unwrap(), db.system_id("other").unwrap());
            db.flush().unwrap();
            let exprs: Vec<String> = CALL_COLS.iter().map(|c| match *c {
                "path" => "'g/' || g".to_string(),
                "start_ms" => "c.start_ms + g * 1000".to_string(),
                "system_id" => format!("case when g % 10 = 0 then {a} else {b} end"),
                "talkgroup" => "(g / 10) % 40".to_string(),
                c => format!("c.{c}"),
            }).collect();
            store.batch_for_test(&format!(
                "insert into calls ({}) select {} from calls c cross join generate_series(1, 50000) g where c.path = 's/1';
                 insert into call_transmissions (call_id, seq, system_id, call_start_ms, unit, first, time, pos, emergency, encrypted, duplex, mode, priority, signal_system, tag, tag_ota)
                 select n.id, 0, n.system_id, n.start_ms, ((n.start_ms / 1000 - 1) / 10) % 500, true, 0, 0, false, false, false, false, 0, '', '', '' from calls n where n.path like 'g/%';
                 analyze",
                CALL_COLS.join(", "),
                exprs.join(", ")
            ));
            let f = |systems: &[i64], tg: Option<u32>, unit: Option<i64>| (CallsQuery { talkgroup: tg, unit, cursor: Some("40000000:g/9".into()), ..Default::default() }, systems.to_vec());
            // (A system's calls: by its index, or by every call's with the others
            // passed over, as the planner costs it; a tenth of them, either is cheap.)
            let system = &["calls_system_start", "calls_start"][..];
            for (what, (q, systems), index) in [
                ("every call", f(&[], None, None), &["calls_start"][..]),
                ("a system", f(&[a], None, None), system),
                ("two systems", f(&[a, b], None, None), system),
                ("a talkgroup", f(&[a], Some(1), None), &["calls_system_tg_start"][..]),
                ("a radio", f(&[a], None, Some(7)), &["transmissions_radio"][..]),
                ("a radio on two", f(&[a, b], None, Some(7)), &["transmissions_radio"][..]),
            ] {
                let (sql, ps) = Where::of(&q, &systems).query(&columns(), 101);
                let plan = store.plan(&sql, &ps);
                let p = plan.join("\n");
                assert!(index.iter().any(|i| p.contains(&format!("Backward using {i} "))), "{what}:\n{p}");
                assert!(!p.contains("Seq Scan on calls") && !p.contains("Seq Scan on call_transmissions") && !p.contains("Bitmap Heap Scan"), "{what} reads a table whole:\n{p}");
                // (A full sort only ever right under a limit, of the arms cut to it; an
                // incremental one, of the path within a start, as the index is walked.)
                for (i, line) in plan.iter().enumerate() {
                    if line.trim_start().trim_start_matches("->").trim_start().starts_with("Sort  (") {
                        assert!(i > 0 && plan[i - 1].contains("Limit"), "{what} sorts what it matched:\n{p}");
                    }
                }
            }
        }
    }

    #[test]
    fn parameters_are_numbered_as_written() {
        let q = CallsQuery { talkgroup: Some(1), ..Default::default() };
        let (sql, ps) = Where::of(&q, &[1, 2]).query("c.path", 10);
        // (Two arms of system, talkgroup and limit, then the union's limit.)
        assert!(sql.contains("c.system_id = $1") && sql.contains("c.talkgroup = $2") && sql.contains("c.system_id = $4") && sql.ends_with("limit $7"), "{sql}");
        assert_eq!(ps.len(), 7);
    }
}
