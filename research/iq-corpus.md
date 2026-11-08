# Plan: a corpus of contributed IQ recordings

Status (2026-11-08): design only. Nothing is built, and the Cloudflare repo does not exist yet.

Today's regression test replays eight captures of 90 s each (3.3 GB, [captures/](../captures/README.md)),
all recorded here, of four local systems. This plan adds recordings from other people's systems
and radios, contributed from inside Trunk Recorder Pro (TRP), reviewed by maintainers, and replayed
against expected outcomes. The same path gets us a recording of the problem when a user reports one.

## Goals

1. **Regression tests on real systems we can't hear**, at three levels: the whole radio, one control
   channel, one voice channel.
2. **Debugging user reports** from a recording and a sanitized description of their machine.
3. **Contributions from healthy systems too**, not only from failures. A corpus made only of
   failures can't show whether a change helps normal traffic.
4. **Captures triggered by rules** that can grow more expressive without changing the format.
5. **No structural change to the engine.** Capture is an optional tap at a few places, and costs
   nothing when off.

## Not goals

- **A public dataset.** Reads need a maintainer's GitHub login or a CI credential.
- **Live streaming from users' machines.** A contribution is always a finished file.
- **Replacing `captures/regress.sh`.** Its byte-identical check stays as the fast local test.

## Decisions

| Decision | Status |
|---|---|
| Three kinds of capture: wideband, control channel, voice channel | Decided |
| Storage on Cloudflare R2, with a Worker and a D1 index | Decided |
| The Cloudflare side lives in its own new repo | Decided |
| Every send goes through a local review screen | Decided |
| A manual capture-and-send path exists, for healthy systems | Decided |
| Promotion into the corpus is tied to GitHub permission on the TRP repo | Decided |
| A Cloudflare-hosted review page with a waterfall and a decode | Decided |
| Raw spectrum in storage is acceptable, as it isn't decoded | Decided, see [Data handling](#data-handling) for what decoding adds |
| Everything needed to decode and understand a file is in its `sigmf-meta` | Decided |
| The rule language is flexible and expressive | Decided |
| CEL as that language | Proposed |
| Sanitized system info as a separate `diagnostics.json` | Proposed |
| Decode in a Cloudflare Container running the real `trunk-pro` | Proposed |

## How it fits together

```
 TRP (this repo)                              trunk-recorder-corpus (new repo)
 ─────────────────                            ────────────────────────────────
 rules / manual button                        Worker: trunkrecorder.pro/corpus*
   │                                            ├─ upload API (presigned R2 URLs)
   ▼                                            ├─ review pages (GitHub sign-in)
 taps ──► RAM rings ──► local capture folder    └─ promote: opens a PR on the TRP repo
                            │                   │
                            ▼                   ▼
                     local review ──send──►  R2 blobs + D1 records
                                                │ queue
 trunk-pro replay / corpus check                ▼
   ▲                                          Container: same trunk-pro image
   │ manifest (sha256s, in this repo)           └─ decode.json, waterfall, audio,
   └──────────── nightly CI ◄── signed URLs        candidate expected outcome
```

## The three kinds of capture

| Tier | Tap | Format | Data rate | Tests |
|---|---|---|---|---|
| **Wideband** | `feed_source` in [runtime.rs](../crates/trunk-pro/src/runtime.rs), before the engine | Radio's own: `cu8`, or `ci16_le` from float sources | RTL 2.4 MS/s: 4.8 MB/s. Airspy 10 MS/s: 40 MB/s | Channelizer, tuning, whole-system behaviour |
| **Control channel** | `BlockDsp.taps`, already copied out per block, in [engine.rs](../crates/trunk-core/src/trunk/engine.rs) | `cf32_le` | 300 kB/s at a 37.5 kHz head | Control decoders |
| **Voice channel** | `SourceRx::decode_channel` and the pre-roll in `open_channel`, in [engine.rs](../crates/trunk-core/src/trunk/engine.rs). Conventional: `Conventional::open` and its per-block read, in [conventional.rs](../crates/trunk-core/src/trunk/conventional.rs) | `cf32_le` | 300 kB/s at a 37.5 kHz head | Voice decoders, TDMA, FEC |

The channel rate follows from the source rate: the largest power-of-two decimation (at most 256) that
stays at or above 24 kHz. That is 37.5 kHz at 2.4 MS/s and 39.06 kHz at 10 MS/s, between 24 and 48 kHz
for any radio up to about 12 MS/s and higher above that. Each file records its own.

