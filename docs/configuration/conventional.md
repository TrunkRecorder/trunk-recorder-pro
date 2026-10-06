# Conventional Systems

Each entry in `conventional` is a group of single-frequency channels with its own short name,
recordings folder, call rules, unit names and plugin settings. This page lists the keys for
conventional systems and their channels, and the channel CSV format. For how detection works,
choosing a squelch and debugging, see the [conventional guide](../guides/conventional.md); for
sharing a frequency by tone, NAC, colour code or RAN, see the [tones guide](../guides/tones.md).

```json
"conventional": [
  { "shortName": "fire", "squelchDb": 8, "channels": [
      { "freqHz": 154430000, "mode": "fm", "name": "County Fire Dispatch", "talkgroup": 1001 },
      { "freqHz": 460125000, "mode": "p25", "name": "PD Tac 2", "squelchDb": 12 } ] },
  { "shortName": "police", "channelFile": "police.csv" }
]
```

A conventional channel is one frequency that radios talk on directly or through a repeater, with no
control channel. There are no recorders to assign: each channel's frequency is watched in the
spectrum the sources already receive, and a call is opened when the signal there rises far enough
above the measured noise floor. Channels can be analog FM, P25 Phase 1, DMR or NXDN, and can sit
anywhere a source covers, including alongside a trunked system.

