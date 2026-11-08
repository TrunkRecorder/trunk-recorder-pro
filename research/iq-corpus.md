# Plan: a corpus of contributed IQ recordings

Status (2026-11-08): being built on branch `iq-corpus`. The Cloudflare repo does not exist yet.

**Built so far**, ahead of the phase order because a simple way to record a known-good state came first:

- A "Capture known-good sample" button on the RF page, recording every radio for 30 to 120 s
  ([iq_capture.rs](../crates/trunk-pro/src/iq_capture.rs), part of phase 3).
- The `sigmf-meta` writer with its allow-lists and leak test, and `docs/sigmf/trp.sigmf-ext.md`.
- `replay` of SigMF captures, including collections and gaps, the manifest, `tool corpus`, and the
  regression scenarios as metadata-only captures (phase 1, done).

Not yet: `diagnostics.json`, expected outcomes, and everything after phase 3.

Recordings are public. Anyone can download them to test their own build or help debug someone else's
report. The only gate is on what counts as a regression test: promoting a recording needs GitHub write
access to this repo.

Today's regression test replays eight captures of 90 s each (3.3 GB, [captures/](../captures/README.md)),
all recorded here, of four local systems. This plan adds recordings from other people's systems
and radios, contributed from inside Trunk Recorder Pro (TRP), published for anyone to use, and, once a
maintainer promotes them, replayed against expected outcomes kept for each TRP version. The same path
gets us a recording of the problem when a user reports one.

## Goals

1. **Regression tests on real systems we can't hear**, at three levels: the whole radio, one control
   channel, one voice channel.
2. **Debugging user reports**, by anyone, from a public recording and a sanitized description of the
   reporter's machine.
3. **Contributions from healthy systems too**, not only from failures. A corpus made only of
   failures can't show whether a change helps normal traffic.
4. **Captures triggered by rules** that can grow more expressive without changing the format.
5. **Results tracked by TRP version**, so improvements over time are kept and visible.
6. **No structural change to the engine.** Capture is an optional tap at a few places, and costs
   nothing when off.

## Not goals

- **Access control on the recordings.** They are plain public URLs: no accounts, tokens or signed links.
- **Our own sign-in system.** GitHub's repo permissions are the only maintainer check.
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
| Recordings are public, for community testing and debugging, with no complex access system | Decided |
| Promotion into the regression tests needs GitHub write access to the TRP repo | Decided |
| Expected output is stored per TRP version, so it can improve over time | Decided |
| A Cloudflare-hosted page with a waterfall and decode counts | Decided |
| Everything needed to decode and understand a file is in its `sigmf-meta` | Decided |
| The rule language is flexible and expressive | Decided |
| CEL as that language | Proposed |
| Sanitized system info as a `diagnostics.json`, stored and published beside the IQ | Decided |
| Publish IQ, not decoded radio traffic | Decided |
| Decode in a GitHub Action running the released `trunk-pro` | Proposed |
| Promote by a `workflow_dispatch` workflow, which GitHub limits to users with write access | Proposed |
| Expected output in this repo, as text of hashes and counts, with `next` for unreleased changes | Proposed |

## How it fits together

