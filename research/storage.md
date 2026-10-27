# Storage: what's kept on disk, how it's written, and SQLite

Status (2026-10-26): the first steps are in the PR that adds this file:

- a single background writer and one save rule;
- text settings saved once you finish typing, not per letter.
 The rest is open for discussion before anyone builds it. Several people have
mentioned SQLite, and this is meant to settle on one plan so we don't end up with competing
implementations.

## Why

Users with large systems on slow disks (SD cards, USB sticks) reported:

- **Ignore clicks were slow.** One click took 5+ seconds to show, and several clicks in a row
  could end in the wrong state (#98 works around this in the interface).
- **Too many whole-file rewrites.** The recorder rewrote whole files to change one small thing.

Both came from the same pattern: state is kept in memory, but every change rewrote its whole file.
Each write also waited for the disk to confirm it (`write_atomic`, which ends with an fsync), on
the thread that was handling the change.

The interface added to it. A typed setting was sent whenever typing paused for 300 ms, so a rename
typed at an ordinary pace was several saves, as a user put it: "Save 1: F, Save 2: FI, Save 3:
FIR…". See [When the interface saves](#when-the-interface-saves).

## What's kept, and how (after this PR)

| File | What | Who edits it | Written |
|---|---|---|---|
| `config.json` | the setup | the user (Setup, or by hand) | on Save (still inline: the user waits to hear it saved) |
| `<sys>.talkgroups.csv` | talkgroups, Ignore, tags | the user (Setup, Radio pages, by hand); Trunk Recorder's format | in the background, as last edited |
| `<sys>.units.csv` | talker aliases learned on the air | the recorder | every 10 s when changed |
| `sharing.json` | what sharing with the directory did | the recorder | every 10 s when changed |
| `<site>.bandplan` | learned band plans / DMR channel tables | the recorder | every 10 s when changed |
| `conventional.heard.json` | codes each conventional frequency carried, counted | the recorder | every 60 s when changed |
| `tune.json` | AutoTune scores | the recorder | every 60 s when changed |
| `radio/<sys>.json` | the radio registry (talkgroups, radios, pairs, frequencies heard; a week by hour) | the recorder | every 15 min (browser: 1 min), whole |
| `stats/YYYY-MM-DD.jsonl` | the dashboard's minute history | the recorder | appended every minute (statstore) |

Everything is written on stop. trunk-app's `saving.rs` decides when each file is due, and both
the desktop recorder and the browser build follow it. trunk-pro's `saver.rs` writes them on one
thread. If a file is queued several times, only the latest copy is written.

Still open:

1. **The talkgroup file is still sent and broadcast whole.** An Ignore click no longer waits for
   the disk, but the browser still sends the whole CSV. The server re-reads all of it, rebuilds
   the name lookup from the full config, and sends the whole CSV to every browser.
2. **The radio registry is rewritten whole.** See the numbers below.

## The radio registry's size

The cost is measured by `registry_size` in `trunk-app/src/stats/radio.rs` (ignored test). It
serializes one system's registry, filled to every cap (5,000 talkgroups with a week of hours,
20,000 radios with their last 30 transmissions, 50,000 pairs, 512 frequencies) and to about half
of each. Measured on an Apple M-series Mac, release build:

| | JSON | copy (under the lock) | to JSON | read back |
|---|---|---|---|---|
| at the caps | 41 MB | 8 ms | 42 ms | 70 ms |
| about half | 22 MB | 3 ms | 18 ms | 38 ms |

