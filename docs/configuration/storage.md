# The store

`storage` in the config: the database where the recorder keeps what it keeps between runs, and
how much it writes. In Setup: **Data**. For where the files themselves are, see
[Files kept beside the config](README.md#files-kept-beside-the-config).

```json
"storage": { "backend": "sqlite", "retentionDays": 120, "commitSeconds": 300, "statsDays": 365 }
```

| Key | Default | Description |
|---|---|---|
| `backend` | `"sqlite"` | `"sqlite"` or `"postgres"`. See [Which database](#which-database) |
| `url` | `""` | PostgreSQL's connection string (`"postgres://user:password@host/database"`). Not sent to View users or listen-only interfaces |
| `lowWrite` | `false` | Write as little as can be, for an SD card. See [Writing less](#writing-less) |
| `talkgroupIndex`, `radioIndex` | `true` | The call indexes by talkgroup and by radio. See [Calls by talkgroup and by radio](#calls-by-talkgroup-and-by-radio) |
| `retentionDays` | `120` | Days a call stays in the store; `0` keeps them all |
| `commitSeconds` | `300` | Seconds between commits when calls are coming in, 10 to 3600 |
| `statsDays` | `365` | Days of the dashboard's history; `0` keeps it all |
| `registryRadios` | `20000` | Radios the radio registry keeps per system; `0`: no cap. See [The radio registry](#the-radio-registry) |
| `registryDays` | `365` | Days a talkgroup, radio or pair not heard is kept; `0`: however old |

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../img/shots/setup-data-dark.png">
  <img src="../img/shots/setup-data-light.png" alt="The Data tab of Setup">
</picture>

## What it keeps

Everything the recorder keeps between runs is kept in a database:

- the call index, so the interface can list calls by talkgroup, radio and time as far back as it
  keeps them;
- each system's talkgroups;
- talker aliases heard over the air;
- the radio registry (the talkgroups, radios and frequencies each system has heard);
- band plans;
- the codes conventional frequencies carried;
- AutoTune's scores;
- what sharing with the directory did;
- the dashboard's history.

The recordings and their `.json` files are written as before. They're the calls' truth: the store
mirrors each call once, and a call index lost is rebuilt from them.

## Which database

`backend` says which database:

| `backend` | For | Kept |
|---|---|---|
| `"sqlite"` (default) | a Raspberry Pi to a mid-size server | `trunk-pro.sqlite`, beside the config |
| `"postgres"` | servers: years of history, your own backups and replication | the PostgreSQL database `url` names: `"postgres://user:password@host/database"`. One recorder per database (or schema): a second one pointed at it runs with its store off |

These are in Setup → **Data**. What the store holds, each table's and index's size and how
they've grown, and what it writes to disk a day are on the dashboard's Platform → **Storage**.
What it writes is counted on Linux (the store's writer thread, as the kernel counts it:
commits, checkpoints and the daily backup). With PostgreSQL it's the server's write-ahead log, all
of its databases. macOS doesn't count a thread's writes, so it isn't shown there.

## Writing less

`lowWrite: true` writes as little as can be, for an SD card:

- no call index (the call list and every query answer from the recent calls, and say so);
- the dashboard's history by the hour only;
- a commit an hour.

Everything else is kept as usual.

### Calls by talkgroup and by radio

`talkgroupIndex` and `radioIndex`, both on by default. The store keeps two indexes for finding
calls:

- by **talkgroup**: a talkgroup's calls, newest first;
- by **radio**: the calls a radio talked on, newest first.

They're used when you pick a talkgroup or a radio on the dashboard's **Radio** page, whose call
list pages back as far as the store keeps calls. They're also used by anything else that asks the
recorder for one talkgroup's or one radio's calls: `callsQuery` with `talkgroup` or `unit`
(interfaces, `client.js`'s `rec.queryCalls`), `GET /api/v1/calls?talkgroup=…` or `?unit=…`, and
plugins' `calls.query`.

Turning one off saves the writes it costs. Its lists then show only the recent calls, and the
answer says why. Nothing else changes: recording, the Listen page, a system's call list and the
other searches use other indexes. On a busy system the radio index is most of what the store
writes, and the talkgroup index about a third as much. Platform → Storage shows how big each is.

What the store writes on a Pi's eMMC in a day, for a busy system (a call a second, 10,000 radios,
2,500 talkgroups; `db::tests::day_write_budget`):

| | GB a day |
|---|---|
| Standard | 1.6 |
| Standard, `radioIndex: false` | 0.8 |
| `lowWrite: true` | 0.4 |

A system like DC Fire and EMS (3,700 calls a day, 23 talkgroups, 500 radios) writes a small
fraction of that.

## When it can't be opened, and backups

A store that can't be opened (a full disk, a PostgreSQL server that can't be reached, another
recorder using it) is left off. The recorder records anyway, every connection is told, and
nothing is kept until it opens. `url` isn't sent to View users or listen-only interfaces.

Once a day, SQLite copies what can't be made again to `trunk-pro.backup`, beside the store: the
systems, talkgroups, talker aliases and other documents, and the radio registry. It's small, and
written whole each time. Calls aren't in it, because they're rebuilt from their files. With
PostgreSQL, backups are the database's.

## How long things are kept

`retentionDays` (default 120) is how long a call stays in the store; older ones are removed a
thousand at a time, when the app starts and every hour after. `0` keeps them all. It never deletes
the calls' audio and JSON files: they stay where they were recorded (deleting recordings is issue
#25).

### The radio registry

The radio registry is every talkgroup, radio and pair of radios each system has heard: the
Radio page, first-heard alerts and `radioQuery` answer from it. It's held in memory (about 2 kB a
radio) and saved in the store as it changes. Two settings bound it, per system:

| Setting | Default | |
|---|---|---|
| `registryRadios` | 20,000 | Radios kept. Past it, the least recently heard go. Talkgroups get a quarter as many (5,000 at least) and pairs two and a half times as many. `0`: no cap, so the days alone bound it. |
| `registryDays` | 365 | Days a talkgroup, radio or pair not heard is kept. `0`: however old. |

What's forgotten comes back as new if it's heard again. A statewide system can fill 20,000 radios
in days, so raise it on a computer with the memory. Both apply at once (Setup → Data), and
Platform → Storage shows how full each system's registry is. `radioQuery`'s `summary` gives the
caps (`caps`), and a `limit` up to `caps.radios` returns every radio heard.

### Commits

`commitSeconds` (default 300, 10 to 3600) is how far apart the store's commits are when calls are
coming in. Each commit writes whole pages, twice on SQLite (the log, then the database), so fewer
commits write far less: on the Pi's eMMC, a busy system (a call a second) writes 0.21 GB a day at
300, 0.61 GB at 60. On a PostgreSQL server, 10 to 60 is fine. A finished call is in the call list
and every query at once whatever this is. A crash loses at most one interval of the index, and
the next start finds those calls again from their `.json` files (but not where none are kept, as
with upload-only settings).

These are read when the app starts.

### Talkgroups, history and the registry on disk

- **Talkgroups** are kept a row each (table `talkgroups`): number, alpha tag, description, tag,
  category, mode, priority, preferred NAC and site, and Ignore, in the order loaded, with the
  comment line directly above a row and any columns of the file's own.
  - An edit in Setup (Ignore, a name) writes only that row, and commits at once.
  - Setup's **Download** exports a talkgroup file (Trunk Recorder's CSV); **Load CSV…** replaces
    the list.
- **The dashboard's history** goes back `statsDays` (default 365; `0` keeps it all).
  - Every series is kept by the hour, and the dashboard's charts offer 30 days, 90 days and a
    year, as far as that goes.
  - Only the last day is held in memory, by the minute. Anything longer is asked of the store and
    comes back as a few hundred points per series, so a long chart doesn't grow the recorder or
    the browser tab.
  - With no store, the charts have only the last week, in memory.
  - On the Pi's eMMC this writes about 17 MB a day. A year of hours is about 70 MB on disk for a
    recorder with two systems.
- **The radio registry** is stored as the changes of each 15 minutes, and written whole once a
  day. On the Pi's eMMC that's 0.10 GB a day for a busy system, against 0.44 GB for the whole file
  every 15 minutes it was before.

## Reporting from the store

To report from the store yourself (the `sqlite3` shell, Grafana, Metabase, Datasette…), use its
views. They give times as timestamps (UTC; PostgreSQL's `timestamptz`, ISO 8601 text in SQLite),
systems by short name, and talkgroups and radios by name:

| View | One row per |
|---|---|
| `calls_report` | call: system, start and stop, talkgroup with its name and category, frequency, site, signal, path |
| `transmissions_report` | radio's transmission in a call: its unit, name (talker alias or tag), time, position |
| `stats_report` | dashboard series' hour: average, low, high, minutes measured |

```sql
select talkgroup_name, count(*) from calls_report
where start_at >= '2026-10-01' group by talkgroup_name order by 2 desc;
```

The tables behind them may change from version to version; the views are what to build on.

## A store from another version

While in beta, a store made by another version is set aside and a new one started (nothing is
migrated): SQLite's file is renamed `trunk-pro.sqlite.v<n>-<time>`, PostgreSQL's tables are made
afresh. Views of your own built on PostgreSQL's tables go with them. The call index is then
rebuilt from the recordings' `.json` files.
