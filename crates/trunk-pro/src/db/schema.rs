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
//! (a system's page), by system, talkgroup and start (a talkgroup's), and
//! `call_units`, a radio's calls by system and the call's start. Each costs
//! page writes on every commit (research/storage.md). Tables keyed by what
//! they're read by are `without rowid` in SQLite: their primary key is the
//! table, not a second index beside it.

use super::sql::Dialect;

/// This build's schema. Raise it with any change to [`TABLES`] or
/// [`INDEXES`]: a store of another version is made afresh.
pub const SCHEMA_VERSION: u32 = 3;

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
    /// `table(column)`, its rows deleted with the one referred to.
    pub cascade: Option<&'static str>,
}

const fn col(name: &'static str, ty: Ty) -> Col {
    Col { name, ty, null: false, unique: false, cascade: None }
}

const fn opt(name: &'static str, ty: Ty) -> Col {
    Col { name, ty, null: true, unique: false, cascade: None }
}

const fn unique(name: &'static str, ty: Ty) -> Col {
    Col { name, ty, null: false, unique: true, cascade: None }
}

const fn of(name: &'static str, ty: Ty, parent: &'static str) -> Col {
    Col { name, ty, null: false, unique: false, cascade: Some(parent) }
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
}

use Ty::*;

/// Every table, parents before children.
pub const TABLES: &[Table] = &[
    Table { name: "meta", cols: &[col("key", Text), col("value", Text)], key: &["key"] },
    Table {
        name: "calls",
        cols: &[
            col("id", Id),
            unique("path", Text),
            col("short_name", Text),
            opt("nac", Text),
            opt("site", Text),
            col("talkgroup", BigInt),
            col("start_ms", BigInt),
            col("stop_ms", BigInt),
            col("length_ms", BigInt),
            col("freq_hz", BigInt),
            col("emergency", Bool),
            col("encrypted", Bool),
            col("decrypted", Bool),
            col("duplex", Bool),
            opt("priority", BigInt),
            opt("audio_type", Text),
            opt("signal", Real),
            opt("noise", Real),
            opt("snr", Real),
            opt("clean_voice_pct", Real),
            col("audio", Bool),
            col("json", Bool),
            opt("recorder", Text),
            col("record", Json),
            col("indexed_ms", BigInt),
        ],
        key: &[],
    },
    Table {
        name: "call_transmissions",
        cols: &[
            of("call_id", BigInt, "calls(id)"),
            col("seq", Int),
            col("unit", BigInt),
            col("start_ms", BigInt),
            col("length_ms", BigInt),
            col("emergency", Bool),
            opt("signal_system", Text),
            opt("tag", Text),
        ],
        key: &["call_id", "seq"],
    },
    Table {
        name: "call_frequencies",
        cols: &[
            of("call_id", BigInt, "calls(id)"),
            col("seq", Int),
            col("freq_hz", BigInt),
            col("start_ms", BigInt),
            col("length_ms", BigInt),
            col("error_count", BigInt),
            col("spike_count", BigInt),
        ],
        key: &["call_id", "seq"],
    },
    Table {
        name: "call_units",
        cols: &[col("short_name", Text), col("unit", BigInt), col("start_ms", BigInt), of("call_id", BigInt, "calls(id)")],
        key: &["short_name", "unit", "start_ms", "call_id"],
    },
    Table { name: "kept", cols: &[col("kind", Text), col("key", Text), col("value", Text)], key: &["kind", "key"] },
    Table { name: "registry", cols: &[col("system", Text), col("whole", Json)], key: &["system"] },
    Table { name: "registry_changes", cols: &[col("id", Id), col("system", Text), col("changes", Json)], key: &[] },
    Table {
        name: "talkgroups",
        cols: &[
            col("system", Text),
            col("seq", Int),
            col("line", Text),
            opt("number", BigInt),
            opt("alpha_tag", Text),
            opt("description", Text),
            opt("tag", Text),
            opt("category", Text),
            opt("mode", Text),
            opt("priority", BigInt),
            opt("ignore", Bool),
        ],
        key: &["system", "seq"],
    },
    Table { name: "stats_minutes", cols: &[col("id", Id), col("t", BigInt), col("s", Json)], key: &[] },
    Table {
        name: "stats_hours",
        cols: &[col("series", Text), col("t", BigInt), col("avg", Real), col("lo", Real), col("hi", Real), col("n", BigInt)],
        key: &["series", "t"],
    },
];

