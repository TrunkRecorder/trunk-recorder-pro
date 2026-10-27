# Plan: a SQLite store

Status (2026-10-27): agreed direction, not started. It builds on issue #100's design (auggiesoft)
and on the measurements in [storage.md](storage.md). PR #101 laid the groundwork:

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
- **Few indexes.** Each one adds scattered page writes. Phase 1 has only:
  - `calls (talkgroup, start)`;
  - `call_transmissions (unit, call)`.
- **Never `VACUUM`.** Retention deletes in time-ordered batches.
- **Memory answers what's live.** The in-memory registry, the recent-calls list and the minute
  grid keep answering the dashboard. The store holds the record. A query that needs rows not yet
  committed merges the writer's pending batch with what's stored.

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

### Phase 1: the store and the call index (#100 step 1)

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

- **Read:** the HTTP API under `/api/` first, since it works for plugins and interfaces on other
  machines. trunk-reporter's OpenAPI spec is the starting point. The file path in `hello`, for
  local plugins to open read-only, can follow, with the schema version as the contract.
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
2. **Retention:** call rows as long as their files, with an option to keep the row after the
   files go (#25). Other facts kept for 90 days.
3. **Read access:** the HTTP API first, the file path later.
4. **`annotations`:** later.
5. **The call's JSON in `calls`:** yes.
6. **Commit interval:** 300 s by default.
7. **Phase 3:** after phases 1 and 2 are released and have run on a Pi for a while.
