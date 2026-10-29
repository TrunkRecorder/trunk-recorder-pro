# Plan: the storage redesign (PR #104, schema version 4)

Status (2026-10-29):

- the review's four bugs are fixed (66556e8);
- phase 1 is done (e88e35a);
- phase 2 is done, as below.

Phase 2 as built:

- `systems (id, short_name, kind)`. No `sites` table yet: a call's site stays a column.
- Ids are assigned in `Config::assign_system_ids`, with the `nextSystemId` high-water mark.
- `Db::set_systems` maps each name a system's calls were filed under to its id, its names from
  before it had sites included.
- Calls of a system not in the config aren't indexed.
- Band plans and talker aliases stay keyed by name until phase 5.

This plan answers the PR #104 review: an outside architecture review and a DBA's review of the
data model. Both found the same thing. The store works, but it took its shapes from the flat files
it replaced, and it grew parallel paths and types for the same jobs. This is a green-field beta: no
imports, no migrations, no back-compat. The aim is a design that's sustainable to maintain, not one
that preserves what we did before.

## Decisions (owner, 2026-10-29)

1. **A database always: `files` goes.** Talkgroups, aliases, the registry, band plans, Heard, tune,
   sharing and the dashboard's history always live in the database. The only choice is how much is
   written: `storage.lowWrite` for SD cards.
2. **Systems get an id that isn't the short name**, used across every table, with a basic
   `systems` table. `config.json` stays the source of truth for systems; the table mirrors it.
3. **PostgreSQL stays**, but the two backends become one implementation over a thin dialect layer.
4. **The call files are the truth for calls.** The database mirrors each call in one place, not
   three or four.

## Principles

- **One owner per fact.**
  - Calls: the `.json` file is the truth; the database holds one mirror.
  - Systems: `config.json`.
  - Everything else: the database.
- **One type per concept across the layers.**
  - A call: `StoredCall`.
  - A query: `CallsQuery`.
  - A page: `CallsPage<StoredCall>`.
  - A talkgroup: `trunk_core::trunk::Talkgroup`.
- **One implementation per job.**
  - One SQL layer.
  - One filter and paging for calls, shared with the browser build via wasm.
  - One live-list merge.
  - One persistence actor, with one error policy.
- **Every page is an index walked to its limit.** Never a sort of everything that matched. The
  schema test checks the plan of every query the builder makes.
- **Storage strategy stays in trunk-pro.** trunk-app (also built for wasm) holds domain state and
  reports what changed. It doesn't decide journals, snapshots or compaction.
- **A flash budget per mode, measured on the Pi.** Not estimated.

## The target data model

Dialect-neutral DDL, with types written as logical types:

