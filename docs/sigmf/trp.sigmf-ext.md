# The `trp` SigMF extension

Version 0.1.0. Used by the wideband captures Trunk Recorder Pro makes
([Capture a known-good sample](../guides/recordings.md#capture-a-known-good-sample)). It's declared
in `core:extensions` as `{ "name": "trp", "version": "0.1.0", "optional": true }`, so any SigMF tool
can open the recording and ignore these fields.

## Global

| Name | Type | What it says |
|---|---|---|
| `trp:tier` | string | What the file holds: `wideband` |
| `trp:purpose` | string | Why it was made: `healthy`, a recording of a system that was working well |
| `trp:app` | object | The recorder that made it: `version` |
| `trp:source` | object | How the radio was set: `type`, `rateHz`, `ppm`, `autoTune`, `guardHz` and its gain settings. No serial number or device address |
| `trp:start_unix_ms` | double | When the first sample was received, Unix milliseconds (radios of a collection start together by it) |
| `trp:config` | object | A Trunk Recorder Pro config for this capture's files: its systems and conventional channels and what decides which calls are recorded. No names, keys, plugin settings or paths |
| `trp:talkgroups` | object | By system short name, talkgroups that aren't plain digital, or are ignored: `number`, `mode`, `ignore`. No names |
| `trp:bandplans` | array of strings | Each site's learned band plan, in site order |
| `trp:live` | object | How decoding went while it was made: `calls`, and for each of `sites` its identity (`nac`, `wacn`, `sysid`, `rfss`, `site_id`), `modulation`, `control_channel_hz`, `messages_good`, `messages_bad` and `calls` |

## Captures

Samples lost while recording show as a jump in `core:global_index` from one segment to the next.
A recording that changed frequency isn't saved.

## Collections

A capture's folder has one recording per radio, `source-N.sigmf-meta` and `.sigmf-data`, and a
`capture.sigmf-collection` listing them.
