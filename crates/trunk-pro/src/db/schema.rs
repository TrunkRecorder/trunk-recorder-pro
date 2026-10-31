//! The store's schema, declared once ([`TABLES`], [`INDEXES`]) and made into
//! each dialect's DDL ([`ddl`]): the two can't drift apart. Logical types
//! ([`Ty`]) say what a column holds; each dialect says how (a boolean is
//! SQLite's 0/1 and PostgreSQL's `boolean`, JSON is `text` and `jsonb`).
//!
//! While in development nothing is migrated: [`SCHEMA_VERSION`] is raised
//! with any change, and a store of any other version is set aside and made
//! afresh (`sqlite.rs`; PostgreSQL's tables dropped and made again).
//!
//! Indexes follow the queries, each page an index walked in order to its
//! limit (`read::Where`): calls by start (every page), by system and start
//! (a system's page), by system, talkgroup and start (a talkgroup's), and a
//! radio's calls by system and the call's start (a partial index on each
//! radio's first transmission in a call, so each call once). Each costs
//! page writes on every commit (research/storage.md). Tables keyed by what
//! they're read by are `without rowid` in SQLite: their primary key is the
//! table, not a second index beside it.

use super::sql::Dialect;

/// This build's schema. Raise it with any change to [`TABLES`] or
/// [`INDEXES`]: a store of another version is made afresh.
pub const SCHEMA_VERSION: u32 = 8;

/// What a column holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    /// The table's surrogate key, made by the database.
    Id,
    Int,
    BigInt,
    Real,
    Bool,
    Text,
    /// A JSON document (PostgreSQL can query into it).
    Json,
}

pub struct Col {
    pub name: &'static str,
    pub ty: Ty,
    pub null: bool,
    pub unique: bool,
    /// `table(column)` it refers to, and whether its rows go with that one.
    pub refs: Option<(&'static str, bool)>,
}

const fn col(name: &'static str, ty: Ty) -> Col {
    Col { name, ty, null: false, unique: false, refs: None }
}

const fn opt(name: &'static str, ty: Ty) -> Col {
    Col { name, ty, null: true, unique: false, refs: None }
}

const fn unique(name: &'static str, ty: Ty) -> Col {
    Col { name, ty, null: false, unique: true, refs: None }
}

/// A child's column: its rows go with the parent's.
const fn of(name: &'static str, ty: Ty, parent: &'static str) -> Col {
    Col { name, ty, null: false, unique: false, refs: Some((parent, true)) }
}

/// A system's: `systems(id)`.
const fn system(name: &'static str) -> Col {
    Col { name, ty: BigInt, null: false, unique: false, refs: Some(("systems(id)", false)) }
}

pub struct Table {
    pub name: &'static str,
    pub cols: &'static [Col],
    /// The primary key's columns (none: the `Id` column is it).
    pub key: &'static [&'static str],
}

pub struct Index {
    pub name: &'static str,
    pub table: &'static str,
    pub cols: &'static [&'static str],
    /// A partial index's condition.
    pub cond: Option<&'static str>,
}

impl Index {
    /// Its `create index` (`if not exists`: to make it again in a store
    /// that dropped it).
    pub fn create(&self) -> String {
        let cond = self.cond.map_or(String::new(), |c| format!(" where {c}"));
        format!("create index if not exists {} on {}({}){cond}", self.name, self.table, self.cols.join(", "))
    }
}

/// The index a radio's calls are found by (each radio's calls in a system,
/// newest first): optional (`storage.radioIndex`), made with the schema and
/// dropped by a store that doesn't keep it.
pub const RADIO_INDEX: &str = "transmissions_radio";
/// The index a talkgroup's calls are found by: optional the same way
/// (`storage.talkgroupIndex`).
pub const TALKGROUP_INDEX: &str = "calls_system_tg_start";

use Ty::*;

