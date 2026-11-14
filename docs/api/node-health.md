# Node health reports

**Version 1, draft.** A common way for a recorder to tell a call-collecting service (Broadcastify
Calls, OpenMHz, Rdio Scanner, …) how well it is hearing what it uploads, so that a service fed by
several recorders for the same system or channel can prefer the one hearing it best, and can tell
a recorder that is up but hearing nothing from one that is down.

The report is about reception, not uploads: a service already sees what arrives. It is the same
for every service; a service adds only where it goes and how it's authorised (its
[binding](#bindings)).

## Terms

- **Recorder:** the program that receives and records (Trunk Recorder Pro, Trunk Recorder,
  SDRTrunk, …), on one computer.
- **Node:** what a service knows one feed by. On Broadcastify Calls it is a system ID with its API
  key; a recorder usually feeds several, often one per radio system. A node is fed by one or more
  **sites** (trunked) or **channels** (conventional) of the recorder.
- **Window:** the stretch of time a report covers, `[start, end)`.

## Principles

1. **Counts, not scores.** A report carries what happened in its window as counts and seconds.
   The service computes rates and scores, and can change how it weighs them without a recorder
   release. Two windows' counts add up; two windows' percentages don't.
2. **A small core every recorder can fill**, and optional parts for what only some measure.
   A field a recorder doesn't measure is left out, never sent as 0.
3. **Say what is heard.** Each site states the identity its control channel broadcasts, so the
   service compares nodes that really hear the same site (or simulcast).
4. **Nothing about people.** No talkgroups, radio IDs, aliases or audio: totals only.

## When to send

| | |
|---|---|
| Interval | 300 s, unless the service says otherwise (`nextS`, below) |
| Phase | A random offset in `[0, interval)`, drawn once per node when the recorder starts. Reports go at that offset in each interval |
| Jitter | Each send moves by a random amount in `[-15, +15]` s |
| First report | The first send time at least 60 s after recording starts |
| Failure | No retry. The next report covers both windows (its `start` is the last delivered report's `end`), up to 900 s; anything older is dropped and the window starts later (`gap: true`) |
| `429` or `503` | Wait `Retry-After` if given, else double the interval (at most 3600 s) until one is accepted |

The random phase, chosen per node, spreads a service's load evenly over the interval: N nodes make
about N / 300 requests a second at any moment, not N requests every five minutes. Drawing it at
start (not from the clock) keeps recorders that restart together, after a power cut, from
reporting together.

When recording stops, a recorder may send what it has (the window up to then, `final: true`).

## The report

One JSON object per node, `POST`ed with `Content-Type: application/json`. Sizes are a few hundred
bytes to a few kilobytes.

```json
{
  "v": 1,
  "node": { "systemId": 1234 },
  "recorder": { "name": "trunk-recorder-pro", "version": "0.3.8", "upS": 86400 },
  "window": { "start": 1794590100.0, "end": 1794590400.0, "seq": 412 },
  "sites": [
    {
      "name": "Downtown",
      "protocol": "p25",
      "id": { "wacn": 781824, "sysId": 939, "rfss": 1, "site": 5, "nac": 928 },
      "cc": { "hz": 854787500, "lockedS": 300, "good": 11200, "bad": 410,
              "airSym": 1180000, "symbolRate": 4800, "snrDb": 15.0, "phaseErrDeg": 17.2 },
      "calls": { "started": 812, "recorded": 790,
                 "notRecorded": { "encrypted": 0, "noSource": 19, "noRecorder": 3 } },
      "voice": { "imbe": { "frames": 0, "bad": 0, "errors": 0 },
                 "ambe": { "frames": 9167, "bad": 862, "errors": 4120 } }
    }
  ],
  "radios": [ { "name": "800", "kind": "airspy", "seconds": 300, "droppedS": 0, "clipPct": 0.0,
                "ppmError": -0.03 } ],
  "clock": { "sync": "ntp", "systemOffsetMs": 12 }
}
```

A conventional node has `channels` instead of `sites` ([below](#conventional-channels)). A node
fed by both has both.

### Top level

| Field | Core | Type | Meaning |
|---|:-:|---|---|
| `v` | ✓ | int | This format's version: `1` |
| `node` | ✓ | object | Which node: the service's fields ([bindings](#bindings)) |
| `recorder.name` | ✓ | string | The recorder program (`trunk-recorder-pro`, `trunk-recorder`, `sdrtrunk`) |
| `recorder.version` | ✓ | string | Its version |
| `recorder.upS` | | number | Seconds since recording started |
| `window.start`, `window.end` | ✓ | number | Unix seconds. Counts are over `[start, end)` |
| `window.seq` | ✓ | int | Counts up by one per report of this node since the recorder started (0 first). A service seeing it fall knows the recorder restarted |
| `window.gap` | | bool | Some time before `start` was never reported (sends failed for over 900 s) |
| `window.final` | | bool | Recording stopped at `end` |
| `sites` | ✓ trunked | array | One per site feeding the node |
| `channels` | ✓ conventional | array | One per channel feeding the node |
| `radios` | | array | The receivers those sites and channels are heard on |
| `clock` | | object | How right the recorder's clock is |

### Trunked sites

| Field | Core | Type | Meaning |
|---|:-:|---|---|
| `name` | ✓ | string | The recorder's name for the site |
| `protocol` | ✓ | string | `p25`, `smartnet`, `dmr`, `nxdn`, `edacs` |
| `id` | | object | What its control channel says it is, as numbers: P25 `wacn`, `sysId`, `rfss`, `site`, `nac`; DMR `network`, `site`, `colorCode`; NXDN `system`, `site`, `ran`; SmartNet `sysId`, `site`. Only what has been heard |
| `cc.hz` | ✓ | int | The control channel it is on at `end` (absent: hunting) |
| `cc.lockedS` | ✓ | number | Seconds of the window it was decoding its control channel: a good block within the last 2 s |
| `cc.good` | ✓ | int | Control channel blocks decoded: the protocol's unit of control message (P25 TSBKs, SmartNet OSWs, NXDN CAC frames, …) |
| `cc.bad` | | int | Blocks it framed but couldn't decode (failed CRC or FEC) |
| `cc.airSym`, `cc.symbolRate` | | int | Symbols of air the good blocks account for, and the channel's symbol rate. `airSym / (symbolRate × window)` is the share of the control channel decoded, comparable across sites and protocols |
| `cc.snrDb` | | number | Mean level of the control channel over the noise floor beside it, dB, over the seconds locked |
| `cc.phaseErrDeg` | | number | Mean RMS phase error of its symbols, degrees, over the seconds locked (PSK/C4FM) |
| `calls.started` | ✓ | int | Calls (voice grants) the control channel announced |
| `calls.recorded` | ✓ | int | Of those, calls recorded |
| `calls.notRecorded` | | object | Of those, calls not recorded, by why: `ignored` (the user chose not to), `unknownTg` (not in the talkgroup file, and only those are recorded), `encrypted`, `noSource` (no receiver covers the frequency), `noRecorder` (every recorder busy). The last two are calls the node missed |
| `voice.imbe`, `voice.ambe` | | object | Vocoder frames of recorded calls, by codec: IMBE (P25 Phase 1, 20 ms) and AMBE+2 half rate (P25 Phase 2, DMR, NXDN, 20 ms) |
| `voice.*.frames` | | int | Frames received |
| `voice.*.bad` | | int | Frames too damaged to play: those FEC couldn't correct and the vocoder repeated or muted |
| `voice.*.errors` | | int | Bit errors FEC corrected |

`calls.started − calls.recorded − Σ notRecorded` is calls not recorded for other reasons (none, or
a reason this version doesn't name).

A site that is a node's on its own (Broadcastify can take one site of a system as its own system
ID) is reported to that node alone.

### Conventional channels

A conventional channel has no control channel to prove it's heard, so a report says three things
of each: that the receiver is alive and steady, how often the channel opened and whether what
opened it was the transmitter the node is for, and how clean that was.

| Field | Core | Type | Meaning |
|---|:-:|---|---|
| `hz` | ✓ | int | The channel's frequency |
| `mode` | ✓ | string | `fm`, `am`, `p25`, `dmr`, `nxdn` |
| `name` | | string | The recorder's name for it |
| `access` | | string | The code it records by, when it has one: `CTCSS 151.4`, `DCS 023N`, `NAC 293`, `CC 1` |
| `heardS` | ✓ | number | Seconds of the window its receiver was delivering samples without drops |
| `noiseDb` | ✓ | number | The channel's mean level while closed, dBFS. Steady from window to window means the receiver and antenna are as they were; a fall of several dB to the radio's own floor suggests a lost antenna |
| `opens` | ✓ | int | Times the channel opened (squelch, or a digital frame sync) |
| `openS` | ✓ | number | Seconds open, to the second: a recorder may count its hang time after each transmission |
| `calls` | ✓ | int | Calls recorded |
| `openedByOther` | | int | Openings that carried another code than `access`, or none when one is required: someone else, interference, or the squelch falsing |
| `signalDb` | | number | Mean level while open, dBFS |
| `snrDb` | | number | Mean level while open over the noise floor under the channel, dB |
| `voice` | | object | Digital channels: as for sites |

Analog FM gives the weakest evidence of the lot: a quiet channel and a deaf receiver look much
alike, except in `noiseDb`. A service choosing between nodes for one conventional channel should
lean on what it sees itself as well: which node delivers transmissions the others miss, and
`snrDb`.

### Radios

| Field | Type | Meaning |
|---|---|---|
| `name`, `kind` | string | The recorder's name for it; `airspy`, `hackrf`, `rtlsdr`, `sdrplay`, `usrp`, … |
| `seconds` | number | Seconds of the window it was running |
| `droppedS` | number | Seconds in which it lost samples (USB too slow, a CPU too busy) |
| `clipPct` | number | Share of samples at full scale, % |
| `ppmError` | number | Its frequency error left after correction, ppm, as measured on signals of known frequency |

### Clock

| Field | Type | Meaning |
|---|---|---|
| `sync` | string | `ntp`, `ptp`, `gps`, `none`, `unknown` |
| `systemOffsetMs` | number | The recorder's clock less the time a P25 system broadcasts (SYNC_BCST), ms, when heard. Services judge late calls by the recorder's clock, so a wrong clock looks like a slow recorder |

## The answer

`200` with a JSON object; all fields optional.

| Field | Meaning |
|---|---|
| `nextS` | Send the next report this many seconds after this one (60 to 3600); the phase moves with it |
| `message` | Something for the recorder to show its user ("This node is standby for 12 talkgroups: node 4411 hears Downtown better") |
| `role` | What the service makes of the node, for display: free-form |

`400` with `{ "error": "…" }` means the report is wrong: the recorder logs it and keeps sending.
`401`/`403` mean the node's credentials are wrong: the recorder says so and stops reporting that
node until its settings change.

## For services: reading a report (informative)

From one report, or several added up:

| | |
|---|---|
| Alive | A report within the last `2 × interval + 60` s, whether or not there were calls |
| Control channel held | `cc.lockedS / window` |
| Control channel decoded | `cc.good / (cc.good + cc.bad)`; with `airSym`, `airSym / (symbolRate × lockedS)` |
| Calls missed | `(noSource + noRecorder) / calls.started` |
| Voice lost | `voice.bad / voice.frames`, per codec |
| Conventional receiver steady | `heardS / window` and `noiseDb` against the node's own past |

Compare nodes that report the same site `id` (or, conventional, the same `hz` and `access`).
Prefer the one ahead over several reports, not one: these figures move minute to minute on a busy
or simulcast system. Check what nodes report against what they deliver: a node claiming every
call but delivering fewer than another is wrong somewhere.

## Bindings

### Broadcastify Calls

| | |
|---|---|
| Endpoint | `POST https://api.broadcastify.com/call-health` (proposed) |
| `node` | `{ "systemId": 2898, "apiKey": "…" }`: as uploads send them |
| Nodes | One report per system ID. A recorder system uploaded as one system ID is one node; a site uploaded as a system ID of its own is a node of its own |

## In Trunk Recorder Pro

The recorder sends plugins that subscribe to `health` its running totals every 10 s
([Writing plugins](../plugins/writing-plugins.md#topics)). The plugin SDK's `health` module keeps
them (`History`), schedules each node (`Reporter`: the random phase, jitter, carried windows and
backing off above) and makes its report (`report`), so every plugin reports the same figures the
same way. A plugin adds the HTTP and its service's `node` fields.

## Versions

A recorder sends `v`; a service reads the fields it knows and ignores the rest. New optional fields
don't change `v`; a change in a field's meaning does.