```sql
systems (
  id            int  primary key,        -- config.json `systems[].id` (and conventional's)
  short_name    text not null unique,    -- as the config has it now (renames update it)
  kind          text not null,           -- p25 | smartnet | dmr | nxdn | edacs | conventional
  sysid int, wacn int, nac int           -- identity, when known (Grafana, joins)
)
sites (
  id        int primary key,             -- config.json `sites[].id`
  system_id int not null references systems,
  name      text not null,
  unique (system_id, name)
)

calls (
  id          bigint primary key,        -- surrogate (SQLite rowid / PG identity)
  path        text not null unique,      -- the files' path: the link to the truth
  system_id   int  not null references systems,
  site_id     int  references sites,
  talkgroup   int  not null,
  start_ms    bigint not null, stop_ms bigint not null, length_ms int not null,
  freq_hz     bigint not null, freq_error int, signal real, noise real, snr real, clean_voice_pct real,
  emergency bool not null, encrypted bool not null, decrypted bool not null, duplex bool not null,
  priority int, algid int, key_id int, tdma_slot int, nac text, color_code int, ran int,
  audio_type text, system_type text, voice_mode text, talkgroup_ota int,
  tone_mode text, tone_detected text, tone_confidence real,
  audio bool not null, json bool not null,              -- which files are kept
  detail json not null,                                 -- the lists only: freqList, errorList,
                                                        -- tones, tonePatterns, signals, patched, extra
  recorder text, indexed_ms bigint not null
)
  index (start_ms)                       -- every call
  index (system_id, start_ms)            -- a system
  index (system_id, talkgroup, start_ms) -- a talkgroup

call_transmissions (                     -- srcList: the one list queried
  call_id   bigint not null references calls on delete cascade,
  seq       int    not null,
  system_id int    not null,             -- the call's, for the radio index
  unit      bigint not null,
  call_start_ms bigint not null,         -- the call's start: the radio page's order
  start_ms bigint not null, length_ms int not null, emergency bool not null,
  signal_system text, tag text,
  primary key (call_id, seq)
)
  index (system_id, unit, call_start_ms, call_id)   -- a radio's page, walked in order

talkgroups (
  system_id int not null references systems,
  number    int not null,
  alpha_tag text not null, description text not null, tag text not null, category text not null,
  mode text not null, priority int not null, ignore bool not null,
  preferred_nac int, preferred_site text,
  sort_order int not null,               -- the order loaded, for export
  extra json,                            -- columns this app doesn't know, kept for export
  primary key (system_id, number)
)
units (
  system_id int not null references systems,
  unit      bigint not null,
  alias     text not null,               -- talker alias heard over the air
  seen_ms   bigint not null,
  primary key (system_id, unit)
)

registry (system_id int primary key references systems, snapshot json not null)  -- RegistryChanges::whole
registry_changes (id bigint primary key, system_id int not null, changes json not null)

series (id int primary key, name text not null unique, system_id int references systems)
stats_hours (series_id int not null references series, t bigint not null,
             avg double not null, lo double not null, hi double not null, n int not null,
             primary key (series_id, t))
stats_minutes (t bigint not null, seq int not null, rollup json not null,
               primary key (t, seq))     -- a 2-day refill buffer (not in low-write mode)

documents (kind text not null, system_id int, key text not null, value json not null,
           primary key (kind, key))      -- bandplan, heard, tune, sharing: read and written whole
meta (key text primary key, value text not null)
```

Notes on these choices:

- **System and site ids live in `config.json`.** The app assigns the next free integer when a
  system or site is added, and never reuses one. That keeps the config the source of truth, and a
  rename doesn't orphan anything.
- **Startup syncs the config into `systems` and `sites`.** It upserts by id. A system that's gone
  from the config keeps its row, so its calls still join to a name.
- **A call is mirrored once.**
  - Every scalar of `CallRecord` gets a typed column.
  - `srcList` becomes `call_transmissions`, because it's queried.
  - The lists nothing queries go in `detail`.
  - `StoredCall` is rebuilt from these three, and the conformance test compares the result with
    the original record.
  - Gone: `calls.record` (the whole record a second time), `call_frequencies` (never read) and
    `call_units` (its index moves onto `call_transmissions`).
- **Talkgroups are rows**, keyed by system and number.
  - CSV is only import and export, written once in trunk-app so the browser build shares it.
  - Comment lines aren't kept. Unknown columns are kept in `extra` for export.
  - An edit is a one-row upsert: no text diff and no header rules.
- **Radio aliases are rows.** They're no longer a CSV document.
- **Stats series get ids.**
  - A pattern (`sys/dcfd/*`) resolves in Rust to a list of ids; there are about 136 series.
  - Retention walks each series' key, instead of scanning the whole table every hour.
- **`documents`** (renamed from `kept`) is only for state that is genuinely read and written whole.
- **Times are Unix milliseconds or seconds as integers in both dialects.** Each column's unit is in
  its name: `_ms`, or `t` for stats seconds.
  - PostgreSQL gets reporting views with `timestamptz` columns (`calls_v`, `stats_hours_v`), so
    Grafana doesn't convert on every panel.
  - The tables themselves stay index-friendly integers.

## Architecture

```
config.json ──► systems/sites sync ─┐
                                    ▼
Session::due() ──► Saved ──► Persist (one actor, trunk-pro) ──► Sql<D: Dialect> ──► SQLite | PostgreSQL
finished calls ──► index_call ─────┘      batches, savepoint per change,
backfill (.json files) ────────────┘      errors reported back to the hub
```

- **`db::sql`: one implementation of the database's work.** Each domain module (`calls`,
  `talkgroups`, `units`, `registry`, `stats`, `documents`, `meta`) is written once, against:
  - `trait Exec`: `execute`, `query`, `savepoint`, prepared-statement caching.
  - `trait Dialect`: placeholders written as `$n` or `?n` when the SQL is built (no rewriting
    afterwards), upsert and `returning` (both support them), type names, and DDL tweaks
    (`without rowid` vs nothing, `bigserial` vs `integer primary key`).
  - `sqlite.rs` and `postgres.rs` shrink to opening a connection: pragmas, the lock or advisory
    lock, the reader pool. Each implements `Exec`.
