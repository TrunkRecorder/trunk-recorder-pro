//! The store: everything kept, read and written through the one SQL layer
//! ([`super::sql`]), the same for every backend. A [`Backend`] only connects:
//! SQLite (`sqlite.rs`) or PostgreSQL (`postgres.rs`) hands [`Store`] an
//! [`Exec`] in a transaction (the writer's) or on a read connection.
//! Everything above is shared too: the writer thread's batching and
//! retention timing (`writer.rs`), the live list merged into every query
//! (`runtime::calls`), the paging rules (`read.rs`), and the backfill.

use serde_json::Value;

use trunk_app::stats::radio::{RegistryChanges, SystemRadio};

use super::read::{self, CallsFilter, Fetched, Page, Where, COLUMNS};
use super::row::{entry, CallRow};
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
    /// What changed in a system's radio registry since the last ([`StoredRegistry`]).
    Registry { system_id: i64, changes: Box<RegistryChanges> },
    /// A system's whole radio registry, in place of what's stored.
    RegistryWhole { system_id: i64, radio: Box<SystemRadio> },
    /// A minute of the dashboard's history: a [`trunk_app::stats::Rollup`]
    /// as its JSON line (a minute may have several, merged when read).
    StatsMinute { t: i64, json: String },
    /// Hours of the dashboard's history, in place of any there.
    StatsHours(Vec<trunk_app::stats::HourRow>),
    /// The dashboard's minutes before `minutes_before` go, and its hours
    /// before `hours_before` (none: kept).
    RetireStats { minutes_before: i64, hours_before: Option<i64> },
    /// A system's talkgroup file edited from `old` to `new` ("" for `old`:
    /// in place of what's stored; "" for `new`: none). Committed at once:
    /// people make these.
    Talkgroups { system_id: i64, old: String, new: String },
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
            Change::Registry { system_id, .. } | Change::RegistryWhole { system_id, .. } => format!("registry write (system {system_id})"),
            Change::StatsMinute { .. } | Change::StatsHours(_) => "stats write".into(),
            Change::RetireStats { .. } => "stats retention".into(),
            Change::Talkgroups { system_id, .. } => format!("talkgroups write (system {system_id})"),
        }
    }

    /// Whether it should be committed at once, not at the writer's interval
    /// (people make it).
    pub fn urgent(&self) -> bool {
        matches!(self, Change::Talkgroups { .. })
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
}

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
    /// not at all, and one that fails is logged and the rest go on.
    pub fn apply(&self, batch: Vec<Change>) -> Result<(), String> {
        self.write(|x| {
            for change in &batch {
                if let Err(e) = savepoint(x, |x| apply_one(x, change)) {
                    log::warn!("Call store: {} failed: {e}", change.what());
                }
            }
            Ok(())
        })
    }

    /// A page of the calls `f` wants, newest first, equal starts whole.
    pub fn page(&self, f: &CallsFilter) -> Result<Page, String> {
        self.read(|x| {
            let mut fetch = |w: &Where, limit: usize| -> Result<(Vec<Fetched>, bool), String> {
                let want = i64::try_from(limit).unwrap_or(i64::MAX - 1);
                let (sql, ps) = w.query(COLUMNS, want.saturating_add(1));
                let rows = x.query(&sql, &ps)?;
                let extra = rows.len() as i64 > want;
                let out = rows.iter().take(want as usize).map(|r| Fetched { entry: entry(&r.text(0), &r.text(1), r.bool(2), r.bool(3)), id: r.int(4), start_ms: r.int(5) }).collect();
                Ok((out, extra))
            };
            read::paged(f, &mut fetch)
        })
    }

    /// The call filed under `path`, as its entry.
    pub fn one(&self, path: &str) -> Result<Option<Value>, String> {
        self.read(|x| Ok(x.query_opt("select path, record, audio, json from calls where path = $1", vals![path])?.map(|r| entry(&r.text(0), &r.text(1), r.bool(2), r.bool(3)))))
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

    /// Every system's radio registry, as stored (at startup).
    pub fn registry(&self) -> Result<Vec<StoredRegistry>, String> {
        self.read(|x| {
            let whole = x.query("select system_id, whole from registry", &[])?.iter().map(|r| (r.int(0), r.text(1))).collect();
            let changes = x.query("select system_id, changes from registry_changes order by id", &[])?.iter().map(|r| (r.int(0), r.text(1))).collect();
            Ok(stored_registry(whole, changes))
        })
    }

    /// A system's talkgroup file, as stored ([`talkgroup_file`]).
    pub fn talkgroups(&self, system_id: i64) -> Result<Option<String>, String> {
        self.read(|x| Ok(talkgroup_file(x.query("select line from talkgroups where system_id = $1 order by seq", vals![system_id])?.iter().map(|r| r.text(0)).collect())))
    }

    /// The dashboard's minutes from `since` (Unix s) on, as JSON lines, oldest first.
    pub fn stats_minutes(&self, since: i64) -> Result<Vec<String>, String> {
        self.read(|x| Ok(x.query("select s from stats_minutes where t >= $1 order by t, id", vals![since])?.iter().map(|r| r.text(0)).collect()))
    }

    /// The dashboard's hours, added up by bucket ([`HoursQuery`]).
    pub fn stats_hours(&self, q: &HoursQuery) -> Result<Vec<HoursSum>, String> {
        let (sql, ps) = q.sql();
        self.read(|x| Ok(x.query(&sql, &ps)?.iter().map(|r| (r.text(0), r.int(1), r.real(2), r.int(3), r.real(4), r.real(5))).collect()))
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
            x.execute("insert into registry_changes (system_id, changes) values ($1, $2)", vals![*system_id, json]).map(drop)
        }
        Change::RegistryWhole { system_id, radio } => {
            let json = serde_json::to_string(radio).map_err(|e| e.to_string())?;
            x.execute("insert into registry (system_id, whole) values ($1, $2) on conflict (system_id) do update set whole = excluded.whole", vals![*system_id, json])?;
            x.execute("delete from registry_changes where system_id = $1", vals![*system_id]).map(drop)
        }
        Change::StatsMinute { t, json } => x.execute("insert into stats_minutes (t, s) values ($1, $2)", vals![*t, json]).map(drop),
        Change::StatsHours(rows) => {
            for r in rows {
                x.execute(
                    "insert into stats_hours (series, t, avg, lo, hi, n) values ($1, $2, $3, $4, $5, $6)
                     on conflict (series, t) do update set avg = excluded.avg, lo = excluded.lo, hi = excluded.hi, n = excluded.n",
                    vals![&r.series, r.t, r.avg, r.lo, r.hi, r.n],
                )?;
            }
            Ok(())
        }
        Change::RetireStats { minutes_before, hours_before } => {
            x.execute("delete from stats_minutes where t < $1", vals![*minutes_before])?;
            hours_before.map_or(Ok(()), |h| x.execute("delete from stats_hours where t < $1", vals![h]).map(drop))
        }
        Change::Talkgroups { system_id, old, new } => {
            let system = *system_id;
            let (lines, len) = talkgroup_lines(old, new);
            for l in &lines {
                let t = l.tg.as_ref();
                x.execute(
                    "insert into talkgroups (system_id, seq, line, number, alpha_tag, description, tag, category, mode, priority, ignore) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                     on conflict (system_id, seq) do update set line = excluded.line, number = excluded.number, alpha_tag = excluded.alpha_tag, description = excluded.description,
                     tag = excluded.tag, category = excluded.category, mode = excluded.mode, priority = excluded.priority, ignore = excluded.ignore",
                    vals![system, l.seq, &l.line, t.map(|t| t.number), t.map(|t| &t.alpha_tag), t.map(|t| &t.description), t.map(|t| &t.tag), t.map(|t| &t.group), t.map(|t| &t.mode), t.map(|t| t.priority), t.map(|t| t.ignore)],
                )?;
            }
            x.execute("delete from talkgroups where system_id = $1 and seq >= $2", vals![system, len]).map(drop)
        }
    }
}

