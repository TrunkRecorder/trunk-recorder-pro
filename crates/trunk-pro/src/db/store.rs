//! The store: everything kept, read and written through the one SQL layer
//! ([`super::sql`]), the same for every backend. A [`Backend`] only connects:
//! SQLite (`sqlite.rs`) or PostgreSQL (`postgres.rs`) hands [`Store`] an
//! [`Exec`] in a transaction (the writer's) or on a read connection.
//! Everything above is shared too: the writer thread's batching and
//! retention timing (`writer.rs`), the live list merged into every query
//! (`runtime::calls`), the paging rules (`read.rs`), and the backfill.

use trunk_app::calls::{CallsPage, CallsQuery, StoredCall};
use trunk_app::stats::radio::{RegistryChanges, SystemRadio};
use trunk_app::stats::Bucket;
use trunk_core::trunk::{TalkgroupRow, UnitAlias};

use super::read::{self, Where};
use super::row::{call_values, src_entry, stored_call, tx_values, CallRow, CALL_COLS, TX_COLS};
use super::schema::SCHEMA_VERSION;
#[cfg(test)]
use super::sql::Dialect;
use super::sql::{savepoint, Exec, Params, V};
use crate::vals;

/// A change for the store, as the writer hands it over in a batch: each is
/// whole or not at all, and one that fails is logged and the rest go on.
#[allow(clippy::large_enum_variant)] // one row per call; boxing it would only add a hop
pub enum Change {
    /// The config's systems (id, short name, kind): kept by id, a renamed one
    /// renamed, one gone left (its calls still have a name).
    Systems(Vec<(i64, String, String)>),
    /// A call's row and its children: a live one replaces what is there, a
    /// backfilled one only fills a gap.
    Call { row: CallRow, live: bool },
    /// Which of a call's files are kept.
    CallFiles { path: String, audio: bool, json: bool },
    /// A `meta` value (the backfill's markers).
    Meta { key: String, value: String },
    /// A document kept between runs, by kind and key (a band plan, a
    /// system's talker aliases, AutoTune's scores…: `crate::kept`); `None`
    /// removes it.
    Kept { kind: String, key: String, value: Option<String> },
    /// What changed in a system's radio registry since the last
    /// ([`COMPACT_AFTER`] says how it's kept).
    Registry { system_id: i64, changes: Box<RegistryChanges> },
    /// A minute of the dashboard's history: a [`trunk_app::stats::Rollup`]
    /// as its JSON line (a minute may have several, merged when read).
    StatsMinute { t: i64, json: String },
    /// Hours of the dashboard's history, in place of any there.
    StatsHours(Vec<trunk_app::stats::HourRow>),
    /// The dashboard's minutes before `minutes_before` go, and its hours
    /// before `hours_before` (none: kept).
    RetireStats { minutes_before: i64, hours_before: Option<i64> },
    /// A system's talkgroups, in place of what's stored (a file loaded).
    /// Committed at once, as [`Change::Talkgroup`]: people make these.
    Talkgroups { system_id: i64, rows: Vec<TalkgroupRow> },
    /// One talkgroup of a system edited (`order`: its place among them).
    Talkgroup { system_id: i64, row: TalkgroupRow, order: usize },
    /// A system's talker aliases now (what changed is written).
    Units { system_id: i64, aliases: Vec<(u32, UnitAlias)> },
}

impl Change {
    /// What it is, for the log when it fails.
    pub fn what(&self) -> String {
        match self {
            Change::Systems(_) => "systems write".into(),
            Change::Call { row, .. } => format!("call write {}", row.path),
            Change::CallFiles { path, .. } => format!("files write {path}"),
            Change::Meta { key, .. } => format!("meta write {key}"),
            Change::Kept { kind, key, .. } => format!("{kind} {key} write"),
            Change::Registry { system_id, .. } => format!("registry write (system {system_id})"),
            Change::StatsMinute { .. } | Change::StatsHours(_) => "stats write".into(),
            Change::RetireStats { .. } => "stats retention".into(),
            Change::Talkgroups { system_id, .. } | Change::Talkgroup { system_id, .. } => format!("talkgroups (system {system_id})"),
            Change::Units { system_id, .. } => format!("talker aliases write (system {system_id})"),
        }
    }

    /// Whether it should be committed at once, not at the writer's interval
    /// (people make it).
    pub fn urgent(&self) -> bool {
        matches!(self, Change::Talkgroups { .. } | Change::Talkgroup { .. })
    }
}

