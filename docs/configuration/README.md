# The Config File

Everything Trunk Recorder Pro does is set in one JSON file. The interface's **Setup** and **Plugins**
pages edit it for you, so most people never open it. This section is for writing or reviewing it by
hand: where it lives, the rules it follows, and every key. For the "how and why" of each part, see the
guides; these pages list what each key does.

| Page | Covers |
|---|---|
| [sources.md](sources.md) | Radios and capture files (`sources`) |
| [audio-streams.md](audio-streams.md) | HTTP/HTTPS audio input (`audioStreams`) |
| [systems.md](systems.md) | Trunked systems (`systems`), talkgroup files, unit names, site lock |
| [conventional.md](conventional.md) | Conventional systems and channels (`conventional`), the channel CSV |
| [recording.md](recording.md) | Where and how calls are saved (`recording`), per-system rules, file names |
| [server-and-log.md](server-and-log.md) | The web server (`server`), the log (`log`), the dashboard's checks (`monitor`) |
| [plugins.md](plugins.md) | Which plugins run and their settings (`plugins`) |

## Where the file is

| Where you run it | Default location |
|---|---|
| macOS | `~/Library/Application Support/trunk-pro/config.json` |
| Linux, Raspberry Pi | `$XDG_CONFIG_HOME/trunk-pro/config.json`, else `~/.config/trunk-pro/config.json` |
| Windows | `%APPDATA%\trunk-pro\config.json` |
| Docker image | `/data/config/trunk-pro/config.json` inside the container (the `/data` volume) |
| Browser version | Not a file: the browser's `localStorage`, under the key `trp.config` |

To use another file, start the app with `--config`:

```bash
trunk-pro --config /home/me/scanner/config.json
```

