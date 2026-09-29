# Trunk Recorder Lite

A lightweight, self-contained trunked-radio recorder: point one or more
RTL-SDRs at a **P25** system and it follows the control channel and records
every call it can hear as WAV + Trunk Recorder–compatible JSON. It is written
in Rust, with no GNU Radio or OP25 dependency; a desktop build (macOS, Linux,
Windows), a browser build and a browser-based interface for both are the goal.

**Status:** the desktop app works end to end — live RTL-SDR input (one or
several dongles), decoding, recording, and a browser interface — on macOS,
with builds for Linux and Windows. The browser version runs the same engine as
WebAssembly. Phase 2 is next (see [Roadmap](#roadmap)). The previous TypeScript/browser implementation lives in
[`archive/ts-engine`](archive/ts-engine) and serves as a reference.

## Run it

Download a release binary (a single file, ~3 MB, the interface built in), or
build it. Then:

```bash
trunk-lite          # opens http://localhost:8080 — set up the system, press Start
```

Set the control channels and your dongle(s) in the browser, press **Start**.
Calls are written to the recordings folder (default `~/TrunkRecorderLite`) as
`<system>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>.wav|json`, Trunk
Recorder's layout and JSON fields; the interface shows live status, a
waterfall per dongle, active calls (listen live) and recent recordings. The
config lives in `~/Library/Application Support/trunk-lite/` (macOS),
`%APPDATA%\trunk-lite\` (Windows) or `~/.config/trunk-lite/` (Linux).

Dongle setup: **macOS** works as-is. **Windows** needs the WinUSB driver for
the dongle (Zadig), as every RTL-SDR app. **Linux** needs USB access: install
`packaging/linux/60-trunk-lite-rtlsdr.rules` into `/etc/udev/rules.d/` (the
kernel DVB driver is detached automatically).

`trunk-lite devices` lists dongles; `trunk-lite capture out.cu8 --freq Hz
--serial SN --seconds 30` records raw IQ like `rtl_sdr`.

### In the browser (no install)

The browser version is the same engine compiled to WebAssembly, running in a
Web Worker, with the dongle over WebUSB (Chrome or Edge) and calls kept in the
browser's private storage (OPFS); **Export to folder…** copies them out in
Trunk Recorder's layout. Serve `web/dist-web` (or the `-browser.zip` release)
from any static web server over HTTPS or on `localhost` — WebUSB and module
workers don't run from `file://`. Press **Connect…** on a dongle source to pick
it. The desktop app is the better choice for several dongles or long
unattended runs.

## Build

Needs Rust 1.82+ and Node 20+ (for the interface).

```bash
(cd web && npm ci && npm run build)     # the interface → web/dist, embedded in the binary
cargo build --release                   # target/release/trunk-lite
cargo test --release
cargo build --profile dist              # stripped, as released
```

`web/`: `npm run dev` serves the interface with hot reload on :5173, talking to
a running `trunk-lite` on :8080.

The browser version additionally needs the `wasm32-unknown-unknown` target and
`wasm-bindgen-cli` at the version in `Cargo.lock` (0.2.129):

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
(cd web && npm run wasm && npm run build:web)   # → web/dist-web
(cd web && npm run dev:web)                     # hot reload, engine in the page
```
 Releases are built by
`.github/workflows/build.yml` (macOS universal, static Linux x86-64 / ARM64,
Windows).

## Replay captures

```bash
# Record from an rtl_sdr capture (unsigned 8-bit IQ):
rtl_sdr -f 858300000 -s 2400000 -g 38.6 -n 72000000 capture.cu8        # 30 s
./target/release/trunk-lite replay capture.cu8 --center 858300000 --rate 2400000 \
    --cc 857987500 --out calls/ [--talkgroups tg.csv] [--bandplan site.bandplan]

# Several dongles on one system (control channel on either):
./target/release/trunk-lite replay --source a.cu8,858300000,2400000 \
    --source b.cu8,860700000,2400000 --cc 857987500 --out calls/
```

Calls are written as `<talkgroup>-<epoch>_<freq>.wav|json` with Trunk
Recorder's JSON fields. `--bandplan` keeps the system's IDEN tables between
runs, so a grant heard before the next IDEN broadcast can be followed at once.

## How it works

```
source u8 IQ ─► Channelizer (one shared FFT, N channels, 1 s pre-roll history)
   ├─ control channel ─► receiver bank ─► framer ─► TSDU ─► TSBKs ─► parser ─► CallManager
   └─ voice channels  ─► receiver bank ─► framer ─► LDUs  ─► soft FEC ─► IMBE ─► audio
receiver bank = CQPSK + CQPSK with a T/2 CMA equaliser + C4FM, best of each frame
```

| Path | What |
|---|---|
| `crates/trunk-core` | Platform-independent core (native and WebAssembly): no I/O, one dependency (`rustfft`) |
| `…/dsp/channelizer.rs` | Overlap-save multi-head channelizer (after CyberEther's `filter_engine`), pre-roll, waterfall spectrum |
| `…/dsp/cqpsk.rs`, `c4fm.rs` | Streaming receivers with soft bits; optional CMA equaliser |
| `…/p25/frame.rs` | Framer with flywheel sync and NID recovery |
| `…/p25/tsbk.rs` | Viterbi (soft) trellis decoder + CRC — 98–99 % of TSBKs on simulcast, vs 62 % for op25's greedy decoder |
| `…/p25/fec.rs`, `voice.rs` | Golay / Hamming (hard and soft), Reed–Solomon, IMBE framing, LC / ES / HDU / TDULC |
| `…/p25/diversity.rs` | Receiver diversity: per-frame best of several receivers |
| `…/mbe/` | IMBE vocoder (mbelib + Trunk Recorder's enhanced synthesis) |
| `…/trunk/` | TSBK parser (Trunk Recorder's `p25_parser.cc`), call manager (`monitor_systems.cc`), voice call tracker, engine (multi-source) |
| `crates/trunk-lite` | The app: `serve` (default; source threads, engine thread, web server + WebSocket), `replay`, `capture`, `devices`, `tool` |
| `…/src/sdr.rs` | RTL-SDR over USB via `rtlsdr-nusb` (pure Rust; no libusb / librtlsdr) |
| `crates/trunk-app` | The app layer shared by desktop and browser: config and a recording `Session` (status, spectrum, log, calls, files) |
| `crates/trunk-web` | The browser build: `Session` and the RTL-SDR driver (WebUSB) exported to JavaScript with `wasm-bindgen` |
| `web/` | The browser interface (React + Vite), embedded in the binary; `src/web/` runs the engine in a worker for the browser version |
| `research/native-bench` | Benchmarks, the C++ prototype, synthetic simulcast ground truth, comparison scripts — see its `RESULTS.md` |
| `scripts/gen_tables.ts` | Regenerates `trunk-core/src/tables.rs` from the archived sources |

## Verified

Against the archived TypeScript engine, the C++ prototype and Trunk Recorder,
on synthetic P25 (with ground truth) and on real air (a CQPSK simulcast site,
NAC 0x443, from an R820T RTL-SDR):

- **Bit-exact** with the TypeScript decoders on identical input: IMBE / LC / ES
  / HDU / TDULC decoding (0 mismatches) and vocoder audio (max difference 0).
- **Same results as the C++ prototype:** identical TSBK sets on real air
  (1149 / 2354 of 1149 / 2354), identical ground-truth scores, the same calls
  and durations.
- **Simulcast, synthetic ground truth** (two transmitters, 40 µs, 0.7 echo):
  100 % of TSBKs and 0 wrong voice codewords.
- **Real air vs Trunk Recorder on the same capture:** more control messages
  decoded (1156 vs ~282) and more audio per call (e.g. TG 102 9.5 s vs 8.1 s).
- **CPU:** 1.6 % of one core for a 2.4 MSPS site (CC + 2 voice channels,
  three receivers each); 6 % for 8 MSPS with 16 simultaneous calls.
- **Browser (WebAssembly, Chrome):** the same calls as the native build on the
  same capture (identical lengths; one bit-exact, the other within ±2 LSB from
  floating-point rounding), 7 % of a core in real time, 30 s of air decoded in
  1.5 s.

## Roadmap

1. ~~Rust core: channelizer, receivers, P25 Phase 1, vocoder, trunking~~ — done
2. ~~Verification against TS, C++ and Trunk Recorder~~ — done
3. ~~Live input (pure-Rust USB, several dongles), desktop app with embedded
   web server and browser interface, single-binary builds for macOS, Linux
   (x86-64, ARM), Windows~~ — done (verified live on macOS; Linux and Windows
   binaries build, hardware testing there pending)
4. ~~Web build: the same core as WebAssembly in a Web Worker, WebUSB, OPFS
   storage, the same interface~~ — done (verified on captures; live WebUSB
   needs a hands-on test)
5. Phase 2 TDMA voice (H-DQPSK, AMBE+2) — feature parity with the archive
6. Release packaging (prebuilt binaries); optional USRP support via UHD (C++,
   an opt-in build feature)

## License

GPL-3.0-or-later (the P25 code derives from op25). The vocoder derives from
mbelib (ISC).
