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
anywhere a source covers, including alongside a trunked system. Analog channels can be FM
(narrowband or wide) or AM (airband).

## Conventional system keys

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `shortName` | | `"conv"` | string | Its identity: recordings folder, `short_name` in call JSON, how plugins know it. No other system, trunked or conventional, may have it |
| `name` | | `""` | string | What people call it ("County Fire"), for display |
| `fcc` | | `{}` | object | Optional: who is licensed on each channel's frequency (they can be different licensees'). See [FCC licenses](systems.md#fcc-licenses) |
| `enabled` | | `true` | bool | `false` keeps it without recording any of its channels |
| `squelchDb` | | `8` | number | How far above the measured noise floor a signal must rise to open a channel, dB. The interface allows 3–40 |
| `demod` | | `{}` | object | How its FM and AM channels are demodulated. See [Demodulation](#demodulation-demod) |
| `channels` | | `[]` | array | The channels. See [Channel keys](#channel-keys) |
| `channelFile` | | `""` | string | A CSV to read the channels from instead (desktop app only). Absolute, or relative to the config file's folder. See [Channel file](#channel-file-channelfile) |
| `recording` | | `{}` | object | Its own call rules; a rule left out follows the global `recording`. See [recording.md](recording.md#per-system-rules) |
| `unitNames` | | empty | object | Names for the radios heard on it. See [systems.md](systems.md#unit-names) |
| `keys` | | `""` | string | The encryption keys its P25 channels decrypt with, a key source's name. See [systems.md](systems.md#encryption-keys-keys) |
| `plugins` | | `{}` | object | Its plugin settings, by plugin id. See [plugins.md](plugins.md#per-system-settings) |
| `pluginDelays` | | `[]` | array | How long plugins wait for its calls. See [plugins.md](plugins.md#delays) |

A frequency belongs to only one conventional system.

### Demodulation (`demod`)

Applies to every FM and AM channel in the system. Each key left out is the default; an empty
`demod` isn't written. See the [conventional guide](../guides/conventional.md#demodulation-settings)
for when to change them.

```json
"demod": { "fmDeemphasis": false, "amLevel": "slow", "amLowCutHz": 300, "squelchOpenMs": 30, "squelchHangMs": 150 }
```

| Key | Default | Type | Description |
|---|---|---|---|
| `fmDeemphasis` | `true` | bool | FM: undo the transmitter's 750 µs pre-emphasis. `false` for channels sent without it |
| `amLevel` | `"fast"` | string | AM: how fast the audio level follows the carrier, `"fast"` (50 ms) or `"slow"` (0.5 s) |
| `amLowCutHz` | `200` | number | AM: the audio high-pass's corner (4th-order Butterworth), Hz, 0–600. `0`: none |
| `squelchOpenMs` | `30` | number | The carrier must hold this long before audio passes, ms, 0–500 |
| `squelchHangMs` | `2` | number | Audio keeps passing this long after the carrier drops, ms, 0–1000. Bridges fades; the noise after a transmission is still cut |

Notes:

- **System number.** To plugins and in some call data, systems are numbered from 0: the trunked
  systems being recorded, then the conventional ones being recorded (enabled, with a channel that
  is), in the order the config lists them.
- **Pre-roll.** Conventional calls start 0.3 s before the signal was detected; `recording.prerollS`
  applies to trunked calls only.
- **Talkgroup names.** A conventional call takes its row's names, if the row has any; else the
  conventional system's own talkgroup file's for the number it's filed under
  (`<shortName>.talkgroups.csv` beside the config; Setup shows it on a conventional system with
  digital channels) — so a digital row with no names lets the file name each talkgroup the air sends. A
  conventional system doesn't borrow the names, or the settings, of a trunked system: give it its
  own file. The file's format is the [talkgroup file](systems.md#talkgroup-file)'s.
- **Frequency error.** A conventional call's `freq_error` is measured as a trunked call's is, from
  the decoder's offset (P25 only: analog and DMR receivers don't measure one).
- **Channel width.** A conventional DMR channel is opened 6.25 kHz wide, as a trunked one is.

## Channel keys

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `freqHz` | ✓ | | number | The frequency, Hz |
| `mode` | | `"fm"` | string | `"fm"` (analog FM), `"am"` (analog AM, airband), `"p25"` (P25 Phase 1, C4FM or CQPSK), `"dmr"` (both slots), `"nxdn48"` or `"nxdn96"` (any case; anything else is refused when the config is read) |
| `name` | | `""` | string | Alpha tag, written to the call JSON |
| `description` | | `""` | string | Description in the call JSON |
| `tag` | | `""` | string | Tag in the call JSON |
| `group` | | `""` | string | Category in the call JSON |
| `talkgroup` | | see below | integer | The number its calls are filed under (Setup: TG Number) |
| `code` | | `""` (any) | string | Record only transmissions carrying this code: a tone, NAC, colour code or RAN. See [Codes, slots and talkgroups](#codes-slots-and-talkgroups) |
| `slot` | | any | integer | `dmr`: record only this slot, 1 or 2 |
| `matchTalkgroup` | | any | integer | `p25`, `dmr`, `nxdn48`, `nxdn96`: record only transmissions naming this talkgroup (Setup: Match TG) |
| `squelchDb` | | the system's | number | This channel's own squelch, dB above the noise floor |
| `bandwidthKhz` | | the mode's | number | `fm` and `am`: the channel filter's width, kHz, 4–20. FM: 11 (12.5 kHz channels), 16 for wide FM (25 kHz). AM: 11 below 225 MHz, 16 from 225 MHz (UHF military air); 6 for 8.33 kHz channels. Rows sharing a frequency need the same |
| `enabled` | | `true` | bool | `false` keeps the row without recording it |

### Talkgroup numbers

A call is filed under:

1. the row's `talkgroup`, if it gives one;
2. else the talkgroup the air named (`p25`, `dmr`, `nxdn48`, `nxdn96`), if it did;
3. else the frequency in kHz (154.430 MHz → `154430`). A further row on the same frequency gets that
   with a digit added: `1543251`, `1543252`… (counted among all rows on the frequency, enabled or
   not, so switching one off renumbers nothing).

| Mode | An empty `talkgroup` files under |
|---|---|
| `fm`, `am` | the frequency default |
| `p25`, `dmr`, `nxdn48`, `nxdn96` | the air's talkgroup; the frequency default when a transmission names none |

A digital call's record keeps the talkgroup the air named as `talkgroup_ota`, also when a row files it
under another number. That lets several P25 channels whose radios all send TG 1 — the common case —
each file under a number of its own, and keep the 1:

```json
{ "freqHz": 460125000, "mode": "p25", "talkgroup": 46012, "name": "Townville PD" },
{ "freqHz": 460175000, "mode": "p25", "talkgroup": 46017, "name": "Townville FD" }
```

A new talkgroup on the air is a new call, also when a row files both under one number. The names are
the row's, if it has any; else the conventional system's talkgroup file's for the number filed under.

### Codes, slots and talkgroups

`code`, `slot` and `matchTalkgroup` pick a row's transmissions out of everything on its frequency;
a row records the transmissions that fit all it gives. Several rows on one frequency split it among
users: the most specific row that fits takes a transmission, and a row with none of them (at most
one) takes the rest. A transmission no row takes isn't recorded (Setup lists it under the
frequency, with an Add button).

| Mode | `code` examples | What it matches | `slot` | `matchTalkgroup` |
|---|---|---|---|---|
| `fm` | `151.4`, `151.4 PL`, `D023N`, `023 DPL`, `D047I` | A CTCSS tone (67.0–254.1 Hz, standard tones only) or DCS code | — | — |
| `p25` | `293`, `NAC 293`, `293 NAC`, `$293`, `0x293` | The NAC, in hex (up to 3 digits). `F7E` and `F7F` (a radio's "receive any") mean any | — | 1–65535 |
| `dmr` | `1`, `CC 1` | Colour code 0–15 | 1, 2 | 1–16777215 |
| `nxdn48`, `nxdn96` | `5`, `RAN 5` | RAN 0–63. RAN 0 means any | — | 1–65535 |
| `am` | (none) | AM carries no code: all three must be empty, and a frequency has one row | — | — |

Empty, `0`, `S`, `search`, `any` or `none` mean any. A `code` may name the slot and talkgroup too —
RadioReference's `CC1 TS2 TG201`, `NAC 293 TG 1`, `RAN 5 TG 201` — and is read as the three (if it
agrees with `slot` and `matchTalkgroup`). Example: one P25 frequency, TG 1 and TG 2 filed apart, the
rest as the air names them:

```json
{ "freqHz": 453500000, "mode": "p25", "code": "293", "matchTalkgroup": 1, "talkgroup": 45351, "name": "Townville PW" },
{ "freqHz": 453500000, "mode": "p25", "code": "293", "matchTalkgroup": 2, "talkgroup": 45353, "name": "Townville Parks" },
{ "freqHz": 453500000, "mode": "p25", "code": "293", "name": "Townville (other)" }
```

Rules for rows sharing a frequency:

- They must all have the same `mode`.
- At most one may have no code, slot or talkgroup.
- No two may record the same transmissions (`D023N` and `D047I` are the same DCS signal).
- The channel opens at the lowest `squelchDb` among them.

Before v0.2.6 a row had one `tone` for all three, and P25 rows sharing a frequency filed under their
own (default) number; an older config is upgraded when it's read, those P25 rows given the number
they filed under.

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
TG Number,Frequency,Code,Slot,Match TG,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Bandwidth kHz,Enable
1001,154.4300,,,,fm,County Fire Dispatch,,Fire Dispatch,Fire,,,true
,154.3250,D223N,,,fm,County A Fire,,,Fire,,,true
,154.3250,151.4,,,fm,County B Fire,,,Fire,,,true
,460.1250,,,,p25,PD Tac 2,,Law Tac,Police,12,,true
45351,453.5000,293,,1,p25,Townville PW,,Public Works,Public Works,,,true
9001,451.5000,1,1,1,dmr,Dispatch,,Business,Business,,,true
,119.1000,,,,am,Tower,,Aircraft,Airport,,,true
```

A header row is required. Columns are matched in any case and any order; others are ignored.

| Column | Also accepted | Meaning |
|---|---|---|
| `Frequency` ✓ | `Freq`, `FreqHz` | MHz when it has a decimal point (`154.4300`), otherwise Hz (`154430000`) |
| `Mode` | | `fm`, `am`, `p25`, `dmr`, `nxdn48` (or `nxdn`), `nxdn96`; also `nfm`, `analog`, `A` (= fm), `digital`, `D` (= p25). Empty = `fm`, or `p25` / `dmr` / `nxdn48` when `Code` holds a NAC / colour code / RAN |
| `TG Number` | `Talkgroup`, `TG` | A positive whole number; empty = as [above](#talkgroup-numbers) |
| `Code` | `Tone` | As `code` above. RadioReference's spellings (`151.4 PL`, `023 DPL`, `293 NAC`, `CC1 TS2 TG201`, `RAN 5`) work; a slot or talkgroup in it fills `Slot` / `Match TG` |
| `Slot` | `TS` | As `slot` above |
| `Match TG` | `MatchTalkgroup` | As `matchTalkgroup` above |
| `Alpha Tag` | `Name` | The channel's name |
| `Description`, `Tag` | | Names in the call JSON |
| `Category` | `Group` | Category in the call JSON |
| `Squelch dB` | `SquelchDb` | 3–40, dB above the noise floor; empty = the system's |
| `Bandwidth kHz` | `BandwidthKhz`, `Bandwidth` | `fm` and `am` rows: the filter's width, 4–20; empty = the mode's |
| `Enable` | `Enabled` | `false`, `no`, `0` or `off` = off; anything else or empty = on |

Parsing:

- Commas, semicolons or tabs (guessed from the header row). With semicolons or tabs, decimal commas
  (`154,43`) are read as decimal points, as Excel writes them in some locales.
- Fields can be quoted. Blank lines, `#` comments and a byte-order mark are skipped.
- A row with no usable frequency is skipped. A bad `Mode`, `TG Number`, `Squelch dB`, `Bandwidth kHz`,
  `Code`, `Slot` or `Match TG` is read as the default. Setup lists each such row after reading the file.
- A file with a `Tone` column and no `Code` (Trunk Recorder's, or this recorder's before v0.2.6) gets
  the same upgrade as a config: P25 rows sharing a frequency with no `TG Number` are given the number
  they filed under before.
- **Trunk Recorder's channel file reads as is.** Its `Squelch` column (an absolute level) is not
  read: use `Squelch dB`. A `Tone` of `S` (search) needs no setting here, since every analog call's
  tone is identified and written to its JSON anyway.