Most of it is per-hour activity (a talkgroup's week is 168 entries) and each radio's recent
transmissions. Real systems are sparser than this, because only hours with calls are kept, so
this is an upper bound. Still, a large statewide system can plausibly reach tens of MB.

What this costs:

- **Disk writes.** Rewritten every 15 minutes, a 20–40 MB file comes to 2–4 GB a day, almost
  all of it unchanged data. That is bad for an SD card, and the RAM spool exists to avoid
  exactly this kind of wear.
- **What a crash loses.** A crash or power cut loses up to 15 minutes of it. Saving more often
  means writing more.
- **Startup.** The whole file is parsed when the recorder starts.

Fixing this means writing only what changed. There are two ways to do it.

## Option A: SQLite for runtime state, and talkgroups and units as internal data

The first version of this option kept SQLite to runtime state, leaving talkgroup and unit files as
files because people edit them. The maintainer has since agreed that talkgroups and units can be
**internal data** too, with **Import** and **Export** in a format people can edit (Trunk Recorder's
CSV). So:

| In SQLite | Stays a file |
|---|---|
| the radio registry, heard codes, learned aliases, sharing, AutoTune, band plans | `config.json` |
| talkgroups (Ignore, alpha tags, tags) and unit names, with CSV Import/Export | a conventional system's **linked** channel CSV (re-read from a spreadsheet; or made an explicit Reload) |
| perhaps the minute history (statstore's appending already works) | |

Moving talkgroups and units in does the following:

- An Ignore click or an alpha-tag edit is one row. It no longer rewrites a 10,000-row CSV or sends
  the whole file to every browser. The per-row `setTalkgroup` command below is still needed for
  the interface; SQLite only changes what sits behind it.
- All runtime state lives in one place, rather than a database for some of it and files for the
  rest.
- Little is lost: talkgroup files are only read when recording starts, so editing them by hand
  while recording doesn't work today anyway. Import and Export keep the spreadsheet and
  RadioReference workflows working.

For talkgroups alone, the speed gain over this PR's in-memory map and background writer is modest,
because edits come at human pace. The case is strong once the registry moves to SQLite anyway.

### Benefits

- **Writes in proportion to what changed.** A call updates a handful of rows: its talkgroup's
  hour, a few radios, a pair. One transaction can carry everything from the last few seconds, so
  saving every 10 s instead of every 15 min becomes cheap, and a crash loses seconds, not minutes.
- **Crash safety is handled.** WAL mode with `synchronous=NORMAL` gives atomic commits and syncs
  only at checkpoints. It is well tested.
- **No large parse at startup.** The registry could even be queried in place rather than held
  entirely in memory, which matters at 20,000 radios × 30 transmissions.
- **Readers don't block the writer.** The dashboard, `trunk-pro` tools and a backup can read
  while recording writes.
- **One file** for all runtime state, kept consistent across tables.

### Problems

- **A C dependency in every build.** `rusqlite` with `bundled` compiles `sqlite3.c`. Our
  release targets are macOS universal (two `--target` builds then `lipo`), Linux via `cargo
  zigbuild` for glibc 2.28 (zig compiles the C), Windows MSVC, and the Docker image. All of them
  can do it, but each needs checking in CI, and the binary grows by about 1–1.5 MB.
- **The browser build can't use it as-is.** `trunk-web` targets `wasm32-unknown-unknown`, where
  rusqlite doesn't build. The options are SQLite's own wasm build with OPFS (driven from JS,
  async) or a second storage path. Either way, storage has to sit behind an interface with two
  implementations. This PR's `Session::due()` / `Saved` split is already that seam.
- **What leaves the filesystem has to be exported.** Talkgroup and unit files are used in
  Trunk Recorder's formats, kept in git by some users, and visible to plugins through `data_dir`.
  As internal data they need:
  - **Import** and **Export** in the interface and on the command line;
  - perhaps an export written beside the config after changes, for anyone who wants the file;
  - a one-time import of the existing files.

  `config.json` stays a file.
- **Schema work.** `SystemRadio` is nested: per-talkgroup hours, ciphers, per-radio talkgroup
  lists and recent transmissions, a pairs map. That becomes about 8 tables, plus a schema version
  and a one-time import of existing `radio/*.json`.
- **Row-level change tracking.** The registry only marks a whole system as changed today. Rows
  need their own dirty sets, which is needed for Option B too.
- **It doesn't remove the writer thread.** The registry changes on the recording loop. To keep
  that loop off the disk, commits still go through a background writer like `saver.rs`, just
  carrying row changes instead of files.
- **It doesn't get rid of WAL writes.** SQLite writes pages to the WAL and again at checkpoint,
  so about 2× the changed data. That is still orders of magnitude less than rewriting 40 MB.
- **Operational costs:**
  - not inspectable with a text editor;
  - backups need `VACUUM INTO` or the backup API rather than a file copy mid-write;
  - locking on network filesystems (SMB/NFS) is unreliable;
  - one more thing to explain when a user's data needs recovering.

## Option B: append-only changes + snapshot (no new dependency)

This is how `statstore` already works. Every 10 s, each changed row (a talkgroup, a radio, a pair,
a frequency) is appended as a JSON line to `radio/<sys>.jsonl`. At startup the snapshot is loaded
and the lines replayed. When the log grows past the snapshot's size (or once a day), a new snapshot
is written and the log truncated.

- **Benefits:**
  - writes in proportion to what changed, with no new dependency;
  - works in the browser (OPFS can append);
  - text, so a person can read it;
  - reuses statstore's pattern and the writer from this PR.
- **Problems:**
  - compaction and replay logic to write and test;
  - startup still parses the snapshot;
  - no queries in place;
  - consistency across files is our own job.

## Recommendation

1. **Merge this PR's step.** The recording loop and browser requests no longer wait on the disk,
   and there is one save rule and one writer.
2. **Row-level change tracking in the registry.** Both A and B need it, and it's useful on its
   own.
3. **Per-row talkgroup edits.** Add `setTalkgroup { system, tg, fields }`. The server patches
   memory, sends only that row to browsers, and hands the file to the writer. The CSV stays the
   file of record. This is independent of A vs B.
4. **Then pick A or B**, behind the `Saved` seam, by benchmarking both on a Pi with an SD card
   against a real large system's registry:
   - **A** (SQLite, with talkgroups and units as internal data, plus Import/Export) if the registry
     should be queried in place, memory becomes the limit, or one store for everything is wanted.
     Only once the browser build's path (OPFS or skip) is decided.
   - **B** (append-only log) if it keeps writes and startup acceptable and the C dependency isn't
     wanted. Talkgroup and unit files then stay files, written by the background writer.

`config.json` stays a file either way.

## When the interface saves

The other half of the reports: "the UI lacks a save button, and can be aggressive about
automatically doing things for you… every single keystroke would restart the plugin… Save 1: F,
Save 2: FI, Save 3: FIR…".

**What it did.** Setup's text fields passed every change into the config. The only brake was a
300 ms timer after the last change, so pausing between keys meant a save per pause. Each save:

- rewrote `config.json`, waiting for the disk, inside the request handler with the config locked;
- re-read every linked channel CSV;
- rebuilt the name lookup by re-parsing every talkgroup file;
- sent the whole config to every browser;
- restarted any plugin whose settings changed, if it can't take new settings while running.

Typed into a system's **short name**, it renamed the system with each pause: `f`, `fi`, `fir`…
Each rename also moved its talkgroup file and the plugin settings that name it.

**What this PR changes.** Every text setting is now a `DraftInput` (`TextInput`, `TextArea`,
`NumberInput`). It keeps what's typed and passes it on once:

- when the field is left;
- when Enter is pressed (a `TextArea`: Ctrl/⌘+Enter);
- when its page goes away.

Escape puts it back. Switches, menus and buttons still act at once. The rule is in
`docs/developer/ui-style.md`.

**Measured** in headless Chrome, with "fire4a" typed into a system's short name, pausing 500 ms
after each key:

| | saves while typing | saves in all |
|---|---|---|
| before | 6 | 6 |
| after | 0 | 1 (on leaving the field) |
| after, then Escape | 0 | 0 (field back to `p25`) |

**Not changed yet:**

- **No Save button, on purpose.** Saving once per finished field means every save is one the user
  meant. A global Save button brings unsaved-changes state, edits lost on navigating away, and
  conflicts between browsers.
- **A visible "Saved" / "Couldn't save"** in Setup would answer "did it take?". Not built yet.
- **Disruptive changes** should be marked and applied deliberately: plugin settings that restart
  a plugin, and source changes that need a recording restart. Not built yet.
- **Sliders** (gain) still save as they're dragged, debounced. That's kept for now, because
  hearing gain change live while dragging is useful.