/// Write a call's row and its children. A live call replaces what is there;
/// a backfilled one only fills a gap, and brings its children only if it
/// landed.
fn write_call(x: &mut dyn Exec, row: &CallRow, live: bool) -> Result<(), String> {
    const COLS: [&str; 24] = [
        "path", "system_id", "nac", "site", "talkgroup", "start_ms", "stop_ms", "length_ms", "freq_hz", "emergency", "encrypted", "decrypted", "duplex", "priority", "audio_type", "signal", "noise", "snr",
        "clean_voice_pct", "audio", "json", "recorder", "record", "indexed_ms",
    ];
    let conflict = if live {
        format!("on conflict (path) do update set {}", COLS[1..].iter().map(|c| format!("{c} = excluded.{c}")).collect::<Vec<_>>().join(", "))
    } else {
        "on conflict (path) do nothing".into()
    };
    let marks: Vec<String> = (1..=COLS.len()).map(|i| format!("${i}")).collect();
    let sql = format!("insert into calls ({}) values ({}) {conflict} returning id", COLS.join(", "), marks.join(", "));
    let ps = vals![
        &row.path, row.system_id, row.nac.as_ref(), row.site.as_ref(), row.talkgroup, row.start_ms, row.stop_ms, row.length_ms, row.freq_hz, row.emergency, row.encrypted, row.decrypted, row.duplex, row.priority,
        row.audio_type.as_ref(), row.signal, row.noise, row.snr, row.clean_voice_pct, row.audio, row.json, row.recorder.as_ref(), &row.record, row.indexed_ms,
    ];
    let Some(landed) = x.query_opt(&sql, ps)? else { return Ok(()) };
    let id = landed.int(0);
    for table in ["call_transmissions", "call_frequencies", "call_units"] {
        x.execute(&format!("delete from {table} where call_id = $1"), vals![id])?;
    }
    for t in &row.transmissions {
        x.execute("insert into call_units (system_id, unit, start_ms, call_id) values ($1, $2, $3, $4) on conflict do nothing", vals![row.system_id, t.unit, row.start_ms, id])?;
    }
    for (seq, t) in row.transmissions.iter().enumerate() {
        x.execute(
            "insert into call_transmissions (call_id, seq, unit, start_ms, length_ms, emergency, signal_system, tag) values ($1, $2, $3, $4, $5, $6, $7, $8)",
            vals![id, seq, t.unit, t.start_ms, t.length_ms, t.emergency, t.signal_system.as_ref(), t.tag.as_ref()],
        )?;
    }
    for (seq, f) in row.frequencies.iter().enumerate() {
        x.execute(
            "insert into call_frequencies (call_id, seq, freq_hz, start_ms, length_ms, error_count, spike_count) values ($1, $2, $3, $4, $5, $6, $7)",
            vals![id, seq, f.freq_hz, f.start_ms, f.length_ms, f.error_count, f.spike_count],
        )?;
    }
    Ok(())
}