pub const INDEXES: &[Index] = &[
    Index { name: "calls_start", table: "calls", cols: &["start_ms"] },
    Index { name: "calls_system_start", table: "calls", cols: &["short_name", "start_ms"] },
    Index { name: "calls_system_tg_start", table: "calls", cols: &["short_name", "talkgroup", "start_ms"] },
    Index { name: "talkgroups_number", table: "talkgroups", cols: &["system", "number"] },
    Index { name: "stats_minutes_t", table: "stats_minutes", cols: &["t"] },
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
                if let Some(p) = c.cascade {
                    s.push_str(&format!(" references {p} on delete cascade"));
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
        out.push_str(&format!("create index {} on {}({});\n", i.name, i.table, i.cols.join(", ")));
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
    }

    /// Every page the query builder makes walks an index in order to its
    /// limit, never sorting all the calls it matches: every call, a system,
    /// several, a talkgroup, a radio, and the paging cursor's parts.
    #[test]
    fn every_page_walks_an_index_without_sorting() {
        use crate::db::read::{CallsFilter, Where, COLUMNS};
        let c = Connection::open_in_memory().unwrap();
        prepare(&c).unwrap();
        let plan = |f: CallsFilter| -> String {
            let (sql, ps) = Where::of(&f).query(COLUMNS, 101);
            let ps: Vec<rusqlite::types::Value> = ps.into_iter().map(|p| match p {
                crate::db::sql::V::Int(i) => i.into(),
                crate::db::sql::V::Text(t) => t.into(),
                v => panic!("{v:?}"),
            }).collect();
            let mut st = c.prepare(&format!("explain query plan {}", crate::db::sql::question_marks(&sql))).unwrap();
            let rows: Vec<String> = st.query_map(rusqlite::params_from_iter(ps), |r| r.get::<_, String>(3)).unwrap().map(Result::unwrap).collect();
            rows.join(" | ")
        };
        let f = |systems: &[&str], tg: Option<u32>, unit: Option<i64>| CallsFilter { systems: systems.iter().map(|s| s.to_string()).collect(), talkgroup: tg, unit, before_ms: Some(5), limit: 100, ..Default::default() };
        for (what, filter, index) in [
            ("every call", f(&[], None, None), "calls_start"),
            ("a system", f(&["a"], None, None), "calls_system_start"),
            ("two systems", f(&["a", "b"], None, None), "calls_system_start"),
            ("a talkgroup", f(&["a"], Some(1), None), "calls_system_tg_start"),
            ("a talkgroup on two", f(&["a", "b"], Some(1), None), "calls_system_tg_start"),
            // (call_units, as `u`: its key is system, radio, start.)
            ("a radio", f(&["a"], None, Some(7)), "SEARCH u USING PRIMARY KEY (short_name=? AND unit=? AND start_ms<?)"),
            ("a radio on two", f(&["a", "b"], None, Some(7)), "SEARCH u USING PRIMARY KEY (short_name=? AND unit=? AND start_ms<?)"),
        ] {
            let p = plan(filter);
            assert!(p.contains(index), "{what}: {p}");
            // A sort only ever of a system's part once it's cut to the limit (the union
            // merging them), right after that part's scan: never of the calls matched.
            let steps: Vec<&str> = p.split(" | ").collect();
            for (i, s) in steps.iter().enumerate() {
                if s.contains("TEMP B-TREE") {
                    assert!(i > 0 && steps[i - 1].starts_with("SCAN a"), "{what} sorts what it matched: {p}");
                }
            }
        }
    }
}
