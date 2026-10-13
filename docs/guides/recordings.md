# Recordings

This page covers what Trunk Recorder Pro saves for each call: where the files go and how they are
named, what is in the call JSON, how the audio is processed, M4A files, which files are kept or
deleted once plugins have uploaded them, the RAM spool, and the rules that decide which calls are
kept at all.

Every setting mentioned here is on **Setup → Recording**, and is listed with its key and default in
the [recording configuration reference](../configuration/recording.md).

## Where calls go

Calls are saved under the **Recordings folder** (`captureDir`). The default is `TrunkRecorderPro`
in your home folder. In the config, give an absolute path: `~` isn't expanded.

Out of the box, files are laid out as Trunk Recorder lays them out, in local time:

```text
<recordings folder>/<short name>/<year>/<month>/<day>/<talkgroup>-<start epoch>_<frequency>.wav
```

For example, a call on talkgroup 1201 at 858.4875 MHz on a system with the short name `county`,
on 6 October 2026:

```text
~/TrunkRecorderPro/county/2026/10/6/1201-1791298712_858487500.wav
~/TrunkRecorderPro/county/2026/10/6/1201-1791298712_858487500.json
```

- Month and day aren't zero-padded (`10/6`, not `10/06`), as in Trunk Recorder.
- The short name is the system's (or the conventional system's) **Short name**.
- Calls on a two-slot channel (P25 Phase 2 and DMR) get the slot added: `..._858487500.1.wav`.
- The frequency is in Hz, and the epoch is the call's start in seconds since 1970 (UTC).

The browser version doesn't write files to a folder: it keeps calls in the browser's own storage.

## Choosing folders and file names

To lay files out differently, set **Folders and file names** (`filenameFormat`) under **Call
rules → Files**. It is a path under the recordings folder, built from tokens such as
`{talkgroup}` and `{time:%Y}`, with `/` making folders. Leave it empty for the layout above.

### The editor