/// A connection to a database: all a backend does. Blocking: the writer is
/// a thread of its own, and queries run under `spawn_blocking`. **Never call
/// one from async code** (a tokio worker): PostgreSQL's client refuses
/// outright (it runs a runtime of its own).
pub trait Backend: Send + Sync {
    /// Its name, as `storage.backend` says it.
    fn kind(&self) -> &'static str;
    /// Where it is (the SQLite file, the database without its password): no query.
    fn location(&self) -> String;
    /// `f` in a transaction on the writer's connection, committed if it's Ok.
    fn write(&self, f: &mut dyn FnMut(&mut dyn Exec) -> Result<(), String>) -> Result<(), String>;
    /// `f` on a read connection.
    fn read(&self, f: &mut dyn FnMut(&mut dyn Exec) -> Result<(), String>) -> Result<(), String>;
    /// Fold what's pending into the store proper (SQLite: the WAL), at exit.
    fn checkpoint(&self) {}
    /// The writer is going: the last tidy-up (SQLite: `pragma optimize`).
    fn close(&self) {}
    /// Copy `tables` (what can't be rebuilt) somewhere of its own; where
    /// (none: the database's operator backs it up, as PostgreSQL's).
    fn backup(&self, _tables: &[&str]) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// What a backup keeps: what people made and the recorder learned, which
/// can't be made again (calls can, from their files; the dashboard's
/// history is the dashboard's). Small: written once a day.
pub const KEEP_SAFE: &[&str] = &["systems", "talkgroups", "units", "kept", "registry", "registry_changes", "meta"];

/// The store over a backend. Only [`Backend::kind`] and [`Store::location`]
/// are free to call from async code.
pub struct Store {
    backend: Box<dyn Backend>,
}

impl Store {
    pub fn new(backend: Box<dyn Backend>) -> Store {
        Store { backend }
    }

    pub fn kind(&self) -> &'static str {
        self.backend.kind()
    }

    pub fn location(&self) -> String {
        self.backend.location()
    }

    fn read<T>(&self, f: impl FnOnce(&mut dyn Exec) -> Result<T, String>) -> Result<T, String> {
        let (mut f, mut out) = (Some(f), None);
        self.backend.read(&mut |x| {
            out = Some(f.take().expect("run once")(x)?);
            Ok(())
        })?;
        out.ok_or_else(|| "nothing read".into())
    }

    fn write<T>(&self, f: impl FnOnce(&mut dyn Exec) -> Result<T, String>) -> Result<T, String> {
        let (mut f, mut out) = (Some(f), None);
        self.backend.write(&mut |x| {
            out = Some(f.take().expect("run once")(x)?);
            Ok(())
        })?;
        out.ok_or_else(|| "nothing written".into())
    }

    /// One batch in one transaction; each change in a savepoint, whole or
    /// not at all, and one that fails is logged and the rest go on. What
    /// people made that failed (a talkgroup edit), said for them.
    pub fn apply(&self, batch: Vec<Change>) -> Result<Vec<String>, String> {
        self.write(|x| {
            let mut failed = Vec::new();
            for change in &batch {
                if let Err(e) = savepoint(x, |x| apply_one(x, change)) {
                    log::warn!("Store: {} failed: {e}", change.what());
                    if change.urgent() {
                        failed.push(format!("Couldn't save the {}: {e}", change.what()));
                    }
                }
            }
            Ok(failed)
        })
    }

    /// A page of the calls `q` asks for, of `systems` (none: any), newest
    /// first: up to its limit and, when more remain, the cursor after them.
    /// Without the live list (`runtime::calls` merges it in).
    pub fn calls(&self, q: &CallsQuery, systems: &[i64]) -> Result<CallsPage, String> {
        let limit = trunk_app::calls::limit(q);
        let (sql, ps) = Where::of(q, systems).query(&read::columns(), limit as i64 + 1);
        let found = self.read(|x| calls_of(x, &sql, &ps))?;
        Ok(trunk_app::calls::cut(found, limit))
    }

    /// The call filed under `path`.
    pub fn one(&self, path: &str) -> Result<Option<StoredCall>, String> {
        let sql = format!("select {} from calls c join systems s on s.id = c.system_id where c.path = $1", read::columns());
        self.read(|x| Ok(calls_of(x, &sql, vals![path])?.pop()))
    }

