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
  SDRTrunk, SDRTrunk VCE, …), on one computer.
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
fed by both has both. [Examples](#examples) shows reports as different recorders send them.

### What a node's report covers

- **Whole sites and channels.** A node's report has every site and conventional channel that
  feeds it any calls, and each one's counts are for all it heard, not only the talkgroups that go
  to this node. Recorders choose what each node gets by talkgroup (allow and deny lists, aliases),
  so one site often feeds several nodes: it is in each of their reports, with the same counts.
- **When the recorder can't tell exactly.** A recorder that can only work out which sites could
  feed a node (one whose talkgroups could be routed to it) reports those. It never leaves out a
  site that does feed the node, because a deaf site is what a report must show.
- **Several receivers on one site.** A recorder that decodes one site with more than one receiver
  (each its own control channel decoder, the best copy of each call kept) sends an entry per
  receiver, each with its `receiver` name and the same `id`. One that combines them sends one
  entry. Services compare entries by `id`.

### Top level

| Field | Core | Type | Meaning |
|---|:-:|---|---|
| `v` | ✓ | int | This format's version: `1` |
| `node` | ✓ | object | Which node: the service's fields ([bindings](#bindings)) |
| `recorder.name` | ✓ | string | The recorder program, lower case: `trunk-recorder-pro`, `trunk-recorder`, `sdrtrunk`, `sdrtrunk-vce`. A fork that measures differently uses a name of its own |
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
| `protocol` | ✓ | string | `p25`, `smartnet`, `edacs`, `dmr`, `nxdn`, `mpt1327`, `ltr`, `ltrnet`, `passport` |
| `receiver` | | string | Which of the recorder's receivers this entry is, when it decodes the site with more than one ([above](#what-a-nodes-report-covers)) |
| `id` | | object | What its control channel says it is, as numbers: P25 `wacn`, `sysId`, `rfss`, `site`, `nac`; DMR `network`, `site`, `colorCode`; NXDN `system`, `site`, `ran`; SmartNet `sysId`, `site`; MPT1327 `sysId`. Only what has been heard |
| `cc.hz` | ✓ | int | The control channel it is on at `end` (absent: hunting). On a system with no fixed control channel (DMR Capacity Plus, Hytera XPT), the rest channel it is following; `cc` then counts the rest channel's signalling |
| `cc.lockedS` | ✓ | number | Seconds of the window it was decoding its control channel: a good block within the last 2 s |
| `cc.good` | ✓ | int | Control channel blocks decoded, in the protocol's unit of control message: P25 Phase 1 TSBKs, P25 Phase 2 MAC messages, SmartNet and EDACS OSWs, DMR CSBKs, NXDN CAC frames, MPT1327 codewords, LTR and Passport data words |
| `cc.bad` | | int | Blocks it framed but couldn't decode (failed CRC or FEC). Not time without sync: that's `syncLossSym` |
| `cc.syncLossSym` | | int | Symbols of the window, while running, the decoder had no frame sync on |
| `cc.airSym`, `cc.symbolRate` | | int | Symbols of air the good blocks account for, and the channel's symbol rate in baud (4800 for P25 Phase 1 and DMR, 6000 for a P25 Phase 2 TDMA control channel, 3600 for SmartNet, 1200 for MPT1327). `airSym / (symbolRate × window)` is the share of the control channel decoded, comparable across sites and protocols |
| `cc.snrDb` | | number | Mean level of the control channel over the noise floor beside it, dB, over the seconds locked |
| `cc.merDb` | | number | Modulation error ratio of its symbols, dB, over the seconds locked: the power of the ideal symbol points over that of the symbols' distance from them. A recorder that can't measure `snrDb` can send this; both may be sent |
| `cc.phaseErrDeg` | | number | Mean RMS phase error of its symbols, degrees, over the seconds locked (PSK/C4FM) |
| `calls.started` | ✓ | int | Calls the control channel announced: voice grants, and data grants for a recorder that counts them as calls (then `data` says how many it didn't follow) |
| `calls.recorded` | ✓ | int | Of those, calls a recorder was assigned to and followed, whether or not they then went to any service |
| `calls.notRecorded` | | object | Of those, calls not recorded, by why: `ignored` (the user chose not to), `unknownTg` (not in the talkgroup file, and only those are recorded), `encrypted`, `data` (data calls, not followed), `noFrequency` (the grant's channel number couldn't be turned into a frequency: a channel map missing from the recorder's settings), `noSource` (no receiver covers the frequency), `noRecorder` (every recorder busy). The last three are calls the node missed |
| `voice.imbe`, `voice.ambe` | | object | Vocoder frames of recorded calls, by codec: IMBE (P25 Phase 1, 20 ms) and AMBE+2 half rate (P25 Phase 2, DMR, NXDN, 20 ms) |
| `voice.*.frames` | | int | Frames received |
| `voice.*.bad` | | int | Frames too damaged to play: those FEC couldn't correct and the vocoder repeated or muted |
| `voice.*.errors` | | int | Bit errors FEC corrected |
| `voice.*.missing` | | int | Frames the calls' lengths say should have come but never did (sync lost mid-call). Not in `frames` |

`calls.started − calls.recorded − Σ notRecorded` is calls not recorded for other reasons (none, or
a reason this version doesn't name).

A site that is a node's on its own (Broadcastify can take one site of a system as its own system
ID) is reported to that node alone.

Some recorders decode P25, DMR or NXDN the same way on a control channel as on a conventional
channel. Such a recorder reports a channel as a site once it has decoded control messages on it,
and as a conventional channel otherwise.

### Conventional channels

A conventional channel has no control channel to prove it's heard, so a report says three things
of each: that the receiver is alive and steady, how often the channel opened and whether what
opened it was the transmitter the node is for, and how clean that was.

| Field | Core | Type | Meaning |
|---|:-:|---|---|
| `hz` | ✓ | int | The channel's frequency |
| `mode` | ✓ | string | `fm`, `am`, `p25`, `dmr`, `nxdn` |
| `name` | | string | The recorder's name for it |
| `receiver` | | string | Which receiver this entry is, when the recorder hears the channel on more than one |
| `access` | | string | The code it records by, when it has one: `CTCSS 151.4`, `DCS 023N`, `NAC 293`, `CC 1` |
| `heardS` | ✓ | number | Seconds of the window its receiver was delivering samples without drops |
| `noiseDb` | | number | The channel's mean level while closed, dBFS. Steady from window to window means the receiver and antenna are as they were; a fall of several dB to the radio's own floor suggests a lost antenna. A recorder whose levels aren't referenced to full scale leaves `noiseDb` and `signalDb` out and sends `snrDb`, a ratio, alone |
| `opens` | ✓ | int | Times the channel opened (squelch, or a digital frame sync) |
| `openS` | ✓ | number | Seconds open, to the second: a recorder may count its hang time after each transmission |
| `calls` | ✓ | int | Calls recorded |
| `openedByOther` | | int | Openings that carried another code than `access`, or none when one is required: someone else, interference, or the squelch falsing |
| `signalDb` | | number | Mean level while open, dBFS |
| `snrDb` | | number | Mean level while open over the noise floor under the channel, dB |
| `voice` | | object | Digital channels: as for sites |

Analog FM gives the weakest evidence of the lot: a quiet channel and a deaf receiver look much
alike, except in `noiseDb`, so send it whenever the recorder can. A service choosing between nodes for one conventional channel should
lean on what it sees itself as well: which node delivers transmissions the others miss, and
`snrDb`.

### Radios

| Field | Type | Meaning |
|---|---|---|
| `name`, `kind` | string | The recorder's name for it; `airspy`, `hackrf`, `rtlsdr`, `sdrplay`, `usrp`, … |
| `seconds` | number | Seconds of the window it was running |
| `droppedS` | number | Seconds in which it lost any samples (USB too slow, a CPU too busy): a second with one lost buffer counts as a whole second |
| `clipPct` | number | Share of samples at full scale, % |
| `ppmError` | number | Its frequency error, ppm, as measured on signals of known frequency, against the correction it's configured with |
| `ppmApplied` | number | The correction the recorder applies by itself on top of the configured one (Trunk Recorder Pro's AutoTune), ppm; left out when it applies none. `ppmError − ppmApplied` is how far off it still tunes: a large `ppmError` with a `ppmApplied` close to it is a radio being corrected, not a mistuned one. A recorder whose automatic correction changes the radio's own setting (SDRTrunk's Auto-PPM) leaves it out: its `ppmError` is already what's left |

### Clock

| Field | Type | Meaning |
|---|---|---|
| `sync` | string | `ntp`, `ptp`, `gps`, `none`, `unknown` (the recorder can't tell; or leave it out) |
| `systemOffsetMs` | number | The recorder's clock less the time a P25 system broadcasts (SYNC_BCST), ms, when heard. Services judge late calls by the recorder's clock, so a wrong clock looks like a slow recorder |

## The answer

`200` with a JSON object; all fields optional.

| Field | Meaning |
|---|---|
| `nextS` | Send the next report this many seconds after this one (60 to 3600); the phase moves with it |
| `message` | Something for the recorder to show its user ("This node is standby for 12 talkgroups: node 4411 hears Downtown better") |
| `role` | What the service makes of the node, for display: free-form |

| Answer | The recorder |
|---|---|
| `200` with a JSON object | Counts the report delivered |
| `400` with `{ "error": "…" }` | The report is wrong: logs it and keeps sending; the window isn't sent again |
| `401`, `403` | The node's credentials are wrong: says so and stops reporting that node until its settings change |
| `429`, `503` | Busy, or reports turned off for now: waits as in [When to send](#when-to-send), then carries on. A service pauses every recorder with `503` and `Retry-After` |
| `404`, `405`, or a `2xx` that isn't a JSON object | The service doesn't take reports: says so once and stops sending to it until recording restarts |
| Anything else, or no answer | Not delivered: the next report carries the window |

## For services: reading a report (informative)

From one report, or several added up:

| | |
|---|---|
| Alive | A report within the last `2 × interval + 60` s, whether or not there were calls |
| Control channel held | `cc.lockedS / window` |
| Control channel decoded | `cc.good / (cc.good + cc.bad)`; with `airSym`, `airSym / (symbolRate × lockedS)`. Recorders count `bad` differently, so compare nodes on `airSym` where both send it |
| Calls missed | `(noFrequency + noSource + noRecorder) / calls.started` |
| Voice lost | `voice.bad / voice.frames`, per codec |
| Conventional receiver steady | `heardS / window` and `noiseDb` against the node's own past |

Compare nodes that report the same site `id` (or, conventional, the same `hz` and `access`), and
the entries of one node's receivers the same way.
Prefer the one ahead over several reports, not one: these figures move minute to minute on a busy
or simulcast system. Check what nodes report against what they deliver: a node claiming every
call but delivering fewer than another is wrong somewhere.

## Bindings

### Broadcastify Calls

| | |
|---|---|
| Endpoint | `POST https://api.broadcastify.com/call-health`: beside the upload server's `…/call-upload` |
| `node` | `{ "systemId": 2898, "apiKey": "…" }`: as uploads send them |
| Nodes | One report per system ID. A recorder system uploaded as one system ID is one node; a site uploaded as a system ID of its own is a node of its own |
| Off | Recorders send reports unless their user turns them off; Broadcastify uses them to choose between nodes and to tell their owners about problems |
| Validate | `POST …/call-health?validate=1` checks a report and answers its figures: `200 {"ok":true,"valid":true,"summary":{…}}`, or `400` saying what's wrong. No API key check, nothing stored, no rate limit: for recorder developers to try their reports |

## In Trunk Recorder Pro

The recorder sends plugins that subscribe to `health` its running totals every 10 s
([Writing plugins](../plugins/writing-plugins.md#topics)). The plugin SDK's `health` module keeps
them (`History`), schedules each node (`Reporter`: the random phase, jitter, carried windows and
backing off above) and makes its report (`report`), so every plugin reports the same figures the
same way. A plugin adds the HTTP and its service's `node` fields.

## Examples

[`node-health/`](node-health/) holds example reports, each as a recorder of that kind would send
it. They are the conformance cases: a recorder's reports should look like them, and a service
should take every one. The plugin SDK's tests read each into its report types and write it back
unchanged.

| File | What it shows |
|---|---|
| [`p25-trunked.json`](node-health/p25-trunked.json) | Trunk Recorder Pro: a P25 site with SNR and phase error, a conventional FM channel, radios and clock |
| [`dmr-capacity-plus-two-receivers.json`](node-health/dmr-capacity-plus-two-receivers.json) | An SDRTrunk VCE kind: one DMR Capacity Plus site on two receivers, MER instead of SNR, sync loss, missing voice frames |
| [`conventional-relative-levels.json`](node-health/conventional-relative-levels.json) | An SDRTrunk kind: an MPT1327 site, and conventional channels with levels not referenced to full scale (SNR only) |

## Versions

A recorder sends `v`; a service reads the fields it knows and ignores the rest. New optional fields
don't change `v`; a change in a field's meaning does.

| Date | Change |
|---|---|
| 2026-11-15 | Still `1`, all additive or clarifying, after reviewing what SDRTrunk and SDRTrunk VCE can measure: what a node's report covers (whole sites, a site in several nodes' reports, several receivers on one site, `receiver`); protocols `mpt1327`, `ltr`, `ltrnet`, `passport`; control message units per protocol, `symbolRate` in baud, rest channels; `cc.syncLossSym`, `cc.merDb`, `voice.*.missing`; `calls.recorded` defined; reasons `data` and `noFrequency`; conventional `noiseDb` optional; the answers a recorder acts on |
| 2026-11-15 | Still `1`, additive and clarifying: radio `ppmApplied`; `ppmError` said to be measured against the configured correction (as recorders already measured it) |