The field shows the current format as a row of chips, with an **Example:** path below it as it
would be for a call right now. **macOS / Linux** and **Windows** switch the example's separators
(the recorder takes `/` or `\` as a folder either way). Click **Edit…** to change it:

- Drag chips from the palette into the format, drag them around to reorder, and drag one back out
  (or press Delete) to remove it.
- **Folders and joiners** has `/ folder`, `-`, `_`, `.`, a space, and a box for your own text
  (**Add text**). Characters that can't be in a file name (`/ \ : * ? " < > | { }`) aren't
  accepted there.
- **Talkgroup**, **Call** and **Radio** hold the call tokens. **Start time** holds time pieces,
  with **Local** or **UTC**.
- **Start from…** loads a ready-made layout: "Trunk Recorder's layout", "By talkgroup, then day",
  "By day, named by talkgroup" or "By category (UTC)".
- **Edit as text** shows the format as a string you can type or paste.
- **Reset to default** goes back to the default layout.

Problems, such as an unknown token or a `{` without a `}`, are listed under the field in red.

Each system and conventional system can have its own format in its **Recording override**; left at
default, it follows the Recording tab's.

### Tokens

| Token | Gives |
|---|---|
| `{short_name}` | The system's short name |
| `{sys_num}` | The system's number: trunked systems from 0; conventional systems 65535, 65534, ... |
| `{talkgroup}`, `{talkgroup_display}` | The talkgroup number |
| `{talkgroup_alpha_tag}` | The talkgroup's alpha tag, e.g. `FD_Disp` |
| `{talkgroup_tag}` | The talkgroup's tag, e.g. `Fire_Dispatch` |
| `{talkgroup_description}` | The talkgroup's description |
| `{talkgroup_group}` | The talkgroup's category |
| `{freq}` | Frequency in Hz |
| `{freq_mhz}` | Frequency in MHz, four decimals: `858.4875` |
| `{epoch}` | Start time, seconds since 1970 |
| `{time:FORMAT}` | Start time in local time |
| `{ztime:FORMAT}` | Start time in UTC |
| `{call_num}` | The call's number |
| `{tdma_slot}` | The slot, on two-slot channels (Phase 2, DMR); empty otherwise |
| `{audio_type}` | `analog`, `digital` or `digital tdma` |
| `{emergency}`, `{encrypted}` | `1` or `0` |
| `{priority}`, `{source_num}`, `{recorder_num}` | As in the call JSON |
| `{color_code}`, `{ran}` | DMR colour code, NXDN RAN; `-1` when the call has none |
| `{signal}`, `{noise}` | The call's signal and noise levels in whole dBFS (-45.7 becomes `-45`, as in Trunk Recorder); `0` when not measured |

`FORMAT` is strftime's: `%Y` year, `%m` month, `%d` day, `%H%M%S` time, `%j` day of the year, `%a`
weekday, `%b` month name, and so on. A `-` after the `%` drops the zero padding (`%-m`, `%-d`).
`%f` gives milliseconds. `iso` and `iso_ms` are shortcuts for an ISO 8601 time. A time format can
make folders (`{time:%Y/%m}`).

```json
"filenameFormat": "{short_name}/{time:%Y-%m-%d}/{talkgroup}-{talkgroup_alpha_tag}/{time:%H%M%S}_{freq}"
```

### Rendering rules

- `-call_<call number>` is always added to the end of the name, as Trunk Recorder does. (So even a
  format that copies the default layout gives different names from an empty format.)
- Text from the talkgroup list has `\ / : * ? " < > |` and spaces turned into `_`.
- A `:` from a time format (`iso`, `%H:%M`) becomes `-`, because Windows can't have it in a name.
- Empty folders, `.` and `..` are dropped, so a format can't climb out of the recordings folder.
- An unknown token is left in the name as typed.

## What each call saves

| File | When |
|---|---|
| `<name>.wav` | 8 kHz mono 16-bit PCM. Written unless nothing keeps or needs it (see [What's kept](#whats-kept-and-whats-deleted)) |
| `<name>.json` | The [call JSON](#the-call-json) |
| `<name>.m4a` | With **Also save an M4A**, or when an upload plugin wants M4A |
| `<name>.sdr` | With **Save vocoder frames**, for digital calls (NXDN: `<name>.frames.jsonl`) |

## The call JSON

Each call's JSON uses Trunk Recorder's field names, so tools written for Trunk Recorder read it.
It is written on a single line.

| Field | Meaning |
|---|---|
| `call_num` | The call's number |
| `freq` | Frequency, Hz |
| `freq_error` | How far off the channel the voice was, Hz (0 when not measured) |
| `signal`, `noise` | The channel's power during the call and the noise floor under it, dBFS, one decimal; `null` if not measured |
| `snr` | `signal` minus `noise`, dB |
| `clean_voice_pct` | Share of vocoder frames decoded cleanly, %; `null` for analog calls |
| `source_num` | Always `0` |
| `recorder_num` | Trunked: the recorder slot. Conventional: the index of the channel (frequency) it came on |
| `tdma_slot`, `phase2_tdma` | The slot, and `1` for a P25 Phase 2 call |
| `start_time`, `stop_time` | Start and end, seconds since 1970 |
| `start_time_ms`, `stop_time_ms` | The same in milliseconds |
| `emergency`, `encrypted` | `1` or `0` |
| `priority`, `mode`, `duplex` | As Trunk Recorder writes them (`mode` and `duplex` are always `0` on conventional calls) |
| `call_length`, `call_length_ms` | Length of the saved audio, seconds (rounded) and milliseconds |
| `talkgroup` | Talkgroup number |
| `talkgroup_tag` | The talkgroup's alpha tag |
| `talkgroup_description` | Its description |
| `talkgroup_group_tag` | Its tag |
| `talkgroup_group` | Its category |
| `color_code` | DMR colour code; `-1` for none |
| `ran` | NXDN RAN; `-1` for none |
| `tone_mode`, `tone_detected`, `tone_confidence` | Analog conventional calls' tone: see [Tones](tones.md#how-tones-are-identified) |
| `audio_type` | `analog`, `digital` or `digital tdma` |
| `short_name` | The system's short name |
| `patched_talkgroups` | Only when the talkgroup was patched with others: all of them |
| `freqList` | One entry for the call: `freq`, `time`, `pos` (0), `len` (seconds of audio), `error_count` (FEC errors), `spike_count` (0) |
| `errorList` | Per 10 s of audio: `pos`, `len`, `frames`, `error_count`, `bad_frames` (repeated, muted or erased), `max_frame_errors` |
| `srcList` | Each radio heard: `src` (unit ID), `time`, `pos` (seconds into the call), `emergency`, `signal_system` (empty), `tag` (its name), `tag_ota` (its talker alias) |

Compared with Trunk Recorder's JSON:

- `snr`, `clean_voice_pct`, `ran`, `errorList` and the three tone fields are additions.
- `signal` and `noise` are in dBFS with a decimal, not whole numbers.
- `freqList` has one entry for the whole call rather than one per transmission.
- Trunk Recorder pretty-prints its JSON; this is one line. JSON readers don't mind.

How `tag` is chosen for each unit (your unit names first, or talker aliases first) is covered in
[Talkgroups and units](talkgroups-and-units.md).

## Audio processing

Before a call is saved:

1. **Short transmissions and encrypted ones are cut out.** Transmissions shorter than **Shortest
   transmission, s** are removed. Encrypted transmissions are always removed: what a vocoder makes of
   them is noise.
2. **Loudness is evened out** (**Even out call loudness**, on by default). Each call's speech is
   brought to about -18.5 LUFS, close to the level Trunk Recorder's uploads have. The gain is
   limited to between -18 dB and +24 dB, and a limiter keeps peaks below -2.5 dBFS. A call with less
   than 0.2 s of speech (a keyed radio sending silence, a squelch tail) is left alone rather than
   having its hiss brought up.
3. **Your level adjustment is applied:** **Digital Level Adjustment, dB** or **Analog Level
   Adjustment, dB** (-20 to +20 in the interface).

Analog FM audio is also de-emphasised (750 µs) and high-passed at 300 Hz when it is demodulated,
which removes CTCSS tones from what you hear.

## M4A files

M4A (AAC audio) is about a tenth the size of WAV. Some upload plugins send M4A, and **Also save an
M4A** (`compressWav`) keeps one of every call.

Nothing is built in for this: Trunk Recorder Pro runs an encoder installed on the computer. The
**M4A audio** panel on the Recording tab picks it:

- **Automatic (ffmpeg, then afconvert, then fdkaac)** uses the first one it finds. ffmpeg works
  everywhere; `afconvert` comes with macOS. Programs in `/opt/homebrew/bin`, `/usr/local/bin`,
  `/usr/bin` and `/snap/bin` are found even when the app wasn't started from a shell.
- Or name one: **ffmpeg**, **afconvert (macOS)**, **fdkaac**, or **None: WAV only**.
- **Bitrate, kbps**: 32 by default, as Trunk Recorder uses. ffmpeg and afconvert encode 16 kHz
  mono AAC.

The panel's header shows which encoder was found, or "no encoder found". If **Also save an M4A**
is on and there's no encoder, the log says `No M4A encoder found (install ffmpeg): calls are kept
as WAV only`. If encoding a call fails, the error is logged and the call keeps its WAV.

The WAV is fed to the encoder from memory. If the call's WAV isn't being kept, it is never written
to disk at all.

## What's kept and what's deleted

Trunk Recorder's three settings decide what stays in the recordings folder after upload plugins
have handled a call:

| Setting | Key | Default | Effect |
|---|---|---|---|
| **Keep the audio after uploading** | `audioArchive` | on | Off: the WAV (and frames file) is deleted once every upload plugin has finished with the call |
| **Keep the call JSON after uploading** | `callLog` | on | Off: the JSON is deleted then |
| **Keep everything when an upload fails** | `archiveFilesOnFailure` | on | If any plugin reports a failure, everything is kept, whatever the two above say |

How they work together:

- **They only matter when an upload plugin is running.** "After uploading" means after every
  enabled plugin that takes finished calls has reported back. With no such plugin, every call is
  kept whole, whatever these say: deleting the files would leave nothing of the call.
- **An M4A made only for a plugin** (with **Also save an M4A** off) is deleted afterwards too,
  unless it is the only audio the call has.
- **A call no plugin reports on within an hour** keeps its files.
- With **Also save an M4A** on and **Keep the audio after uploading** off, both the WAV and the M4A
  are deleted after uploading.

So, to upload calls without keeping them, turn off the first two and leave the third on: calls
whose upload failed stay, so you can see what went wrong. These are desktop settings; they aren't
shown in the browser version. Plugins and their settings are covered in [Plugins](../plugins/README.md).

## The RAM spool

A call that is only uploaded, then deleted, still gets written to disk and erased again. On an SSD
or SD card that runs for months, that's wear for nothing. The RAM spool keeps those files in memory
instead.

Turn on **Keep calls waiting to upload in memory** on the Recording tab and set **RAM spool size,
MB** (256 by default). It takes effect the next time recording starts.

- **macOS:** the app makes a RAM disk of that size (no administrator password needed), mounted out
  of sight inside its data folder (`~/Library/Application Support/trunk-pro/spool`). It doesn't
  appear in the Finder sidebar and isn't indexed by Spotlight.
- **Linux:** a folder in `/dev/shm`, which is RAM-backed. The app keeps its use under the size you
  set. If `/dev/shm` isn't a RAM filesystem on your machine, set `ramSpool.dir` in the config to a
  tmpfs mount of your own.
- **Other systems (Windows):** there's no automatic spool; set `ramSpool.dir` to a RAM-backed
  folder if you have one.

What goes in it: only the files that won't be kept, because of the settings above. Files that will
be kept go straight to the recordings folder. If an upload fails and **Keep everything when an
upload fails** is on, the call's files are moved from the spool to the recordings folder.

When things go wrong, nothing is lost:

- **Full:** when the spool has less than 5% free, calls are written to the recordings folder as if
  there were no spool.
- **Can't be made:** recording goes ahead without it, and the log says `No RAM spool (...): calls go
  to the recordings folder`.
- **Restarts:** the spool outlives the app (not the computer), so plugins still find calls they had
  queued.
- **Left behind:** files still in the spool after two hours (a plugin that never reported) are moved
  to the recordings folder. This is checked every ten minutes.
- **Turned off:** the next time recording starts, anything left in the app's spool is moved to the
  recordings folder and the RAM disk or folder is removed.

The **Overview** page's **Computer** card shows how full the spool is. The dashboard's events
report it as it fills: `The RAM spool has only N% free: uploads aren't keeping up` (below 25%
free), `... calls will soon go to the disk` (below 10%), `The RAM spool was full: N calls written
to the recordings folder instead`, and `The RAM spool has room again`.

## Spotlight on macOS

macOS's Spotlight reads every file saved in an indexed folder into its search index. For a
recordings folder that gains thousands of calls a day, that's a lot of disk writes for files nobody
searches for.

The app checks once a day (and when you change the recordings folder) whether Spotlight indexes the
folder. If it does, or might, the **Platform** page shows a note: **Spotlight indexes the
recordings folder** (or **Spotlight may be indexing the recordings folder**) with the steps to stop
it:

1. Open **System Settings → Spotlight** and click **Search Privacy**.
2. Click **+** and choose the recordings folder (⇧⌘G in the dialog lets you type a path).

Apps can't read or change that list, so the app finds out by asking Spotlight, which can take a
couple of minutes. When it can't tell, the note has an **I've done it** button to hide it. A folder
whose name ends in `.noindex` is never indexed, which is another way to exclude it.

## Call rules

These are under **Call rules** on the Recording tab. Each system and conventional system can
override them in its **Recording override**; each one left at **Default** follows the Recording
tab.

**What is recorded**

- **Talkgroups not in the talkgroup file** (`recordUnknown`, on): applies only when a system has a
  talkgroup list. A talkgroup patched with a listed one is recorded anyway.
- **Unit-to-unit calls** (`recordUnitToUnit`, on): private calls between two radios.
- **Call timeout, s** (`callTimeoutS`, 3): a call ends this long after its last grant or audio.

These first two apply to trunked systems; they have no effect on conventional channels.

**Which calls are kept**

- **Shortest call, s** (`minCallS`, 0 = keep all): calls with less audio than this are deleted and
  not uploaded. This is measured after short and encrypted transmissions are removed.
- **Shortest transmission, s** (`minTransmissionS`, 0 = keep all): transmissions shorter than this
  (key-ups, data bursts) are removed from the call. A pause of more than half a second separates two
  transmissions.
- **Longest call, s** (`maxCallS`, 0 = no limit): a longer call is saved and a new one started, with
  nothing lost in between. Even with no limit, a call is split every 600 s, so a stuck carrier
  can't record forever.
- **Calls with no audio** (`keepSilentCalls`, off): keep calls that ended with no audio, such as
  encrypted or undecoded ones.

`recordEncrypted` (off) is in the config only, not the interface. Turned on, encrypted calls are
recorded and kept even with no audio, so you have a record of who talked and when; their
encrypted audio is still removed.

A call that isn't kept logs `Call not saved - no audio, or shorter than the minimum`.

## Vocoder and frame capture

- **P25 voice decoder** (`vocoder`): which IMBE vocoder turns P25 Phase 1 voice into audio.
  **Fixed-point** (the default) is Pavel Yazev's decoder, Trunk Recorder's `softVocoder: false`, and
  usually sounds most natural. **Enhanced** is Trunk Recorder's floating-point synthesis. **mbelib**
  behaves exactly like mbelib.
- **Save vocoder frames** (`captureFrames`, off): also saves each digital call's raw vocoder frames
  as `<name>.sdr`, MimoSDR's call file ([sdr-stream](https://github.com/MimoCAD/sdr-stream)):
  the call's identity and the voice still coded, about a tenth the size of the WAV. MimoSDR's
  tools read it, and `trunk-pro tool revoice` turns it back into a WAV with a different vocoder, so
  you can compare them on the same call. NXDN, which the format doesn't cover, is saved as
  `<name>.frames.jsonl`. See the [command line](../command-line.md).
