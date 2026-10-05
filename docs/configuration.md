# Configuration

Everything the recorder does is set in one JSON file. The interface's Setup
and Plugins pages edit it for you; this page is for writing or reviewing it
by hand. The schema is defined in
[`crates/trunk-app/src/config.rs`](../crates/trunk-app/src/config.rs). For
how the pieces fit together, see [architecture.md](architecture.md).

## The file

| OS | Default location |
|---|---|
| macOS | `~/Library/Application Support/trunk-pro/config.json` |
| Linux | `$XDG_CONFIG_HOME/trunk-pro/config.json`, else `~/.config/trunk-pro/config.json` |
| Windows | `%APPDATA%\trunk-pro\config.json` |
| Browser build | the browser's `localStorage` (`trp.config`) |

`trunk-pro --config <file>` uses another file.

General rules:

- Keys are camelCase.
- A missing key takes its default (listed below), except the few fields
  marked **required**.
- Keys this version doesn't know are kept and saved back as they were at the
  top level and in a system, a conventional system, `recording`, `server` and
  `log`, so notes of your own survive being saved from the interface. Inside
  a source, a channel, `unitNames`, `expect`, `m4a`, `interfaces` or a
  plugin's entry they are dropped on save.
- A value of the wrong type (a string where a number belongs, an unknown
  source `type`, a fraction in an integer field) makes the whole file unreadable.
  The app then refuses to start, saying `not a config this version reads`.
- Frequencies are in **Hz**, everywhere. Keys carry their unit: `…Hz`,
  `…Db`, `…S`, `…Kbps`, `…PerS`; the Airspy gains are driver steps (`…Step`).
- A key ending in `Csv` holds a file's **contents** (`talkgroupsCsv`); one
  ending in `File` holds a **path** (`channelFile`).