- **One schema source.**
  - The tables are declared once in Rust (name, columns with logical types, keys, indexes).
  - Each dialect renders its DDL from that.
  - A test renders both, creates them, and compares the column lists. The parity is structural,
    not trusted.
- **The writer's messages.**
  - `enum Msg { Change(Change), Flush(..), Checkpoint }`.
  - `Change` carries typed payloads. Encoding (JSON for registry and detail) happens on the writer
    thread.
  - `Write` goes away.
  - Every change gets a savepoint, in one shared loop (done in 66556e8).
- **Errors.** A failed change of something people make (talkgroups, aliases, system sync) is
  reported to the browsers' save status, as the saver did for files. Background changes (calls,
  stats) are logged. `saver.rs` goes once `files` does.
- **No call index without a database.**
  - `Db` is a store, or a failure that is shown loudly: in the Setup banner, in `hello`, and in
    `/api/v1/storage`.
  - Low-write mode is a store too: `lowWrite` only turns parts off.
- **Low-write mode (`storage.lowWrite`):**
  - no call index (the call list comes from recent calls);
  - no `stats_minutes` (hours only);
  - commits every 3600 s.

  Everything else is kept as usual. Target: about 0.1 GB a day on the Pi's eMMC, to be measured.
- **A nightly backup (SQLite).** `VACUUM INTO trunk-pro.backup.sqlite`: one sequential write a day,
  so talkgroups and aliases can't be lost to a bad card. On PostgreSQL, backups are the operator's.

## Calls: one type, one query, one place for filter and paging

- `StoredCall` is the call entry everywhere:
  - the live list (`recent`);
  - the greeting's history;
  - pages;
  - plugins;
  - the HTTP API.

  No untyped `serde_json::Value` entries mutated in place.
- One `CallsQuery` (trunk-app protocol, re-exported to the SDK) and one `CallsPage`.
- **Filter and paging over memory** (`matches`, `page_of`, `with_live`) move to trunk-app.
  - The browser build calls them through wasm, replacing `web/src/calls.ts`'s copy.
  - The SQL tie-break matches memory: order by start, then path.
- **One merge of live and stored calls.** `runtime::history` becomes `calls(limit)`.
- **One meaning of the `calls` capability.** It means "the call index is there". When it isn't
  (low-write mode or a failed store), WebSocket and HTTP both answer from the recent calls and say
  so.
- `olderCalls` goes; the greeting's history is a `callsQuery` page. The greeting reads aliases from
  the session, not the store.

## Registry and stats

- **The registry has one shape.** `RegistryChanges` everywhere; a snapshot is `whole()`.
  - trunk-app always tracks changed rows, which is cheap. `Session` stops deciding when to write
    the whole thing, and `registry_as_changes` and `REGISTRY_WHOLE_EVERY` leave trunk-app.
  - The persistence actor owns compaction: a whole copy once a day, or at load after 24 changes.
  - It stays a journal, not rows. The measurements (rows 0.71 GB/day vs the journal 0.10) stand,
    and nothing queries it in SQL. The plan's phase 2 (`radioQuery` by SQL) would build real rows
    from the same changes.
- **Stats.**
  - `series` ids; `HoursSum` becomes `Bucket`.
  - Retention runs once a day, by series key.
  - Minutes stay a JSON buffer of two days (and none in low-write mode).
  - `Doc` (kept.rs) merges into `Saved`, which gets `kind()`/`key()`.

## PostgreSQL extras

- **Reporting views** with `timestamptz`, joined to `systems`.
- **Per-table autovacuum** on `calls` and `call_transmissions` (`autovacuum_vacuum_scale_factor`
  0.01).
- **Partitioning by month: deferred.** Worth it once someone keeps years. When it comes,
  retention becomes `drop partition`. Partitioned primary keys must include `start_ms`, so it's a
  schema version of its own.
- **The version policy stays.** Another version's tables are dropped and made afresh. The
  release notes must say this cascades to any views users built on them.

## Phases

The redesign goes onto PR #104 (unreleased) as a series of commits, so `main` never sees the
interim schema. Each phase leaves everything working, with the conformance tests passing on both
backends.

