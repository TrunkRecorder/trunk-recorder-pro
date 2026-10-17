# Removing the performance report

The performance report (Platform → **Download performance report**, `perf-report.json`, the
`trunk_core::prof` stage timers) was added to diagnose users whose recorder ran out of CPU. It is
temporary. It was merged as **one self-contained commit** so it can be taken out with a single
`git revert`, leaving everything that was built with its help (per-source decoding threads, NXDN
decimation, P25 modulation detection, faster MSD) in place.

## The commit

| | |
|---|---|
| Subject | `A performance report: where the recorder's time goes, stage by stage` |
| Hash | `df84fde` (on `main` via the merge of branch `per-source-threads`; hashes kept, not squashed) |

Find it by subject if the hash is ever in doubt (an earlier version of the same commit, `fa6cce9`,
lives only on the old `stage-timing` branch and is **not** on `main`; don't revert that one):

```sh
git log --oneline --grep="A performance report: where the recorder's time goes" main
```

## Steps

```sh
git switch main && git pull
git switch -c remove-perf-report
git revert --no-edit df84fde
```

If the revert stops on conflicts, later work touched the same lines. The commit only **adds**
code (spans, a module, a route, a UI card, a docs section), so each conflict resolves the same way:
keep the later code and drop the monitoring lines. What the commit added, file by file:

| File | What to remove |
|---|---|
| `crates/trunk-core/src/prof.rs` | The whole file (stage timers). |
| `crates/trunk-core/src/lib.rs` | `pub mod prof;` |
| `crates/trunk-core/src/dsp/channelizer.rs` | `prof::span(...)` guards (`ChzInput`, `ChzFft`, `ChzHead`, `ChzPreroll`), `use crate::prof`, and `head_count()`. |
| `crates/trunk-core/src/mbe/mod.rs` | The two `prof::span` lines at the top of `Decoder::imbe` / `Decoder::ambe`. |
| `crates/trunk-core/src/trunk/conventional.rs` | `prof::span` guards (`ConvScan`, `ConvFm`/`P25`/`Dmr`/`Nxdn`) and `Conventional::load()`. |
| `crates/trunk-core/src/trunk/engine.rs` | `use crate::prof`, the voice-kind span in `Radio::run_channel`, the control span in `BlockDsp::decode_control`, the `TrunkCalls` span in `Core::after_block`, `prof::source` guards and the `Gap` span in `Engine::push_u8` / `push_iq` / `push_gap`, `struct SourceLoad`, and `Engine::load()`. |
| `crates/trunk-core/src/trunk/mod.rs` | `SourceLoad` in the `pub use engine::{…}` list. |
| `crates/trunk-app/src/session.rs` | `use trunk_core::prof`, the `SessionPush` / `SessionPoll` / `SessionSpectrum` / `SessionStatus` spans and `prof::source` guards, and `Session::load()`. |
| `crates/trunk-pro/src/perf.rs` | The whole file (sampler, report, OS thread CPU). |
| `crates/trunk-pro/src/main.rs` | `mod perf;`, `perf::enable();` in `serve`, `perf::start(ctx.clone());`. |
| `crates/trunk-pro/src/runtime.rs` | `use trunk_core::prof`, `crate::perf::received(...)` in `feed_source`, `recording_started` / `Ticker` / `recording_stopped` and the `fin_queued` / `fin_inline` counters in `engine_thread`, the `Housekeeping` / `Deliver` spans, and the `FinishWrite` / `FinishEncode` spans in `finish_one`. |
| `crates/trunk-pro/src/server.rs` | The `/perf-report.json` route and `async fn perf_report`. |
| `docs/troubleshooting.md` | The **Performance report** section and the two links to it (under *Dropped samples* and *Reporting a bug*). |
| `web/src/dash/Platform.tsx` | The **Performance report** card, and `web` in the `controller.ts` import if nothing else uses it. |

## Check

```sh
grep -rn "prof::\|perf::\|perf-report\|SourceLoad\|Performance report" crates web/src docs   # only research/ should mention it
cargo check --workspace --tests
cargo check -p trunk-web --target wasm32-unknown-unknown
cargo test --release --workspace
(cd web && npm run build)
```

`captures/regress.sh` output must be byte-identical before and after: the timers never ran in
`replay` (no clock is set there), so removing them changes no decode.

```sh
cargo build --release && cp target/release/trunk-pro /tmp/with
# … revert, rebuild …
cargo build --release && cp target/release/trunk-pro /tmp/without
(cd captures && BIN=/tmp/with ./regress.sh /tmp/r-with && BIN=/tmp/without ./regress.sh /tmp/r-without)
diff -r -x '*.stdout' /tmp/r-with /tmp/r-without && echo identical
```

Then add a line to `CHANGELOG.md` under `## [Unreleased]` (for example "Removed: the performance
report") and open a pull request.

## What stays (don't remove)

These came with or after the report but are not part of it:

- `crates/trunk-core/src/trunk/bench.rs` and `crates/trunk-core/src/nxdn/bench.rs`: ignored
  benchmark tests of decoder cost, independent of `prof`.
- The `cqpsk-only` and `nodetect` variants of `trunk-pro tool snr`, and `tool cc --detect 0|1`.
- `research/perf-report-2026-10-16.md`: the analysis of the first user report, kept as history.