- The file is only checked for *problems* (overlapping channels, nothing in
  range, duplicate names…) when recording starts, not when it is saved. See
  [Validation](#validation).
- A missing `sources` key gives one RTL-SDR with default settings, not none.

A minimal config: one dongle, one P25 system.

```json
{
  "sources": [
    { "type": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "gainDb": 25.4, "ppm": 0 }
  ],
  "systems": [
    { "shortName": "dcfd", "type": "p25", "controlChannelsHz": [857987500, 858987500] }
  ],
  "recording": { "captureDir": "/home/me/TrunkRecorderPro" }
}
```

### Top level

| Key | Type | Default | |
|---|---|---|---|
| `sources` | array | one default RTL-SDR | Radios and capture files: [Sources](#sources) |
| `systems` | array | `[]` | Trunked systems, one entry per site: [Trunked systems](#trunked-systems) |
| `conventional` | array | `[]` | Conventional systems: [Conventional systems](#conventional-systems) |
| `recording` | object | see below | Where and how calls are saved: [Recording](#recording) |
| `server` | object | see below | The web server: [Server](#server) |
| `log` | object | see below | Logging: [Log](#log) |
| `plugins` | object | `{}` | Which plugins run, and their settings: [Plugins](#plugins) |
| `monitor` | object | see below | The dashboard's checks: [Monitor](#monitor) |

## Sources

Each source is one radio (or a capture file). It has a `type` and covers its
sample rate around its centre, less a guard band at each edge.

- **`centerHz` = 0 means Auto.** The source is centred over whatever the
  sources before it don't cover yet: systems' control channels, DMR
  `dmrChannelsHz`, NXDN `nxdnChannelsHz` and known voice channels, and
  conventional channels.
- **`ppm` corrects the radio's frequency error.** A frequency *f* is tuned
  as *f* / (1 + ppm·10⁻⁶).
- **`autoTune` follows the measured error.** The error is measured on P25
  and SmartNet control channels. With `autoTune` on, new voice channels open
  at the corrected frequency, and a P25 control channel more than 150 Hz off
  is reopened (at most every 200 s).
- **`guardHz` is left unused at each edge** (default 75 000), where the
  radio's anti-alias filter rolls off: a channel is used only if it is within
  `rateHz` / 2 − `guardHz` of the centre (never less than a quarter of the
  rate). Setup → Radios → *Profile roll-off* measures where the radio's noise
  floor sags and suggests a value; so does `trunk-pro rolloff`. Trunk Recorder
  left 32–64 kHz, depending on the rate.

### `"type": "rtlsdr"`

| Key | Type | Default | |
|---|---|---|---|
| `serial` | string | **required** | `""` = the first free dongle |
| `centerHz` | number | **required** | Hz; 0 = Auto |
| `rateHz` | number | **required** | Samples/s. 2 400 000 is typical; 2 048 000, 2 560 000, 3 200 000, 1 920 000, 1 024 000 also work |
| `gainDb` | number | `25.4` | Tuner gain, dB (ignored with `agc`) |
| `agc` | bool | `false` | The tuner's AGC instead of `gainDb` |
| `ppm` | integer | **required** | Frequency correction. Whole numbers only |
| `autoTune` | bool | `false` | |
| `guardHz` | number | `75000` | Hz left unused at each edge of the band |

Only R820T / R828D tuners are driven natively. Use a `soapy` source with
`"args": "driver=rtlsdr"` for E4000, FC0012, FC0013 and FC2580 dongles.

### `"type": "usrp"` (needs UHD installed)

| Key | Type | Default | |
|---|---|---|---|
| `args` | string | `""` | UHD device arguments: `""` = the first found, `serial=…`, `addr=192.168.10.2`. `num_recv_frames=256` is added unless set (UHD's default buffer overflows at high rates) |
| `centerHz` | number | **required** | Hz; 0 = Auto |
| `rateHz` | number | **required** | Any rate the device's clock supports, e.g. 8 000 000 |
| `gainDb` | number | `40` | dB |
| `agc` | bool | `false` | The device's AGC (B200 / B210 / E3xx) |
| `antenna` | string | `""` | e.g. `RX2`, `TX/RX`; `""` = the device's default |
| `ppm` | number | `0` | |
| `autoTune` | bool | `false` | |
| `guardHz` | number | `75000` | Hz left unused at each edge of the band |

### `"type": "airspy"` (needs libairspy installed)

| Key | Type | Default | |
|---|---|---|---|
| `serial` | string | `""` | Hex serial; `""` = the first found |
| `centerHz` | number | **required** | Hz; 0 = Auto |
| `rateHz` | number | **required** | R2: 10 000 000 or 2 500 000. Mini: 6 000 000 or 3 000 000 |
| `gainMode` | string | `"linearity"` | `"linearity"`, `"sensitivity"` or `"manual"` |
| `gainStep` | integer | `14` | Linearity / sensitivity step, 0–21 |
| `lnaStep` | integer | `10` | Manual mode: LNA step 0–14 |
| `mixerStep` | integer | `10` | Manual mode: mixer step 0–15 |
| `vgaStep` | integer | `10` | Manual mode: VGA step 0–15 |
| `agc` | bool | `false` | Manual mode only: LNA and mixer by AGC (VGA still applies) |
| `biasTee` | bool | `false` | Power an LNA on the antenna port |
| `ppm` | number | `0` | |
| `autoTune` | bool | `false` | |
| `guardHz` | number | `75000` | Hz left unused at each edge of the band |

The Airspy gains are the driver's **steps**, not dB.

### `"type": "soapy"` (needs SoapySDR and the device's module)

| Key | Type | Default | |
|---|---|---|---|
| `args` | string | `""` | Device arguments, e.g. `driver=hackrf`, `driver=sdrplay,serial=…`, `driver=rtlsdr` |
| `centerHz` | number | **required** | Hz; 0 = Auto |
| `rateHz` | number | **required** | |
| `agc` | bool | `true` | The device's AGC |
| `gainDb` | number or null | `null` | Overall gain, dB; `null` leaves it as the device has it |
| `gains` | object | `{}` | Gain stages by name, dB, applied after `gainDb`, e.g. `{ "LNA": 32, "VGA": 20, "AMP": 0 }`, `{ "IFGR": 40, "RFGR": 2 }` |
| `antenna` | string | `""` | |
| `settings` | string | `""` | Device settings as `key=value,key=value`, e.g. `biastee=true` |
| `ppm` | number | `0` | |
| `autoTune` | bool | `false` | |
| `guardHz` | number | `75000` | Hz left unused at each edge of the band |

### `"type": "file"` (replay a capture)

| Key | Type | Default | |
|---|---|---|---|
| `path` | string | **required** | The capture file |
| `centerHz` | number | **required** | The frequency it was recorded at |
| `rateHz` | number | **required** | Its sample rate |
| `realtime` | bool | **required** | `true`: play at the speed it was recorded. `false`: as fast as possible |
| `format` | string | from the name | `"cu8"` (rtl_sdr), `"cs16"` or `"cf32"` (GNU Radio, UHD). Unset: from the file's extension (`.cs16` / `.sc16`; `.cf32` / `.fc32` / `.cfile` / `.complex`), otherwise `cu8` |
| `autoTune` | bool | `false` | |
| `guardHz` | number | `75000` | Hz left unused at each edge of the band |

## Trunked systems

Each entry in `systems` is one trunked system, or one **site** of a
multi-site system. A system is recorded only when it is `enabled` **and** has
at least one control channel.

| Key | Type | Default | Applies to | |
|---|---|---|---|---|
| `shortName` | string | `"sys1"` | all | The system's identity: its recordings folder, the name in call JSON, and how plugins know it. Every system, trunked or conventional, on or off, needs its own. Keep it to letters, digits, `-` and `_` (it becomes folder and file names) |
| `name` | string | `""` | all | What people call it |
| `type` | string | `"p25"` | | `"p25"`, `"smartnet"`, `"dmr"` or `"nxdn"`. Anything else is treated as P25 |
| `enabled` | bool | `true` | all | `false` keeps it in the config without recording it |
| `controlChannelsHz` | numbers | `[]` | all | P25 / SmartNet: the control channel and its alternates; the recorder hunts through them. DMR, NXDN: every frequency listed is watched (NXDN Type-D: the repeaters) |
| `modulation` | string | `"auto"` | P25, SmartNet | `"auto"` (every receiver, best of each frame), `"qpsk"` (CQPSK / simulcast) or `"fsk4"` (C4FM) |
| `talkgroupsCsv` | string | `""` | all | The talkgroup file's **contents** (not a path): [Talkgroups](#talkgroups) |
| `talkgroupsName` | string | `""` | all | The name of the file it came from, for display |
| `unitNames` | object | empty | all | Radio names: [Unit names](#unit-names) |
| `expect` | object | `{}` | P25, SmartNet | Site lock: [Site lock](#site-lock-expect) |
| `voiceChannelsHz` | numbers | `[]` | all | Voice channels seen by the survey; used only to place Auto sources |
| `siteGroup` | string | `""` | all | Groups sites whose calls are the same calls: [Multi-site](#multi-site) |
| `recording` | object | `{}` | all | This system's own call rules: [Per-system rules](#per-system-rules) |
| `plugins` | object | `{}` | all | This system's plugin settings: [Plugins](#plugins) |
| `bandplan` | string | `""` (= `800_standard`) | SmartNet | `800_standard`, `800_reband`, `800_splinter`, `900`, or `400_custom` (VHF / UHF / OBT). Aliases `800_domestic`, `800_rebanded`, `800_domestic_splinter` and `obt` also work |
| `bandplanBaseHz` | number | — | SmartNet 400 | Frequency of channel `bandplanOffset` |
| `bandplanSpacingHz` | number | — | SmartNet 400 | Channel spacing |
| `bandplanOffset` | integer | `0` | SmartNet 400 | First channel number |
| `bandplanHighHz` | number | — | SmartNet 400 | Top of the band |
| `defaultMode` | string | `""` | SmartNet | `"analog"`: a talkgroup never heard granted is recorded as analog FM |
| `dmrChannelsHz` | numbers | `[]` | DMR | Voice frequencies to watch (Trunk Recorder's `channels`). Each must be inside a source |
| `lcnTableHz` | object | `{}` | DMR | Logical channel → frequency, e.g. `{ "101": 452275000 }`; wins over channels learned from the air |
| `colorCode` | integer or null | `null` | DMR | Only this colour code (0–15). Default: the control channel's |
| `nxdnType` | string | `"typeC"` | NXDN | `"typeC"`: a control channel assigns calls to channels (Kenwood NEXEDGE, Icom IDAS Type-C). `"typeD"`: IDAS distributed trunking, no control channel — list every repeater |
| `nxdnRate` | string | `"nxdn48"` | NXDN | `"nxdn48"` (4800 bps, 6.25 kHz) or `"nxdn96"` (9600 bps, 12.5 kHz): the control channel's (Type-D: the repeaters'). Voice channels go at the rate each grant names |
| `nxdnChannelsHz` | numbers | `[]` | NXDN | Voice frequencies to watch besides the control channels. A grant to a channel number not in `lcnTableHz` is learned when its call comes up on one of them. Each must be inside a source |
| `lcnTableHz` | object | `{}` | NXDN | Channel number → frequency, e.g. `{ "12": 451118750 }` (RadioReference lists them as LCNs for many systems); wins over channels learned from the air. Not needed when the site uses Direct Frequency Assignment |
| `ran` | integer or null | `null` | NXDN | Only this RAN (0–63). Default: the control channel's |

`400_custom` needs `bandplanBaseHz`, `bandplanSpacingHz` and `bandplanHighHz`. The
survey fills them in for SmartNet OBT systems.

An NXDN Type-C grant names a 10-bit channel number. Its frequency comes from
`lcnTableHz`; from the site itself when it uses Direct Frequency Assignment
(its SITE_INFO says so, and grants then carry the frequency); or is learned
when the granted group's call comes up on a frequency in
`nxdnChannelsHz`. Until then the grant is logged as "frequency not known
yet", and the dashboard lists the channel numbers still unknown.

### Site lock (`expect`)

A control channel announcing a different identity is not followed: its grants
are ignored and the recorder moves on to the next control channel. Grants
wait until every locked field has been heard.

| Key | Type | |
|---|---|---|
| `nac` | integer | P25 NAC |
| `wacn` | integer | P25 WACN |
| `sysId` | integer | P25 or SmartNet System ID; NXDN Type-C system code |
| `rfss` | integer | P25 RFSS |
| `site` | integer | P25 site (SmartNet: when the system sends it); NXDN Type-C site code |

Values are plain **decimal** numbers in the file, though the interface shows
NAC, WACN and System ID in hex. For example, NAC `0x443` is `"nac": 1091`.

Only the fields a protocol announces are compared. On SmartNet that is
`sysId` and `site`; `nac`, `wacn` and `rfss` are ignored. Lock `site` only
if the system sends it (OBT systems do): otherwise grants wait for it
forever. NXDN Type-C sites state `sysId` and `site` (from SITE_INFO). DMR
and NXDN Type-D systems ignore `expect`; use `colorCode` / `ran`.

### Multi-site

A call on a multi-site system is usually granted on several sites. Each copy
is recorded, and when the last one ends the best copy is saved
(`recording.dropDuplicateCalls`, on by default).

Sites are grouped into one system by what their control channels announce:
P25 by WACN and System ID, SmartNet by System ID, NXDN Type-C by system
code. Two config entries that
follow the *same* site are never grouped.

A non-empty `siteGroup` overrides that grouping:

- Systems with the same `siteGroup` are grouped.
- A `siteGroup` used by only one system keeps that system on its own.

Give DMR and NXDN Type-D sites a group name, since they don't announce a
system identity.
Give ISSI-linked systems one too. To prefer a site for a talkgroup, use the
talkgroup file's `Preferred Site` (or `Preferred NAC`) column.

### Talkgroups

`talkgroupsCsv` holds Trunk Recorder's talkgroup file, either form:

- **With a header.** The first cell must be `Decimal`. These column names
  are recognised, in any case and any order:
  - `Decimal`, `Mode`, `Alpha Tag`, `Description`, `Tag`, `Category`
  - `Priority`, `Preferred NAC`, `Preferred Site`, `Ignore`
- **Without a header.** The columns are, in order: `Decimal,Hex,Mode,Alpha
  Tag,Description,Tag,Group[,Priority]`.

Parsing:

- Lines starting with `#` and blank lines are skipped.
- A row whose `Decimal` isn't a number is skipped.
- Commas only; a byte-order mark is ignored.

| Column | |
|---|---|
| `Mode` | `E`, `TE` or `DE` mean encrypted (not recorded unless `recordEncrypted`) |
| `Priority` | Negative: never record |
| `Ignore` | `true`, `yes`, `y`, `1`, `x`, `ignore`: never record |
| `Preferred Site` | A site's `shortName`: its copy is kept when it has at least 90 % of the best copy's clean audio |
| `Preferred NAC` | The same, as a NAC or as RFSS and site (`RRRRssss`) |

### Unit names

`unitNames` holds Trunk Recorder's unit tags file. Each system and each
conventional system has its own.

| Key | Type | Default | |
|---|---|---|---|
| `csv` | string | `""` | The file's **contents**: `unit,name` lines without a header, `#` for comments. A unit written `/regex/` matches by regular expression; `$1` / `\1` in the name insert its groups. The first match wins |
| `name` | string | `""` | The file it came from, for display |
| `mode` | string | `""` (= `"user"`) | How these combine with talker aliases heard over the air: `"user"` (yours first, then over the air), `"ota"` (over the air first), `"user_only"`, `"none"` |

Talker aliases heard over the air are kept in `<shortName>.units.csv`
beside the config file ([Files beside the config](#files-beside-the-config)).

## Conventional systems

Each entry in `conventional` is a group of single-frequency channels with its
own folder and rules. At most 256.

| Key | Type | Default | |
|---|---|---|---|
| `shortName` | string | `"conv"` | Its identity and recordings folder; no other system, trunked or conventional, may have it |
| `name` | string | `""` | What people call it |
| `enabled` | bool | `true` | `false` keeps it without recording any of its channels |
| `squelchDb` | number | `8` | How far above the measured noise floor a signal must be to open a channel, dB (3–40 sensible) |
| `channels` | array | `[]` | The channels, below |
| `channelFile` | string | `""` | A CSV to read the channels from instead (desktop app). Absolute, or relative to the config file's folder. Re-read on every start and on **Reload** |
| `recording` | object | `{}` | Its own call rules: [Per-system rules](#per-system-rules) |
| `unitNames` | object | empty | Radio names: [Unit names](#unit-names) |
| `plugins` | object | `{}` | Its plugin settings: [Plugins](#plugins) |

To plugins and in call data, conventional system *k* (counting from 0) is
system number 65535 − *k*. Conventional channels use a fixed 0.3 s pre-roll,
not `recording.prerollS`.

### Channels

| Key | Type | Default | |
|---|---|---|---|
| `freqHz` | number | **required** | Hz. A frequency can belong to only one conventional system |
| `mode` | string | `"fm"` | `"fm"` (analog narrowband FM), `"p25"` (Phase 1, C4FM or CQPSK), `"dmr"` (both slots), `"nxdn48"` or `"nxdn96"` |
| `name` | string | `""` | Alpha tag in the call JSON |
| `description`, `tag`, `group` | string | `""` | Description, tag and category in the call JSON |
| `talkgroup` | integer | the frequency in kHz | The number calls are filed under. A further row on the same frequency gets the kHz with a digit added (1543251, 1543252…). P25 calls keep the talkgroup the radio sends, unless several rows split the frequency. DMR and NXDN calls always keep the talkgroup (group) on the air when it names one (the rows give it names); a DMR / NXDN row with no `talkgroup` takes the `TG` in its `tone` |
| `tone` | string | `""` (any) | Record only transmissions carrying this code. FM: CTCSS `151.4` or DCS `D023N`. P25: NAC, `NAC 293`. DMR: `CC 1`, `CC 1 TS 2`, `CC 1 TS 2 TG 201`. NXDN: `RAN 5`, `RAN 5 TG 201` (RAN 0 means any) |
| `squelchDb` | number | the system's | Per channel |
| `enabled` | bool | `true` | |

Rows on one frequency must share a mode. At most one row may have no code,
and no two rows may have codes that can't be told apart. The
[README](../README.md#tones-several-users-of-one-frequency) has the details.

### Channel CSV (`channelFile`)

The CSV needs a header row. Columns are matched case-insensitively and can be
in any order:

| Column | |
|---|---|
| `Frequency` (`Freq`, `FreqHz`) | **Required.** MHz with a decimal point (`154.4300`), or Hz |
| `Mode` | `fm`, `p25`, `dmr`, `nxdn48` (or `nxdn`), `nxdn96` (also `nfm`, `analog`, `A`, `digital`, `D`); empty = `fm`, or `p25` / `dmr` / `nxdn48` when `Tone` holds a NAC / colour code / RAN |
| `TG Number` (`Talkgroup`, `TG`) | Empty = the default above |
| `Tone` | As `tone` above; RadioReference spellings (`151.4 PL`, `023 DPL`, `293 NAC`, `CC1 TS2 TG201`, `RAN 5`) work |
| `Alpha Tag` (`Name`), `Description`, `Tag`, `Category` (`Group`) | Names |
| `Squelch dB` | 3–40; empty = the system's |
| `Enable` (`Enabled`) | `false`, `no`, `0`, `off` = off |

Parsing:

- Commas, semicolons or tabs, decimal commas, and a byte-order mark are all
  handled.
- Trunk Recorder's own channel file reads as is. Its `Squelch` column (an
  absolute level) is ignored.

## Recording

Where calls go and which are kept. Keys marked ✓ can be set per system too
([Per-system rules](#per-system-rules)).

| Key | Type | Default | ✓ | |
|---|---|---|---|---|
| `captureDir` | string | `~/TrunkRecorderPro` | | The recordings folder. Use an absolute path: `~` isn't expanded, and a relative path is taken from wherever the app was started |
| `prerollS` | number | `1` | | Seconds of air before a grant replayed into a trunked call (the interface allows 0–3) |
| `maxRecorders` | integer | `32` | | Calls recorded at once, across all trunked systems (the interface allows 1–64) |
| `callTimeoutS` | number | `3` | ✓ | A call ends this long after its last grant or audio |
| `recordUnknown` | bool | `true` | ✓ | Record talkgroups not in the talkgroup file. Applies only when there is one; a talkgroup patched with a listed one is recorded anyway |
| `recordEncrypted` | bool | `false` | ✓ | Record calls flagged encrypted (by the grant or the talkgroup's `Mode`), keeping the call and its radios even with no audio. Encrypted transmissions are always left out of the audio. Without it, a call flagged encrypted at its grant isn't recorded at all, and one that turns encrypted part-way is kept with its clear transmissions (if none were clear, only with `keepSilentCalls`). Not in the interface |
| `recordUnitToUnit` | bool | `true` | ✓ | Record P25 unit-to-unit (private) calls |
| `keepSilentCalls` | bool | `false` | ✓ | Keep calls with no audio |
| `minCallS` | number | `0` | ✓ | Drop calls with less audio than this, s |
| `maxCallS` | number | `0` | ✓ | Save calls longer than this in parts, the conversation continuing on the same recorder; 0 = parts of 600 s (a stuck carrier never records without bound) |
| `minTransmissionS` | number | `0` | ✓ | Leave out transmissions shorter than this (key-ups, data bursts) |
| `normalizeAudio` | bool | `true` | ✓ | Even out loudness |
| `digitalLevelDb` | number | `0` | ✓ | Gain on digital audio, dB (±40) |
| `analogLevelDb` | number | `0` | ✓ | Gain on analog audio, dB (±40) |
| `compressWav` | bool | `false` | ✓ | Also save an `.m4a` of every call (needs an encoder, see `m4a`) |
| `audioArchive` | bool | `true` | ✓ | Keep the audio once every upload plugin has had the call. `false` deletes it |
| `callLog` | bool | `true` | ✓ | Keep the call JSON once every upload plugin has had the call |
| `archiveFilesOnFailure` | bool | `true` | ✓ | Keep the files when an upload failed, whatever the two above say |
| `filenameFormat` | string | `""` | ✓ | Folders and file names: [File names](#file-names) |
| `dropDuplicateCalls` | bool | `true` | | Multi-site: save only the best copy of a call |
| `captureFrames` | bool | `false` | | Also save each call's vocoder frames as `<call>.frames.jsonl` (for `trunk-pro tool revoice`) |
| `vocoder` | string | `"fixed"` | | IMBE vocoder: `"fixed"`, `"enhanced"` or `"mbelib"` |
| `ramSpool` | object | `{ "enabled": false, "sizeMb": 256 }` | | Files only the upload plugins need (with `audioArchive` / `callLog` off, and an `.m4a` made only for them) wait in RAM, not in `captureDir`: on macOS a RAM disk the app makes (no administrator needed, hidden from the Finder and Spotlight), on Linux a folder in `/dev/shm`. A failed upload's files are moved to `captureDir` (`archiveFilesOnFailure`); when the spool is full, calls go there directly. `sizeMb`: 16–65536. `dir`: a RAM-backed folder of your own (a tmpfs mount) instead. Applies at the next start; its room is on the dashboard's Computer page, with an event as it fills |
| `m4a` | object | `{ "encoder": "auto", "bitrateKbps": 32 }` | | The M4A encoder for `compressWav` and plugins. `encoder`: `"auto"` (ffmpeg, then afconvert, then fdkaac), `"ffmpeg"`, `"afconvert"`, `"fdkaac"` or `"none"`. `bitrateKbps`: 8–320 |

### Per-system rules

A trunked or conventional system's `recording` object can override any key
marked ✓ above. A key left out follows the global `recording`.

```json
{ "shortName": "fireground", "type": "p25", "controlChannelsHz": [851012500],
  "recording": { "minCallS": 2, "recordUnknown": false } }
```

### File names

With `filenameFormat` empty, calls are saved as Trunk Recorder does, in local
time:

```text
<captureDir>/<shortName>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>[.<slot>].wav|json
```

Month and day are not zero-padded. `.<slot>` is added for Phase 2 and DMR.

Otherwise `filenameFormat` is a path template, relative to `captureDir`, built
from Trunk Recorder's tokens:

| Token | |
|---|---|
| `{short_name}`, `{sys_num}` | The system |
| `{talkgroup}`, `{talkgroup_display}`, `{talkgroup_alpha_tag}`, `{talkgroup_description}`, `{talkgroup_tag}`, `{talkgroup_group}` | The talkgroup |
| `{freq}` (Hz), `{freq_mhz}` | The frequency |
| `{epoch}`, `{time:FORMAT}` (local), `{ztime:FORMAT}` (UTC) | Time. `FORMAT` is strftime (`%Y/%m/%d`), plus `%f` for milliseconds, `iso` and `iso_ms` |
| `{call_num}`, `{tdma_slot}`, `{source_num}`, `{recorder_num}`, `{color_code}`, `{ran}` | Call details (`{ran}`: an NXDN call's RAN, −1 otherwise) |
| `{audio_type}`, `{emergency}`, `{encrypted}`, `{priority}`, `{signal}`, `{noise}` | Call properties |

```json
"filenameFormat": "{short_name}/{time:%Y-%m-%d}/{talkgroup}-{time:%H%M%S}"
```

Rendering rules:

- `-call_<n>` is always added to the name.
- Characters that can't be in a file name (`\ / : * ? " < > |` and spaces)
  inside a token's text become `_`.
- An unknown token is left as typed.
- A `:` from a time format (`iso`, `%H:%M`) becomes `-`.

## Server

| Key | Type | Default | |
|---|---|---|---|
| `bind` | string | `"127.0.0.1"` | Address the interface listens on. `"0.0.0.0"` makes it reachable from other machines. It has **no login**: anyone who can reach it can change settings and run plugins. `--bind` overrides it for one run |
| `port` | integer | `8080` | `--port` overrides it for one run |
| `autoStart` | bool | `false` | Start recording when the app starts (as `--start` does) |
| `allowedOrigins` | strings | `[]` | Web pages on other origins allowed to use the API: `"http://host:port"`, `"null"` (a page opened from a file), or `"*"` |
| `interfaces` | array | `[]` | Interfaces of your own, each `{ "name": "wall", "path": "~/wall-display" }`, served at `/ui/<name>/`. `name`: letters, digits, `-`, `_`. `path`: absolute, `~/…`, or relative to the config file's folder |
| `home` | string | `""` | What `/` shows: `""` = the built-in interface, or an interface's `name`. `--ui <folder>` overrides it |

## Log

Trunk Recorder's log options. The log goes to stderr, and optionally to files
and the system log.

| Key | Type | Default | |
|---|---|---|---|
| `level` | string | `"info"` | `trace`, `debug`, `info`, `warning`, `error`, `fatal`. `--log-level` overrides it |
| `console` | bool | `true` | Log to stderr |
| `file` | bool | `false` | Also log to files: a new one each day, or at 100 MB |
| `dir` | string | `""` (= `logs`) | The log folder, absolute or relative to the config file's folder |
| `syslogFriendly` | bool | `false` | With `file`: one `trunk-pro.log`, never rotated (SIGHUP reopens it, for logrotate) |
| `syslog` | bool | `false` | Also send to the system log (Linux, macOS) |
| `color` | string | `""` | `"console"`, `"logfile"`, `"all"`, `"none"`; `""` = on the console when it's a terminal and `NO_COLOR` isn't set |
| `frequencyFormat` | string | `"mhz"` | `"mhz"`, `"hz"` or `"exp"` |
| `talkgroupDisplayFormat` | string | `"id"` | `"id"`, `"id_tag"` or `"tag_id"` |
| `statusAsString` | bool | `true` | |
| `controlWarnRatePerS` | number | `10` | Log an error when a control channel decodes fewer messages/s than this; −1 = log the rate every time (info) |

## Monitor

| Key | Type | Default | |
|---|---|---|---|
| `probeHosts` | strings | `["1.1.1.1:443", "dns.google:443"]` | `host:port`s the dashboard connects to every 15 s for its uplink history; `[]` = no checks. Not in the interface |

## Plugins

Plugins are installed from the Plugins page or `trunk-pro plugin install
<id>`, into `plugins/<id>/` beside the config file. The config says which
run and with what settings.

Recorder-wide, under the top-level `plugins`:

```json
"plugins": {
  "openmhz": { "enabled": true, "settings": { "server": "https://api.openmhz.com" } }
}
```

| Key | Type | Default | |
|---|---|---|---|
| `enabled` | bool | `false` | Run it while recording |
| `settings` | any | `null` | Its settings, as the plugin's own schema describes (`trunk-pro plugin describe <id>`) |
| `path` | string | `""` | Run this executable instead of the installed one (for developing a plugin). Use an absolute path |

Per system, under a trunked or conventional system's `plugins`, keyed by plugin
id:

```json
{ "shortName": "wmata", "type": "smartnet", …,
  "plugins": { "openmhz": { "apiKey": "…", "systemName": "wmata" } } }
```

The keys are whatever the plugin's per-system schema asks for. Plugins know
each system by its short name. Changing any plugin setting restarts the
plugins at once, even while recording.

## Validation

When recording starts (not when settings are saved), the config is refused,
with a message, if:

- there is no source, or nothing to record (no recorded trunked system and no
  enabled conventional channel);
- two systems share a `shortName` (any two: trunked or conventional, on or
  off), or one has none;
- an Auto source has nothing left to centre on;
- a SmartNet `bandplan` is unknown, or a `400_custom` plan lacks its base,
  spacing or high, or has high ≤ base;
- a recorded system has no control channel inside any source, or a DMR /
  NXDN control channel, `dmrChannelsHz` or `nxdnChannelsHz` frequency is
  outside every source;
- a `file` source has no `path`;
- there are more than 256 conventional systems;
- an enabled conventional frequency is in two systems, is ≤ 0, or is
  outside every source;
- a `tone` doesn't parse (including a CTCSS tone that isn't a standard one);
- rows sharing a frequency have different modes, two have no code, or two
  have codes that can't be told apart.

## Files beside the config

The app keeps these in the folder the config file is in (with `--config
elsewhere/config.json`, in `elsewhere/`, so two recorders with their own
configs keep their own):

| File | |
|---|---|
| `<shortName>.bandplan` | Each system's learned band plan (P25 IDEN tables, DMR and NXDN channel tables), so grants can be followed at once next time |
| `<shortName>.units.csv` | Talker aliases heard over the air |
| `conventional.heard.json` | The tones, NACs, colour codes and RANs each conventional frequency carried (the "Heard" list) |
| `plugins/<id>/` | Installed plugins |
| `plugin-data/<id>/` | Each plugin's own data (e.g. calls waiting to upload); also its working folder |
| `plugin-registry.json` | The plugin store's cached index |
| `radio/` | The radio registry: the radios each system has heard, one file per system |
| `stats/` | The dashboard's history, a `YYYY-MM-DD.jsonl` per day, gzipped after the day and kept about a week |

Log files go in `log.dir`, relative to the config file. Channel files
(`channelFile`) are relative to the config file too.

## From Trunk Recorder

**Import Trunk Recorder config…** in Setup reads a Trunk Recorder
`config.json` and the talkgroup, unit and channel files it names. It
converts:

- **Sources.** osmosdr dongles become `rtlsdr`, Airspy or SoapySDR devices;
  `usrp` becomes `usrp`. Gains, `agc`, `ppm` (or `error`) and `autoTune` come
  across.
- **Systems.** `p25`, `smartnet` and `dmr` become trunked systems, with
  control channels, modulation, band plan, `lcnTableHz` / `dmrChannelsHz`,
  talkgroups and unit tags. `multiSite` with `multiSiteSystemName` becomes
  `siteGroup` (an empty name: grouped from the air); when some systems have
  `multiSite` on, one with it off gets a group of its own.
- **Conventional.** `conventional`, `conventionalP25` and `conventionalDMR`
  each become a conventional system, from `channels` or `channelFile`.
- **Call rules.**
  - Copied as they are: `recordUnknown`, `compressWav`, `audioArchive`,
    `callLog`, `filenameFormat`.
  - Renamed: `minDuration` → `minCallS`, `maxDuration` → `maxCallS`,
    `minTransmissionDuration` → `minTransmissionS`.
  - Converted to dB: `digitalLevels`, `analogLevels`.
  - Top level: `callTimeout`, `recordUUVCalls`, `archiveFilesOnFailure`,
    `captureDir`.
  - A rule that is the same on every system becomes the global one.
- **Log options**: `logLevel`, `consoleLog`, `logFile`, `logDir` (relative to
  the config file), `syslogFriendly`, `logColor`, `frequencyFormat`,
  `statusAsString`, `controlWarnRate`, and the first system's
  `talkgroupDisplayFormat`.
- **Upload settings.** OpenMHz, Broadcastify, `uploadScript`, rdio-scanner
  and simplestream settings are offered as plugin settings.

These are **not** carried over:

- squelch levels (Trunk Recorder's are absolute; here squelch is dB above
  the noise)
- `siteId` (set a site lock instead)
- per-recorder counts, `signalDetectorThreshold`
- `hideEncrypted`, `hideUnknownTalkgroups`
- analog filter settings (`tau`, `maxDev`, `filterWidth`)
- `decodeMDC` / `decodeFSync` and the like (MDC1200 and FleetSync are
  always decoded)
- `softVocoder`, `tempDir`, `instanceId` / `instanceKey`, `audioStreaming`,
  `broadcastSignals`
- a conventional system's `talkgroupsFile`
- `conversationMode: false` (a file per transmission)

Where Trunk Recorder's defaults differ from these (`compressWav`,
`archiveFilesOnFailure`, `multiSite`), a key absent from the Trunk Recorder
config gets *this* app's default: duplicate copies of a call are dropped
unless `dropDuplicateCalls` is off.