/// A system's radio registry as stored: the last whole copy (the
/// registry's own JSON, as `files` keeps it) and the changes since, oldest
/// first (each a [`trunk_app::stats::radio::RegistryChanges`] as JSON).
/// Changes are appended, not rows updated in place: a busy system changes
/// thousands of rows every 15 minutes, which as row updates writes more
/// than the whole file did (research/sqlite-plan.md).
pub struct StoredRegistry {
    pub system_id: i64,
    pub whole: Option<String>,
    pub changes: Vec<String>,
}

/// A line of a system's talkgroup file, as the `talkgroups` table keeps it:
/// the line itself (the file comes back exactly, comments, extra columns
/// and delimiter too), and the talkgroup it's the row of, read, for queries
/// (a call's talkgroup by name).
pub struct TalkgroupLine {
    pub seq: i64,
    pub line: String,
    pub tg: Option<trunk_core::trunk::Talkgroup>,
}

/// What changed from talkgroup file `old` to `new` ("" for `old`: all of
/// it), as lines to write and the new length: an edit writes only the
/// lines it changed, unless it changed the header (then every row, read
/// again).
pub fn talkgroup_lines(old: &str, new: &str) -> (Vec<TalkgroupLine>, i64) {
    let unmarked = |s: &str| s.strip_prefix('\u{feff}').unwrap_or(s).to_string();
    let (old, new) = (unmarked(old), unmarked(new));
    let (was, lines): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    fn header<'a>(l: &[&'a str]) -> Option<&'a str> {
        l.iter().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).copied()
    }
    let all = header(&was) != header(&lines);
    let numbers = trunk_core::trunk::line_numbers(&new);
    let parsed = trunk_core::trunk::parse_csv(&new);
    let changed = lines
        .iter()
        .enumerate()
        .filter(|&(i, l)| all || was.get(i) != Some(l))
        .map(|(i, l)| TalkgroupLine { seq: i as i64, line: l.to_string(), tg: numbers.get(i).copied().flatten().and_then(|n| parsed.get(&n).cloned()) })
        .collect();
    (changed, lines.len() as i64)
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

