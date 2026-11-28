# The `trp` SigMF extension

Version 0.1.0. Used by the wideband captures Trunk Recorder Pro makes
([Capture what TRP receives](../guides/captures.md#capture-what-trp-receives)). It's declared
in `core:extensions` as `{ "name": "trp", "version": "0.1.0", "optional": true }`, so any SigMF tool
can open the recording and ignore these fields.

## Global

| Name | Type | What it says |
|---|---|---|
| `trp:tier` | string | What the file holds: `wideband`, `control` or `voice` |
| `trp:purpose` | string | Why it was made: by hand, `bug` (showing a problem) or `healthy` (a system working well); or `rule` |
| `trp:rule` | object | The rule that made it: `id`, `on`, `if` |
| `trp:channel` | object | A channel tape's (`VoiceTape` / `ControlTape` as serde writes them, the IQ and marks aside): for `voice`, `freq_hz`, how its decoder is built (`spec`: `voice::VoiceSpec` without keys), `first` and `block` (its push sizes) and `truncated`; for `control`, `site`, `carrier`, `freq_hz`, `rate`, `block`, `t0_s` and `bandplan` |
| `trp:app` | object | The recorder that made it: `version` |
| `trp:source` | object | How the radio was set: `type`, `rateHz`, `ppm`, `autoTune`, `guardHz` and its gain settings. No serial number or device address |
| `trp:start_unix_ms` | double | When the first sample was received, Unix milliseconds (radios of a collection start together by it) |
| `trp:config` | object | A Trunk Recorder Pro config for this capture's files: its systems and conventional channels and what decides which calls are recorded. No names, keys, plugin settings or paths |
| `trp:talkgroups` | object | By system short name, talkgroups that aren't plain digital, or are ignored: `number`, `mode`, `ignore`. No names |
| `trp:bandplans` | array of strings | Each site's learned band plan, in site order |
| `trp:live` | object | How decoding went while it was made, from `from_s` to `to_s` (seconds from where a replay starts: the moment the last radio's first sample came; with a look-back, about all of it): `calls`, and for each of `sites` its `system` and `site` names, its identity (`nac`, `wacn`, `sysid`, `rfss`, `site_id`), `modulation`, `control_channel_hz`, `messages_good`, `messages_bad` (control channel blocks decoded and lost: a replay's "CC N good / M bad") and `calls` |
| `trp:trigger` | object | A rule's capture: what set it off. `on` (the event), `at_unix_ms` and `at_s` (seconds from where a replay starts; negative: before its first sample), and the event's facts as the rule's condition saw them: `call` (`system`, `site`, `talkgroup`, `freq`, `start_time`, `stop_time`, `call_length`, `clean_voice_pct`, `snr`, `signal`, `noise`, `freq_error`, `encrypted`, `decrypted`, `emergency`, `analog`, `voice`, `tdma_slot`, `units`, `unit_ids`, `not_saved` — `silent`, `short` or `encrypted` —, `audio_s`, `frames`, `bad_frames`, `recorder`, `source`), `control`, `source`, `site`, or `note` (its `system` and `site`, not its text) |
| `trp:state` | object | What the recording was doing when the capture was asked for, which a replay can't tell: at `at_s`, `active_calls`, `recording` (recorders in use), `channels_open`, and for each of `sites` its `active_calls` and `recording`, and a DMR site's `dmr` or an NXDN site's `nxdn` (channel table and carriers as learned so far, talkgroups and radios by number) |

## Captures

Samples lost while recording show as a jump in `core:global_index` from one segment to the next.
A recording that changed frequency isn't saved.

## Annotations

On a voice tape, what its decoder was told and when: `trp:voice_params` (`tdma_key`, `nac`, `dmr`: the DMR variant, null when none), `trp:slots` (which
slots were being listened to, not which talkgroups) and `trp:squelch`.

## Collections

A capture's folder has one recording per radio, `source-N.sigmf-meta` and `.sigmf-data`, and a
`capture.sigmf-collection` listing them.