/// Every table, parents before children.
pub const TABLES: &[Table] = &[
    Table { name: "meta", cols: &[col("key", Text), col("value", Text)], key: &["key"] },
    // The config's systems (trunked and conventional), by the id it gives
    // them: what everything else is kept by, so a rename orphans nothing. A
    // system gone from the config keeps its row (its calls still have a name).
    Table { name: "systems", cols: &[col("id", BigInt), col("short_name", Text), col("kind", Text)], key: &["id"] },
    // A call mirrored once (`row.rs`): the record's fields, typed; the lists
    // nothing queries in `detail`; its radios in `call_transmissions`.
    Table {
        name: "calls",
        cols: &[
            col("id", Id),
            unique("path", Text),
            system("system_id"),
            col("site", Text),
            col("talkgroup", BigInt),
            opt("talkgroup_ota", BigInt),
            col("talkgroup_tag", Text),
            col("talkgroup_description", Text),
            col("talkgroup_group_tag", Text),
            col("talkgroup_group", Text),
            col("call_num", BigInt),
            col("transmission_id", Text),
            col("freq_hz", BigInt),
            col("freq_error", BigInt),
            opt("signal", Real),
            opt("noise", Real),
            opt("snr", Real),
            opt("clean_voice_pct", Real),
            col("source_num", BigInt),
            col("recorder_num", BigInt),
            col("tdma_slot", BigInt),
            col("phase2_tdma", Bool),
            col("start_ms", BigInt),
            col("stop_ms", BigInt),
            col("length_ms", BigInt),
            col("length_s", Real),
            col("emergency", Bool),
            col("priority", BigInt),
            col("mode", Bool),
            col("duplex", Bool),
            col("encrypted", Bool),
            col("algid", BigInt),
            col("key_id", BigInt),
            col("decrypted", Bool),
            col("clock_off", Bool),
            col("nac", Text),
            col("color_code", BigInt),
            col("ran", BigInt),
            col("tone_mode", Text),
            col("tone_detected", Text),
            col("tone_confidence", Real),
            col("audio_type", Text),
            col("system_type", Text),
            col("voice_mode", Text),
            col("audio", Bool),
            col("json", Bool),
            opt("recorder", Text),
            col("indexed_ms", BigInt),
            col("detail", Json),
        ],
        key: &[],
    },
    Table {
        name: "call_transmissions",
        cols: &[
            of("call_id", BigInt, "calls(id)"),
            col("seq", Int),
            // (The call's system and start: a radio's page is these, in order.)
            system("system_id"),
            col("call_start_ms", BigInt),
            col("unit", BigInt),
            // (Its radio's first in the call: the radio index takes these alone.)
            col("first", Bool),
            col("time", BigInt),
            col("pos", Real),
            col("emergency", Bool),
            col("encrypted", Bool),
            col("duplex", Bool),
            col("mode", Bool),
            col("priority", BigInt),
            col("signal_system", Text),
            col("tag", Text),
            col("tag_ota", Text),
            opt("lat", Real),
            opt("lon", Real),
            opt("extra", Json),
        ],
        key: &["call_id", "seq"],
    },
    Table { name: "kept", cols: &[col("kind", Text), col("key", Text), col("value", Text)], key: &["kind", "key"] },
    Table { name: "registry", cols: &[system("system_id"), col("whole", Json)], key: &["system_id"] },
    Table { name: "registry_changes", cols: &[col("id", Id), system("system_id"), col("changes", Json)], key: &[] },
    // A system's talkgroups, by number, in the order loaded (`sort_order`):
    // a talkgroup file is only how they come in and go out
    // (trunk_core::trunk::parse_rows / rows_csv).
    Table {
        name: "talkgroups",
        cols: &[
            system("system_id"),
            col("number", BigInt),
            col("alpha_tag", Text),
            col("mode", Text),
            col("description", Text),
            col("tag", Text),
            col("category", Text),
            col("priority", BigInt),
            col("preferred_nac", BigInt),
            col("preferred_site", Text),
            col("ignore", Bool),
            col("sort_order", BigInt),
            col("comment", Text),
            opt("extra", Json),
        ],
        key: &["system_id", "number"],
    },
    // Each system's radios' talker aliases, as heard over the air.
    Table {
        name: "units",
        cols: &[system("system_id"), col("unit", BigInt), col("alias", Text), col("source", Text), col("heard", BigInt), col("wacn", Text), col("sys", Text), opt("talkgroup", BigInt)],
        key: &["system_id", "unit"],
    },
    Table { name: "stats_minutes", cols: &[col("id", Id), col("t", BigInt), col("s", Json)], key: &[] },
    // The dashboard's series by name ("sys/<short name>/calls", "cpu"…),
    // and each one's hours, by its key.
    Table { name: "series", cols: &[col("id", Id), unique("name", Text)], key: &[] },
    Table {
        name: "stats_hours",
        cols: &[of("series_id", BigInt, "series(id)"), col("t", BigInt), col("avg", Real), col("lo", Real), col("hi", Real), col("n", BigInt)],
        key: &["series_id", "t"],
    },
];

