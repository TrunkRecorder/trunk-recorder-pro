# Captures

A capture is a recording of the raw radio signal (IQ samples): what the radios received, not the
audio. It can be replayed later, through this version or a newer one, and it is the most useful
thing to send with a decoding problem. This page covers the **IQ capture** card, sending a capture
to the public corpus, and rules that capture by themselves when something goes wrong. The card
works while recording; `trunk-pro capture` on the command line needs recording stopped (see
[Making a capture](../troubleshooting.md#making-a-capture)).

## Capture what TRP receives

The **IQ capture** card at the bottom of the **RF** page records what TRP is receiving, to replay
later or to send in when something isn't working. Pick what to capture, say whether it shows **A problem** or
**Everything working**, add a note if you like, and press the button. Recording carries on as usual. Nothing is sent anywhere.

| Capture | What it records | Use it for |
|---|---|---|
| **Control channel** | One site's control channel, for 30 or 60 seconds from when you press it (a DMR site: each of its frequencies) | Calls that aren't granted or followed, a control channel that decodes badly. A few MB. |
| **Next call** | The voice channel of the next call to begin on a system, or on one talkgroup, from start to end | Calls that sound bad, are silent or go missing |
| **Every radio** | Every radio, for 30, 60 or 120 seconds | A problem that isn't one channel's, or a known-good sample of your system to test new versions against |

A capture that waits on the air (a control channel's time, or the next call) is listed under the button with how it's
going, and **Cancel** gives it up. A channel capture is replayed with `trunk-pro tool channel <folder>`.

Each capture is a folder in `iq-captures/` under the capture directory, or in the folder you choose under **Save captures
in** (`diagnostics.captureDir` in the config), in [SigMF](https://sigmf.org/): one recording per radio, or the channel's. Use a fast disk:
several radios can write tens of MB per second, and a busy disk can slow your recordings too. It takes as much disk as the radios' samples: 288 MB a minute for an
RTL-SDR at 2.4 MS/s, and 2.4 GB a minute for a radio at 10 MS/s. A capture won't start unless it would
leave 1 GB free.

The capture's metadata has what it takes to decode it: the systems' frequencies and band plans, and
how the radios were set ([the fields](../sigmf/trp.sigmf-ext.md)). It leaves out names, unit IDs,
encryption keys, plugin settings, serial numbers and paths. Radio traffic in the samples themselves
can still be decoded by anyone who has the files.

Beside the recordings, `diagnostics.json` describes the computer and how the radios were doing: operating
system, CPU, memory, each radio's settings and dropped samples, the shape of the config (how many systems and
channels), and recent drops, clipping and clock problems. For each second of the capture it also gives each radio's lost
samples, how full its write queue got and its slowest write, with what the disk was doing and whether it's an HDD or SSD. It
has no names, frequencies, paths or addresses.

To play one back, with the systems it was made with:

```bash
trunk-pro replay ~/TrunkRecorderPro/iq-captures/20261108-140311 --out replayed
```

A capture ends early, and isn't kept, if recording stops, the radio is retuned, or the disk can't keep
up. The card says which. Samples the radio lost are kept as gaps and shown in the list, and a capture
with any isn't a good reference.

## Sending a capture to the public corpus

**Review and send…** on a capture shows what would be published: its waterfall,
metadata and diagnostics. Nothing leaves your computer until you agree. The recording and its diagnostics then become
public for anyone to download and decode, including all the radio traffic it holds. **Withdraw…** removes it, but
copies others made can't be recalled. A send picks up where it stopped if the app restarts.

## Capture rules

For a problem that comes and goes, **Capture automatically when something goes wrong**, on the same card, keeps a recording when it happens, within a daily limit:
20 captures and 5 GB by default. Changes apply the next time recording starts, and nothing is sent anywhere.

The presets cover the common problems. You can add your own as JSON, under `diagnostics.rules` in the config:

```json
{
  "id": "weak-calls",
  "enabled": true,
  "on": "call.end",
  "systems": ["county"],
  "if": "call.call_length > 3.0 && !call.encrypted && call.clean_voice_pct < 70.0",
  "repeat": { "count": 3, "within": "5m", "by": "call.freq" },
  "capture": [{ "tier": "voice" }, { "tier": "control" }],
  "limit": { "cooldown": "30m" }
}
```

| Field | What it does |
|---|---|
| `on` | The event it's checked on (below) |
| `systems` | Only these systems (left out: any) |
| `if` | A condition in [CEL](https://github.com/google/cel-spec) on what's known of the event (left out: always) |
| `repeat` | Only after `count` matches `within` a window, counted apart for each value of `by` |
| `every` | A `schedule` rule: how often (`24h`) |
| `capture` | What to keep: `tier` is `wideband` (every radio), `control` (the last minute of a control channel) or `voice` (the call's channel). A wideband capture can have `before` and `after` (`5s`); `before` needs memory while recording runs |
| `limit` | `cooldown`: how long before it can fire again |

| `on` | What a condition can name |
|---|---|
| `call.end`, `call.not_saved` | `call.system`, `site`, `talkgroup`, `freq`, `start_time`, `stop_time` (Unix s), `call_length`, `clean_voice_pct`, `snr`, `signal`, `noise`, `freq_error`, `encrypted`, `decrypted`, `emergency`, `analog`, `voice` (`p25`, `dmr`, `fm`, …), `tdma_slot`, `units` (how many radios), `unit_ids`, `recorder`, `source`; `call.not_saved` also `not_saved` (`silent` or `short`), `audio_s`, `frames`, `bad_frames` |
| `control.lost`, `control.regained` | `control.system`, `site`, `freq_hz` |
| `source.clipping`, `source.drops`, `source.quiet` | `source.index`, `pct`, `per_s`, `peak_db` (quiet: the last minute's peaks, dBFS) |
| `note.warning` | `note.system`, `site`, `text` |
| `tick` (every 10 s, each site) | `site.system`, `name`, `good`, `bad`, `failed_pct`, `messages` |
| `schedule` | nothing |

A wideband capture a rule made says what set it off (`trp:trigger`: the event's facts, a note's text aside, and
when, seconds into the capture) and what the recording was doing then (`trp:state`: recorders in use, each DMR / NXDN
site's channel table): see [the `trp` extension](../sigmf/trp.sigmf-ext.md).

A rule that can't run is refused when it's saved. `budget` beside `rules` sets `capturesPerDay`, `mbPerDay` and
`ramMb`, the memory held for looking back.