```
 TRP (this repo)                              trunk-recorder-corpus (new repo)
 ─────────────────                            ────────────────────────────────
 rules / manual button                        Worker: trunkrecorder.pro/corpus*
   │                                            ├─ upload API (presigned R2 URLs)
   ▼                                            └─ public pages: list, waterfall, decode
 taps ──► RAM rings ──► local capture folder    │
                            │                   ▼
                            ▼                 R2 public bucket: iq.trunkrecorder.pro
                     local review ──send──►   D1: submissions
                                                │ dispatch
                                                ▼
 corpus/ (manifest, expected)        GitHub Action: released trunk-pro
   ▲          │                                 └─ checks, decode counts, waterfall
   │          ▼
   │        trunk-pro tool corpus ◄── anyone, CI, forks (plain HTTPS)
   │
 corpus-promote workflow (write access only) ──► PR adding manifest + expected
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
  source-N.sigmf-meta         what the file is, and everything needed to decode it
  source-N.sigmf-data         one pair per radio
  capture.sigmf-collection    names them, even for one radio
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
| System type, identity, band plan, modulation | `trp:config`, `trp:talkgroups`, `trp:bandplans`, `trp:live` | All replays |
| Everything `voice::VoiceSpec` holds except keys | `trp:channel` | Voice replay |
| `VoiceParams`, and later changes to it | `trp:channel.voice_params`, then annotations | Voice replay (Phase 2 scrambling, NAC) |
| Grants: slot, talkgroup, encrypted | Annotations at their sample | Voice replay (`VoiceDecoder::listen`), reviewer |
| Site identity changes, control channel moves | Annotations | Control replay, reviewer |
| Lost samples | `core:global_index` jumps between `captures` segments | All replays |
| Retunes | Not saved: retuning ends the capture | n/a |

A wideband file needs no grants, since it contains its own control channel. Annotations there are
for the reviewer only.

Left out of the metadata: unit IDs, talker aliases, GPS, encryption keys, and device serial numbers.
None is needed to decode. `core:geolocation` is off unless the contributor adds it.

### `diagnostics.json`

Sanitized system info, stored and published beside the IQ. Separate from the capture's metadata because it
describes the machine rather than the recording, and can be sent alone.

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

**Local review, before every send:** a waterfall, the metadata in readable form, the size, and consent text
saying the recording and its diagnostics become public and can't be recalled once others have copied them. Rules never send on their own.

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

### Expected output, by TRP version

Byte-exact matching can't be the test here: every intended DSP change would fail it. Each run gets one
result per capture, against the expected output for its TRP version:

| Result | Meaning |
|---|---|
| `match` | Same as expected |
| `improved` | Everything expected, and more |
| `regressed` | Something expected is missing or worse |
| `known-failing` | Tied to an open issue, still failing as expected |
| `fixed` | Was known-failing, now meets its criteria |
| `changed` | The same amount decoded, with different bits |
| `no-baseline` | Nothing expected at this version: the capture was promoted later |

| Tier | Compared | Regressed when |
|---|---|---|
| Control | CRC-passing messages, as a multiset | An expected message no longer decodes |
| Voice (digital) | Call boundaries, error counts, and a hash per 20 vocoder frames | Good frames fall below the baseline, past a per-capture tolerance (default 0) |
| Voice (analog) | Call boundaries, audio envelope correlation (as `tests/mimosdr.rs` does) | Correlation under a floor |
| Wideband | Calls by a hash of talkgroup, slot and encryption, frequency, start time ±0.5 s, plus message hashes | An expected call is missing |

CRC-passing messages are effectively ground truth, so new ones count as improvements. Expected files hold hashes
and counts, not decoded traffic, which also keeps them small enough for git.

**Files live in this repo, as text, one per version the output changed in:**

```
corpus/
  corpus.json                     the manifest
  expected/<capture-id>/
    0.4.0.jsonl                   as of the release that promoted it
    0.6.2.jsonl                   a later release decoded it differently
    next.jsonl                    unreleased changes on main
```

Each file starts with a header line (capture, TRP version, commit, date), then sorted lines of one message
hash or call each. Text means a pull request shows how much changed: one more message decoded, one call lost. A file
appears only when the output changes, so a release that decodes a capture the same adds nothing.

**Which one a run compares against:** `next.jsonl` if there is one, otherwise the newest version at or below
the binary's own. `--against <version>` picks any other. A checkout of an old tag has the files of its day,
so it compares against its own results.

**How the files change:**

1. **Promotion** writes the first file, named for the release that decoded the capture.
2. **A pull request that moves results:** `regressed` fails CI, and `improved` passes with a notice.
   `tool corpus bless` writes `next.jsonl` in the same PR, so the code and its new expected output are
   reviewed and merged together. Accepting a regression on purpose is visible in that diff.
3. **A release** renames each `next.jsonl` to the version being released (`tool corpus release <version>`, in
   the release commit), as the changelog's Unreleased section becomes a version.

The same files give each capture's score by TRP version, for the public page to chart.

Bug captures start as `known-failing` with criteria set by the maintainer who promotes them, since their
output at capture time is wrong by definition.

### The manifest

`corpus/corpus.json`:

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
      "files": [{ "name": "source-0.sigmf-meta", "sha256": "…" }, { "name": "source-0.sigmf-data", "sha256": "…" }],
      "state": "active",
      "issue": null
    }
  ]
}
```

Each file is at `https://iq.trunkrecorder.pro/blobs/<sha256>`, so anyone can fetch it with `curl`.

`trunk-pro tool corpus corpus/corpus.json [--tier …] [--only …] [--against …]` downloads what it
doesn't have (resuming with range requests, into a local cache), verifies hashes, runs, and writes a report.
Corpus files, like today's captures, stay out of git.