    /// Whether a call is filed under `path` (the backfill skips it).
    pub fn known(&self, path: &str) -> Result<bool, String> {
        self.read(|x| Ok(x.query_opt("select 1 from calls where path = $1", vals![path])?.is_some()))
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>, String> {
        self.read(|x| Ok(x.query_opt("select value from meta where key = $1", vals![key])?.map(|r| r.text(0))))
    }

    /// A document kept between runs ([`Change::Kept`]).
    pub fn kept(&self, kind: &str, key: &str) -> Result<Option<String>, String> {
        self.read(|x| Ok(x.query_opt("select value from kept where kind = $1 and key = $2", vals![kind, key])?.map(|r| r.text(0))))
    }

    /// Every system's radio registry, as stored (at startup); none: what's
    /// stored of it couldn't be read.
    pub fn registry(&self) -> Result<Vec<(i64, Option<SystemRadio>)>, String> {
        self.read(|x| {
            let mut ids: Vec<i64> = x.query("select system_id from registry union select system_id from registry_changes", &[])?.iter().map(|r| r.int(0)).collect();
            ids.sort_unstable();
            ids.into_iter().map(|id| Ok((id, registry_of(x, id)?))).collect()
        })
    }

    /// A system's talkgroups, in their order.
    pub fn talkgroups(&self, system_id: i64) -> Result<Vec<TalkgroupRow>, String> {
        let sql = format!("select {} from talkgroups where system_id = $1 order by sort_order", TG_COLS.join(", "));
        self.read(|x| Ok(x.query(&sql, vals![system_id])?.iter().map(talkgroup_row).collect()))
    }

    /// A system's talker aliases.
    pub fn units(&self, system_id: i64) -> Result<Vec<(u32, UnitAlias)>, String> {
        self.read(|x| units_of(x, system_id))
    }

    /// The dashboard's minutes from `since` (Unix s) on, as JSON lines, oldest first.
    pub fn stats_minutes(&self, since: i64) -> Result<Vec<String>, String> {
        self.read(|x| Ok(x.query("select s from stats_minutes where t >= $1 order by t, id", vals![since])?.iter().map(|r| r.text(0)).collect()))
    }

    /// The dashboard's hours, added up by bucket ([`HoursQuery`]): (series,
    /// bucket, what its hours add up to).
    pub fn stats_hours(&self, q: &HoursQuery) -> Result<Vec<(String, i64, Bucket)>, String> {
        let (sql, ps) = q.sql();
        let bucket = |r: &super::sql::Row| Bucket { sum: r.real(2), n: r.int(3) as u32, lo: r.real(4) as f32, hi: r.real(5) as f32 };
        self.read(|x| Ok(x.query(&sql, &ps)?.iter().map(|r| (r.text(0), r.int(1), bucket(r))).collect()))
    }

    /// Delete up to `max` calls that started before `before_ms`, oldest
    /// first, with their children; how many went.
    pub fn retire(&self, before_ms: i64, max: usize) -> Result<usize, String> {
        self.write(|x| Ok(x.execute("delete from calls where id in (select id from calls where start_ms < $1 order by start_ms limit $2)", vals![before_ms, max])? as usize))
    }

    /// The newest call's start (retention counts back from it at most a day).
    pub fn newest_start(&self) -> Result<Option<i64>, String> {
        self.read(|x| Ok(x.query_opt("select max(start_ms) from calls", &[])?.and_then(|r| r.opt_int(0))))
    }

    pub fn info(&self) -> Result<StoreInfo, String> {
        let (calls, oldest_ms, newest_ms) = self.read(|x| {
            let r = x.query_opt("select count(*), min(start_ms), max(start_ms) from calls", &[])?.ok_or("no answer")?;
            Ok((r.int(0), r.opt_int(1), r.opt_int(2)))
        })?;
        Ok(StoreInfo { location: self.location(), schema_version: SCHEMA_VERSION, calls, oldest_ms, newest_ms })
    }

    pub fn checkpoint(&self) {
        self.backend.checkpoint()
    }

    pub fn close(&self) {
        self.backend.close()
    }

    /// Back up what can't be rebuilt ([`KEEP_SAFE`]); where, if anywhere.
    pub fn backup(&self) -> Result<Option<String>, String> {
        self.backend.backup(KEEP_SAFE)
    }

    /// A number from a plain SQL query, the same in every dialect (tests).
    #[cfg(test)]
    pub fn scalar(&self, sql: &str) -> i64 {
        self.read(|x| Ok(x.query_opt(sql, &[])?.map_or(0, |r| r.int(0)))).unwrap()
    }

    /// Statements run as the writer (tests).
    #[cfg(test)]
    pub fn batch_for_test(&self, sql: &str) {
        self.write(|x| x.batch(sql)).unwrap()
    }

    /// The first column of each row of a plain SQL query (tests).
    #[cfg(test)]
    pub fn texts(&self, sql: &str) -> Vec<String> {
        self.read(|x| Ok(x.query(sql, &[])?.iter().map(|r| r.text(0)).collect())).unwrap()
    }

    /// `sql`'s query plan with `ps`, a line each (tests).
    #[cfg(test)]
    pub fn plan(&self, sql: &str, ps: &[V]) -> Vec<String> {
        self.read(|x| {
            let prefix = if x.dialect() == Dialect::Postgres { "explain " } else { "explain query plan " };
            let rows = x.query(&format!("{prefix}{sql}"), ps)?;
            Ok(rows.iter().map(|r| r.0.iter().map(|v| match v {
                V::Text(t) => t.clone(),
                v => format!("{v:?}"),
            }).collect::<Vec<_>>().join(" ")).collect())
        })
        .unwrap()
    }

    /// Keep the radio index (`on`), made from the transmissions stored if
    /// it was dropped, or drop it.
    pub fn radio_index(&self, on: bool) -> Result<(), String> {
        let index = super::schema::INDEXES.iter().find(|i| i.name == super::schema::RADIO_INDEX).expect("the radio index");
        let sql = if on { index.create() } else { format!("drop index if exists {}", index.name) };
        self.write(|x| x.batch(&sql))
    }

    /// Make system `system_id`'s registry changes over its whole copy now (tests).
    #[cfg(test)]
    pub fn compact_registry(&self, system_id: i64) {
        self.write(|x| compact_registry(x, system_id)).unwrap()
    }

    /// A trigger refusing rows of `table` whose `column` is `value` (an SQL
    /// literal), to make a change fail part-way (tests).
    #[cfg(test)]
    pub fn refuse(&self, table: &str, column: &str, value: &str) {
        self.write(|x| {
            let sql = match x.dialect() {
                Dialect::Sqlite => format!("create trigger refuse_{table} before insert on {table} when new.{column} = {value} begin select raise(abort, 'refused'); end;"),
                Dialect::Postgres => format!(
                    "create or replace function refuse_{table}() returns trigger as $$ begin if new.{column} = {value} then raise exception 'refused'; end if; return new; end $$ language plpgsql;
                     create trigger refuse_{table} before insert on {table} for each row execute function refuse_{table}();"
                ),
            };
            x.batch(&sql)
        })
        .unwrap()
    }
}

/// One change, in the savepoint `x` is in.
fn apply_one(x: &mut dyn Exec, change: &Change) -> Result<(), String> {
    match change {
        Change::Systems(rows) => {
            for (id, short_name, kind) in rows {
                x.execute("insert into systems (id, short_name, kind) values ($1, $2, $3) on conflict (id) do update set short_name = excluded.short_name, kind = excluded.kind", vals![*id, short_name, kind])?;
            }
            Ok(())
        }
        Change::Call { row, live } => write_call(x, row, *live),
        Change::CallFiles { path, audio, json } => x.execute("update calls set audio = $2, json = $3 where path = $1", vals![path, *audio, *json]).map(drop),
        Change::Meta { key, value } => x.execute("insert into meta (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", vals![key, value]).map(drop),
        Change::Kept { kind, key, value: Some(v) } => x.execute("insert into kept (kind, key, value) values ($1, $2, $3) on conflict (kind, key) do update set value = excluded.value", vals![kind, key, v]).map(drop),
        Change::Kept { kind, key, value: None } => x.execute("delete from kept where kind = $1 and key = $2", vals![kind, key]).map(drop),
        Change::Registry { system_id, changes } => {
            let json = serde_json::to_string(changes).map_err(|e| e.to_string())?;
            x.execute("insert into registry_changes (system_id, changes) values ($1, $2)", vals![*system_id, json])?;
            let n = x.query_opt("select count(*) from registry_changes where system_id = $1", vals![*system_id])?.map_or(0, |r| r.int(0));
            if n >= COMPACT_AFTER {
                compact_registry(x, *system_id)?;
            }
            Ok(())
        }
        Change::StatsMinute { t, json } => x.execute("insert into stats_minutes (t, s) values ($1, $2)", vals![*t, json]).map(drop),
        Change::StatsHours(rows) => {
            for r in rows {
                let id = series_id(x, &r.series)?;
                x.execute(
                    "insert into stats_hours (series_id, t, avg, lo, hi, n) values ($1, $2, $3, $4, $5, $6)
                     on conflict (series_id, t) do update set avg = excluded.avg, lo = excluded.lo, hi = excluded.hi, n = excluded.n",
                    vals![id, r.t, r.avg, r.lo, r.hi, r.n],
                )?;
            }
            Ok(())
        }
        Change::RetireStats { minutes_before, hours_before } => {
            x.execute("delete from stats_minutes where t < $1", vals![*minutes_before])?;
            if let Some(h) = hours_before {
                // (Each series by its key: a range of the index, not a scan of every hour.)
                for id in x.query("select id from series", &[])?.iter().map(|r| r.int(0)) {
                    x.execute("delete from stats_hours where series_id = $1 and t < $2", vals![id, *h])?;
                }
            }
            Ok(())
        }
        Change::Talkgroups { system_id, rows } => {
            x.execute("delete from talkgroups where system_id = $1", vals![*system_id])?;
            rows.iter().enumerate().try_for_each(|(i, r)| write_talkgroup(x, *system_id, r, i))
        }
        Change::Talkgroup { system_id, row, order } => write_talkgroup(x, *system_id, row, *order),
        Change::Units { system_id, aliases } => {
            // (What changed alone: an alias heard again is no write.)
            let had: std::collections::HashMap<u32, UnitAlias> = units_of(x, *system_id)?.into_iter().collect();
            for (unit, a) in aliases {
                if had.get(unit) != Some(a) {
                    x.execute(
                        "insert into units (system_id, unit, alias, source, heard, wacn, sys, talkgroup) values ($1, $2, $3, $4, $5, $6, $7, $8)
                         on conflict (system_id, unit) do update set alias = excluded.alias, source = excluded.source, heard = excluded.heard, wacn = excluded.wacn, sys = excluded.sys, talkgroup = excluded.talkgroup",
                        vals![*system_id, *unit, &a.alias, &a.source, a.time, &a.wacn, &a.sys, a.talkgroup],
                    )?;
                }
            }
            let now: std::collections::HashSet<u32> = aliases.iter().map(|(u, _)| *u).collect();
            for unit in had.keys().filter(|u| !now.contains(u)) {
                x.execute("delete from units where system_id = $1 and unit = $2", vals![*system_id, *unit])?;
            }
            Ok(())
        }
    }
}

/// The calls `sql` finds (columns [`read::columns`]), each with its radios'
/// transmissions (one more query for them all).
fn calls_of(x: &mut dyn Exec, sql: &str, ps: &[V]) -> Result<Vec<StoredCall>, String> {
    let rows = x.query(sql, ps)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids = Params::default();
    let marks: Vec<String> = rows.iter().map(|r| ids.add(r.int(0))).collect();
    let mut txs: std::collections::HashMap<i64, Vec<trunk_recorder_plugin::SrcEntry>> = Default::default();
    for t in x.query(&format!("select {} from call_transmissions where call_id in ({}) order by call_id, seq", TX_COLS.join(", "), marks.join(", ")), &ids.0)? {
        txs.entry(t.int(0)).or_default().push(src_entry(&t, 0));
    }
    Ok(rows.iter().map(|r| stored_call(r, 2, r.text(1), txs.remove(&r.int(0)).unwrap_or_default())).collect())
}

/// Write a call's row and its transmissions. A live call replaces what is
/// there; a backfilled one only fills a gap, and brings its transmissions
/// only if it landed.
fn write_call(x: &mut dyn Exec, row: &CallRow, live: bool) -> Result<(), String> {
    let conflict = if live {
        format!("on conflict (path) do update set {}", CALL_COLS[1..].iter().map(|c| format!("{c} = excluded.{c}")).collect::<Vec<_>>().join(", "))
    } else {
        "on conflict (path) do nothing".into()
    };
    let marks: Vec<String> = (1..=CALL_COLS.len()).map(|i| format!("${i}")).collect();
    let sql = format!("insert into calls ({}) values ({}) {conflict} returning id", CALL_COLS.join(", "), marks.join(", "));
    let Some(landed) = x.query_opt(&sql, &call_values(row))? else { return Ok(()) };
    let id = landed.int(0);
    x.execute("delete from call_transmissions where call_id = $1", vals![id])?;
    let tx_sql = format!("insert into call_transmissions ({}) values ({})", TX_COLS.join(", "), (1..=TX_COLS.len()).map(|i| format!("${i}")).collect::<Vec<_>>().join(", "));
    let mut seen = std::collections::HashSet::new();
    for (seq, s) in row.record.src_list.iter().enumerate() {
        x.execute(&tx_sql, &tx_values(id, seq, row, s, seen.insert(s.src)))?;
    }
    Ok(())
}

/// A system's radio registry is kept in one shape, [`RegistryChanges`]:
/// each 15 minutes' changes appended to `registry_changes`, and once this
/// many have piled up (a day's), made over the whole copy in `registry`
/// ([`SystemRadio::whole`]) and let go. Appended, not rows updated in
/// place: a busy system changes thousands of rows every 15 minutes, which
/// as row updates writes several times as much (`registry_write_budget`).
pub const COMPACT_AFTER: i64 = 96;

/// System `system_id`'s registry as stored (none: nothing stored that could be read).
fn registry_of(x: &mut dyn Exec, system_id: i64) -> Result<Option<SystemRadio>, String> {
    let whole = x.query_opt("select whole from registry where system_id = $1", vals![system_id])?.map(|r| r.text(0));
    let changes: Vec<String> = x.query("select changes from registry_changes where system_id = $1 order by id", vals![system_id])?.iter().map(|r| r.text(0)).collect();
    // (A change that can't be read: the registry as it was before it.)
    let mut each = changes.iter().map_while(|j| serde_json::from_str::<RegistryChanges>(j).ok());
    // (A whole copy that can't be read: the changes since alone, and the next compaction writes it afresh.)
    let whole = whole.and_then(|j| serde_json::from_str::<RegistryChanges>(&j).inspect_err(|e| log::warn!("Store: radio registry of system {system_id}: its whole copy can't be read ({e})")).ok());
    let mut radio = match &whole {
        Some(w) => SystemRadio::from_changes(w),
        None => match each.next() {
            Some(c) => SystemRadio::from_changes(&c),
            None => return Ok(None),
        },
    };
    for c in each {
        radio.apply(&c);
    }
    Ok(Some(radio))
}

/// Make system `system_id`'s changes over its whole copy, and let them go.
fn compact_registry(x: &mut dyn Exec, system_id: i64) -> Result<(), String> {
    let Some(radio) = registry_of(x, system_id)? else { return Ok(()) };
    let json = serde_json::to_string(&radio.whole()).map_err(|e| e.to_string())?;
    x.execute("insert into registry (system_id, whole) values ($1, $2) on conflict (system_id) do update set whole = excluded.whole", vals![system_id, json])?;
    x.execute("delete from registry_changes where system_id = $1", vals![system_id]).map(drop)
}

/// `talkgroups`' columns, in the order [`write_talkgroup`] gives them and
/// [`talkgroup_row`] reads them.
const TG_COLS: &[&str] = &["system_id", "number", "alpha_tag", "mode", "description", "tag", "category", "priority", "preferred_nac", "preferred_site", "ignore", "sort_order", "comment", "extra"];

/// Talkgroup `r` of system `system_id`, at `order` among them.
fn write_talkgroup(x: &mut dyn Exec, system_id: i64, r: &TalkgroupRow, order: usize) -> Result<(), String> {
    let t = &r.tg;
    let extra = (!r.extra.is_empty()).then(|| serde_json::to_string(&r.extra).unwrap_or_default());
    let marks: Vec<String> = (1..=TG_COLS.len()).map(|i| format!("${i}")).collect();
    let set: Vec<String> = TG_COLS[2..].iter().map(|c| format!("{c} = excluded.{c}")).collect();
    let sql = format!("insert into talkgroups ({}) values ({}) on conflict (system_id, number) do update set {}", TG_COLS.join(", "), marks.join(", "), set.join(", "));
    let ps = vals![system_id, t.number, &t.alpha_tag, &t.mode, &t.description, &t.tag, &t.group, i64::from(t.priority), t.preferred_nac, &t.preferred_site, t.ignore, order, &r.comment, extra];
    x.execute(&sql, ps).map(drop)
}

/// A talkgroup read back (columns [`TG_COLS`]).
fn talkgroup_row(r: &super::sql::Row) -> TalkgroupRow {
    let tg = trunk_core::trunk::Talkgroup {
        number: r.int(1) as u32,
        alpha_tag: r.text(2),
        mode: r.text(3),
        description: r.text(4),
        tag: r.text(5),
        group: r.text(6),
        priority: r.int(7) as i32,
        preferred_nac: r.int(8) as u32,
        preferred_site: r.text(9),
        ignore: r.bool(10),
    };
    TalkgroupRow { tg, comment: r.text(12), extra: r.opt_text(13).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
}

/// System `system_id`'s talker aliases as stored.
fn units_of(x: &mut dyn Exec, system_id: i64) -> Result<Vec<(u32, UnitAlias)>, String> {
    let rows = x.query("select unit, alias, source, heard, wacn, sys, talkgroup from units where system_id = $1 order by unit", vals![system_id])?;
    Ok(rows.iter().map(|r| (r.int(0) as u32, UnitAlias { alias: r.text(1), source: r.text(2), time: r.int(3), wacn: r.text(4), sys: r.text(5), talkgroup: r.opt_int(6).map(|t| t as u32) })).collect())
}

/// Hours of the dashboard's history wanted: the series matching `patterns`
/// (names, or prefixes ending in `*`), from `from` to before `before`
/// (Unix s), added up into `span`'s buckets.
pub struct HoursQuery<'a> {
    pub patterns: &'a [String],
    pub span: trunk_app::stats::Span,
    pub from: i64,
    pub before: i64,
}

/// Series `name`'s id, given one if it has none.
fn series_id(x: &mut dyn Exec, name: &str) -> Result<i64, String> {
    if let Some(r) = x.query_opt("select id from series where name = $1", vals![name])? {
        return Ok(r.int(0));
    }
    x.execute("insert into series (name) values ($1)", vals![name])?;
    Ok(x.query_opt("select id from series where name = $1", vals![name])?.map_or(0, |r| r.int(0)))
}

impl HoursQuery<'_> {
    /// The query and its parameters: each bucket's sum, so a year of hours
    /// comes back as a few hundred rows.
    pub fn sql(&self) -> (String, Vec<V>) {
        let mut ps = Params::default();
        let (t0, step, from, before) = (ps.add(self.span.m0 * 60), ps.add(self.span.step * 60), ps.add(self.from), ps.add(self.before));
        let mut any = Vec::new();
        for p in self.patterns {
            match p.strip_suffix('*') {
                Some("") => any.push("1 = 1".to_string()),
                Some(prefix) => match after(prefix) {
                    Some(end) => any.push(format!("(s.name >= {} and s.name < {})", ps.add(prefix), ps.add(end))),
                    None => any.push(format!("s.name >= {}", ps.add(prefix))),
                },
                None => any.push(format!("s.name = {}", ps.add(p))),
            }
        }
        if any.is_empty() {
            any.push("1 = 0".into());
        }
        // (The series the patterns name, then each one's hours by its key.)
        let sql = format!(
            "select s.name, (h.t - {t0}) / {step} as b, cast(sum(h.avg * h.n) as double precision), cast(sum(h.n) as bigint), cast(min(h.lo) as double precision), cast(max(h.hi) as double precision)
             from series s join stats_hours h on h.series_id = s.id where h.t >= {from} and h.t < {before} and ({}) group by s.name, b",
            any.join(" or ")
        );
        (sql, ps.0)
    }
}

/// The first string after every string starting with `prefix`: its last
/// character one higher (none: nothing is).
fn after(prefix: &str) -> Option<String> {
    let mut s: Vec<char> = prefix.chars().collect();
    while let Some(c) = s.pop() {
        if let Some(next) = char::from_u32(c as u32 + 1) {
            s.push(next);
            return Some(s.into_iter().collect());
        }
    }
    None
}


/// What `GET /api/v1/storage` says of a store.
pub struct StoreInfo {
    /// Where it is: the SQLite file, or the PostgreSQL database (no password).
    pub location: String,
    pub schema_version: u32,
    pub calls: i64,
    pub oldest_ms: Option<i64>,
    pub newest_ms: Option<i64>,
}
