# Recording

The `recording` object says where calls are saved and which are kept. This page lists its keys, the
per-system overrides and the `filenameFormat` tokens. For what a recording folder looks like, the
call JSON, M4A files and the RAM spool, see the [recordings guide](../guides/recordings.md).

```json
"recording": {
  "captureDir": "/home/me/TrunkRecorderPro",
  "minCallS": 1,
  "compressWav": true,
  "filenameFormat": "{short_name}/{time:%Y-%m-%d}/{talkgroup}-{time:%H%M%S}"
}
```

In Setup these are on the **Recording** tab: the **Recorder** panel, **Call rules**, and (desktop)
**M4A audio**. Rules marked "per system" can be overridden on each trunked or conventional system
under **Recording override**.

## Recording keys

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `captureDir` | | `~/TrunkRecorderPro` | string | The recordings folder. Write an absolute path: `~` isn't expanded, and a relative path is taken from wherever the app was started |
| `prerollS` | | `1` | number | Seconds of air from before each grant added to the start of a trunked call. The interface allows 0–3 |
| `maxRecorders` | | `32` | integer | Calls recorded at once, across all trunked systems. The interface allows 1–64 |
| `callTimeoutS` | | `3` | number | Per system. A call ends this long after its last grant or audio, s. The interface allows 1–30 |
| `recordUnknown` | | `true` | bool | Per system. Record talkgroups that aren't in the talkgroup file. Only matters when there is a talkgroup file; a talkgroup patched with a listed one is recorded anyway |
| `recordEncrypted` | | `false` | bool | Per system. Keep calls flagged encrypted (by the grant or the talkgroup's `Mode`), with their radios, even with no audio. Encrypted transmissions are always left out of the audio. Config only: not in the interface |
| `recordUnitToUnit` | | `true` | bool | Per system. Record P25 unit-to-unit (private) calls |
| `keepSilentCalls` | | `false` | bool | Per system. Keep calls with no audio (encrypted, or nothing decoded) |
| `minCallS` | | `0` | number | Per system. Delete calls with less audio than this, s, before they're uploaded. `0` keeps all. The interface allows 0–60 |
| `maxCallS` | | `0` | number | Per system. Save a call this long and carry on in a new one, s; nothing is lost. `0`: parts of 600 s, so a stuck carrier never records without bound. The interface allows 0–3600 |
| `minTransmissionS` | | `0` | number | Per system. Leave out transmissions shorter than this (key-ups, data bursts), s. `0` keeps all. The interface allows 0–10 |
| `normalizeAudio` | | `true` | bool | Per system. Bring every call's speech to the same loudness (about −14 LUFS, peaks limited to −2.5 dBFS) |
| `digitalLevelDb` | | `0` | number | Per system. Then raise or lower digital calls by this much, dB; the limiter keeps them from clipping. Applied up to ±40; the interface allows ±20 |
| `analogLevelDb` | | `0` | number | Per system. The same for analog calls |
| `compressWav` | | `false` | bool | Per system. Also save an `.m4a` of every call. Needs an encoder: see `m4a` |
| `audioArchive` | | `true` | bool | Per system. Keep the audio once every upload plugin has had the call. `false`: deleted then |
| `callLog` | | `true` | bool | Per system. Keep the call JSON once every upload plugin has had the call |
| `archiveFilesOnFailure` | | `true` | bool | Per system. When an upload failed, keep the files whatever the two above say |
| `filenameFormat` | | `""` | string | Per system. Folders and file names under `captureDir`. See [File names](#file-names) |
| `dropDuplicateCalls` | | `true` | bool | Multi-site: a call heard on several sites of one system is saved once, keeping the cleanest copy or the talkgroup's Preferred Site. See [systems.md](systems.md#multi-site) |
| `vocoder` | | `"fixed"` | string | The IMBE voice decoder for P25 Phase 1 (trunked and conventional): `"fixed"` (fixed-point, usually sounds most natural), `"enhanced"` or `"mbelib"`. An unknown value is `"fixed"` |
| `captureFrames` | | `false` | bool | Also save each call's vocoder frames as `<call>.frames.jsonl`, for diagnosis and `trunk-pro tool revoice` |
| `m4a` | | see below | object | The M4A encoder for `compressWav` and for plugins that upload M4A |
| `ramSpool` | | see below | object | Keep files only the upload plugins need in RAM instead of `captureDir` |

When a call flagged encrypted at its grant isn't kept (`recordEncrypted` off), it isn't recorded at
all. One that turns encrypted part-way is kept with its clear transmissions.

### `m4a`

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `encoder` | | `"auto"` | string | `"auto"` (ffmpeg, then afconvert, then fdkaac, whichever is found first), `"ffmpeg"`, `"afconvert"` (macOS), `"fdkaac"` or `"none"` (WAV only) |
| `bitrateKbps` | | `32` | integer | AAC bitrate, 8–320 kb/s |

Each call is encoded once, and the same `.m4a` serves `compressWav` and every plugin. Changing
`m4a` while recording restarts the plugins.

### `ramSpool`

With `audioArchive` or `callLog` off, some files exist only so the upload plugins can send them, and
are deleted right after. The RAM spool keeps those in memory instead of writing them to the disk
(easier on an SD card or SSD).

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `enabled` | | `false` | bool | Use the spool. macOS: a RAM disk the app makes (no administrator needed, hidden from the Finder and Spotlight). Linux: a folder in `/dev/shm` |
| `sizeMb` | | `256` | integer | Its size, MB (Linux: the most it may hold). Read as 16–65536; the interface allows 16–8192 |
| `dir` | | `""` | string | A RAM-backed folder of your own (a tmpfs mount) instead. Config only |

- A failed upload's files are moved to `captureDir`, following `archiveFilesOnFailure`.
- When the spool is full, calls go straight to `captureDir`.
- It applies at the next **Start**. If it can't be made, the log says why and calls go to
  `captureDir`.
- Its room is shown on the dashboard's **Platform** page, with an event as it fills.

## Per-system rules

A trunked or conventional system's own `recording` object can override any rule marked "per system"
above. A rule left out follows the global `recording`.

```json
{ "shortName": "fireground", "type": "p25", "controlChannelsHz": [851012500],
  "recording": { "minCallS": 2, "recordUnknown": false } }
```

| Key | Type |
|---|---|
| `callTimeoutS`, `minCallS`, `maxCallS`, `minTransmissionS`, `digitalLevelDb`, `analogLevelDb` | number |
| `recordUnknown`, `recordEncrypted`, `recordUnitToUnit`, `keepSilentCalls`, `normalizeAudio`, `compressWav`, `audioArchive`, `callLog`, `archiveFilesOnFailure` | bool |
| `filenameFormat` | string |

Other keys in a system's `recording` (`captureDir`, `vocoder` and the like) are recorder-wide only;
they're ignored there and dropped on save.

## File names

With `filenameFormat` empty, calls are saved as Trunk Recorder saves them, in local time:

```text
<captureDir>/<shortName>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>[.<slot>].wav
<captureDir>/<shortName>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>[.<slot>].json
```

Month and day aren't zero-padded (`2026/10/4`). `<freq>` is in Hz. `.<slot>` is added for P25
Phase 2 and DMR calls.

Otherwise `filenameFormat` is a path template relative to `captureDir`, with `/` making folders,
built from Trunk Recorder's tokens. The interface's **Folders and file names** editor shows a preview
and flags unknown tokens.

```json
"filenameFormat": "{short_name}/{time:%Y-%m-%d}/{talkgroup}-{time:%H%M%S}"
```

| Token | Gives |
|---|---|
| `{short_name}` | The system's short name |
| `{sys_num}` | The system's number (trunked from 0; conventional 65535 down) |
| `{talkgroup}`, `{talkgroup_display}` | The talkgroup number |
| `{talkgroup_alpha_tag}` | The talkgroup's alpha tag |
| `{talkgroup_description}` | Its description |
| `{talkgroup_tag}` | Its tag ("Fire Dispatch") |
| `{talkgroup_group}` | Its category |
| `{freq}` | The frequency, Hz |
| `{freq_mhz}` | The frequency, MHz with 4 decimals |
| `{epoch}` | The start time, Unix seconds |
| `{time:FORMAT}` | The start time in local time, `FORMAT` being strftime's: `%Y %m %d %H %M %S`, `%j`, `%a`, `%b`, `%F`, `%T`, `%z` and so on, `%f` for milliseconds, `%-m` (or `%-d`…) without padding. Or `iso` / `iso_ms` |
| `{ztime:FORMAT}` | The same in UTC (`iso` ends in `Z`) |
| `{call_num}` | The call's number |
| `{tdma_slot}` | The slot (Phase 2, DMR); empty when the call has none |
| `{color_code}` | DMR colour code |
| `{ran}` | An NXDN call's RAN; −1 otherwise |
| `{source_num}`, `{recorder_num}` | Which source and recorder |
| `{audio_type}` | `analog`, `digital` or `digital_tdma` (the call JSON's `audio_type`, space made `_`) |
| `{emergency}`, `{encrypted}`, `{priority}` | Call flags, as numbers |
| `{signal}`, `{noise}` | The call's signal and noise levels, in whole dBFS (cut toward zero, as Trunk Recorder does: -45.7 becomes `-45`); `0` when not measured |

Rendering rules:

- `-call_<call_num>` is always added to the name, as Trunk Recorder does, then `.wav`, `.json`,
  `.m4a`.
- Characters that can't be in a file name (`\ / : * ? " < > |` and spaces) in a token's text become
  `_`, so a talkgroup tag can't make folders.
- A `:` from a time format (`iso`, `%H:%M`) becomes `-`.
- Empty folder names, `.` and `..` are dropped, so the path can't leave `captureDir`.
- An unknown token is left as typed.