### CI

| When | What |
|---|---|
| Every push | Unchanged: the current build and tests |
| Pull requests that touch decoding, or `corpus/` | A small subset of the corpus |
| Nightly, `main` | The full corpus |

The files are public, so fork PRs run this the same as anyone.

## The corpus service (new repo)

**`TrunkRecorder/trunk-recorder-corpus`**: TypeScript, one Worker on `trunkrecorder.pro/corpus*`, set up
like `trunk-recorder-directory` (React and Vite site, D1 migrations, staging and production deploys gated
on tests). The repo name is available as of this writing. TRP talks to it only over its API.

### Storage

One public R2 bucket, `trp-iq`, on the Standard tier, served at `iq.trunkrecorder.pro` (a custom domain:
Cloudflare caches it and range requests work). Objects are keyed by content hash and never overwritten, so
identical uploads are stored once and files can be cached forever.

```
blobs/<sha256>                       sigmf-data, sigmf-meta, the collection, diagnostics.json
derived/<submission>/waterfall.png
derived/<submission>/checks.json
```

One D1 table holds submissions (receipt, install token, state, purpose, tier, files, check results), and a
small one holds install tokens and their quotas.

| State | Next | Listed publicly | Kept |
|---|---|---|---|
| `uploading` | `received`, or expires in 24 h | No | Incomplete uploads aborted |
| `received` | `ready`, or `failed` | No | |
| `ready` | | Yes | 90 days, or as long as the manifest lists it |
| `failed` | | No | Deleted |

"Promoted" isn't a state here. A scheduled Worker reads `corpus/corpus.json` from this repo's `main`
and keeps every blob it lists, so the service needs no sync with GitHub. Costs, at about $0.015 per GB-month
with free egress: 1 TB is about $15 a month.

### Upload API

```
POST   /corpus/api/v1/installs                      a token for this install
POST   /corpus/api/v1/submissions                   declare files (hashes, sizes); get id and receipt
POST   /corpus/api/v1/submissions/:id/parts         presigned URLs for parts
POST   /corpus/api/v1/submissions/:id/complete
GET    /corpus/api/v1/submissions                   the public list
GET    /corpus/api/v1/submissions/:id               a submission, public
DELETE /corpus/api/v1/submissions/:id               withdraw, by receipt
```

Bytes go straight to R2, since Worker request bodies are size-limited. Contributors don't need a GitHub
account.

**Abuse.** A public bucket that accepts uploads can be misused as free file hosting. So: quotas per token and
per IP; at most 5 GB a submission; nothing listed until the decode job has validated it; anything that
fails validation deleted automatically; and a maintainer-only `corpus-remove` workflow for takedowns.

### The decode job

A Worker can't run this: it has CPU limits and can't run a native binary. **GitHub Actions** can, in the
corpus repo, for free on a public repo, with no extra infrastructure:

1. `complete` sends a `repository_dispatch`.
2. The job downloads the latest TRP release and the submission's blobs, and recomputes every hash, which R2
   multipart doesn't do for us.
3. It parses and validates the SigMF.
4. It runs the same replay CI uses, with no keys, so encrypted traffic stays encrypted.
5. It writes a waterfall from `Channelizer::power_spectrum` and decode counts (messages, calls). Decoded audio and
   message text stay out.
6. It records the checks: hash and size against rate, clipping, gaps, whether the metadata agrees with what
   decoded, and near-duplicates of existing corpus entries.
7. It reports to the Worker with a token that can only write that submission's results.

The upload is untrusted input, so this job has no write access to any repo.

### The public page

No sign-in. A list by purpose, tier, protocol and age; for each submission its waterfall, metadata, diagnostics,
decode counts, checks, and the linked issue. It gives the download URLs and the `trunk-pro replay` command for
anyone who wants to decode it themselves, and for a promoted capture its score by TRP version.

### Promoting, with GitHub permission

**`.github/workflows/corpus-promote.yml`** in this repo, run by `workflow_dispatch`, with inputs for the
submission ID, `active` or `known-failing`, and any issue. GitHub only lets users with write access to the repo
run it, so there is no sign-in or permission code of ours. The public page links to it.

It downloads the capture, decodes it with the latest release, adds the manifest entry and first expected file,
and opens a PR. Merging makes it a test. **`corpus-remove.yml`** is the same for takedowns, calling the Worker
with a repo secret.

## Data handling