Channel tiers are taken after the channelizer on purpose, so they isolate the decoders. A change to
the channelizer shows up in wideband tests only.

A multi-radio capture is a SigMF collection with one recording per radio, which maps onto
`replay --source`. Captures from different radios must be time-aligned, so each recording's
`core:datetime` and first sample position are taken at the same moment.

`tool slice` (phase 6) cuts channel files out of a wideband file, so most submissions need only one.

### Holding samples until a rule decides

| Tier | Held in | Cost |
|---|---|---|
| Control | A RAM ring per channel | 60 s is about 18 MB |
| Voice | The whole call in memory, decided at call end, as `Recorded` already does with frames | 30 s is about 9 MB per active channel |
| Wideband | A raw ring in the source's thread | 30 s is 144 MB (RTL) or 1.2 GB (Airspy 10 MS/s) |

Wideband lookback is short, or absent on small machines. The channelizer's existing history is not
a substitute: it stores overlapping float spectra, about 5× the size of raw `cu8`.

Rings exist only when a rule or a manual capture needs them. The capture writer runs on its own
thread with a bounded queue. If it falls behind it abandons the capture and logs why. It never
stalls a DSP thread.

## The file format

A submission is a folder:

```
<id>/
  capture.sigmf-meta          what the file is, and everything needed to decode it
  capture.sigmf-data
  capture.sigmf-collection    multi-radio only
  diagnostics.json            sanitized host and application state
```

A diagnostics-only submission, with no IQ, is valid.

### `sigmf-meta`

Standard SigMF 1.2 where a core field exists, and a `trp` extension for the rest, declared in
`core:extensions` with `optional: true` so that other SigMF tools still open the file. The extension
is defined in `docs/sigmf/trp.sigmf-ext.md`, which the spec requires. Dates and frequencies below are
made up.

```json
{
  "global": {
    "core:datatype": "cf32_le",
    "core:sample_rate": 37500,
    "core:version": "1.2.0",
    "core:recorder": "Trunk Recorder Pro 0.4.0",
    "core:hw": "RTL-SDR (R820T), via rtlsdr-nusb",
    "core:sha512": "…",
    "core:extensions": [{ "name": "trp", "version": "0.1.0", "optional": true }],
    "trp:tier": "voice",
    "trp:purpose": "rule",
    "trp:issue": null,
    "trp:app": { "version": "0.4.0", "commit": "a5999a8" },
    "trp:rule": { "id": "low-clean-voice", "on": "call.end", "if": "call.clean_voice_pct < 70.0" },
    "trp:source": { "center_hz": 858300000, "rate_hz": 2400000, "ppm": 0.5, "gain": "28", "guard_hz": 75000, "auto_tune": true },
    "trp:system": { "protocol": "p25", "wacn": 781824, "sysid": 680, "nac": 1091, "bank": "auto", "control_channels": [857987500] },
    "trp:channel": {
      "kind": "p25_phase2",
      "freq_hz": 859037500,
      "cutoff_hz": 7000,
      "applied_offset_hz": 412.0,
      "noise": 1.2e-7,
      "squelch": 3.1e-6,
      "seed": 859037500,
      "t0_s": 1812.44,
      "vocoder": "enhanced",
      "analog": null,
      "ignore_hz": [],
      "voice_params": { "tdma_key": [1091, 680, 781824], "nac": 1091 }
    }
  },
  "captures": [
    { "core:sample_start": 0, "core:global_index": 0, "core:frequency": 859037500, "core:datetime": "2026-11-08T14:03:11.250Z" }
  ],
  "annotations": [
    {
      "core:sample_start": 37500,
      "core:label": "grant",
      "trp:event": "grant",
      "trp:slot": 1,
      "trp:talkgroup": 12345,
      "trp:encrypted": false
    }
  ]
}
```

| What | Where | Needed by |
|---|---|---|
| Why it was captured, the rule, linked issue | `trp:purpose`, `trp:rule`, `trp:issue` | Reviewer |
| App version, radio, gain, ppm | `core:recorder`, `core:hw`, `trp:app`, `trp:source` | Reviewer, wideband replay |
| System type, identity, band plan, modulation | `trp:system` | All replays |
| Everything `voice::VoiceSpec` holds except keys | `trp:channel` | Voice replay |
| `VoiceParams`, and later changes to it | `trp:channel.voice_params`, then annotations | Voice replay (Phase 2 scrambling, NAC) |
| Grants: slot, talkgroup, encrypted | Annotations at their sample | Voice replay (`VoiceDecoder::listen`), reviewer |
| Site identity changes, control channel moves | Annotations | Control replay, reviewer |
| Lost samples | `core:global_index` jumps between `captures` segments | All replays |
| Retunes | A new `captures` segment | All replays |

