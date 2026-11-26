# Plugin events

Every topic a plugin can subscribe to, field by field. How to choose topics and write a plugin is
in [Writing plugins](../../plugins/writing-plugins.md#topics); the wire format around these
messages is in [Protocol](protocol.md).

The examples are the JSON on the wire: each message is the event's fields plus `type`. The Rust
SDK hands you the same data as structs, with the same field names. Times are Unix seconds and
frequencies are in hertz, unless a field says otherwise.

| Topic | SDK method | Struct | When | How often |
|---|---|---|---|---|
| [`call.concluded`](#callconcluded) | `call_concluded` | `ConcludedCall` | A recorded call's files are on disk | Once a call |
| [`call.start`](#callstart-callupdate-and-callend) | `call_start` | `CallInfo` | A call starts, recorded or not | Once a call |
| [`call.update`](#callstart-callupdate-and-callend) | `call_update` | `CallInfo` | A call changes while it runs | As it changes |
| [`call.end`](#callstart-callupdate-and-callend) | `call_end` | `CallInfo` | A call ends | Once a call |
| [`unit`](#unit) | `unit` | `UnitEvent` | A radio registers, affiliates, … | Many a second on a busy system |
| [`audio`](#audio) | `audio` | `AudioChunk` | Live audio of recording calls | Many a second for each call |
| [`voice`](#voice) | `voice` | `VoiceChunk` | Live voice of recording digital calls, as vocoder frames | One beside each `audio` chunk |
| [`tap.<id>`](#tapid) | `tap` | `TapChunk` | A signal tap: data from part-way through a voice channel's receive chain | Many a second for each transmission |
| [`status`](#status) | `status` | `Status` | Every site's state | Every 5 s |
| [`totals`](#totals) | `totals` | `Totals` | Every series the recorder measures, as running totals | Every 10 s |
| [`tone_pattern`](#tone_pattern) | `tone_pattern` | `TonePatternEvent` | A call's tones made a pattern: a page, an alert tone, a DTMF or KNOX string, a Morse ID | As heard |
| [`log`](#log) | `log_line` | `LogLine` | A line the recorder logged | Each line |

A plugin is sent only the topics in its manifest's `subscribe`. The recorder doesn't build events
nobody subscribed to, so subscribing to a topic you don't use costs everyone something.

## Systems and sites in events

Events name their system three ways:

| Field | What it is |
|---|---|
| `system_id` | Its identity: the same across runs and renames, never another system's (`SystemInfo::id` in `hello`). Store anything by it. A concluded call has it as `call.system_id` |
| `short_name` | What users know it by. Unique among the recorder's systems, and it changes when the system is renamed |
| `system` | Its number for this run only (`SystemInfo::index`). From 0: the trunked systems first, then the conventional ones. Adding, removing or turning off a system renumbers the rest |

A system's `kind` in `hello` (`p25`, `smartnet`, `dmr`, `nxdn`, `edacs` or `conventional`) says
which kind it is; tell a conventional system by it, not by its number.

Calls and unit events also carry **`site`**, the name of the site they were heard on (a system of
one site: its short name; none for a conventional system's call), and **`site_id`**, the site's
identity as `system_id` is the system's (`SiteInfo::id`).

A call heard on several sites of a system is one call: one `call.start` when its first copy
begins and one `call.end` when its last copy ends, under the first copy's `id`. Its `site` and
`freq_hz` are those of the copy speaking for it (one being recorded, if any is), and a
`call.update` says when that changes. `call.concluded` is for the copy kept, and it and live audio
carry that copy's own call id.

## `call.concluded`

A recorded call is finished and its files are written. This is the event for uploaders and
archivers.

```json
{
  "type": "call.concluded",
  "path": "county/2026/9/30/101-1790771550_857587500",
  "system": 0,
  "call": {
    "call_num": 412, "short_name": "county", "system_id": 1, "site": "county", "site_id": 1,
    "sites": [{ "id": 1, "name": "county" }], "talkgroup": 101,
    "talkgroup_tag": "Fire Dispatch", "talkgroup_description": "", "talkgroup_group": "Fire", "talkgroup_group_tag": "Fire Dispatch",
    "freq": 857587500, "start_time": 1790771550, "stop_time": 1790771554, "call_length": 4, "call_length_ms": 4180,
    "emergency": 0, "encrypted": 0, "priority": 0, "phase2_tdma": 0, "tdma_slot": 0, "audio_type": "digital",
    "freqList": [{ "freq": 857587500, "time": 1790771550, "pos": 0, "len": 4, "error_count": 0, "spike_count": 0 }],
    "srcList": [{ "src": 1116707, "time": 1790771550, "pos": 0, "emergency": 0, "signal_system": "", "tag": "", "tag_ota": "E14" }]
  },
  "files": {
    "json": "/home/me/TrunkRecorderPro/county/2026/9/30/101-1790771550_857587500.json",
    "wav": "/home/me/TrunkRecorderPro/county/2026/9/30/101-1790771550_857587500.wav",
    "m4a": "/home/me/TrunkRecorderPro/county/2026/9/30/101-1790771550_857587500.m4a"
  }
}
```

| Field | |
|---|---|
| `path` | The call's key: its files' path under the capture folder, without an extension (`<shortName>/<Y>/<M>/<D>/<tg>-<start>_<freq>`). Report on the call by it: `host.call_result(&call.path, …)` |
| `system` | The system's number for this run. Know the system by `call.system_id` instead |
| `call` | The call's JSON file (`CallRecord`), in Trunk Recorder's format so existing tools and services understand it. Every field is in [The call JSON](../../guides/recordings.md#the-call-json). `CallRecord` names every field the recorder writes; fields it doesn't know (from another recorder's file) are in `call.extra` |
| `files.json`, `files.wav` | Full paths. The WAV is 16-bit mono, 8 kHz. A call without an M4A always has it; with an M4A it may not have been written ([Audio](../../plugins/writing-plugins.md#audio)) |
| `files.m4a` | There only when the plugin asked for M4A and the recorder could make it |

What the SDK adds to `CallRecord`:

| Method | |
|---|---|
| `audio_ms()` | How long the audio is, ms, from `call_length_ms`. `call_length` is whole seconds, as Trunk Recorder writes it, so a 0.4 s call says 0 |
| `has_audio()` | `false` for a call kept with no audio (an encrypted one, or with the recorder's **Keep calls with no audio** on): there's nothing to play or upload |
| `error_count()`, `spike_count()` | The per-frequency counts in `freqList`, added up, as Trunk Recorder's uploaders sent them |
| `to_json()` | The record as the `.json` file has it |

Fields worth knowing:

- **`patched_talkgroups`** is there only when the call's talkgroup was patched with others during
  the call: every talkgroup in the patch, its own included, ascending (empty when absent). A
  patched call is usually recorded on the patch's supergroup, which is often not in the talkgroup
  list, so this is how to tell what it carried.
- **`site`** is the site the call was kept from, and **`sites`** every site of its system it was
  recorded on (`id` and `name`), the kept copy's first. A copy the saved one doesn't hold all of
  is saved too (better a duplicate than a transmission lost), and copies of one transmission share
  **`transmission_id`**. A system with `conversationMode` off makes each transmission a call of its
  own.
- **`end_reason`** says why it ended: `terminator` (its voice channel's, with conversations off),
  `squelch` (analog, with conversations off), `next_transmission`, `timeout`, `too_long`,
  `reassigned` or `stopped`; empty when not said (a recorder before it). (Since SDK 0.16.1.)

Only recorded calls conclude. Calls the recorder didn't record (encrypted, no free recorder, an
unknown talkgroup when those aren't recorded) have no files.

## `call.start`, `call.update` and `call.end`

A call started, changed, or ended. Unlike `call.concluded`, these cover calls that aren't recorded
too. All three carry the whole call as it is at that moment (`CallInfo`).

`call.update` comes whenever any of it changes while it runs: another radio talking (`sources`
gains it, as the voice channel or a grant update names it), its flags (an emergency, encrypted),
its patched talkgroups, whether it's recorded, its key or its decryption, a radio's position; on a
conventional channel, its talkgroup from the air or its tone. `call.end` comes as soon as the call
ends; its files, if it was recorded, come later in `call.concluded`.

```json
{
  "type": "call.start",
  "id": 412, "system": 0, "system_id": 1, "short_name": "county", "site": "county", "site_id": 1,
  "talkgroup": 101, "talkgroup_tag": "Fire Dispatch",
  "freq_hz": 857587500, "tdma_slot": null,
  "analog": false, "encrypted": false, "algid": null, "key_id": null, "decrypted": false,
  "emergency": false, "duplex": false, "packet": false, "priority": 4,
  "recording": true, "reason": null,
  "start_time": 1790771550.2,
  "units": [1116707],
  "sources": [{ "unit": 1116707, "tag": "Engine 12", "time": 1790771550.2, "emergency": false, "encrypted": false, "duplex": false, "packet": false, "priority": 4 }],
  "patched_talkgroups": [101, 102],
  "patched": [{ "talkgroup": 102, "alpha_tag": "Fire Tac 2" }]
}
```

| Field | |
|---|---|
| `id` | Unique while the recorder runs: the concluded call's `call_num`. A call heard on several sites keeps its first copy's `id`, and the copy that concludes may be another site's, with another number: match those by system, talkgroup and time if you need to |
| `system`, `system_id`, `short_name`, `site`, `site_id` | [Its system and site](#systems-and-sites-in-events) |
| `talkgroup`, `talkgroup_tag` | The talkgroup and its alpha tag, when known |
| `talkgroup_ota` | On a conventional P25, DMR or NXDN channel: the talkgroup the air named, when a channel row files the call under another `talkgroup` |
| `freq_hz` | The voice frequency |
| `tdma_slot` | 0 or 1 on a two-slot channel (P25 Phase 2, DMR); else `null` |
| `analog`, `encrypted`, `emergency` | Flags |
| `algid`, `key_id` | The cipher an encrypted P25 call's voice named, once heard (`0x84` AES-256, `0xAA` ADP, …) |
| `decrypted` | Some of its voice was decrypted with the system's keys |
| `duplex`, `packet`, `priority` | The grant's service options. Priority 1 is lowest, 4 the default, 7 highest; 0 not given |
| `recording` | Whether it's recorded |
| `reason` | Why not: `encrypted`, `unknown_tg` (not in the talkgroup list, and unknown talkgroups aren't recorded), `no_recorder` (all in use), `no_source` (no radio covers its frequency). More may be added |
| `start_time` | Unix s |
| `units` | Radios heard on the call so far. At `call.end`, all of them |
| `sources` | The same radios, in order, each with its name (`tag`, as the call JSON's `srcList` will have it), when it was first heard (`time`), and what its own transmission said of itself: `emergency`, `encrypted`, `duplex`, `packet`, `priority` |
| `patched_talkgroups` | The talkgroups patched with this one so far, its own included, ascending; empty when it isn't patched |
| `patched` | The others, without its own (which `talkgroup_tag` names), each with its `alpha_tag` from the system's talkgroups (left out when it has none). Since SDK 0.15.1 |

## `unit`

A radio did something on the control channel.

```json
{ "type": "unit", "system": 0, "system_id": 1, "short_name": "county", "site": "county", "site_id": 1, "kind": "affiliation", "unit": 1116707, "tag": "Engine 12", "talkgroup": 101, "alpha_tag": "Fire Dispatch", "emergency": false, "time": 1790771551.4 }
```

| Field | |
|---|---|
| `system`, `system_id`, `short_name`, `site`, `site_id` | [Its system](#systems-and-sites-in-events), and the site whose control channel told it |
| `kind` | What it did (below) |
| `unit` | The radio's ID |
| `tag` | Its name, as a call's would be (the units file, its talker alias); left out when there's none |
| `talkgroup` | The talkgroup involved, for the kinds that have one (below); else `null` |
| `alpha_tag` | The talkgroup's name, from the system's talkgroups; left out when there's none |
| `emergency` | The message said it's an emergency (an emergency alarm, a data grant's options) |
| `time` | Unix s |

| `kind` | Meaning | `talkgroup` |
|---|---|---|
| `registration` | The radio joined the system | none |
| `deregistration` | It left (switched off, out of range) | none |
| `affiliation` | It selected a talkgroup | the talkgroup |
| `acknowledge` | It acknowledged the system | none |
| `location` | It reported its location | the talkgroup |
| `data_grant` | It was granted a data channel | none |
| `answer_request` | Another radio asked to call it | the talkgroup |
| `call_alert` | It was paged | the talkgroup |
| `emergency_alarm` | It sent an emergency alarm | the talkgroup |

P25 systems report all of these. SmartNet systems report fewer.

## `audio`

Live audio of a call being recorded, as it's decoded: 16-bit mono PCM, little-endian,
base64-encoded. `chunk.samples()` decodes it.

```json
{ "type": "audio", "call_id": 412, "system": 0, "system_id": 1, "short_name": "county", "talkgroup": 101, "sample_rate": 8000, "pcm": "AAABAP7/…" }
```

Chunks of one call come in order. Chunks of different calls are interleaved: tell them apart by
`call_id`, and subscribe to `call.start` and `call.end` to know when a call's audio begins and
ends. A call heard on several sites plays from whichever site's copy has it at the moment, so a
chunk can carry the `call_id` of a copy that had no `call.start`: it is the call of the same system
and talkgroup that did.

Live audio is only for plugins that get the call as it happens. A plugin the user delays calls for
(`pluginDelays`) isn't sent it, nor one that is catching up, and the audio of a call decrypted with
the system's keys goes only to plugins allowed decrypted calls (`shareDecrypted`).

This topic is busy. Hand the audio to a thread of your own straight away.

## `voice`

The vocoder frames each `audio` chunk of a recording digital call was made from, FEC-corrected and
in order, sent beside it: for a plugin that relays digital voice (to a hotspot network, say)
without decoding it to audio and encoding it again. Since SDK 0.11.0.

```json
{
  "type": "voice", "call_id": 412, "system": 0, "system_id": 1, "short_name": "county", "talkgroup": 101,
  "unit": 1116707, "unit_name": "Engine 12", "decrypted": false,
  "frames": [{ "codec": "imbe", "bits": "a3f0…", "e0": 0, "errs": 1, "erased": false, "kind": "voice" }]
}
```

| Field | |
|---|---|
| `call_id`, `system`, `system_id`, `short_name`, `talkgroup` | As in `audio` |
| `unit`, `unit_name` | Who is talking (0: not known yet), and their name, as the call's `sources` give it |
| `decrypted` | Voice decrypted with the system's keys |
| `frames` | The frames (`VoiceFrame`), 20 ms each |

Each frame:

| Field | |
|---|---|
| `codec` | `imbe` (P25 Phase 1, ProVoice: 88 bits) or `ambe` (AMBE+2 half rate: P25 Phase 2, DMR, NXDN: 49 bits) |
| `bits` | The frame's information bits as hex, first bit first, the last byte padded with zeros. `VoiceFrame` has them as a list of 0s and 1s |
| `errs` | The bit errors FEC corrected in the frame |
| `e0` | IMBE: those in u0, the word carrying pitch. AMBE: 0 |
| `erased` | The frame was lost; `errs` then means nothing |
| `kind` | What the vocoder made of it: `voice`, `repeat`, `muted`, `erasure` or `tone` |
| `tone` | AMBE+2: the tone it played, `[id, level]`: a tone frame's, or the one a damaged frame repeated. Left out otherwise |

Voice goes where live audio goes: not to a plugin delayed for the call, decrypted only to plugins
allowed decrypted calls, and encrypted voice the recorder can't decrypt not at all. This topic is as
busy as `audio`.

## `tap.<id>`

A signal tap: what a voice channel's receiver makes on the way to a call's audio, as it is made.
Each tap is a topic of its own, and subscribing to it is what turns the tap on, on every system the
plugin is on for: the user sets nothing up. Since SDK 0.17.0.

| Topic | From | `data` | Notes | |
|---|---|---|---|---|
| `tap.fm.discriminator` | Analog FM channels | `pcm`, 22,050 Hz | None | The FM demodulator's output before de-emphasis, the voice filters and the CTCSS notch, while the squelch is open; ±5 kHz deviation is half of full scale. For data decoders (POCSAG, FLEX, …) |
| `tap.voice.air` | Digital voice: P25 (Phase 1 and 2), DMR, NXDN, EDACS ProVoice | `frames` | `AirFrame`, one a frame | Each voice frame as it came off the air: error-corrected, not decrypted |
| `tap.voice.frames` | Digital voice | `frames` | `FrameErrors`, one a frame | Each voice frame the vocoder was given: after error correction and decryption |
| `tap.voice.errors` | Digital voice | `none` | `FrameErrors`, one a frame vocoded | How each frame decoded: no data, only the figures |

The SDK lists them as `TAPS`, and `Tap::VoiceAir.topic()` is the topic to subscribe to. All of them
call the one method, `tap(chunk)`; `chunk.tap` says which.

```json
{
  "type": "tap", "tap": "voice.air", "stream": 41, "seq": 0, "first": true, "offset": 0,
  "system": 0, "system_id": 3, "short_name": "dcfd", "freq_hz": 854612500.0, "slot": 0,
  "call_id": 912, "talkgroup": 101, "unit": 7001, "voice_mode": "p25", "time": 1795600000.2,
  "data": { "kind": "frames", "codec": "imbe", "frames": ["a3f0…", "7c21…"] },
  "notes": [
    { "at": 0, "n": 0, "errs": 1, "e0": 0, "encrypted": true, "algid": 132, "keyid": 66, "mi": "0102030405060708a9" },
    { "at": 1, "n": 1, "errs": 0, "e0": 0, "encrypted": true, "algid": 132, "keyid": 66, "mi": "0102030405060708a9" }
  ]
}
```

A **stream** is one transmission on one channel (on one slot of a TDMA channel): a channel is
decoded only while something is on it, so a tap has nothing between transmissions. It ends at the
air's terminator, when the squelch closes, after half a second of nothing, or when the channel
closes; a carrier that never drops is cut every 10 minutes.

| Field | |
|---|---|
| `tap` | Which tap: the topic without `tap.` |
| `stream` | The stream the chunk is a piece of: a number no other stream of this run has |
| `seq` | The chunk's number in its stream, from 0. Chunks of a stream come in order; those of different streams are interleaved |
| `first`, `last` | The stream's first and last chunk (left out when false). The last may carry no data |
| `offset` | Where in the stream its data starts: samples, frames, or (for a tap with no data) notes before it |
| `system`, `system_id`, `short_name` | As in `audio` |
| `freq_hz`, `slot` | The channel, and its TDMA slot (0 where there is one) |
| `call_id`, `talkgroup` | The call on the channel as the chunk was made. Left out when there was none: a tap doesn't wait for a call |
| `unit` | Who is talking, as the call's `sources` give it. Left out when it isn't known |
| `voice_mode` | What the channel's voice is sent as, named as a call's `voice_mode`: `p25`, `p25_phase2`, `dmr`, `nxdn48`, `nxdn96`, `provoice`, `fm`, `am`. Left out until the decoder knows |
| `decrypted` | The chunk holds voice the recorder decrypted with the system's keys (`voice.frames` only). Left out when false. Such chunks go only to a plugin the user allowed decrypted calls (`shareDecrypted`) |
| `time` | Unix seconds when the recorder handed it out; its data is from just before |
| `data` | `{"kind": "pcm", "rate": 22050, "pcm": "<base64>"}`: 16-bit mono PCM, little-endian, as `audio`'s. `{"kind": "frames", "codec": "imbe", "frames": ["<hex>", …]}`: vocoder frames, each its bits as `voice`'s. `{"kind": "none"}`: notes only |
| `notes` | What is known about items of the data: each has `at`, the sample or frame it is about counted from the chunk's first (`offset + at` in the stream), and its tap's fields beside it. Left out when empty |

In the SDK `chunk.data` is a `TapData` (`Pcm { rate, samples }`, `Frames { codec, frames }`, each
frame a list of 0s and 1s, or `None`), and `chunk.notes_as::<AirFrame>()` gives each note with its
place in the stream.

`AirFrame` (`voice.air`):

| Field | |
|---|---|
| `n` | The frame's place in its superframe. Left out when it isn't known |
| `errs`, `e0` | As a `voice` frame's |
| `erased` | Known lost: its bits mean nothing. Left out when false |
| `encrypted` | Whether the transmission is encrypted, once the air has said (a header, link control, an encryption sync, a VCALL). Left out until then |
| `algid`, `keyid`, `mi` | The encryption the frame is under, once the air has said: the algorithm, the key ID and the message indicator or initialization vector (hex) |

`n` and the encryption fields are those of the chunk's `voice_mode`:

| `voice_mode` | Frame | `n` | `algid`, `keyid`, `mi` |
|---|---|---|---|
| `p25` | IMBE, 88 bits | 0–17: LDU1's nine, then LDU2's | The encryption sync's ALGID, KID and MI (9 bytes) |
| `p25_phase2` | AMBE+2, 49 bits | 0–17: the four 4V bursts' frames, then the 2V's. Unknown after a lost burst | The same |
| `dmr` | AMBE+2, 49 bits | 0–17: bursts A–F, three each. Unknown for a transmission joined late | The privacy indicator header's algorithm, key ID and IV (4 bytes). Basic privacy sends none: only `encrypted` |
| `nxdn48`, `nxdn96` | AMBE+2, 49 bits | 0–15: four frames of four (at 9600 baud a FACCH1 takes two at a time). Unknown where the frame's SACCH didn't decode, and on Type-D | The VCALL's cipher type (1 scrambler, 2 DES, 3 AES) and key ID, and the VCALL_IV's IV (8 bytes) |
| `provoice` | IMBE, 88 bits | 0–3: the frame's four | Never: the air doesn't say |

NXDN full-rate voice (EFR) is another codeword and isn't tapped.

`FrameErrors` (`voice.frames`, `voice.errors`): `codec`, `errs`, `e0`, `erased`, `kind` and
`tone`, as a `voice` frame's. With its frame's bits it is that frame: `chunk.voice_frames()` gives
a `voice.frames` chunk's as `VoiceFrame`s, each with its place in the stream.

`voice.frames` carries what the [`voice`](#voice) topic does, from the channel instead of the
call: every transmission as it is heard, whether or not a call records it, with no call needed
(`call_id` when there is one). Take `voice` to follow calls; take the tap to study a channel.

Taps are live, like `audio`: a plugin the user delays calls for isn't sent them, and they aren't
queued for one that is catching up. Only `voice.frames` holds decrypted voice (marked `decrypted`, and kept from a plugin the user hasn't
allowed it); in `voice.air` encrypted voice comes as it was sent, whether or not the recorder has its key. Unlike `voice`, a tap doesn't follow the call's
recording rules: it is of the channel, so it comes for voice ahead of its grant and for a call the
recorder decodes but doesn't keep.

The user can save the same streams to files, a `.jsonl` sidecar holding what the notes hold: see
[`taps`](../../configuration/recording.md#taps). These topics are busy: hand each chunk to a thread
of your own straight away.

## `status`

Every site's state, every 5 seconds: an entry for each site, so a system of several sites has
several, with the same `index`, `system_id` and `short_name` and its own `site_name` and `site_id`.

```json
{
  "type": "status", "time": 1790771560.0,
  "systems": [
    { "index": 0, "system_id": 1, "short_name": "county", "site_name": "county", "site_id": 1,
      "key": "sys/county", "protocol": "p25", "control_channel_hz": 851012500, "source": 0,
      "identity": { "nac": 928, "wacn": 781824, "sysId": 939, "rfss": 1, "site": 5 },
      "decode_rate": 38.5, "active_calls": 3, "recording": 2 }
  ]
}
```

| Field | |
|---|---|
| `index`, `system_id`, `short_name` | [Its system](#systems-and-sites-in-events) |
| `site_name`, `site_id` | This site's name (a system of one site: its short name) and id |
| `key` | Where the site's series are in [`totals`](#series) |
| `protocol` | `p25`, `smartnet`, `edacs`, `dmr` or `nxdn` |
| `control_channel_hz` | The control channel being decoded; `null` while it searches |
| `source` | The radio it's heard on (`SourceScope::index`) |
| `symbol_rate` | The control channel's symbol rate, baud, when it counts the air its blocks account for (`cc/airSym`) |
| `identity` | What the site's control channel says it is, each field once it has: P25 `nac`, `wacn`, `sysId`, `rfss`, `site`; SmartNet `sysId` and `site` (when OBT sends it); DMR `colorCode`, Tier III also `network` and `site`; NXDN `ran`, Type-C also `sysId` and `site` |
| `decode_rate` | Control channel messages decoded per second. A healthy P25 control channel gives about 40 |
| `active_calls`, `recording` | Calls going on, and those being recorded |

## `totals`

Every series the recorder measures (the ones its dashboard charts), as running totals since
recording started, every 10 seconds, with the sites, conventional channels and radios they're of.
Since SDK 0.12.0.

```json
{
  "type": "totals", "time": 1790771560.0, "since": 1790771000.0,
  "series": {
    "sys/county/cc/good": { "sum": 21400, "s": 560, "now": 39 },
    "sys/county/calls/recorded": { "sum": 212, "s": 190 }
  },
  "sites": [{ "index": 0, "system_id": 1, "short_name": "county", "site_name": "county", "site_id": 1, "key": "sys/county", "protocol": "p25", "control_channel_hz": 851012500, "identity": {} }],
  "channels": [],
  "sources": [{ "key": "src/800", "index": 0, "name": "800", "driver": "airspy", "auto_tune": false }]
}
```

| Field | |
|---|---|
| `time` | Unix s |
| `since` | When recording started: the totals count from here. A later `since` than the last message's means recording restarted, and the totals with it |
| `series` | Each series by [key](#series), a `Total` |
| `sites` | Each trunked site, as `status` has it |
| `channels` | Each conventional frequency: its `key`, `index` and `short_name` (its system's), `freq_hz`, `mode` (`fm`, `am`, `p25`, `dmr`, `nxdn48`, `nxdn96`), its rows' `names`, the code its rows record by when they share one (`access`: `CTCSS 151.4`, `DCS 023N`, `NAC 293`, `CC 1`), and its `source` |
| `sources` | Each radio: its `key`, `index`, `name` (its label in Setup), `driver` (`airspy`, `hackrf`, `rtlsdr`, …) and whether AutoTune corrects it (`auto_tune`) |
| `clock` | When it's there: how the recorder's clock is kept (`sync`: `ntp`, `gps`, `ptp`, `none`, `unknown`) and how far it is from a P25 system's broadcast time (`system_offset_ms`) |

A `Total` is a counter's count (`sum`) and the seconds in which it counted any (`s`), or a gauge's
value times the seconds it held it (`sum`) and those seconds (`s`, so its mean is `sum / s`:
`Total::mean`). `now` is its value in the latest tick. Take the difference of two messages for a
window of your own: `totals.since(Some(&earlier))`.

The SDK's `health` module makes node health reports of these: see
[Node health reports](uploading.md#node-health-reports).

### Series

Counts are kept where they happen, at the finest grain: each site's copies of calls (a call heard
on two sites counts at both) and each conventional frequency's. A system's figures are the sum of
its sites' and channels'.

| Key | What |
|---|---|
| `<site>/cc/{good,bad,airSym,syncLossSym}` | Control channel blocks decoded and lost; the symbols of air the good ones account for, and of time without frame sync (P25; `SiteScope::symbol_rate`) |
| `<site>/cc/{locked,snr,phaseErr,signal,noise}` | Gauges: decoding (1/0), its level over the floor beside it, its symbols' phase error |
| `<site>/calls/{started,recorded}`, `<site>/calls/why/<reason>` | Each copy of a call, when it ends, by what finally became of it: `ignored`, `unknownTg`, `encrypted`, `noSource`, `noRecorder`, `noFrequency` |
| `<site>/voice/<imbe\|ambe>/{frames,bad,errors}` | The vocoder frames of every copy recorded (saved or not), by codec |
| `<channel>/{calls,voice}/…` | As for a site, with `calls/skipped`: transmissions no row took |
| `<channel>/{open,signal,closed,noise,snr}` | Gauges: open (1/0), its level while open and while closed, the floor beside it, open over the floor |
| `src/<label>/{samples,dropped,clipPct,ppm,tune}` | Each radio: samples and samples lost (their `s`: seconds running, and seconds losing any), clipping, its frequency error and AutoTune's correction |
| `sys/<short name>/{calls,airMs,audioBytes,notSaved,dup}` | Each system's calls (once each, however many sites heard them) |

`<site>` is the site's `key` (`sys/<short name>`, or `sys/<short name>/site/<site>` for a system
of several), `<channel>` a channel's (`sys/<short name>/ch/<Hz>`), and `src/<label>` a radio's. A
series appears once it has counted something.

## `tone_pattern`

A call's in-band tones made a pattern, as the call JSON's `tone_patterns` will list it, sent as
soon as it's over (a second after its last tone), whether or not it's one of the system's
[signals](../../configuration/systems.md#signals). The pattern's fields are in the event itself,
beside the call's.

```json
{
  "type": "tone_pattern", "short_name": "county", "system_id": 1, "site": "county", "call_num": 412,
  "talkgroup": 101, "alpha_tag": "Fire Dispatch", "src_tag": "Station 1", "time": 1790771552.0,
  "pattern": "two_tone", "tones": [849.63, 1292.85], "lengths": [1.0, 3.04],
  "format": "Quick Call II", "pos": 0.4, "len": 4.04, "src": 1116707, "origin": "frame",
  "text": "two-tone 849.63 → 1292.85 Hz, 1 + 3.04 s (Quick Call II)", "signals": ["FD_PAGE"]
}
```

| Field | |
|---|---|
| `short_name`, `system_id`, `site` | [Its system and site](#systems-and-sites-in-events) |
| `call_num`, `talkgroup`, `alpha_tag` | The call it's on |
| `src`, `src_tag` | The radio talking when it began (0: not known), and its name |
| `decrypted` | Heard in voice decrypted with the system's keys: only plugins allowed decrypted calls (`shareDecrypted`) get those |
| `time` | Unix s |
| `pattern` | `two_tone` (a page: tone A then B), `long` (one tone, a second or more), `hi_low` (a warble), `pulsed`, `digits` (DTMF or KNOX), `morse` (a CW ID), `sequence` (a run of short tones) or `beep` |
| `tones`, `lengths` | Its tones, Hz, and their lengths, s (two-tone: A and B; hi-low: lower first; pulsed lengths: on then off; Morse: its dot) |
| `digits`, `knox` | Digits: the string, and `knox: true` for KNOX tones (DTMF otherwise). Morse: the text |
| `cycles` | Hi-low: its tones; pulsed: its pulses |
| `format` | A two-tone page's format, by its timing (`Quick Call II`, …) |
| `pos`, `len` | Seconds into the call's audio so far, and how long |
| `origin` | `frame` (named by the vocoder's tone frames) or `audio` (found in the audio) |
| `text` | The pattern for a person |
| `signals` | The ids of the system's signals it is |

The SDK has the pattern's fields in `TonePatternEvent::pattern`.

## `log`

Each line the recorder logs, at the level its log is set to. Since SDK 0.10.0.

```json
{ "type": "log", "time": 1790771552.1, "level": "info", "system": "county", "text": "[county]\t412C\tTG: 101 …", "line": "[2026-09-30 14:02:11.123456] (info)   [county]\t412C\tTG: 101 …" }
```

| Field | |
|---|---|
| `time` | Unix s |
| `level` | `trace`, `debug`, `info`, `warning`, `error` or `fatal` |
| `system` | The system's short name, when the line is about one |
| `text` | The line after its time and level |
| `line` | The whole line as the console prints it, without colour |

It's for passing the log on (to MQTT, a chat). A plugin isn't sent its own lines. Don't log
something for each line you get: two plugins that pass lines on would keep each other busy.

## Systems and sites a plugin is off for

The user can turn a plugin off for a system, in Setup → Plugins, or for one site of a system, on
the system's card. Nothing about a system it's off for reaches it: it isn't in `hello`, its events
and live media aren't sent, `status` leaves it out, `totals` leaves out its sites, channels and
their series (the radios', shared by systems, stay), `log` lines about it aren't sent (lines about
no one system still are), and `calls.query` doesn't find its calls.

A site it's off for isn't among its system's `sites` in `hello`, and its unit events, `status`
entry and `totals` site and series aren't sent (the system's own series, of all its sites, still
are), nor a recorded call only it heard (a call another site heard as well still comes). Calls as
they go (`call.start`, `call.update`, `call.end`, live audio) are the system's, and still come.

There's nothing to do in the plugin. Just don't count on hearing of every system or site.

## Order and loss

Events of one topic arrive in the order they happened. `call.concluded` comes after `call.end`,
sometimes a moment later when M4A is being made. The user can delay a plugin's calls
(`pluginDelays`, by talkgroup; see [Delays](../../configuration/plugins.md#delays)): its call
events then come that much later, and its live audio and voice not at all.

The recorder queues 1024 messages for each plugin. While a plugin's queue is full, new messages
are dropped, counted, and logged (`is falling behind: N event(s) dropped so far`), except a call's
`call.start` and `call.end` and the answers to what the plugin asked (`calls`, `settings.saved`):
those wait, in order, until there's room. A call that starts and ends while the plugin is behind
isn't told at all. Messages queued while a plugin restarts are delivered to the new process, after
its `hello`.

Still, time out calls you track: a plugin restarted in the middle of a call gets its `call.end`
without its `call.start`.

## Topics may grow

New fields and new topics may appear in later versions of the recorder. Ignore what you don't
know; the SDK does this for you.