| Artifact | Holds | Who can read it | Kept |
|---|---|---|---|
| `sigmf-data` | Raw IQ, everything in the passband | Everyone | While promoted, or 90 days |
| `sigmf-meta` | System identity, talkgroups that aren't plain digital | Everyone | Same |
| `diagnostics.json` | Machine details, allow-listed | Everyone | Same |
| Waterfall, check results, decode counts | No decoded traffic | Everyone | Same |
| `corpus/` in this repo | Manifest, and expected output as hashes and counts | Everyone | In git history |

We publish IQ, not decoded radio traffic: no audio, message text, talkgroups or unit IDs are published by the
service or in this repo. Anyone can run `trunk-pro replay` on a recording themselves.

The metadata and diagnostics allow-lists still apply: keys, plugin settings, paths and serial numbers never leave
the machine.

The consent text must say plainly that a recording becomes public, since a withdrawn one can already have been
copied, and that system identity places a contributor in a general area.

## Phases

Phases 1 to 3 are useful on their own and need no Cloudflare work.

**Every TRP phase must leave `captures/regress.sh` byte-identical and pass the [CPU cost gate](../perf/gate.py)
with capture off.**

### Phase 1: SigMF and a manifest

*This repo. No engine change.*

- Read SigMF in `replay`, including collections and `core:global_index` gaps. **Built.**
- Write `docs/sigmf/trp.sigmf-ext.md`. **Built.**
- Today's eight recordings as the seven regress scenarios, each a capture folder with its settings in
  its metadata ([corpus/seed.py](../corpus/seed.py)). **Built.**
- Add [corpus/corpus.json](../corpus/corpus.json) and `tool corpus`. **Built.** It lives in `corpus/` because
  `captures/` is gitignored. Each scenario is a manifest entry, and shared recordings are stored once by hash.

*Done when:* the seven regress scenarios run from metadata alone with output identical to the flag form.
**Met 2026-11-08:** `tool corpus run` against `regress.sh`, identical across 152 files.

### Phase 2: Expected output, by version

*This repo. No engine change.*

- The per-version expected files, generation, comparison and the six results.
- `tool corpus bless`, `tool corpus release`, and that step in the release process.
- Generate the first files for today's captures.

*Done when:* a deliberate decoder change on a branch reports `regressed` or `improved` correctly, `bless` puts
the change in `next.jsonl`, a release turns that into a version, and an unchanged build reports `match` on
every capture.

### Phase 3: Manual capture and diagnostics

*This repo. Touches `runtime.rs`, not the engine.*

- The wideband tap, writer thread and budgets, for all radio types and multi-radio collections. **Built**;
  tested on file sources, not yet on real radios.
- `sigmf-meta` writer with its leak test. **Built.** The `diagnostics.json` allow-list is not.
- The capture button, list and delete, on the RF page, with a web render test. **Built.** Local
  review before sending comes with phase 4.
- Docs in `docs/guides/`.

*Done when:* a capture made in the app replays to the same calls TRP recorded live, on an RTL pair and
on an Airspy, and the leak test passes.

### Phase 4: Corpus service and upload

*New repo, plus the send path in this repo.*

- Create the repo, Worker, D1, the public R2 bucket and its domain, staging and production.
- The upload API, quotas, and scheduled retention that reads the manifest.
- TRP: local review with the consent text, resumable send, receipts, withdrawal.

*Done when:* a 2 GB submission survives an interrupted upload and can be downloaded with `curl`.

### Phase 5: Decode, public page and promotion

*New repo, plus workflows in this repo.*

- The decode job and checks. Unvalidated uploads are deleted. Only IQ, diagnostics, a waterfall and counts are published.
- The public list and submission pages.
- `corpus-promote` and `corpus-remove`.
- The CI jobs in this repo, for PRs and nightly.
- Publish today's eight captures as the first entries.

*Done when:* a contributed capture goes from upload to a merged manifest entry to a nightly `match`, without
anyone touching the files by hand, and a GitHub account without write access can't run the promote workflow.

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
| R2 multipart limits, presigned URL expiry, and range requests on the public domain | 4 | Not confirmed |
| A PR opened with `GITHUB_TOKEN` doesn't start CI, as far as I recall | 5 | The promote workflow may need a GitHub App or PAT for that |
| GitHub-hosted runner disk and time for large captures and the full corpus | 5 | May need sharding, or a Cloudflare Container as a fallback |
| Free Actions minutes for a public repo at the decode job's volume | 5 | Recalled, not confirmed |
