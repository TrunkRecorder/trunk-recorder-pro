# Storage: what's kept on disk, how it's written, and SQLite

Status (2026-10-27).

**Done in PR #101:**

- a single background writer and one save schedule;
- text settings saved once you finish typing;
- per-row talkgroup edits;
- a save status in Setup, and Apply for plugin changes that restart a plugin;
- per-row change tracking in the radio registry;
- a benchmark of the ways to store the registry, run on the Pi.

**Issue #100** (auggiesoft) proposes a SQLite store for calls, radios and stats. This document
measures that design next to the alternatives, so one plan can be agreed before anyone builds it.

## Why

Users with large systems on slow disks (SD cards, USB sticks) reported:

- **Ignore clicks were slow.** One click took 5+ seconds to show, and several clicks in a row
  could end in the wrong state.
- **Too many whole-file rewrites.** The recorder rewrote whole files to change one small thing.

Both came from one pattern: state is kept in memory, but every change rewrote its whole file and
waited for the disk to confirm (`write_atomic` ends with an fsync), on the thread handling the
change. The interface added to it: a typed setting was saved whenever typing paused for 300 ms
("Save 1: F, Save 2: FI, Save 3: FIR…"). See [When the interface saves](#when-the-interface-saves).

## What's kept, and how (after #101)

| File | What | Who edits it | Written |
|---|---|---|---|
| `config.json` | the setup | the user (Setup, or by hand) | when a setting is done (left, Enter), inline: the user is told it saved |
| `<sys>.talkgroups.csv` | talkgroups, Ignore, tags | the user (Setup, Radio pages, by hand); Trunk Recorder's format | in the background; an Ignore or name edit is one row (`setTalkgroup`) |
| `<sys>.units.csv` | talker aliases learned on the air | the recorder | every 10 s when changed |
| `sharing.json` | what sharing with the directory did | the recorder | every 10 s when changed |
| `<site>.bandplan` | learned band plans / DMR channel tables | the recorder | every 10 s when changed |
| `conventional.heard.json` | codes each conventional frequency carried, counted | the recorder | every 60 s when changed |
| `tune.json` | AutoTune scores | the recorder | every 60 s when changed |
| `radio/<sys>.json` | the radio registry: talkgroups, radios, pairs, frequencies heard, a week by hour | the recorder | every 15 min (browser: 1 min), **whole** |
| `stats/YYYY-MM-DD.jsonl` | the dashboard's minute history | the recorder | appended every minute (statstore) |

- **Everything is written when recording stops.**
- **When each file is due** is decided by trunk-app's `saving.rs`. The desktop recorder and the
  browser build both follow it.
- **The writing** is done by trunk-pro's `saver.rs` on one thread. If a file is queued several
  times, only the latest copy is written. A write that fails is reported to every browser.

The one file still rewritten whole is the radio registry.

## The radio registry's size

`registry_size` (an ignored test in `trunk-app/src/stats/radio.rs`) fills one system's registry
to every cap: 5,000 talkgroups with a week of hours, 20,000 radios with their last 30
transmissions, 50,000 pairs. Measured on an M-series Mac:

| | JSON | copy (under the lock) | to JSON | read back |
|---|---|---|---|---|
| at the caps | 41 MB | 8 ms | 42 ms | 70 ms |
| about half | 22 MB | 3 ms | 18 ms | 38 ms |

Most of it is per-hour activity and each radio's recent transmissions. Real systems are sparser,
so these are upper bounds.

The caps also mean the registry **forgets** (issue #100's point 2). On a busy system the 2,000
affiliations and 5,000 call lengths fill within days, and past a cap the least recently heard go
without notice.

## The ways to store it, measured

`tools/storage-bench` (its own crate, so SQLite stays out of the app's dependencies until one is
chosen) plays the same activity into each way and counts the bytes the disk was sent
(`/proc/self/io`).

**Setup:**

- a system's registry at about half its caps (18.6 MB as JSON);
- one simulated hour of a busy system: a call a second (three talkers among 10,000 radios, 2,500
  talkgroups) and two affiliations a second;
- run on the Raspberry Pi CM5 testbed's built-in **eMMC** (flash like an SD card, usually faster
  and longer-lived; the testbed has no SD card);
- the whole, log and SQLite runs at 10 s repeated, with the bytes within 2 %. The facts runs were
  done once at each interval.

**The ways:**

| Way | What it writes |
|---|---|
| **whole** | the registry rewritten whole every 15 min (as now) |
| **log** | each changed row (`take_changes`) appended as a JSON line, onto a snapshot |
| **log-n** | the same, normalized: a talkgroup's current hour and a radio's new transmissions, not the whole row's lists |
| **sqlite** | each changed row upserted as one JSON value (WAL, `synchronous=NORMAL`) |
| **sqlite-n** | normalized tables: scalars, hours and transmissions apart |
| **facts** | issue #100's design: each call (with its record JSON), its transmissions and each affiliation appended with rowid keys; talkgroups' and radios' state upserted; indexes by talkgroup and by radio |

**Disk writes, GB per day of such a system** (the row ways save at the interval shown; whole is
always every 15 min):

| Saved every | whole | log | log-n | sqlite | sqlite-n | facts (#100) |
|---|---|---|---|---|---|---|
| 1 s | — | — | — | — | — | 7.2 |
| 10 s | 1.81 | 0.86 | **0.28** | 11.3 | 11.0 | 4.2 |
| 60 s | 1.81 | 0.73 | **0.16** | 9.5 | 11.1 | 1.70 |
| 5 min | 1.81 | 0.68 | **0.14** | 7.2 | 6.6 | **0.54** |

Other figures:

| | whole | log | sqlite | facts |
|---|---|---|---|---|
| a save, median (10 s) | 240 ms | 4 ms | 1.9 ms | 0.9 ms (10 s), 9 ms (5 min) |
| reading it back at startup | 110 ms | 290 ms (snapshot + an hour of lines) | 180 ms | (queried in place) |
| first import of an existing registry | — | 170 ms | 0.9 s | — |

What this shows:

- **SQLite writes whole pages, twice.** A batch that touches 200 rows scattered across tables and
  indexes writes about 200 × 4 kB to the WAL, and again at checkpoint. So when rows are updated
  in place, SQLite writes several times *more* than rewriting the 18 MB file every 15 minutes.
  Normalizing the tables doesn't help (sqlite-n), because the page count is the cost, not the row
  size.
- **Issue #100's facts design is SQLite's good case.** Appends with rowid keys land on the last
  page. Its data grows by only about 5 MB an hour (calls with their record JSON, transmissions,
  affiliations), which is roughly what the recorder already writes as call JSON sidecars. Its
  cost is all in how often it commits:
  - **1 s** (as proposed): 7.2 GB a day, four times today's;
  - **60 s**: about today's;
  - **5 min**: 0.54 GB, under a third of today's, *with a call index today doesn't have*.
- **A normalized append-only log writes the least** (0.14–0.28 GB a day), but it only stores the
  registry. It answers no new questions, and startup has to replay it.

## Issue #100: a SQLite store for calls, radios and stats

**What it proposes:**

- **Step 1, a call index:**
  - every concluded call is a row;
  - `history` and `olderCalls` are answered from it, rather than by walking the recordings tree
    (77 ms for three days today, about 10 s a year, minutes on a cold Pi);
  - new questions become possible: calls by talkgroup, by radio, by time. Today those read every
    record (1.2 s for one talkgroup after three days, about 150 s a year). In SQLite they take
    milliseconds.
- **Step 2, the registry as facts plus state:** no caps, with a retention setting instead.
- **Step 3, metrics (optional):** finished stats days imported.

**What stays the same:** the WAV and JSON files are still written, the minute rollups stay
append-only, and the wire protocol only gains commands. The browser build is untouched.

**Assessment:**

- **The call index is the strongest reason for SQLite.** No file layout gives it cheaply, and it
  is what the UI's missing questions need. A log can keep the registry cheaply, but it can't
  answer "every call on talkgroup 101 last month".
- **The design suits SQLite:** append-mostly, few indexes, and one writer that batches.
- **One change is needed: batch for minutes, not a second.** At 1 s it writes four times today's
  volume on flash, which is the problem we started from. At 1–5 min it writes the same or less,
  and a crash loses little:
  - **calls:** the files are on disk, and step 1's backfill indexes any the store missed when
    it starts;
  - **affiliations and the like:** a few minutes lost, and they're not kept long anyway;
  - **live answers:** the dashboard keeps answering from memory meanwhile, as now, so the batch
    interval only decides how much would need recovering, not how fresh answers are.

  Make it a setting: about 60 s on SSDs, 5 min on SD and eMMC.
- **State tables (talkgroups, radios) should be upserted once per batch, not once per event.**
  This is what the registry's new per-row change tracking (`Registry::take_changes`) is for: at
  each batch it lists the rows that changed, each once. Upserting per event multiplies page
  writes for busy radios.
- **Indexes:** each one adds scattered page writes. Keep to the ones a query in the UI uses: calls
  by talkgroup and time, transmissions by radio.
- **The C dependency is acceptable.** Every release target already compiles C, and the binary
  grows by about 1.5 MB. CI builds of all targets are the check.
- **Talkgroups and unit names as internal data** (agreed by the maintainer) fit in the same file,
  with CSV Import and Export, once steps 1 and 2 are in. #101's per-row `setTalkgroup` is already
  the interface's side of that. `config.json` stays a file. A conventional system's linked channel
  CSV stays a file, or becomes an explicit Reload.
- **The browser build** keeps files on OPFS, as #100 says. Its saving already follows the same
  `Session::due()` schedule.

**Suggested answers to #100's questions** (the maintainer's call):

1. **File:** `<data>/trunk-pro.sqlite`, beside the config. The capture folder may be a RAM spool
   or a network share, and SQLite locking is unreliable on network filesystems.
2. **Retention:** calls kept as long as their files are, with an option to keep the row after the
   files go (#25). Facts that aren't calls kept for a set number of days.
3. **Read access for plugins:** the HTTP API first, since it works for plugins and interfaces on
   other machines. The file path in `hello` can come later for local plugins.
4. **`annotations`:** later, once a transcription plugin needs it.
5. **The record JSON in `calls`:** yes, as proposed. The measurement above included it, and it's
   most of the 5 MB an hour.

## Recommendation

1. **Merge #101.** Nothing waits on the disk any more, there is one save schedule and one writer,
   talkgroup edits are per row, and the registry tracks changes per row.
2. **Adopt issue #100's store as the plan**, with saves batched for 1–5 minutes (a setting) and
   state upserted from `take_changes`. Its step 1 (the call index) goes first, as auggiesoft
   offers.
3. **Then step 2 (the registry).** It replaces the 15-minute whole-file rewrite. The caps become a
   retention setting.
4. **Then talkgroups and units as internal data**, with CSV Import and Export.
5. **If #100 isn't adopted:** store the registry as a normalized append-only log (log-n above), the
   least writing measured and no new dependency.

`config.json` stays a file either way.

The plan for building it, phase by phase, is in [sqlite-plan.md](sqlite-plan.md).

## When the interface saves

The other half of the reports: "the UI lacks a save button, and can be aggressive about
automatically doing things for you… every single keystroke would restart the plugin… Save 1: F,
Save 2: FI, Save 3: FIR…".

**What it did.** Setup's text fields passed every change into the config, with only a 300 ms
timer after the last change, so each pause in typing was a save. Each save:

- rewrote `config.json`, waiting for the disk;
- re-read every linked channel CSV;
- rebuilt the name lookup from every talkgroup file;
- sent the whole config to every browser;
- restarted any plugin whose settings changed, if it can't take new settings while running.

Typed into a system's short name, it renamed the system with each pause (`f`, `fi`, `fir`…), each
time moving its talkgroup file.

**What #101 changes:**

- **Settings typed in** are `DraftInput`s (`TextInput`, `TextArea`, `NumberInput`). Each is kept
  once: when the field is left, when Enter is pressed, or when its page goes away. Escape puts it
  back. The rule is in `docs/developer/ui-style.md`.
- **Switches, menus and buttons** still act at once.
- **The gain sliders** move as you drag them, and the gain is saved and set once you let go.
- **"Saving… / All changes saved / Couldn't save"** shows at the end of Setup's tabs. It covers
  config edits, talkgroup edits, and files the recorder couldn't write in the background.
- **Plugin settings that would restart a running plugin** wait for **Apply** (or **Discard**):
  - a plugin's own settings, when it doesn't take changes while running;
  - a system's copy of a plugin, always.

  Changes made while not recording save at once. Recording settings that need a restart already
  had their **Restart** button and banner.
- **No global Save button, on purpose.** Saving once per finished field means every save is one
  the user meant. A global Save button brings unsaved-changes state, edits lost on navigating
  away, and conflicts between browsers.

**Measured** in headless Chrome, with "fire4a" typed into a system's short name, pausing 500 ms
after each key:

| | saves while typing | saves in all |
|---|---|---|
| before | 6 | 6 |
| after | 0 | 1 (on leaving the field) |
| after, then Escape | 0 | 0 (field back to `p25`) |