pub const INDEXES: &[Index] = &[
    Index { name: "calls_start", table: "calls", cols: &["start_ms"], cond: None },
    Index { name: "calls_system_start", table: "calls", cols: &["system_id", "start_ms"], cond: None },
    Index { name: "calls_system_tg_start", table: "calls", cols: &["system_id", "talkgroup", "start_ms"], cond: None },
    // A radio's calls: each once (its first transmission in each), by system and the call's start.
    Index { name: "transmissions_radio", table: "call_transmissions", cols: &["system_id", "unit", "call_start_ms", "call_id"], cond: Some("first") },
    Index { name: "stats_minutes_t", table: "stats_minutes", cols: &["t"], cond: None },
];

fn ty(d: Dialect, t: Ty) -> &'static str {
    match (d, t) {
        (Dialect::Sqlite, Id) => "integer primary key",
        (Dialect::Postgres, Id) => "bigint generated always as identity primary key",
        (Dialect::Sqlite, Int | BigInt | Bool) => "integer",
        (Dialect::Postgres, Int) => "integer",
        (Dialect::Postgres, BigInt) => "bigint",
        (Dialect::Postgres, Bool) => "boolean",
        (Dialect::Sqlite, Real) => "real",
        (Dialect::Postgres, Real) => "double precision",
        (_, Text) => "text",
        (Dialect::Sqlite, Json) => "text",
        (Dialect::Postgres, Json) => "jsonb",
    }
}

/// The schema's DDL in dialect `d`.
pub fn ddl(d: Dialect) -> String {
    let mut out = String::new();
    for t in TABLES {
        let mut lines: Vec<String> = t
            .cols
            .iter()
            .map(|c| {
                let mut s = format!("    {} {}", c.name, ty(d, c.ty));
                if !c.null && c.ty != Id {
                    s.push_str(" not null");
                }
                if c.unique {
                    s.push_str(" unique");
                }
                match c.refs {
                    Some((p, true)) => s.push_str(&format!(" references {p} on delete cascade")),
                    Some((p, false)) => s.push_str(&format!(" references {p}")),
                    None => {}
                }
                s
            })
            .collect();
        if !t.key.is_empty() {
            lines.push(format!("    primary key ({})", t.key.join(", ")));
        }
        // (Keyed by what it's read by: the key is the table, in SQLite.)
        let rowid = if d == Dialect::Sqlite && !t.key.is_empty() { " without rowid" } else { "" };
        out.push_str(&format!("create table {} (\n{}\n){rowid};\n", t.name, lines.join(",\n")));
    }
    for i in INDEXES {
        out.push_str(&format!("{};\n", i.create()));
    }
    for (name, sql) in VIEWS {
        out.push_str(&format!("create view {name} as {};\n", timestamps(sql, d)));
    }
    if d == Dialect::Postgres {
        // (The big tables, mostly appended and retired by age: vacuumed and
        // analysed after 1% changes rather than 20%, so the planner keeps
        // up with a day's calls.)
        for t in ["calls", "call_transmissions"] {
            out.push_str(&format!("alter table {t} set (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.01);\n"));
        }
    }
    out
}

/// Views to report from, by hand or with a tool (Grafana, Metabase,
/// Datasette…): times as timestamps (UTC), systems by name, talkgroups and
/// radios with their names. `AT_MS(x)` / `AT_S(x)`: Unix ms / s as a
/// timestamp, in the dialect's way. Nothing in the app reads them.
const VIEWS: &[(&str, &str)] = &[
    (
        "calls_report",
        "select c.id, s.short_name as system, AT_MS(c.start_ms) as start_at, AT_MS(c.stop_ms) as stop_at, c.length_s, c.talkgroup,
                coalesce(nullif(g.alpha_tag, ''), c.talkgroup_tag) as talkgroup_name, g.description as talkgroup_description, g.category as talkgroup_category,
                c.freq_hz, c.site, c.emergency, c.encrypted, c.decrypted, c.signal, c.noise, c.snr, c.clean_voice_pct, c.path, c.audio
         from calls c join systems s on s.id = c.system_id left join talkgroups g on g.system_id = c.system_id and g.number = c.talkgroup",
    ),
    (
        "transmissions_report",
        "select u.call_id, s.short_name as system, AT_MS(u.call_start_ms) as call_start_at, u.seq, u.unit,
                coalesce(nullif(a.alias, ''), nullif(u.tag, ''), u.tag_ota) as unit_name, AT_S(u.time) as at, u.pos, c.talkgroup,
                u.emergency, u.encrypted, u.lat, u.lon
         from call_transmissions u join systems s on s.id = u.system_id join calls c on c.id = u.call_id left join units a on a.system_id = u.system_id and a.unit = u.unit",
    ),
    ("stats_report", "select se.name as series, AT_S(h.t) as hour_at, h.avg, h.lo, h.hi, h.n from stats_hours h join series se on se.id = h.series_id"),
];