/// A bucket's worth of a series' hours: (series, bucket, Σ avg × n, n, lo, hi).
pub type HoursSum = (String, i64, f64, i64, f64, f64);

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
                    Some(end) => any.push(format!("(series >= {} and series < {})", ps.add(prefix), ps.add(end))),
                    None => any.push(format!("series >= {}", ps.add(prefix))),
                },
                None => any.push(format!("series = {}", ps.add(p))),
            }
        }
        if any.is_empty() {
            any.push("1 = 0".into());
        }
        let sql = format!(
            "select series, (t - {t0}) / {step} as b, cast(sum(avg * n) as double precision), cast(sum(n) as bigint), cast(min(lo) as double precision), cast(max(hi) as double precision)
             from stats_hours where t >= {from} and t < {before} and ({}) group by series, b",
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

/// A system's talkgroup file from its stored lines.
pub fn talkgroup_file(lines: Vec<String>) -> Option<String> {
    (!lines.is_empty()).then(|| lines.join("\n") + "\n")
}

/// Each system's [`StoredRegistry`] from its whole copy (system, JSON) and
/// changes (system, JSON; oldest first).
pub fn stored_registry(whole: Vec<(i64, String)>, changes: Vec<(i64, String)>) -> Vec<StoredRegistry> {
    let mut out: std::collections::BTreeMap<i64, StoredRegistry> = Default::default();
    fn at(out: &mut std::collections::BTreeMap<i64, StoredRegistry>, system_id: i64) -> &mut StoredRegistry {
        out.entry(system_id).or_insert_with(|| StoredRegistry { system_id, whole: None, changes: Vec::new() })
    }
    for (system, json) in whole {
        at(&mut out, system).whole = Some(json);
    }
    for (system, json) in changes {
        at(&mut out, system).changes.push(json);
    }
    out.into_values().collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An edit writes the lines it changed; a new header, every row; a
    /// shorter file, its length. Each row read as the file reads it.
    #[test]
    fn talkgroup_edits_write_only_their_lines() {
        let file = "# county\nDecimal,Alpha Tag,Ignore\n101,Fire,\n102,Tac,\n";
        let (all, len) = talkgroup_lines("", file);
        assert_eq!((all.len(), len), (4, 4));
        assert!(all[0].tg.is_none() && all[1].tg.is_none());
        assert_eq!(all[3].tg.as_ref().map(|t| (t.number, t.alpha_tag.as_str())), Some((102, "Tac")));
        let edited = trunk_core::trunk::edit_row(file, 102, &[("Ignore", "x")]);
        let (one, len) = talkgroup_lines(file, &edited);
        assert_eq!((one.len(), one[0].seq, len), (1, 3, 4));
        assert!(one[0].tg.as_ref().unwrap().ignore);
        let named = trunk_core::trunk::edit_row(file, 101, &[("Tag", "Fire")]);
        assert_eq!(talkgroup_lines(file, &named).0.len(), 4, "a column added: every line");
        let (none, len) = talkgroup_lines(file, "# county\nDecimal,Alpha Tag,Ignore\n");
        assert_eq!((none.len(), len), (0, 2));
        assert_eq!(talkgroup_file(file.lines().map(str::to_string).collect()).as_deref(), Some(file));
        assert_eq!(talkgroup_file(Vec::new()), None);
    }
}
