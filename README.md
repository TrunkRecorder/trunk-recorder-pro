# Trunk Recorder Lite

A lightweight, self-contained trunked-radio recorder: point one or more
RTL-SDRs at a **P25** system (Phase 1 and Phase 2 TDMA voice) and it follows the control channel and records
every call it can hear as WAV + Trunk Recorder–compatible JSON. It is written
in Rust, with no GNU Radio or OP25 dependency; a desktop build (macOS, Linux,
Windows), a browser build and a browser-based interface for both are the goal.

**Status:** the desktop app works end to end — live RTL-SDR input (one or
several dongles), decoding, recording, and a browser interface — on macOS,
with builds for Linux and Windows. The browser version runs the same engine as
WebAssembly. Phase 2 TDMA voice is decoded too (see [Roadmap](#roadmap)). The previous TypeScript/browser implementation lives in
[`archive/ts-engine`](archive/ts-engine) and serves as a reference.

## Install

Download the package for your system from the
[releases](https://github.com/TrunkRecorder/trunk-recorder-lite/releases)
(each is a single ~3–7 MB program with the interface built in; nothing else to
install). `SHA256SUMS` lists their checksums.

- **macOS** (11+, Apple silicon and Intel): open `trunk-lite-<version>-macos.dmg`
  and drag **Trunk Recorder Lite** to Applications. Until releases are signed
  with an Apple Developer ID, macOS blocks the first launch: open it once,
  then **System Settings → Privacy & Security → Open Anyway**. The app has no
  Dock icon; it opens its interface in your browser. Open the app again to get
  back to it, and use **Quit** in the interface to stop it.
- **Windows** (10/11, 64-bit): unzip `trunk-lite-<version>-windows-x86_64.zip`
  and run `trunk-lite.exe`. SmartScreen may warn about an unrecognised app:
  **More info → Run anyway**. The console window is the app; closing it quits.
  Dongles need the WinUSB driver once, as for every RTL-SDR program: run
  [Zadig](https://zadig.akeo.ie), pick “Bulk-In, Interface (Interface 0)”,
  install WinUSB.
- **Linux** (x86-64 or ARM64, e.g. a Raspberry Pi 4/5 with a 64-bit OS; any
  distribution — the binary is static):

  ```bash
  tar xzf trunk-lite-<version>-linux-x86_64.tar.gz
  cd trunk-lite-<version>-linux-x86_64 && sudo ./install.sh
  trunk-lite
  ```

  `install.sh` puts `trunk-lite` in `/usr/local/bin`, adds a udev rule so
  your user can open RTL-SDRs (the kernel's DVB driver is detached
  automatically) and a menu entry. `trunk-lite.service` in the package runs it
  headless as a systemd user service.
- **Browser**, no install: see [In the browser](#in-the-browser-no-install).

## Run it

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

On a machine that records unattended, turn on **Start recording when the app
starts** (or run `trunk-lite --start`); Ctrl-C, SIGTERM and **Quit** all save
the calls in progress before exiting. `--bind 0.0.0.0` makes the interface
reachable from other machines — it has no login, so only on a network you
trust (or use `ssh -L 8080:localhost:8080`).

`trunk-lite devices` lists dongles; `trunk-lite capture out.cu8 --freq Hz
--serial SN --seconds 30` records raw IQ like `rtl_sdr`.

### In the browser (no install)

The browser version is the same engine compiled to WebAssembly, running in a
Web Worker, with the dongle over WebUSB (Chrome or Edge) and calls kept in the
browser's private storage (OPFS); **Export to folder…** copies them out in
Trunk Recorder's layout. Serve `web/dist-web` (or the `-browser.zip` release) from any static web
server, in any folder, over HTTPS or on `localhost` — WebUSB and module
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

Release packages: `packaging/package.sh <macos|linux-x86_64|linux-aarch64|windows-x86_64|browser> <version> <binary> <out-dir>`
(what CI runs; see the top of the script). Tagging `v<version>` (the
version in `Cargo.toml`, with a matching section in `CHANGELOG.md`) makes CI
publish a release.

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
Recorder's JSON fields (Phase 2 TDMA calls as `…_<freq>.<slot>.wav`; the
scrambler seed — WACN, System ID, NAC — comes from the control channel, no
setup needed). `--bandplan` keeps the system's IDEN tables between
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
| `…/p25/phase2.rs` | Phase 2 TDMA: slot framer, scrambler, ISCH / DUID, AMBE codeword FEC, ESS, MAC PDUs |
| `…/mbe/` | IMBE and AMBE+2 vocoders (mbelib + Trunk Recorder's enhanced synthesis) |
| `…/trunk/` | TSBK parser (Trunk Recorder's `p25_parser.cc`), call manager (`monitor_systems.cc`), Phase 1 and TDMA voice trackers, engine (multi-source) |
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
- **Phase 2 TDMA, real air** (DCFD's 770 MHz channels): bit-exact with the
  TypeScript decoder (10,588 AMBE codewords, 3,741 MAC PDUs, vocoder audio
  max difference 0), the same receiver performance (82–97 % of codewords
  clean, per channel), 0.85 % of a core per channel; clear calls recorded
  with every voice frame on air.
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
5. ~~Phase 2 TDMA voice (H-DQPSK, AMBE+2) — feature parity with the archive~~
   — done (verified on real air from captures)
6. ~~Release packaging~~ — done: macOS app (DMG), static Linux tarballs with an
   installer, Windows zip, browser zip; CI builds, checksums and publishes on
   a version tag (not yet run on GitHub; Apple signing/notarization when the
   secrets are added)
7. Optional USRP support via UHD (C++, an opt-in build feature)

## License

GPL-3.0-or-later (the P25 code derives from op25). The vocoder derives from
mbelib (ISC).