## Conventional system keys

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `shortName` | | `"conv"` | string | Its identity: recordings folder, `short_name` in call JSON, how plugins know it. No other system, trunked or conventional, may have it |
| `name` | | `""` | string | What people call it ("County Fire"), for display |
| `enabled` | | `true` | bool | `false` keeps it without recording any of its channels |
| `squelchDb` | | `8` | number | How far above the measured noise floor a signal must rise to open a channel, dB. The interface allows 3–40 |
| `channels` | | `[]` | array | The channels. See [Channel keys](#channel-keys) |
| `channelFile` | | `""` | string | A CSV to read the channels from instead (desktop app only). Absolute, or relative to the config file's folder. See [Channel file](#channel-file-channelfile) |
| `recording` | | `{}` | object | Its own call rules; a rule left out follows the global `recording`. See [recording.md](recording.md#per-system-rules) |
| `unitNames` | | empty | object | Names for the radios heard on it. See [systems.md](systems.md#unit-names) |
| `plugins` | | `{}` | object | Its plugin settings, by plugin id. See [plugins.md](plugins.md#per-system-settings) |

At most 256 conventional systems. A frequency belongs to only one of them.

Notes:

- **System number.** To plugins and in some call data, conventional system *k* (counting from 0 in
  the list) is system number 65535 − *k*; trunked systems count up from 0.
- **Pre-roll.** Conventional calls start 0.3 s before the signal was detected; `recording.prerollS`
  applies to trunked calls only.
- **Talkgroup names.** When the config has exactly one trunked system, P25 and DMR conventional calls
  that carry a talkgroup take its names from that system's talkgroup file. Otherwise names come from
  the channel rows.

## Channel keys

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `freqHz` | ✓ | | number | The frequency, Hz |
| `mode` | | `"fm"` | string | `"fm"` (analog narrowband FM), `"p25"` (P25 Phase 1, C4FM or CQPSK), `"dmr"` (both slots), `"nxdn48"` or `"nxdn96"`. Exactly these spellings |
| `name` | | `""` | string | Alpha tag, written to the call JSON |
| `description` | | `""` | string | Description in the call JSON |
| `tag` | | `""` | string | Tag in the call JSON |
| `group` | | `""` | string | Category in the call JSON |
| `talkgroup` | | see below | integer | The number its calls are filed under |
| `tone` | | `""` (any) | string | Record only transmissions carrying this code. See [Tones and codes](#tones-and-codes) |
| `squelchDb` | | the system's | number | This channel's own squelch, dB above the noise floor |
| `enabled` | | `true` | bool | `false` keeps the row without recording it |

### Talkgroup numbers

A channel with no `talkgroup` is filed under its frequency in kHz (154.430 MHz → `154430`). A further
row on the same frequency gets that with a digit added: `1543251`, `1543252`… (counted among all rows
on the frequency, enabled or not, so switching one off renumbers nothing).

Digital calls keep what the air says:

- **P25** calls are filed under the talkgroup the radio sends, unless several rows split the
  frequency.
- **DMR and NXDN** calls always keep the talkgroup (group) the air names, when it names one; the rows
  give those talkgroups names. A DMR or NXDN row with no `talkgroup` takes the `TG` in its `tone`.

### Tones and codes

`tone` picks a channel's transmissions out of everything on its frequency. Several rows on one
frequency split it among users: each row records the transmissions with its code, and a row with no
code (at most one) records the rest.

| Mode | `tone` examples | What it matches |
|---|---|---|
| `fm` | `151.4`, `151.4 PL`, `D023N`, `023 DPL`, `D047I` | A CTCSS tone (67.0–254.1 Hz, standard tones only) or DCS code |
| `p25` | `293`, `NAC 293`, `293 NAC`, `$293`, `0x293` | The NAC, in hex (up to 3 digits). `F7E` and `F7F` (a radio's "receive any") mean any |
| `dmr` | `CC 1`, `CC1 TS2`, `CC 1 TS 2 TG 201`, `Slot 2` | Colour code 0–15, slot 1 or 2, talkgroup; any combination |
| `nxdn48`, `nxdn96` | `RAN 5`, `RAN 5 TG 201`, `5` | RAN 0–63, group 1–65535. RAN 0 means any |

Empty, `0`, `S`, `search`, `any` or `none` mean any. Rules for rows sharing a frequency:

- They must all have the same `mode`.
- At most one may have no code.
- No two may have codes that can't be told apart (`D023N` and `D047I` are the same DCS signal).
- The channel opens at the lowest `squelchDb` among them.

Recording won't start while any of these is broken; the message names the frequency.

## Channel file (`channelFile`)

Instead of listing channels in the config, a conventional system can read them from a CSV file you
edit in a spreadsheet. In Setup, type a name under **Channel file (optional)** and press
**Use this file**: if the file doesn't exist, it's created from the current list. While a file is
linked:

- its channels are the file's, and aren't saved in the config (`channels` is written as `[]`);
- it's re-read when the app starts, whenever settings are saved, at every **Start**, and on
  **Reload**;
- a file that can't be read keeps the last good list, and Setup shows why under the file name;
- **Unlink** stops reading it and keeps its channels in the config.

Channel files are a desktop feature; the browser version has no file to link. Setup's
**Export CSV** writes the same format from a list.

### CSV format

```csv
TG Number,Frequency,Tone,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Enable
1001,154.4300,,fm,County Fire Dispatch,,Fire Dispatch,Fire,,true
,154.3250,D223N,fm,County A Fire,,,Fire,,true
,154.3250,151.4,fm,County B Fire,,,Fire,,true
,460.1250,,p25,PD Tac 2,,Law Tac,Police,12,true
```

A header row is required. Columns are matched in any case and any order; others are ignored.

| Column | Also accepted | Meaning |
|---|---|---|
| `Frequency` ✓ | `Freq`, `FreqHz` | MHz when it has a decimal point (`154.4300`), otherwise Hz (`154430000`) |
| `Mode` | | `fm`, `p25`, `dmr`, `nxdn48` (or `nxdn`), `nxdn96`; also `nfm`, `analog`, `A` (= fm), `digital`, `D` (= p25). Empty = `fm`, or `p25` / `dmr` / `nxdn48` when `Tone` holds a NAC / colour code / RAN |
| `TG Number` | `Talkgroup`, `TG` | A positive whole number; empty = the default above |
| `Tone` | | As `tone` above. RadioReference's spellings (`151.4 PL`, `023 DPL`, `293 NAC`, `CC1 TS2 TG201`, `RAN 5`) work |
| `Alpha Tag` | `Name` | The channel's name |
| `Description`, `Tag` | | Names in the call JSON |
| `Category` | `Group` | Category in the call JSON |
| `Squelch dB` | `SquelchDb` | 3–40, dB above the noise floor; empty = the system's |
| `Enable` | `Enabled` | `false`, `no`, `0` or `off` = off; anything else or empty = on |

Parsing:

- Commas, semicolons or tabs (guessed from the header row). With semicolons or tabs, decimal commas
  (`154,43`) are read as decimal points, as Excel writes them in some locales.
- Fields can be quoted. Blank lines, `#` comments and a byte-order mark are skipped.
- A row with no usable frequency is skipped. A bad `Mode`, `TG Number`, `Squelch dB` or `Tone` is
  read as the default. Setup lists each such row after reading the file.
- **Trunk Recorder's channel file reads as is.** Its `Squelch` column (an absolute level) is not
  read: use `Squelch dB`. A `Tone` of `S` (search) needs no setting here, since every analog call's
  tone is identified and written to its JSON anyway.
