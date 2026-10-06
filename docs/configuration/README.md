# The Config File

Everything Trunk Recorder Pro does is set in one JSON file. The interface's **Setup** and **Plugins**
pages edit it for you, so most people never open it. This section is for writing or reviewing it by
hand: where it lives, the rules it follows, and every key. For the "how and why" of each part, see the
guides; these pages list what each key does.

| Page | Covers |
|---|---|
| [sources.md](sources.md) | Radios and capture files (`sources`) |
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
- **A key ending in `Csv` holds a file's contents, not a path** (`talkgroupsCsv`, `unitNames.csv`).
  A key ending in `File` holds a path (`channelFile`).
- **A missing key takes its default**, listed in each table. The few marked ✓ in the Required column
  have no default: a source without its `centerHz`, for example, makes the file unreadable.
- **A missing `sources` key gives one RTL-SDR** with default settings (first free dongle, Auto
  center, 2.4 MSPS, 25.4 dB), not none.
- **A value of the wrong type makes the whole file unreadable**: a string where a number belongs, an
  unknown source `type`, a fraction in an integer field (`"ppm": 1.5` on an RTL-SDR), a misspelled
  value in a fixed list (`"level": "warn"` instead of `"warning"`). The app then refuses to start, with
  `<file>: not a config this version reads (<what was wrong>)`. It never replaces a file it can't read
  with the defaults.
- **Keys this version doesn't know are kept** and saved back as they were, at the top level and
  inside a trunked system, a conventional system, `recording`, `server` and `log`. Notes of your own
  there survive the interface saving the file. Unknown keys anywhere else (inside a source, a
  channel, `expect`, `unitNames`, a system's own `recording`, `m4a`, `ramSpool`, an interface, a
  plugin's entry, `monitor`) are dropped the next time the file is saved.
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
| `systems` | | `[]` | array | Trunked systems, one entry per site. [systems.md](systems.md) |
| `conventional` | | `[]` | array | Conventional systems, each a group of single-frequency channels. [conventional.md](conventional.md) |
| `recording` | | see page | object | Where calls go and which are kept. [recording.md](recording.md) |
| `server` | | see page | object | The web interface's address and port, custom interfaces. [server-and-log.md](server-and-log.md#server) |
| `log` | | see page | object | How much is logged and where to. [server-and-log.md](server-and-log.md#log) |
| `monitor` | | see page | object | The dashboard's internet checks. [server-and-log.md](server-and-log.md#monitor) |
| `plugins` | | `{}` | object | Which plugins run and their recorder-wide settings, by plugin id. [plugins.md](plugins.md) |

## A minimal config

One RTL-SDR placed automatically, one P25 system, recordings in a folder of your choice:

```json
{
  "sources": [
    { "type": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "ppm": 0 }
  ],
  "systems": [
    { "shortName": "dcfd", "type": "p25", "controlChannelsHz": [857987500, 858987500] }
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
      "controlChannelsHz": [857987500, 858987500],
      "modulation": "auto",
      "talkgroupsCsv": "Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category\n1201,4b1,D,FD Disp,Fire Dispatch,Fire Dispatch,DC Fire\n",
      "talkgroupsName": "dcfd.csv",
      "expect": { "nac": 1091 },
      "recording": { "minCallS": 1 },
      "plugins": { "openmhz": { "apiKey": "...", "systemName": "dcfd" } }
    },
    {
      "shortName": "wmata",
      "type": "smartnet",
      "controlChannelsHz": [489087500],
      "bandplan": "400_custom",
      "bandplanBaseHz": 489012500,
      "bandplanSpacingHz": 12500,
      "bandplanOffset": 380,
      "bandplanHighHz": 490000000,
      "defaultMode": "analog"
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
    "compressWav": true
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
- **`dcfd`.** `modulation: "auto"` runs both P25 receivers and keeps the better frame. `expect`
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
- there is nothing to record: no trunked system that is enabled and has a control channel, and no
  enabled channel in an enabled conventional system;
- a system, trunked or conventional, has an empty `shortName`, or two share one (enabled or not);
- an Auto source (`centerHz: 0`) can't be placed: nothing is left for it to cover, or the channels it
  would have to cover don't fit in its bandwidth;
- a SmartNet system's `bandplan` isn't one it knows, or `400_custom` lacks `bandplanBaseHz`,
  `bandplanSpacingHz` or `bandplanHighHz`, or has the high frequency at or below the base;
- a DMR or NXDN system has a control channel, `dmrChannelsHz` or `nxdnChannelsHz` frequency outside
  every source's usable bandwidth;
- a trunked system has no control channel inside any source's usable bandwidth;
- there are more than 256 conventional systems;
- an enabled conventional frequency is in two conventional systems, is 0 or less, or is outside every
  source's usable bandwidth;
- a channel's `tone` can't be read for its mode (including a CTCSS tone that isn't a standard one:
  `151.5 Hz isn't a standard CTCSS tone — 151.4?`);
- rows sharing a frequency have different modes, two of them have no `tone`, or two have codes that
  can't be told apart (`D023N` and `D047I` are the same DCS signal);
- a linked `channelFile` can't be read.

"Usable bandwidth" is the source's sample rate less its guard band at each edge: see
[sources.md](sources.md#guard-band-and-usable-bandwidth).

## Changes while recording

You can change settings while recording. Most take effect the next time you press **Start** (Stop,
then Start). A few apply at once:

| Applies at once | Applies at the next Start | Applies when the app restarts |
|---|---|---|
| A trunked system's talkgroup file (`talkgroupsCsv`): an Ignore or Priority change takes effect immediately | Sources, systems, conventional channels, squelch | `server.bind`, `server.port` |
| Plugin settings (`plugins`, each system's `plugins`) and `recording.m4a`: running plugins restart with them | Recording rules, `filenameFormat`, `captureDir` | `monitor.probeHosts` |
| `log` settings | `recording.ramSpool` (the interface says "from the next start") | |
| `server.allowedOrigins`, `server.interfaces`, `server.home` | A linked `channelFile` is re-read at every Start | |

## Files kept beside the config

The app keeps these in the folder the config file is in. With the default config that is the app's
config folder above; with `--config elsewhere/config.json`, it's `elsewhere/`.

| File or folder | What it holds |
|---|---|
| `<shortName>.bandplan` | Each trunked system's learned band plan (P25 IDEN tables, DMR and NXDN channel tables), so grants can be followed at once next time |
| `<shortName>.units.csv` | Talker aliases heard over the air, per system |
| `conventional.heard.json` | The tones, NACs, colour codes and RANs each conventional frequency carried (the Heard list) |
| `radio/` | The radio registry: the talkgroups, radios and frequencies each system has heard, one `.json` per system |
| `stats/` | The dashboard's history: a `YYYY-MM-DD.jsonl` per day (UTC), gzipped once the day is over, deleted after about a week |
| `plugins/<id>/` | Installed plugins |
| `plugin-data/<id>/` | Each plugin's own data (calls waiting to upload, for example), and its working folder |
| `plugin-registry.json` | The plugin store's cached index |
| `logs/` | Log files, when `log.file` is on and `log.dir` is empty |
| `spool/` | macOS only: where the RAM spool's RAM disk is mounted, when `recording.ramSpool` is on |

A linked conventional `channelFile` and a relative `log.dir` or `server.interfaces` path are also
taken from this folder.

Deleting a `.bandplan` or `.units.csv` file is safe: the app learns it again from the air.