A wideband file needs no grants, since it contains its own control channel. Annotations there are
for the reviewer only.

Left out of the metadata: unit IDs, talker aliases, GPS, encryption keys, and device serial numbers.
None is needed to decode. `core:geolocation` is off unless the contributor adds it.

### `diagnostics.json`

Separate from the capture because it can be sent alone, describes the machine and not the recording,
and is deleted with the submission even when the capture is promoted.

Contents: OS and architecture, CPU and memory, TRP version and build, USB layout and radio models,
source statistics (drops, clipping, clock), per-stage timing if available, recent warnings and errors,
and the shape of the config.

It is built field by field from an **allow-list**. [`redact_config`](../crates/trunk-pro/src/access.rs) is
a deny-list: correct for listen-only pages, wrong for something leaving the machine, because a secret
in a field added later would go out by default. Log lines pass the same way: known message types only,
no free-form text containing paths, hostnames or URLs. A test seeds a config with marker values in
every secret-bearing field and asserts none reaches the output.

## The rule language

Conditions are written in [CEL](https://github.com/google/cel-spec) (Common Expression Language),
inside a small rule object that holds what CEL can't express:

```json
{
  "id": "low-clean-voice",
  "enabled": true,
  "on": "call.end",
  "systems": ["wmata"],
  "if": "call.call_length > 3.0 && !call.encrypted && call.clean_voice_pct < 70.0",
  "repeat": { "count": 3, "within": "5m", "by": "call.freq" },
  "capture": [
    { "tier": "voice" },
    { "tier": "control", "before": "30s" }
  ],
  "limit": { "cooldown": "30m" }
}
```

| Field | Meaning |
|---|---|
| `on` | The event that evaluates this rule |
| `systems` | Static filter. Also tells TRP which channels need tapes, so unlisted systems cost nothing |
| `if` | A CEL expression over the event's facts |
| `repeat` | Fire only after this many matches in a window, optionally counted per key. Saves the tape of the event that fires it |
| `capture` | What to keep: tier, lookback (`before`), and for wideband and control `after` |
| `limit` | Cooldown for this rule, on top of the global budgets |

Why CEL: it can't loop, so evaluation is bounded and a rule can be shared safely. A maintainer can post
one on an issue to hunt a specific bug, and users paste it in. It only captures locally, and sending
still goes through review. It is also a common standard with its own documentation.

**State belongs to the rule object.** CEL has no memory, so patterns over time go in `repeat`, and
aggregates come in through a periodic event. New kinds of pattern add a field or a function, not a
new format.

**Facts come from types that already have a documented form:**

| `on` | Facts | From |
|---|---|---|
| `call.end` | `call.*`: `clean_voice_pct`, `snr`, `freq_error`, `call_length`, `encrypted`, `talkgroup`, … | `CallRecord` (the plugin-facing type; engine `Call` doesn't serialize) |
| `call.not_saved` | `call.*` | `Event::NotSaved` |
| `control.lost`, `control.regained` | `system`, `site`, `freq_hz` | `MonitorEvent` |
| `control.message` | `msg.*` | `Event::Message`, mapped only if a rule subscribes |
| `note.warning` | `system`, `site`, `text` | `Event::Note` |
| `source.clipping`, `source.drops` | `source`, `pct`, `per_s` | `MonitorEvent` |
| `tick` | `stats.*`: per-site decode and failure rates, per-source levels | Every 10 s |
| `schedule` | none | Random or timed sample |
| `manual` | what the user chose | The capture button |

The manual button is a one-shot rule internally, so there is one capture path.

**Built-in presets** are ordinary rules, shipped disabled: control channel lost, high failed-message
rate, channel not in the table, poor call quality, granted but no audio, clipping, and a random
healthy sample.

**Limits and safety:**

- Evaluated on the engine's event poll, never on a DSP thread.
- A rule is compiled when saved. An unknown field or a type error is refused with a message.
- Global budgets: RAM held by rings, disk per day, captures per day, always-on cooldown.
- A capture that overlaps lost samples is marked. `source.drops` exists as a fact but isn't a preset,
  because a recording with holes is a poor test.
- Rules are observe-only, like plugins. Nothing here can change what the recorder does.

## In the app

An advanced feature: **Setup → Diagnostics**, hidden until switched on. Rules live in the config under
`diagnostics`. Capture commands are control-only, added to `Command::listen_may_send`'s refusals.

- **Contribute a sample:** pick a system, tier and duration, record, review, send. This is the path for
  healthy systems. Its `trp:purpose` is `healthy`.
- **Capture now:** the same, from a problem, with an optional GitHub issue number. `trp:purpose` is `bug`.
- **Rules:** presets to switch on, plus an expression editor with save-time checking.
- **Captures:** the local folder. Each row shows tier, size, why it was captured and its state.

**Local review, before every send:** a waterfall, the metadata and diagnostics in readable form, the size,
and consent text saying the submission will be decoded and listened to by maintainers. Rules never
send on their own.

**Sending:** resumable multipart straight to R2, on a background thread, throttled, retrying across
restarts. The app shows a receipt ID, to quote on an issue or use to withdraw the submission.

## Testing against the corpus

### Running a capture

- `replay capture.sigmf-meta`, and `replay capture.sigmf-collection`, take their center, rate, format and
  system from the metadata. The existing flags still work and override it.
- Channel-tier files need a new harness, since `replay` needs a grant to open a voice channel.
  `tool snr` in [snrtool.rs](../crates/trunk-pro/src/snrtool.rs) is the model: it drives receivers
  directly from channel IQ. Control: `control::build` and `ControlChannel::push`. Voice: `voice::build`,
  `set_params` and `listen` from the annotations, then `push` and `flush`.
- CI runs single-threaded. Only that mode is bit-identical on multi-radio captures.

### Expected outcomes

Byte-exact matching can't be the test here: every intended DSP change would fail it. Each capture has
an `expected.json`, and each run gets one result:

| Result | Meaning |
|---|---|
| `match` | Same as expected |
| `improved` | Everything expected, and more |
| `regressed` | Something expected is missing or worse |
| `known-failing` | Tied to an open issue, still failing as expected |
| `fixed` | Was known-failing, now meets its criteria |

| Tier | Compared | Regressed when |
|---|---|---|
| Control | CRC-passing messages, as a multiset | An expected message no longer decodes |
| Voice (digital) | A 16-bit hash per vocoder frame, plus error counts | Good frames fall below the baseline, past a per-capture tolerance (default 0) |
| Voice (analog) | Call boundaries, audio envelope correlation (as `tests/mimosdr.rs` does) | Correlation under a floor |
| Wideband | Calls by talkgroup, frequency, slot and encryption, with start time ±0.5 s, plus message counts | An expected call is missing |

CRC-passing messages are effectively ground truth, so new ones count as improvements. Voice baselines
hold hashes rather than IMBE, because vocoder frames are decoded speech.

An intended change that moves results gets a new baseline: a PR updating the hashes in the manifest,
with the before-and-after report attached. Bug captures start as `known-failing` with criteria set by
the reviewer, since their output at capture time is wrong by definition.

### The manifest

`captures/corpus.json`, in this repo and public, holds only what isn't sensitive:

```json
{
  "version": 1,
  "captures": [
    {
      "id": "c-01HZX",
      "tier": "control",
      "protocol": "p25",
      "seconds": 60,
      "bytes": 18000000,
      "sha256": { "meta": "…", "data": "…", "expected": "…" },
      "state": "active",
      "issue": null
    }
  ]
}
```

`trunk-pro tool corpus <manifest> [--tier …] [--only …]` fetches, verifies hashes, runs, and writes
a report. Corpus files, like today's captures, stay out of git.

### CI

| When | What |
|---|---|
| Every push | Unchanged: the current build and tests |
| Nightly, `main` | The full corpus, against signed URLs |
| On request, same-repo PRs | The corpus, by label or manual dispatch |

Fork PRs don't get the credential, so they can't run it. The repo and its Actions logs are public, so
reports print only IDs, tiers, counts and results, and no decoded output is stored as an artifact.

## The corpus service (new repo)

**`TrunkRecorder/trunk-recorder-corpus`**: TypeScript, one Worker on `trunkrecorder.pro/corpus*`, set up
like `trunk-recorder-directory` (React and Vite site, D1 migrations, staging and production deploys gated
on tests). The repo name is available as of this writing. TRP talks to it only over its API.

### Storage

One R2 bucket, `trp-iq`, on the Standard tier, with objects keyed by content hash and never overwritten.
Retention is decided by D1 state, enforced by a scheduled Worker, so promotion moves no bytes and
identical uploads are stored once.

```
blobs/<sha256>                       sigmf-data, sigmf-meta, diagnostics, expected
derived/<submission>/waterfall.png
derived/<submission>/decode.json
derived/<submission>/audio/<call>.m4a
```

D1 tables: `submissions` (receipt, token, state, purpose, tier, sizes, timestamps), `files` (hash, kind,
size), `checks` (automatic results), `reviews` (who, decision, notes), `installs` (token, quotas).

| State | Next | Retention |
|---|---|---|
| `uploading` | `received`, or expires in 24 h | Incomplete uploads aborted |
| `received` | `decoding` | |
| `decoding` | `ready`, or `failed` | |
| `ready` | `promoted`, or `rejected` | 30 days |
| `promoted` | `retired` | Kept. Diagnostics and audio deleted |
| `rejected`, `withdrawn` | | Deleted |

Costs, at about $0.015 per GB-month with free egress: 1 TB is about $15 a month.

### Upload API

```
POST   /corpus/api/v1/installs                      a token for this install
POST   /corpus/api/v1/submissions                   declare files (hashes, sizes); get id and receipt
POST   /corpus/api/v1/submissions/:id/parts         presigned URLs for parts
POST   /corpus/api/v1/submissions/:id/complete
GET    /corpus/api/v1/submissions/:id               status, by receipt
DELETE /corpus/api/v1/submissions/:id               withdraw, by receipt
```

Bytes go straight to R2, since Worker request bodies are size-limited. Quotas apply per token and per
IP, with a maximum size per tier, starting at 5 GB a submission. Uploaded objects are never served
directly, only through the authenticated Worker, so the service can't be used as free file hosting.
Contributors don't need a GitHub account.

### Reviewer sign-in

A GitHub App on the TrunkRecorder org provides "Sign in with GitHub". The Worker asks GitHub for the
user's permission on `TrunkRecorder/trunk-recorder-pro`:

| Permission | Can |
|---|---|
| write, maintain, admin | Review, listen, reject, promote |
| anything else | Nothing past the sign-in page |

Sessions are short (about 15 minutes before the permission is checked again), and it is checked again
on every promote.

### The decode pipeline

A Worker can't run this: it has CPU limits and can't run a native binary. A **Cloudflare Container** can
(up to 4 vCPU, 12 GiB, 20 GB of disk), built from TRP's own Dockerfile at a pinned release:

1. `complete` puts a message on a queue, which starts a container.
2. The container fetches the blobs and recomputes every hash, which R2 multipart doesn't do for us.
3. It parses and validates the SigMF. Uploaded files are untrusted input, and this is the sandbox for them.
4. It runs the same replay CI uses, with no keys, so encrypted traffic stays encrypted.
5. It writes `decode.json`, a waterfall from `Channelizer::power_spectrum`, audio, and a candidate `expected.json`.
6. It records the checks: hash and size against rate, clipping, gaps, whether the metadata agrees
   with what actually decoded, and near-duplicates of existing corpus entries.

Each job has a timeout. A job at full size for 10 minutes costs a few cents.

### The review page

- **Queue:** by purpose, tier, protocol and age, with check results.
- **A submission:** waterfall, metadata in readable form, diagnostics, control messages, a call list
  with audio, check results, and the linked issue.
- **Actions:** reject with a note; or promote, choosing `active` or `known-failing` and its criteria.

The decode is the candidate baseline, so approving a submission is what accepts its expected outcome.
There is no separate authoring step.

**Promote** has the GitHub App open a PR adding the manifest entry. `expected.json` stays in R2, pinned by
hash. Merging makes it a test, so normal repo review applies. Audio and diagnostics are deleted.

## Data handling

Raw spectrum in storage was accepted because it isn't decoded. Parts of this design do decode it, so:

| Artifact | Holds | Who can read it | Kept |
|---|---|---|---|
| `sigmf-data` | Raw IQ, everything in the passband | Reviewers, CI | While promoted, or 30 days |
| `sigmf-meta` | System identity, talkgroups, grants | Reviewers, CI | Same |
| `diagnostics.json` | Machine details, allow-listed | Reviewers | 30 days at most |
| Decode and audio | Decoded voice and control traffic | Reviewers | Until reviewed |
| `expected.json` | Control messages in the clear, voice as hashes | Reviewers, CI | While promoted |
| Public manifest, CI logs | IDs, counts, hashes | Everyone | Permanent |

Withdrawing a submission deletes everything for it, and removes a promoted entry from the manifest by PR.
System identity places a contributor in a general area even without geolocation, which the consent text
says. If any of this were ever to become public, get legal advice first: Section 705 of the Communications
Act and state laws restrict divulging some radio content.

## Phases

Phases 1 to 3 are useful on their own and need no Cloudflare work.

**Every TRP phase must leave `captures/regress.sh` byte-identical and pass the [CPU cost gate](../perf/gate.py)
with capture off.**

### Phase 1: SigMF and a manifest

*This repo. No engine change.*

- Read SigMF in `replay`, including collections and `core:global_index` gaps.
- Write `docs/sigmf/trp.sigmf-ext.md`.
- Add `sigmf-meta` sidecars for today's eight captures (`core:dataset` points at the existing files).
- Add `captures/corpus.json` and `tool corpus`.

*Done when:* the seven regress scenarios run from metadata alone with output identical to the flag form.

### Phase 2: Expected outcomes

*This repo. No engine change.*

- Define `expected.json` per tier, generation, comparison and the five results.
- Generate baselines for today's captures.

*Done when:* a deliberate decoder change on a branch reports `regressed` or `improved` correctly, and
an unchanged build reports `match` on every capture.

### Phase 3: Manual capture and diagnostics

*This repo. Touches `runtime.rs`, not the engine.*

- The wideband tap, writer thread and budgets, for all radio types and multi-radio collections. Today's
  `trunk-pro capture` is RTL-only and writes `cu8`.
- `sigmf-meta` writer, and the `diagnostics.json` allow-list with its leak test.
- Setup → Diagnostics: capture, local list, local review. A web render test (`npm test`).
- Docs in `docs/guides/`.

*Done when:* a capture made in the app replays to the same calls TRP recorded live, on an RTL pair and
on an Airspy, and the leak test passes.

### Phase 4: Corpus service, upload and review

*New repo, plus the send path in this repo.*

- Create the repo, Worker, D1, R2, staging and production.
- The upload API, quotas, and scheduled retention.
- The GitHub App, sign-in, and permission check.
- A review queue showing metadata and diagnostics only.
- TRP: resumable send, receipts, withdrawal.

*Done when:* a 2 GB submission survives an interrupted upload, a maintainer can see it, and a GitHub
account without write access is refused.

### Phase 5: Decode, waterfall and promotion

*New repo, plus a release image from this repo.*

- The queue, Container job and checks.
- Waterfall, decode tables and audio on the review page.
- Promote as a PR via the GitHub App.
- The nightly CI job in this repo.

*Done when:* a contributed capture goes from upload to a merged manifest entry to a nightly `match`,
without anyone touching the files by hand.

### Phase 6: Channel tiers

*This repo. The only phase with engine changes: small and optional.*

- `Option<Tape>` on channels, the control and voice taps, and pre-roll capture.
- `VoiceSpec`, `VoiceParams` and grants written to `sigmf-meta`.
- The channel harness and `tool slice`.
- Channelizer metrics in the wideband comparison: a head's output against the stored channel tier.
- Channel tiers in the decode job and review page.

*Done when:* a voice file on P25 Phase 1, Phase 2, DMR and analog decodes through the harness to the
same frames or audio as the live call, and a tape-enabled run costs under an agreed CPU budget.

### Phase 7: Rules

*This repo.*

- The rule object, CEL evaluation and fact mapping, with `repeat`, `tick` and budgets.
- Presets, the editor and save-time checks.
- Manual capture becomes a one-shot rule.

*Done when:* each preset fires on a capture replayed to trigger it, a malformed rule is refused when saved,
and with every preset on, DSP threads never wait on capture.

## To confirm before building

| Item | Phase | Why |
|---|---|---|
| The `cel` crate: maintenance, size, whether it builds for WASM | 7 | MIT licensed, activity unconfirmed. The interface already reuses the config's rules as WASM, and rule checks should do the same |
| `GET /repos/{owner}/{repo}/collaborators/{username}/permission` with a GitHub App token | 4 | Recalled, not confirmed in the docs |
| How a Container reaches R2, and egress cost | 5 | Not confirmed |
| Real job times and memory for a large wideband file | 5 | Sets the instance size and timeouts |
| R2 multipart limits and presigned URL expiry | 4 | Not confirmed |
| GitHub-hosted runner disk and time for the full corpus | 5 | May need sharding, or running in a Container |
| Legal position, if anything is ever published | n/a | See [Data handling](#data-handling) |