If the file doesn't exist yet, the app starts with the defaults and writes the file the first time you
change a setting. The folder the config file is in is also where the app keeps what it learns and
installs (see [Files kept beside the config](#files-kept-beside-the-config)), so two recorders started
with `--config` files in different folders never share band plans, talker aliases or plugins.

## General rules

- **Keys are camelCase**: `controlChannelsHz`, `squelchDb`, `captureDir`.
- **Frequencies are in Hz, everywhere.** 857.9875 MHz is `857987500`. Keys carry their unit in the
  name: `…Hz`, `…Db`, `…S` (seconds), `…Kbps`, `…Mb`, `…PerS`. The Airspy gains are the driver's
  steps (`…Step`), not dB.
- **A key ending in `Csv` holds a file's contents, not a path** (`unitNames.csv`).
  A key ending in `File` holds a path (`channelFile`).
- **A missing key takes its default**, listed in each table. The few marked ✓ in the Required column
  have no default: a source without its `centerHz`, for example, makes the file unreadable.
- **A missing `sources` key gives one RTL-SDR** with default settings (first free dongle, Auto
  center, 2.4 MSPS, 25.4 dB), not none.
- **A value of the wrong type makes the whole file unreadable**: a string where a number belongs, an
  unknown source `type`, a fraction in an integer field (`"ppm": 1.5` on an RTL-SDR), a value that
  isn't one of a fixed list (`"level": "warn"` instead of `"warning"`, a system `"type": "P25x"`). The
  app then refuses to start, with `<file>: not a config this version reads (<what was wrong>)`. It
  never replaces a file it can't read with the defaults.
- **Fixed lists are closed.** Where a key takes one of a few names (a system's `type`, `modulation`,
  `vocoder`, `m4a.encoder`, a channel's `mode`…), the pages list them. They are read in any case
  (`"P25"` is `"p25"`), and `""` is the default; any other value is refused as above, where older
  versions fell back to the default without a word. A source's `type` is read in any case too, but
  can't be left empty.
- **Keys this version doesn't know are kept** and saved back as they were, at the top level and
  inside a trunked system, a site, a conventional system, `recording`, `server`, `log`, a key source
  and a delay rule. Notes of your own there survive the interface saving the file. Unknown keys
  anywhere else (inside a source, a channel, `expect`, `unitNames`, a system's own `recording`,
  `m4a`, `ramSpool`, an interface, a plugin's entry, `monitor`) are dropped the next time the file is
  saved.
- **Talkgroups aren't in the config.** Each system's talkgroup file is kept in the
  [store](#the-store-storage); Setup loads and downloads it as Trunk Recorder's CSV.
  A config from an older version that holds them (`talkgroupsCsv`) is upgraded when it's read: the
  talkgroups are moved out into those files, and the log says so.
- **The interface saves the whole file on every change**, so hand edits made while the app is running
  are overwritten by the next change in the interface. Stop the app, or make the change in the
  interface, rather than editing underneath it.
- **The file is written safely**: to a temporary file beside it, then renamed over it, so a crash or
  power cut leaves the old file or the new one, never half of each.
- **Problems are checked when recording starts**, not when the file is saved. See
  [Validation](#validation).

## Top level

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `sources` | | one RTL-SDR | array | Radios and capture files. [sources.md](sources.md) |
| `audioStreams` | | `[]` | array | Independent HTTP/HTTPS call systems, split on silence. [audio-streams.md](audio-streams.md) |
| `systems` | | `[]` | array | Trunked systems, each with its sites. [systems.md](systems.md) |
| `conventional` | | `[]` | array | Conventional systems, each a group of single-frequency channels. [conventional.md](conventional.md) |
| `recording` | | see page | object | Where calls go and which are kept. [recording.md](recording.md) |
| `server` | | see page | object | The built-in interface's address and port; the other interfaces, each on a port of its own. [server-and-log.md](server-and-log.md#server) |
| `log` | | see page | object | How much is logged and where to. [server-and-log.md](server-and-log.md#log) |
| `monitor` | | see page | object | The dashboard's internet checks. [server-and-log.md](server-and-log.md#monitor) |
| `storage` | | `{ "backend": "sqlite", "retentionDays": 120, "commitSeconds": 300, "statsDays": 365 }` | object | The call store: where it's kept, how long calls stay in it, and how far apart it commits. [See below](#the-store-storage) |
| `plugins` | | `{}` | object | Which plugins run and their recorder-wide settings, by plugin id. [plugins.md](plugins.md) |
| `keys` | | `{}` | object | P25 encryption key sources, by name: where each set of keys is read from. [Encryption keys](systems.md#encryption-keys-keys) |

### The store (`storage`)

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
mirrors each call once, and a call index lost is rebuilt from them. `backend` says which database:

| `backend` | For | Kept |
|---|---|---|
| `"sqlite"` (default) | a Raspberry Pi to a mid-size server | `trunk-pro.sqlite`, beside the config |
| `"postgres"` | servers: years of history, your own backups and replication | the PostgreSQL database `url` names: `"postgres://user:password@host/database"`. One recorder per database (or schema): a second one pointed at it runs with its store off |

These are in Setup → **Data**. What the store holds, each table's and index's size and how
they've grown, and what it writes to disk a day are on the dashboard's Platform → **Storage**.
What it writes is counted on Linux (the store's writer thread, as the kernel counts it:
commits, checkpoints and the daily backup). With PostgreSQL it's the server's write-ahead log, all
of its databases. macOS doesn't count a thread's writes, so it isn't shown there.

`lowWrite: true` writes as little as can be, for an SD card:

- no call index (the call list and every query answer from the recent calls, and say so);
- the dashboard's history by the hour only;
- a commit an hour.

Everything else is kept as usual.

**Calls by talkgroup and by radio** (`talkgroupIndex`, `radioIndex`; both on by default). The
store keeps two indexes for finding calls:

- by **talkgroup**: a talkgroup's calls, newest first;
- by **radio**: the calls a radio talked on, newest first.

They're used when you pick a talkgroup or a radio on the dashboard's **Radio** page, whose call
list pages back as far as the store keeps calls. They're also used by anything else that asks the
recorder for one talkgroup's or one radio's calls: `callsQuery` with `talkgroup` or `unit`
(interfaces, `client.js`'s `rec.calls`), `GET /api/v1/calls?talkgroup=…` or `?unit=…`, and
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

A store that can't be opened (a full disk, a PostgreSQL server that can't be reached, another
recorder using it) is left off. The recorder records anyway, every connection is told, and
nothing is kept until it opens. `url` isn't sent to listen-only interfaces.

Once a day, SQLite copies what can't be made again to `trunk-pro.backup`, beside the store: the
systems, talkgroups, talker aliases and other documents, and the radio registry. It's small, and
written whole each time. Calls aren't in it, because they're rebuilt from their files. With
PostgreSQL, backups are the database's.

`retentionDays` (default 120) is how long a call stays in the store; older ones are removed a
thousand at a time, when the app starts and every hour after. `0` keeps them all. It never deletes
the calls' audio and JSON files: they stay where they were recorded (deleting recordings is issue
#25).

`commitSeconds` (default 300, 10 to 3600) is how far apart the store's commits are when calls are
coming in. Each commit writes whole pages, twice on SQLite (the log, then the database), so fewer
commits write far less: on the Pi's eMMC, a busy system (a call a second) writes 0.21 GB a day at
300, 0.61 GB at 60. On a PostgreSQL server, 10 to 60 is fine. A finished call is in the call list
and every query at once whatever this is. A crash loses at most one interval of the index, and
the next start finds those calls again from their `.json` files (but not where none are kept, as
with upload-only settings).

These are read when the app starts.

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
  - On the Pi's eMMC this writes about 17 MB a day. A year of hours is about 70 MB on disk for a
    recorder with two systems.
- **The radio registry** is stored as the changes of each 15 minutes, and written whole once a
  day. On the Pi's eMMC that's 0.10 GB a day for a busy system, against 0.44 GB for the whole file
  every 15 minutes it was before.

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

While in beta, a store made by another version is set aside and a new one started (nothing is
migrated): SQLite's file is renamed `trunk-pro.sqlite.v<n>-<time>`, PostgreSQL's tables are made
afresh. Views of your own built on PostgreSQL's tables go with them. The call index is then
rebuilt from the recordings' `.json` files.

## A minimal config

One RTL-SDR placed automatically, one P25 system, recordings in a folder of your choice:

```json
{
  "sources": [
    { "type": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "ppm": 0 }
  ],
  "systems": [
    { "shortName": "dcfd", "type": "p25", "sites": [{ "controlChannelsHz": [857987500, 858987500] }] }
  ],
  "recording": { "captureDir": "/home/me/TrunkRecorderPro" }
}
```

`centerHz: 0` means Auto: the app centers the dongle over the system's control channels. Everything
not written takes its default.

## A fuller example

Three RTL-SDRs, a P25 system, a UHF SmartNet system using the custom (OBT) band plan, and a small
conventional system, with an upload plugin turned on. JSON has no comments, so the notes are below
the example.

```json
{
  "sources": [
    { "type": "rtlsdr", "serial": "00000101", "centerHz": 0, "rateHz": 2400000,
      "gainDb": 25.4, "ppm": 1, "autoTune": true },
    { "type": "rtlsdr", "serial": "00000102", "centerHz": 0, "rateHz": 2400000,
      "gainDb": 28, "ppm": 0, "guardHz": 100000 },
    { "type": "rtlsdr", "serial": "00000103", "centerHz": 154400000, "rateHz": 2400000,
      "gainDb": 30, "ppm": 0 }
  ],
  "systems": [
    {
      "shortName": "dcfd",
      "name": "DC Fire and EMS",
      "type": "p25",
      "sites": [{ "controlChannelsHz": [857987500, 858987500], "modulation": "auto", "expect": { "nac": 1091 } }],
      "recording": { "minCallS": 1 },
      "plugins": { "openmhz": { "apiKey": "...", "systemName": "dcfd" } }
    },
    {
      "shortName": "wmata",
      "type": "smartnet",
      "defaultMode": "analog",
      "sites": [{
        "controlChannelsHz": [489087500],
        "bandplan": "400_custom",
        "bandplanBaseHz": 489012500,
        "bandplanSpacingHz": 12500,
        "bandplanOffset": 380,
        "bandplanHighHz": 490000000
      }]
    }
  ],
  "conventional": [
    {
      "shortName": "county-fire",
      "squelchDb": 8,
      "channels": [
        { "freqHz": 154430000, "mode": "fm", "name": "Fire Dispatch", "talkgroup": 1001 },
        { "freqHz": 154325000, "mode": "fm", "name": "Station 1", "tone": "151.4" }
      ]
    }
  ],
  "recording": {
    "captureDir": "/home/me/TrunkRecorderPro",
    "archiveM4a": true
  },
  "server": { "bind": "127.0.0.1", "port": 8080, "autoStart": true },
  "plugins": {
    "openmhz": { "enabled": true }
  }
}
```

Notes on the example (the frequencies and band plan numbers are placeholders; use your own system's):

- **Sources.** The dongles are named by serial, so each keeps its own `ppm`. The first two are Auto
  (`centerHz: 0`): the first is placed over the first system, the second over whatever the first
  doesn't reach. The second leaves 100 kHz unused at each edge instead of the default 75 kHz. The
  third is set by hand to 154.4 MHz for the conventional channels (Auto would work too).
- **`dcfd`.** `modulation: "auto"` detects which P25 receiver the channels need, then runs only that. `expect`
  locks it to NAC `0x443`, written in decimal (1091). Its own `recording` drops calls under a second;
  every other rule follows the global `recording`. Its OpenMHz key is a per-system plugin setting.
- **`wmata`.** A UHF SmartNet system needs `400_custom` and the four band plan numbers; Find my
  system fills them in. `defaultMode: "analog"` records a talkgroup it has never seen granted as
  analog FM.
- **`county-fire`.** Two analog channels. The first is filed under talkgroup 1001; the second records
  only transmissions with a 151.4 Hz CTCSS tone and is filed under 154325 (the frequency in kHz).
- **`recording`.** Every call also gets an `.m4a`.
- **`server`.** Recording starts when the app starts.
- **`plugins`.** OpenMHz runs while recording; its settings for `dcfd` are in that system.

## Validation

The file is read when the app starts; only its format is checked then (see the rules above). Whether
it makes sense is checked when recording starts: if it doesn't, **Start** refuses with a message
saying what to fix. The interface runs the same checks as you edit: until the first problem is
fixed, **Start** is greyed out (hover over it to see why) and the dashboard pages show the problem in
a banner with **Open setup**.

Recording won't start if:

- there is no source;
- a `file` source has no `path` (`Choose the capture file to replay.`);
- an RTL-SDR's `bufLen` isn't a multiple of 512 or is over 16 MiB, or its `buffers` is over 256;
- there is nothing to record: no trunked system that is enabled and has a control channel, and no
  enabled channel in an enabled conventional system;
- a system, trunked or conventional, has an empty `shortName`, or two share one (enabled or not);
- an Auto source (`centerHz: 0`) can't be placed: nothing is left for it to cover, or the channels it
  would have to cover don't fit in its bandwidth;
- a SmartNet system's `bandplan` isn't one it knows, or `400_custom` lacks `bandplanBaseHz`,
  `bandplanSpacingHz` or `bandplanHighHz`, or has the high frequency at or below the base;
- a DMR or NXDN system has a control channel, `dmrChannelsHz` or `nxdnChannelsHz` frequency outside
  every source's usable bandwidth;
- a trunked system's site has no control channel inside any source's usable bandwidth;
- two sites of one system have the same name;
- an enabled conventional frequency is in two conventional systems, is 0 or less, or is outside every
  source's usable bandwidth;
- a channel's `tone` can't be read for its mode (including a CTCSS tone that isn't a standard one:
  `151.5 Hz isn't a standard CTCSS tone — 151.4?`);
- rows sharing a frequency have different modes, two of them have no `tone`, or two have codes that
  can't be told apart (`D023N` and `D047I` are the same DCS signal);
- a conventional channel's `bandwidthKhz` is outside 4–20, or its system's `demod` has `amLowCutHz`
  outside 0–600, `squelchOpenMs` outside 0–500 or `squelchHangMs` outside 0–1000;
- a `filenameFormat` (the global one, or a system's own) has a `{` with no `}` or a token that isn't
  listed under [File names](recording.md#file-names) (`Unknown tokens: {shortName}, {tg}.`);
- a linked `channelFile` can't be read.

"Usable bandwidth" is the source's sample rate less its guard band at each edge: see
[sources.md](sources.md#guard-band-and-usable-bandwidth).

## Changes while recording

You can change settings while recording. Most take effect the next time you press **Start** (Stop,
then Start). A few apply at once:

| Applies at once | Applies at the next Start | Applies when the app restarts |
|---|---|---|
| A trunked system's talkgroup file: an Ignore or Priority change takes effect immediately | Sources, systems, conventional channels, squelch | `server.bind`, `server.port` |
| Plugin settings (`plugins`, each system's `plugins`) and `recording.m4a`: running plugins restart with them | Recording rules, `filenameFormat`, `captureDir` | `monitor.probeHosts` |
| `log` settings, `monitor.networkChecks` | `recording.ramSpool` (the interface says "from the next start") | |
| `server.allowedOrigins`, `server.interfaces` (an interface moves to its new port at once) | A linked `channelFile` is re-read at every Start | |

## Files kept beside the config

The app keeps these in the folder the config file is in. With the default config that is the app's
config folder above; with `--config elsewhere/config.json`, it's `elsewhere/`. Talkgroups, band
plans, talker aliases, the Heard list, the radio registry and the dashboard's history are kept
[in the store](#the-store-storage).

| File or folder | What it holds |
|---|---|
| `trunk-pro.sqlite` | The [store](#the-store-storage) (SQLite), and `trunk-pro.backup`, its daily copy of what can't be made again |
| `plugins/<id>/` | Installed plugins |
| `plugin-data/<id>/` | Each plugin's own data (calls waiting to upload, for example), and its working folder |
| `plugin-registry.json` | The plugin store's cached index |
| `interfaces/<id>/` | Interfaces installed from the interface store |
| `interface-registry.json` | The interface store's cached index |
| `plugin-hold/` | Finished calls waiting out a [plugin delay](plugins.md#delays), one `.json` each. Deleting one drops that call for its plugin |
| `logs/` | Log files, when `log.file` is on and `log.dir` is empty |
| `spool/` | macOS only: where the RAM spool's RAM disk is mounted, when `recording.ramSpool` is on |

A linked conventional `channelFile` and a relative `log.dir` or `server.interfaces` path are also
taken from this folder.


