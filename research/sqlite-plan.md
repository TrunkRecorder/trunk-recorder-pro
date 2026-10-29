# Plan: storage backends (files, SQLite, PostgreSQL)

Status (2026-10-28):

- **Phase 1 is built** (the call store and its queries: #102 and #103 by auggiesoft, with the
  maintainer's changes, as one PR, #104).
- **Built, in #104:** the store behind a [backend interface](#backends), with `files`,
  `sqlite` and `postgres`, so a Pi and a Xeon server each get the storage that suits them. The
  conformance tests run on SQLite always and on PostgreSQL in CI (a service container).
- **Built, in #104:** the [flat-file stores](#the-flat-file-stores) behind the same interface:
  the radio registry, talker aliases, band plans, conventional codes heard, AutoTune's scores and
  sharing's state. Existing files are imported once and renamed `.imported`.
- **Built, in #104:** talkgroup files in the store, a line at a time with each row's talkgroup
  read into columns ([phase 3](#phase-3-talkgroups-and-unit-names-as-internal-data)).

It builds on issue #100's design and on the measurements in [storage.md](storage.md). PR #101
laid the groundwork:

- a single background writer (`trunk-pro/src/saver.rs`);
- one save schedule (`Session::due()`);
- per-row talkgroup edits (`setTalkgroup`);
- per-row change tracking in the radio registry (`Registry::take_changes`).

## Goals

1. **Questions across calls**, answered in milliseconds at any archive size: by talkgroup, by
   radio, by time. Today they read every call file.
2. **Startup without walking the recordings tree.** That walk is about 10 s a year of recordings
   warm, and minutes on a cold Pi.
3. **A radio registry that doesn't forget.** It drops data past its caps within days on busy
   systems, and is rewritten whole every 15 minutes.
4. **Less writing on flash than today, not more.** The budget is under 1.8 GB a day for a busy
   system, the whole-file save's figure on the Pi.
5. **Later: talkgroups and unit names as internal data**, with Import and Export in Trunk
   Recorder's CSV.

## Not goals

- **The call files are not replaced.** WAV and JSON are still written as now. They are the
  archive and the Trunk Recorder compatibility. The store is an index over them.
- **`config.json` stays a file.**
- **The minute stats keep their append-only JSONL files.** Finished days may also be imported
  (phase 4).
- **The browser-only build gets no SQLite.** It keeps its files on OPFS. Everything it uses keeps
  the same protocol shapes.
- **No protocol shapes change.** New questions arrive as new commands.

## Rules learned from the measurements

These come from `tools/storage-bench` on the Pi's eMMC.

- **Commit every few minutes, not every second.** SQLite writes each touched page to the WAL and
  again at checkpoint. Issue #100's design writes 7.2 GB a day at 1 s commits, 1.70 at 60 s and
  0.54 at 5 min (call index included). A setting, `storage.commitSeconds`:
  - **default 300**;
  - 60 is reasonable on SSDs;
  - the minimum is 10.
- **Append, don't update in place.** Facts (calls, transmissions, affiliations, tones) are inserted
  with rowid keys, so they land on the last page. Updating rows in place was the expensive case,
  at 7–11 GB a day.
- **Upsert state once per batch.** Talkgroup and radio rows are upserted from
  `Registry::take_changes()` at commit: each changed row once, however many events touched it.
- **Indexes follow the queries.** Each one adds scattered page writes, but with `files` there
  for setups that must write the least, SQLite needn't be starved of them. Phase 1 has:
  - `calls (start)`, `calls (system, start)`, `calls (talkgroup, start)`;
  - `call_transmissions (unit, start)`.

  A new query that needs an index gets one, with its cost measured by `write_budget` on the
  Pi. Adding `calls (system, start)` took a busy system from 0.20 to 0.21 GB a day at 300 s
  (0.59 to 0.61 at 60 s). PostgreSQL, on a server, can afford more.
- **Never `VACUUM`.** Retention deletes in time-ordered batches.
- **Memory answers what's live.** The in-memory registry, the recent-calls list and the minute
  grid keep answering the dashboard. The store holds the record. A query that needs rows not yet
  committed merges the writer's pending batch with what's stored.

## Backends

One recorder runs on a Pi with an SD card, another on a Xeon server with years of history and a
DBA. They need different storage, not different recorders. So the store has a backend chosen in
the config, and everything above it is shared.

### What stays shared, above the interface

- **The writer thread:**
  - batching (`commitSeconds`, a lone write after a quiet spell at once, `flush`);
  - retention's timing (a chunk per pass);
  - the single-writer rule.
- **The live list and the one query** (`runtime::calls` merging `Ctx::recent`): the store is never
  on the live path.
- **Row building** (`row::call_row`), paging rules (whole milliseconds), `CallsFilter`,
  `CallsPage`.
- **The backfill and reconcile walks** (they ask the backend whether a call is known, and keep
  their markers in its `meta`).
- **The store being optional:** a backend that can't be opened (or is unreachable) is off, and
  recording goes on from memory.

### The interface (`trunk-pro/src/db/store.rs`)

```rust
pub trait CallStore: Send + Sync {
    /// One batch, one transaction; each call whole or not at all (a savepoint each).
    fn apply(&self, batch: Vec<Change>) -> Result<(), String>;
    /// A page of calls, newest first, equal starts whole.
    fn page(&self, f: &CallsFilter) -> Result<CallsPage, String>;
    fn one(&self, path: &str) -> Result<Option<Value>, String>;
    fn known(&self, path: &str) -> Result<bool, String>;
    fn meta(&self, key: &str) -> Result<Option<String>, String>;
    /// Delete up to `max` calls that started before `before_ms`, oldest first; how many.
    fn retire(&self, before_ms: i64, max: usize) -> Result<usize, String>;
    fn newest_start(&self) -> Result<Option<i64>, String>;
    fn info(&self) -> Result<StoreInfo, String>;   // /api/v1/storage
    fn checkpoint(&self) {}                         // SQLite: fold the WAL
    fn close(&self) {}                              // SQLite: pragma optimize
}
```

It's small and synchronous: the writer is a thread, and queries already run in
`spawn_blocking`.

### The three backends

| `storage.backend` | For | Holds | Reliability |
|---|---|---|---|
| `files` | the smallest setups, or no index wanted | nothing beyond the call files: queries answer from the live list (the recent calls) and the folder walk, as with the store off | the files themselves |
| `sqlite` (default) | a Pi to a mid-size server | `<data>/trunk-pro.sqlite`: months (0.2 GB a day written on the Pi's eMMC at 300 s) | WAL, crash-safe; a crash loses at most one commit interval of the index, recovered from the call files at the next start |
| `postgres` | servers: years of history, the operator's backups and replication | a PostgreSQL database (`storage.url`) | synchronous commit, replication, point-in-time recovery: the operator's |

- **Settings:** `storage.backend`, `storage.url` (postgres), `retentionDays`, `commitSeconds`.
  On a server, `commitSeconds` can be short (10 to 60): there's no flash to wear.
- **One recorder per database (or schema) for now.** `path` is unique, as in SQLite. A
  PostgreSQL advisory lock plays the lock file's part. Several recorders sharing one database
  (the `recorder` column) is a later step, with `(recorder, path)` keys and queries by recorder.
- **The client:** `postgres` (sfackler's crate), pure Rust, so there's no C toolchain on top of
  SQLite's. Plain TCP to start; TLS (`tokio-postgres-rustls`) follows, for databases off the
  machine. It's synchronous, with a runtime of its own inside, so it **must never be called
  from async code**. `CallStore`'s methods run on the writer thread or under
  `spawn_blocking`; only `kind` and `location` are free. The release smoke run found
  `/api/v1/storage` breaking this rule; it was fixed.
- **The record is `jsonb`** on PostgreSQL (text on SQLite), so a server's database can be
  queried by any field of a call (`record->>'talkgroup'`).
- **The same schema shape in both dialects**, each with its own numbered migrations. SQLite uses
  `user_version`; PostgreSQL a `schema_version` table.
- **Not more databases in core.** MySQL, ClickHouse, Elasticsearch and the like fit as plugins
  fed every call (they already are); each core backend is a dialect to keep in step forever.

### Keeping the backends equal

- **One conformance suite** (`db::conformance`) runs every behaviour test against each backend:
  - paging every limit;
  - live and backfilled rows;
  - a call whole or not at all;
  - retention chunks;
  - meta;
  - one by path;
  - known.
- **SQLite always runs it.** PostgreSQL runs it when `TRP_PG_URL` names a database; CI starts a
  PostgreSQL service for it.
- **The write budget test** (`write_budget`) measures SQLite on the disk it runs on (the
  recorder's own writes). A PostgreSQL server's writes are the server's, outside the recorder.

## The flat-file stores

Built in #104 (`trunk-pro/src/kept.rs`). With `storage.backend: "files"`, or a store that could
not open, they are the files they always were, written by `saver.rs`. Otherwise they are the
store's, written by its writer in its batches.

| File | In the store | Written |
|---|---|---|
| `radio/<sys>.json` (the radio registry) | `registry` (a whole copy per system) and `registry_changes` (`take_changes` as JSON, appended) | changes every 15 min; whole once a day ([below](#the-registry-as-changes-not-rows)) |
| `conventional.heard.json` | `kept` (`heard`) | each minute it changed |
| `<sys>.units.csv` (talker aliases learned) | `kept` (`units`, system) | soon after a change |
| `<site>.bandplan` | `kept` (`bandplan`, key) | when it changes |
| `tune.json`, `sharing.json` | `kept` (`tune`, `sharing`) | as before |
| `<sys>.talkgroups.csv` | `talkgroups` (system, line number): each line as loaded, and its talkgroup's columns | an edit (`setTalkgroup`), only the lines it changed, committed at once; a file loaded (`setTalkgroups`), all of it |
| `stats/YYYY-MM-DD.jsonl` | stays append-only files; finished days optionally into the store (phase 4) | already the right shape for flash |
| `config.json`, plugin and interface folders, registry caches, logs | stay files | edited by people, or other programs' |

- **The seam:** `Session::due()` / `Saved` as before; `runtime::save_due` hands each to
  `kept::write` (the store's `Write::Kept`, or the saver), and reads go through `kept::read`.
- **`kept`** is a document per (kind, key): these are read whole at startup and written whole,
  and nothing queries inside them, so rows of their own would only add tables.
- **Import:** a file found while the store is on was written while it was off (or by an earlier
  release), so it is newer: it's imported, committed, then renamed `<name>.imported`. A store
  that can't be read falls back on that last copy; so does `files` after a switch back.
- **Reads block** (PostgreSQL's client refuses a tokio worker): the greeting's talker aliases
  and conventional codes are read under `spawn_blocking`.
- **The browser build** keeps its files on OPFS.

### The registry as changes, not rows

The first build kept the registry as rows (`registry_rows`, upserted from `take_changes`, and a
capped log). Measured on the Pi's eMMC (`db::tests::registry_write_budget`): a busy system (2,500
talkgroups, 10,000 radios, a call and an affiliation a second, six hours heard) changes about
16,000 rows an hour. The keys are random, so each upsert dirties its own page, and the WAL writes
pages twice:

| Kept as | Writes a day |
|---|---|
| the whole file every 15 min (`files`, 4.6 MB) | 0.44 GB |
| rows upserted every 15 min | 0.71 GB |
| changes appended every 15 min, whole once a day | **0.10 GB** |

Nothing queries those rows: the registry is loaded whole at startup and answered from memory.
So the store keeps what `take_changes` gives as one appended JSON row per system per save. That
writes only new pages. Once a day (`REGISTRY_WHOLE_EVERY`), or at a start that finds a quarter
day's worth, the whole registry is written and the changes before it are deleted. Loading is the
whole copy with each change applied (`SystemRadio::apply`, the same code the round-trip test
checks). Phase 2's SQL-answered `radioQuery` would need real rows; it can build them from the
same changes when it comes.

## Architecture

- **Where the code lives.** A `store` module in **trunk-pro**: `rusqlite` with `bundled`. Not in
  trunk-app, which also builds for wasm. trunk-app keeps the shapes: the protocol types, the
  registry, and `take_changes`.
- **The file.** `<data>/trunk-pro.sqlite`, beside the config, never in the capture folder (it may
  be a RAM spool or a network share).
  - Opened at startup before `Ctx`, with WAL mode and `synchronous=NORMAL`.
  - Checkpoints happen when the WAL passes a size (say 16 MB), so they are periodic sequential
    bursts.
- **One writer thread**, the store's own, beside `saver.rs`:
  - it takes facts as they happen (each concluded call, affiliation and tone) into a pending
    batch;
  - every `commitSeconds`, it adds the state from `take_changes()` and commits the batch in one
    transaction;
  - `flush()` commits at once; used when recording stops and when quitting, as `Saver::flush`
    is.
- **A small read pool** (2 connections) for queries, run with `spawn_blocking` as `radioQuery`
  already is.
- **If the store fails, recording isn't affected.** If the file can't be opened, or is corrupt, the
  recorder records and serves from memory as today. The status shows "History store unavailable:
  …", and the call list falls back to the recordings walk.
- **One process.** `already_running` only catches a second `trunk-pro serve` on the same port. So
  the store takes an exclusive lock (a lock file beside it) and a second process on the same data
  folder runs without it, with a message. Tools that read the file open it read-only.

## Schema

The schema is versioned in `schema_version`. Each migration is a numbered SQL step run in a
transaction at startup, with a copy of the file made first (`VACUUM INTO …-v<N>.bak`). The table
shapes follow issue #100.

**Facts** (append-only, rowid keys):

| Table | Columns | Index |
|---|---|---|
| `calls` | system, talkgroup, start, length, freq, encrypted, emergency, recorded, signal, snr, `path` (unique), `record` (the call's JSON, as written beside its audio), `recorder` (nullable install id) | (talkgroup, start) |
| `call_transmissions` | call, unit, pos, length, tag | (unit, call) |
| `call_frequencies` | call, freq, pos, length, errors, spikes | — |
| `unit_events` | t, system, unit, kind (on, off, join, location, …), talkgroup | — (phase 2: (unit, t) only if a query needs it) |
| `tones` | t, system, talkgroup, call, pattern JSON | — |

**State** (one row per thing, upserted once per batch):

| Table | Columns |
|---|---|
| `systems`, `sites` | short name and the P25 identity (NAC, WACN, system id, RFSS, site), so a rename doesn't orphan history |
| `talkgroups` | system, number, first and last heard, calls, airtime, encrypted calls, name heard |
| `units` | system, id, first and last heard, last transmission, counts, affiliation, registration, talker alias |

**Phase 3 adds:** `talkgroup_file` (the talkgroup file's rows: Decimal, Alpha Tag, Mode, …,
Ignore, plus any other columns as JSON) and `unit_names`.

**Derived by queries, in today's `radioQuery` shapes:** pairs, hourly activity, per-frequency
decode quality, call lengths, ciphers per talkgroup.

## Phases

Each phase is its own PR, reviewed as contributor PRs are (see the project's review notes).
auggiesoft has offered phases 1 and 2. trunk-reporter already has the schema, the backfill and the
query layer, with tests.

### Phase 1: the store and the call index (#100 step 1) — built

As built, against what's below:

- **The live list stayed.** The plan let the startup walk go and answered `history` from the
  store. Built that way, the call list was empty with the store off, and a call's kept files
  reached the interface only at the next commit. So `Ctx::recent` (the last 500 calls) is still the
  live list, and every query merges it with the store (`runtime::calls`). The walk runs only with
  the store off.
- **One query** behind `callsQuery`, `olderCalls` (kept as a thin wrapper), `GET /api/v1/calls`
  and plugins' `calls.query`, with one page type (`CallsPage`).
- **Gate met.** On the Pi CM5's eMMC, the real write path (`db::tests::write_budget`, a call a
  second, 1.2 kB records) writes **0.20 GB a day at 300 s** and 0.59 at 60 s, the same on two
  runs.
- **Not built yet:**
  - the "Back up the history" button and `trunk-pro store backup`;
  - refusing a data folder on a network filesystem;
  - showing the file's size in Setup.

  These are in Operations below.

- **The store module:**
  - open, migrate and the writer thread;
  - the `storage.commitSeconds` setting in the config, with Setup's field and the docs;
  - `rusqlite` with `bundled` in trunk-pro.
- **Calls into it.** `runtime::finish_one` adds each concluded call to the pending batch, where
  it already builds the history entry. `call_files` updates its audio and JSON flags when plugins
  settle it.
- **Answered from it:**
  - `history` on connecting and `olderCalls`;
  - the 24-hour paging window and the startup walk (`scan_history`/`load_history`) go.
- **Backfill.** A background job indexes call JSON files the store lacks, oldest last, throttled.
  It runs on first start and after a crash, so a lost batch is recovered from the files.
- **A new command, `callsQuery`:** system, talkgroup, unit, a time range, a cursor and a limit.
  It answers `callsResult` with today's history entry shape. It needs `protocol.ts`, the schema,
  the API docs and `client.js`.
- **Interface:** the Radio pages' talkgroup and radio views list all their calls, paged, rather
  than only the recent ones.
- **The browser build** keeps its OPFS index. `callsQuery` is answered there from what it has.
- **Tests:**
  - the store: migrate from empty, write and read, a batch lost and backfilled, a corrupt file
    refused with recording still going;
  - protocol samples for `callsQuery` and `callsResult`;
  - `regress.sh` unchanged;
  - `storage-bench` extended with phase 1's real write path.
- **Gate:** on the Pi, a busy system at the default 300 s writes **under 1 GB a day**. The release
  builds for every target pass in CI.

### Phase 2: the registry (#100 step 2)

(Its storage is built: [the registry as changes](#the-registry-as-changes-not-rows). What
follows, queries by SQL and retention in place of the caps, is not.)

- **Facts:** `unit_events` and `tones` appended as they happen.
- **State:** `talkgroups` and `units` upserted per batch from `take_changes()`.
- **Queries:** `radioQuery` answered by SQL in today's shapes. Anything live (the current hour,
  "new") still comes from memory.
- **The registry in memory** becomes a cache of the recent window (a week, as now) loaded from
  the store at startup. `radio/*.json`, the 15-minute rewrite and the final save go away; the
  `Saved::Registry` kind goes.
- **Retention replaces the caps:** `storage.keepDays`, unlimited by default for calls and 90
  days for facts that aren't calls. It deletes in batches.
- **One-time import** of existing `radio/*.json` into state.
- **The browser build** keeps saving the registry as now, through `Session::due()`.
- **Gate:**
  - the same Pi write budget;
  - every `radioQuery` answer equal to the in-memory one on a replayed capture (a test).

### Phase 3: talkgroups and unit names as internal data

Built for talkgroups in #104, more simply than below. The file's text stays the model (the
engine, the browser and the protocol all take a CSV), so the store keeps it **a line at a time**:

- `talkgroups (system, seq, line, number, alpha_tag, description, tag, category, mode, priority,
  ignore)`, keyed by (system, seq), indexed by (system, number).
- The file comes back exactly (comments, extra columns, delimiter), and the columns make a call's
  talkgroup a join.
- `setTalkgroup` writes only the lines `edit_row` changed (all of them when it changed the
  header). Edits commit at once, not at `commitSeconds`: people make them.
- Import and Export were already in Setup (**Load CSV…**, **Download**). A file left beside the
  config is imported at the next start. No CLI, since the running recorder holds the store's
  lock.
- Unit names stay in the config (`unitNames`). Talker aliases learned over the air are a `kept`
  document.

What follows was the first plan:

- **Tables:** `talkgroup_file` and `unit_names`, imported once from `<sys>.talkgroups.csv` and
  `<sys>.units.csv`. The files are renamed `.imported` afterwards and kept.
- **`setTalkgroup`** writes the row; `setTalkgroups` (a whole file loaded or pasted) replaces the
  system's rows. Both commit immediately: edits come at human pace, so that's cheap.
- **Import and Export:**
  - Import… and Export… in Setup's talkgroup field and the radio names;
  - `trunk-pro talkgroups export <system> [file]` and `import` on the command line;
  - Trunk Recorder's CSV in both directions, keeping extra columns.
- **The engine and the name lookup** load talkgroups from the store, not the files.
- **Plugins:** `data_dir` no longer has the CSVs. Note it in the changelog and the plugin docs.
  Plugins get talkgroups in call metadata as now.
- **Conventional systems' linked channel CSVs** stay files (the spreadsheet workflow).

### Phase 4 (optional): finished stats days

At the day rollover, where `statstore::tidy` compresses the finished file, the day is also
imported, so history can be kept longer than eight days. The live path doesn't change.

## Access for plugins and interfaces

- **Read, interfaces and tools:** `callsQuery` on the WebSocket and `GET /api/v1/calls` over HTTP.
- **Read, plugins:** `calls.query` over the plugin's own channel (SDK 0.9.0, `Host::calls`). There
  are no ports or authentication to set up, it works wherever the plugin runs, and it's the same
  query.
- **Not the file.** Handing plugins `trunk-pro.sqlite`'s path makes every table and column public
  API, and any migration then breaks them. A plugin holding a read open also keeps the WAL from
  checkpointing. If heavy offline analysis ever needs SQL, offer read-only views (`v1_*`) with a
  compatibility version that only breaking changes bump, deliberately.
- **Write:** only TRP writes. Per-call data from plugins (transcription, say) arrives through an
  API into `annotations`, which comes later, when a plugin needs it.

## Operations

- **Backups.** A "Back up the history" button and `trunk-pro store backup <file>` use
  `VACUUM INTO`. Copying the file while recording isn't safe; the docs say so.
- **Size.** A busy system's calls add about 5 MB an hour, roughly 45 GB a year (the call files
  themselves are far larger). Setup shows the file's size next to the capture folder's.
- **Network filesystems.** If the data folder is on SMB or NFS, the store is refused with a
  message (SQLite locking isn't reliable there), and the recorder runs from memory.
- **Docker.** The data volume already holds the config; the store goes beside it.

## Decisions for the maintainer

These are issue #100's questions, with suggestions:

1. **File:** `<data>/trunk-pro.sqlite`.
2. **Retention:** decided: `storage.retentionDays`, 120 days by default in the store; the files
   stay on disk past that. Other facts (phase 2): 90 days.
3. **Read access:** decided: the APIs above; no file path.
4. **`annotations`:** later.
5. **The call's JSON in `calls`:** yes.
6. **Commit interval:** 300 s by default.
7. **Phase 3:** after phases 1 and 2 are released and have run on a Pi for a while.