/// `sql` with each `AT_MS(x)` and `AT_S(x)` (x without parentheses) as
/// a timestamp: PostgreSQL's `timestamptz`, SQLite's ISO 8601 text (UTC).
fn timestamps(sql: &str, d: Dialect) -> String {
    let mut out = sql.to_string();
    for (mark, ms) in [("AT_MS(", true), ("AT_S(", false)] {
        while let Some(i) = out.find(mark) {
            let j = i + mark.len() + out[i + mark.len()..].find(')').expect("a closing parenthesis");
            let x = &out[i + mark.len()..j];
            let ts = match (d, ms) {
                (Dialect::Postgres, true) => format!("to_timestamp(0.001 * {x})"),
                (Dialect::Postgres, false) => format!("to_timestamp({x})"),
                (Dialect::Sqlite, true) => format!("strftime('%Y-%m-%dT%H:%M:%fZ', 0.001 * {x}, 'unixepoch')"),
                (Dialect::Sqlite, false) => format!("strftime('%Y-%m-%dT%H:%M:%SZ', {x}, 'unixepoch')"),
            };
            out.replace_range(i..=j, &ts);
        }
    }
    out
}

/// Every table, children before parents (to drop them).
pub fn tables_to_drop() -> String {
    TABLES.iter().rev().map(|t| t.name).collect::<Vec<_>>().join(", ")
}

/// How a SQLite database stands after [`prepare`].
pub enum Prepared {
    /// This build's schema (made now, if the database was new).
    Current,
    /// Another version's (older or newer): for the caller to set aside.
    Other(u32),
}

