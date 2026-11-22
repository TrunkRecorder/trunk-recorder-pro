# Testing

How to run the tests, and which ones a change needs. Building comes first: see
[Building](README.md#building).

| What | Command | Checks |
|---|---|---|
| [Rust tests](#rust-tests) | `cargo test --release --workspace` | Every crate's unit and integration tests |
| [Interface tests](#interface-tests) | `npm test`, `npm run typecheck` (in `web/`) | The interface's code, and the dashboard pages as rendered |
| [Protocol checks](#the-interface-protocol) | `npm run schema -- --check`, `cargo test -p trunk-pro protocol_tests` | `protocol.ts`, its schema and the Rust enums agree |
| [Signing in, in a browser](#signing-in-in-a-browser) | `npm run test:browser` (in `web/`) | Passkeys and the sign-in pages in headless Chrome |
| [Reference captures](#reference-captures-capturesregresssh) | `captures/regress.sh <out>` | A change to the engine changes nothing it shouldn't, on our own captures |
| [The corpus](#the-corpus) | `trunk-pro tool corpus run --out <dir>` | The same, on the public regression captures |
| [Weak signals](#weak-signal-curves) | `trunk-pro tool snr …` | How a receiver change does near the noise |
| [CPU cost](#cpu-cost) | `perf/gate.py` | Instructions per stage, against `perf/baseline.json` |

## House rules

- **Never run `cargo fmt`.** The code is hand-formatted; rustfmt rewraps every file, your
  uncommitted work included.
- **Docs are part of the change.** A change to what the recorder does, a setting, a message or a
  command updates the page that documents it in the same pull request (see [docs/](../README.md)).
- **A UI feature users rely on gets an assertion** in the dashboard render tests (below).
- A change that moves decoding results says so: a new reference for `regress.sh`, or
  `--bless next` for the corpus.

## Rust tests

```bash
cargo test --release --workspace --locked      # as CI runs it
cargo test --release -p trunk-core             # one crate
cargo test --release -p trunk-core multisite   # tests whose path contains "multisite"
```

`trunk-pro` embeds the interface, so `web/dist` must exist before it compiles: build the
interface first ([Building](README.md#building)). For a build that only replays captures, CI
makes an empty one instead (`mkdir -p web/dist && touch web/dist/index.html`).

`crates/trunk-pro/tests/auth_e2e.rs` tests signing in against the real binary over real
sockets, with a config of its own. `crates/rtlsdr-nusb/tests/hardware.rs` needs an RTL-SDR
plugged in, so it is `#[ignore]`d and never runs in CI:

```bash
RTLSDR_SERIAL=<serial> cargo test -p rtlsdr-nusb --test hardware -- --ignored --nocapture --test-threads=1
```

### The call store on PostgreSQL

The call store's tests run on SQLite always, and on PostgreSQL too when `TRP_PG_URL` names a
database (each test uses a schema of its own). CI sets it, against a PostgreSQL 17 service:

```bash
TRP_PG_URL=postgres://trp:trp-test@localhost:5432/trp cargo test --release -p trunk-pro db
```

## Interface tests

In `web/`:

```bash
npm test             # every src/**/*.test.ts(x), through esbuild as vite builds them
npm run typecheck    # tsc (npm run build runs it too)
node --import ./scripts/test-loader.mjs --test src/dash/dash.test.tsx    # one file
```

`npm test` runs Node's own test runner (`node --test`) with `scripts/test-loader.mjs`, which loads
`.ts` and `.tsx` through esbuild, so a test can import the interface's modules and render a page
with `react-dom/server`. The interface runs the recorder's config rules as WebAssembly, so run
`npm run wasm` first, and again after changing a rule in `trunk-app` or `trunk-web`: the tests use
what it last built.

`src/dash/dash.test.tsx` renders the dashboard's pages from what the recorder sends and checks
what a user relies on seeing there. A new dashboard feature that users depend on adds an
assertion here.

## The interface protocol

`web/src/protocol.ts` is the source of truth; `docs/api/protocol.schema.json` is generated from it.

```bash
(cd web && npm run schema)               # regenerate the schema after changing protocol.ts
(cd web && npm run schema -- --check)    # fails when the schema is stale: CI runs this
cargo test --release -p trunk-pro protocol_tests
```

`crates/trunk-pro/src/protocol_tests.rs` checks the schema against what the recorder really sends
and handles: the messages of a recording session, a survey and the server's replies, each against
its type's shape, and protocol.ts's message types against the Rust enums `ToClient` and `Command`,
both ways. A message or command added on one side and not the other fails here.

## Signing in, in a browser

```bash
(cd web && npm run build) && cargo build -p trunk-pro && (cd web && npm run test:browser)
```

`web/scripts/auth-browser.mjs` starts the real `trunk-pro` on a config of its own and drives
headless Chrome over the DevTools Protocol, with a virtual authenticator, so passkeys are made and
used unattended. It proves what the Rust tests can't: the pages' own WebAuthn code with a
browser's, and the cookie as a browser keeps it. `TRUNK_PRO` names the binary (default
`target/debug/trunk-pro`), `CHROME` the browser; with no Chrome it is skipped, saying so. `KEEP=1`
keeps its temporary folder.

## Reference captures (`captures/regress.sh`)

Seven scenarios (P25, P25 Phase 2 and SmartNet trunked on two radios, Capacity Plus, Capacity Max,
conventional DMR, conventional P25 + FM) replayed from our own captures, which aren't in git. See
[captures/README.md](../../captures/README.md) for what each holds and the current reference.

`regress.sh` runs `target/release/trunk-pro` (or `BIN=`), and `cargo test` doesn't rebuild that
binary, so build it fresh first:

```bash
(cd web && npm run build)
cargo build --release -p trunk-pro
captures/regress.sh /tmp/after
captures/compare.sh captures/reference/bugfix-output /tmp/after    # IDENTICAL, or what differs
```

It keeps every call's WAV, JSON and vocoder frames, every control message and the log, counts each
scenario's instructions (`<out>.instructions`, with `perf/gate.py`), and last plays every call's
`.sdr` through MimoSDR's own player (`crates/trunk-core/tests/mimosdr.rs`), printing
`mimosdr: ok` or `FAILED`.

## The corpus

The public regression captures, listed at [trunkrecorder.pro/corpus](https://trunkrecorder.pro/corpus)
and described in [corpus/README.md](../../corpus/README.md):

```bash
cargo build --release -p trunk-pro
target/release/trunk-pro tool corpus run --out /tmp/after               # fails on a regression
target/release/trunk-pro tool corpus run --out /tmp/after --bless next  # a change that moves results
```

`tool corpus` downloads what it doesn't have into `captures/.corpus`. The `corpus` workflow runs it
on pull requests that touch `crates/trunk-core`, `crates/trunk-app` or `corpus/` (the voice and
control tiers), and every night on all of it. Every option is in
[`tool corpus`](tools.md#tool-corpus).

## Weak-signal curves

[`tool snr`](tools.md#tool-snr) takes one strong channel from a capture and decodes it again with
noise added at each SNR, counting how much of the clean run still comes through. It's how receiver
changes are compared:

```bash
trunk-pro tool snr <capture.cu8> --center Hz --rate Hz --freq Hz --kind dmr|p25|smartnet|p2
```

## CPU cost

`perf/gate.py` counts the instructions each stage takes, and CI's `perf` job fails when one is more
than 2 % above `perf/baseline.json`. How to compare two builds locally and when to update the
baseline: [CPU cost](README.md#cpu-cost).