1. **The SQL layer.**
   - `Exec`, `Dialect`, the one schema source with its parity test, `Msg` replacing `Write`, and
     the domain modules written once.
   - No change to the schema's meaning; this proves the unification on what exists.
2. **Systems.**
   - Ids in `config.json` (assigned in Setup and by the config loader), the `systems` and `sites`
     sync.
   - `system_id` on every table, with series and documents keyed by id.
3. **Calls mirrored once.**
   - Typed columns, transmissions and `detail`; `StoredCall` built from rows, with a round-trip
     test against the `.json` file.
   - The unit index on transmissions.
   - The call types made one; filter and paging in trunk-app; one live merge; `olderCalls` gone;
     one capability meaning.
4. **Collapse `files`.**
   - `storage.backend` becomes `sqlite` or `postgres`; `storage.lowWrite` is added.
   - `kept.rs`'s file paths, the files half of `statstore.rs`, and `saver.rs` go.
   - The failed-store state is shown in Setup and `hello`; the nightly backup is added.
5. **Talkgroups and aliases as rows.**
   - The protocol carries talkgroup rows (`talkgroups` / `setTalkgroup` by row), and the browser
     edits rows.
   - CSV import and export are in trunk-app.
   - The engine and `NameBook` take rows. The `units` table is added.
6. **Registry and stats tidy.**
   - One registry shape, with compaction in the persistence actor.
   - Series ids and daily retention by key; `Doc` merged into `Saved`.
7. **PostgreSQL reporting.** Views and autovacuum settings.
8. **Measure and document.**
   - Pi write budgets for standard and low-write.
   - Query-plan and `EXPLAIN` checks on PostgreSQL for each page.
   - Docs (configuration, API, architecture), the CHANGELOG, and PR #104's description.

## Every review finding, and where it's answered

| Finding | Where |
|---|---|
| PostgreSQL lost the batch on one failed change | fixed (66556e8) |
| Unbounded sorts: several systems, a radio, a talkgroup | fixed (66556e8); the indexes move to `system_id` in 2–3 |
| A failed store fell back to files | fixed (66556e8); `files` goes in 4 |
| A talkgroup edit could block an async worker under a lock | fixed (66556e8) |
| `files` isn't a backend; `is_disabled()` checks scattered everywhere | 4 |
| Two SQL backends ~90% alike; two schemas, drifting | 1 |
| `Write` ≈ `Change`; control messages in the data enum | 1 |
| `CallStore` a god-trait | 1 (domain modules over `Exec`) |
| A call stored three or four times; `call_frequencies` never read | 3 |
| No systems dimension; the short name repeated and renames orphaning | 2 |
| Talkgroups as CSV lines, the parsed columns unread | 5 |
| Radios with no table; aliases a CSV document | 5 |
| Series as text; retention scanning the whole hours table | 6 |
| Registry: two shapes, compaction in two places, strategy inside trunk-app | 6 |
| Two or three shapes each for a call, a query and a page; untyped JSON entries | 3 |
| Filter and paging in SQL, Rust and TS, with different tie-breaks | 3 |
| Two live-list merges | 3 |
| Two persisters with different error policies | 1, 4 |
| `calls` capability meaning differs between WebSocket and HTTP | 3 |
| Dead `olderCalls`; the greeting re-reading aliases from the store | 3 |
| `numbered()` rewriting `?` inside literals | 1 (placeholders written by the dialect) |
| Time units mixed; awkward for Grafana | 7 (views); units named per column |
| PostgreSQL bloat over years | 7 (autovacuum); partitioning deferred, with its trigger |
| No CHECK constraints | 1 (in the one schema source: lengths ≥ 0, stop ≥ start) |
| SQLite looks the id up again after an upsert | 1 (`returning id` in both) |
| `bigint` where `int` would do (PostgreSQL) | 1 (logical types) |
| Paging can take up to three queries | 3, with a (start, path) cursor if the protocol change is worth it |

## Answered (owner, 2026-10-29)

1. **System ids are small integers** that the app assigns and keeps in `config.json`.
2. **Talkgroup comments:** a comment line directly above a talkgroup's row is that row's
   (`talkgroups.comment`) and is exported above it again. Other comments (before the header, after
   a blank line, at the end) are dropped. Unknown columns are kept in `extra`.
3. **The paging cursor becomes (start, path)**, opaque to clients, so every page is one query.