/// A new SQLite database (no version, nothing in it) made this schema, in
/// one transaction; one of this version left as it is.
pub fn prepare(conn: &rusqlite::Connection) -> rusqlite::Result<Prepared> {
    let have: u32 = conn.query_row("pragma user_version", [], |r| r.get(0))?;
    if have == SCHEMA_VERSION {
        return Ok(Prepared::Current);
    }
    let tables: i64 = conn.query_row("select count(*) from sqlite_master", [], |r| r.get(0))?;
    if have != 0 || tables > 0 {
        return Ok(Prepared::Other(have));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(&ddl(Dialect::Sqlite))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(Prepared::Current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Both dialects' DDL come from the one declaration: each table has the
    /// same columns in the same order (PostgreSQL's too, with `TRP_PG_URL`).
    #[test]
    fn both_dialects_make_the_same_tables() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(&ddl(Dialect::Sqlite)).unwrap();
        let sqlite = |t: &str| -> Vec<String> { c.prepare(&format!("select name from pragma_table_info('{t}') order by cid")).unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect() };
        for t in TABLES {
            assert_eq!(sqlite(t.name), t.cols.iter().map(|c| c.name).collect::<Vec<_>>(), "{}", t.name);
        }
        let Ok(url) = std::env::var("TRP_PG_URL") else { return };
        let mut pg = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        pg.batch_execute(&format!("drop schema if exists t_schema cascade; create schema t_schema; set search_path to t_schema; {}", ddl(Dialect::Postgres))).unwrap();
        for t in TABLES {
            let cols: Vec<String> = pg.query("select column_name::text from information_schema.columns where table_schema = 't_schema' and table_name = $1 order by ordinal_position", &[&t.name]).unwrap().iter().map(|r| r.get(0)).collect();
            assert_eq!(cols, sqlite(t.name), "{}", t.name);
        }
        for (v, _) in VIEWS {
            let cols: Vec<String> = pg.query("select column_name::text from information_schema.columns where table_schema = 't_schema' and table_name = $1 order by ordinal_position", &[v]).unwrap().iter().map(|r| r.get(0)).collect();
            assert_eq!(cols, sqlite(v), "{v}");
        }
    }

    /// Every page the query builder makes walks an index in order to its
    /// limit, never sorting all the calls it matches: every call, a system,
    /// several, a talkgroup, a radio, and the paging cursor's parts.
    #[test]
    fn every_page_walks_an_index_without_sorting() {
        use crate::db::read::{columns, Where};
        use trunk_app::calls::CallsQuery;
        let c = Connection::open_in_memory().unwrap();
        prepare(&c).unwrap();
        let plan = |(q, systems): (CallsQuery, Vec<i64>)| -> String {
            let (sql, ps) = Where::of(&q, &systems).query(&columns(), 101);
            let ps: Vec<rusqlite::types::Value> = ps.into_iter().map(|p| match p {
                crate::db::sql::V::Int(i) => i.into(),
                crate::db::sql::V::Text(t) => t.into(),
                v => panic!("{v:?}"),
            }).collect();
            let mut st = c.prepare(&format!("explain query plan {}", crate::db::sql::question_marks(&sql))).unwrap();
            let rows: Vec<String> = st.query_map(rusqlite::params_from_iter(ps), |r| r.get::<_, String>(3)).unwrap().map(Result::unwrap).collect();
            rows.join(" | ")
        };
        let f = |systems: &[i64], tg: Option<u32>, unit: Option<i64>| (CallsQuery { talkgroup: tg, unit, cursor: Some("5:p".into()), ..Default::default() }, systems.to_vec());
        for (what, filter, index) in [
            ("every call", f(&[], None, None), "calls_start"),
            ("a system", f(&[1], None, None), "calls_system_start"),
            ("two systems", f(&[1, 2], None, None), "calls_system_start"),
            ("a talkgroup", f(&[1], Some(1), None), "calls_system_tg_start"),
            ("a talkgroup on two", f(&[1, 2], Some(1), None), "calls_system_tg_start"),
            ("a radio", f(&[1], None, Some(7)), "transmissions_radio"),
            ("a radio on two", f(&[1, 2], None, Some(7)), "transmissions_radio"),
        ] {
            let p = plan(filter);
            assert!(p.contains(index), "{what}: {p}");
            // A sort only ever of a system's part once it's cut to the limit (the union
            // merging them), right after that part's scan: never of the calls matched.
            let steps: Vec<&str> = p.split(" | ").collect();
            // (And a sort of the order's last term, the path: within one millisecond,
            // in small groups as the index is walked.)
            for (i, s) in steps.iter().enumerate() {
                if s.contains("TEMP B-TREE") && !s.contains("LAST TERM OF ORDER BY") {
                    assert!(i > 0 && steps[i - 1].starts_with("SCAN a"), "{what} sorts what it matched: {p}");
                }
            }
        }
    }

    /// The dashboard's hours: the series the patterns name, then each one's
    /// hours by its key, never a scan of every hour.
    #[test]
    fn the_hours_are_read_by_series_key() {
        let c = Connection::open_in_memory().unwrap();
        prepare(&c).unwrap();
        let span = trunk_app::stats::Span::of(0, 365 * 86400, 300, 60);
        for patterns in [vec!["sys/a/*".to_string()], vec!["cpu".to_string(), "sys/*".to_string()]] {
            let (sql, ps) = crate::db::store::HoursQuery { patterns: &patterns, span, from: 0, before: 365 * 86400 }.sql();
            let ps: Vec<rusqlite::types::Value> = ps.into_iter().map(|p| match p {
                crate::db::sql::V::Int(i) => i.into(),
                crate::db::sql::V::Text(t) => t.into(),
                v => panic!("{v:?}"),
            }).collect();
            let mut st = c.prepare(&format!("explain query plan {}", crate::db::sql::question_marks(&sql))).unwrap();
            let p: Vec<String> = st.query_map(rusqlite::params_from_iter(ps), |r| r.get::<_, String>(3)).unwrap().map(Result::unwrap).collect();
            assert!(p.iter().any(|s| s.starts_with("SEARCH h USING") && s.contains("series_id=? AND t>? AND t<?")) && !p.iter().any(|s| s.starts_with("SCAN h")), "{p:?}");
        }
    }
}
