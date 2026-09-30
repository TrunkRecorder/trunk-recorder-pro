# Trunk Recorder Lite

A lightweight, self-contained trunked-radio recorder: point one or more
RTL-SDRs (or, optionally, USRPs and Airspys) at a **P25** system (Phase 1 and Phase 2 TDMA voice) and it follows the control channel and records
every call it can hear as WAV + Trunk Recorder–compatible JSON. It also records
**conventional channels** — analog FM and P25 — alongside a trunked system or
on their own (see [Conventional channels](#conventional-channels)). It is written
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
  distribution from about 2019 on — glibc 2.28 or newer, e.g. Debian 10,
  Ubuntu 20.04, RHEL 8, Raspberry Pi OS bullseye):

  ```bash
  tar xzf trunk-lite-<version>-linux-x86_64.tar.gz
  cd trunk-lite-<version>-linux-x86_64 && sudo ./install.sh
  trunk-lite
  ```

  `install.sh` puts `trunk-lite` in `/usr/local/bin`, adds a udev rule so
  your user can open RTL-SDRs, Airspys and USB USRPs (the kernel's DVB driver is detached
  automatically) and a menu entry. `trunk-lite.service` in the package runs it
  headless as a systemd user service.
- **Browser**, no install: see [In the browser](#in-the-browser-no-install).

### USRP and Airspy (optional)

RTL-SDRs work with nothing else installed. USRPs (Ettus / NI, through UHD)
and Airspy R2 / Mini (through libairspy) use their makers' drivers, which you
install yourself; Trunk Recorder Lite finds them when it starts — no special
build — and offers **USRP** and **Airspy** as source types in Setup (which
says what is missing if a driver isn't found).

| | macOS | Debian / Ubuntu / Raspberry Pi OS | Windows |
|---|---|---|---|
| USRP | `brew install uhd` | `sudo apt install libuhd-dev uhd-host` | Ettus's UHD installer (adds `uhd.dll` to PATH) |
| Airspy | `brew install airspy` | `sudo apt install libairspy0` | `airspy.dll` from airspy-tools, next to `trunk-lite.exe` |

USRPs also need UHD's FPGA images once: `uhd_images_downloader` (sudo on
Linux). `trunk-lite devices` shows which drivers were found; `trunk-lite
devices --usrp` also searches for USRPs. A driver installed somewhere unusual
can be named with `TRUNK_LITE_UHD=/path/to/libuhd…` / `TRUNK_LITE_AIRSPY=…`.

Settings: a USRP takes UHD device arguments (blank = the first found,
`serial=…`, `addr=192.168.10.2`), any sample rate its clock supports (e.g. 8
MSPS covers ~7 MHz), a gain in dB and an antenna (e.g. `RX2`, `TX/RX`).
An Airspy runs at 10 or 2.5 MSPS (R2) or 6 or 3 MSPS (Mini), with a
linearity gain step of 0–21 and an optional bias-T. Wider sources cost more
CPU: about 1–2 % of a core per 2.4 MSPS.

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

`trunk-lite devices` lists radios; `trunk-lite capture out.cu8 --freq Hz
--serial SN --seconds 30` records raw IQ from an RTL-SDR like `rtl_sdr`.
Capture files can be `cu8` (rtl_sdr), `cs16` or `cf32` (GNU Radio, UHD's
`rx_samples_to_file`).

### In the browser (no install)

The browser version is the same engine compiled to WebAssembly, running in a
Web Worker, with the dongle over WebUSB (Chrome or Edge) and calls kept in the
browser's private storage (OPFS); **Export to folder…** copies them out in
Trunk Recorder's layout. Serve `web/dist-web` (or the `-browser.zip` release) from any static web
server, in any folder, over HTTPS or on `localhost` — WebUSB and module
workers don't run from `file://`. Press **Connect…** on a dongle source to pick
it. The desktop app is the better choice for several dongles or long
unattended runs.

## Conventional channels

Conventional channels are single frequencies — analog narrowband FM or P25 —
recorded whenever something transmits on them. Add them in the browser under
**Conventional channels** (a table, a box to paste many frequencies, or
**Import CSV…**), or in the config file:

```json
"conventional": {
  "squelchDb": 8,
  "channels": [
    { "freqHz": 154430000, "mode": "fm",  "name": "County Fire Dispatch", "talkgroup": 1001, "group": "Fire" },
    { "freqHz": 155100000, "mode": "fm",  "name": "EMS Ops" },
    { "freqHz": 460125000, "mode": "p25", "name": "PD Tac 2", "squelchDb": 12 },
    { "freqHz": 453000000, "mode": "fm",  "enabled": false }
  ]
}
```

| Field | |
|---|---|
| `freqHz` | The channel frequency, in Hz (the interface takes MHz) |
| `mode` | `fm` (analog narrowband FM, 12.5 kHz) or `p25` (P25 Phase 1, C4FM or CQPSK). Each channel has its own, so one list can mix them |
| `name`, `description`, `tag`, `group` | Written into the call JSON (Trunk Recorder's alpha tag, description, tag, category) |
| `talkgroup` | The number calls are filed under (file names, JSON, uploaders). Default: the frequency in kHz, e.g. 154430 — stable however you reorder the list. P25 channels use the talkgroup the radio sends, when it sends one |
| `squelchDb` | How far above the noise floor a signal must be to open the channel, in dB. Per channel, or for all in the section (default 8). The noise floor is measured, so this doesn't depend on the dongle or gain the way Trunk Recorder's absolute squelch does |
| `enabled` | `false` keeps a channel in the list without recording it |

A config can have a trunked system, conventional channels, or both; leave the
control channels empty for conventional only. Every enabled channel must lie
inside a source's bandwidth. The first source's center is placed
automatically when the channels (and control channels) fit in one source.

**How it works.** Channels are found by energy, like Trunk Recorder's signal
detector, but from the spectrum the channelizer already computes for every
sample — so watching a channel costs almost nothing (about 0.001 % of a core
each on an M4 Pro: 100 idle channels add about 0.1 %), and hundreds per
source are fine. When a channel's signal rises above
its squelch, a channel is opened *with pre-roll*, replaying the air from
before the detection, so the start of a transmission isn't lost. A narrower
filter then confirms the carrier, which keeps a strong neighbour's leakage
from making calls. A call ends after the call timeout (default 3 s) with no
signal, as in Trunk Recorder. Analog audio is de-emphasised and high-passed at
300 Hz, which removes CTCSS tones. Tones and NACs aren't matched yet: a channel
records whatever transmits on its frequency.

**From Trunk Recorder.** **Import CSV…** reads Trunk Recorder's channel file
(`TG Number`, `Frequency`, `Alpha Tag`, `Description`, `Tag`, `Category`,
`Enable`); add a `Mode` column (`fm` / `p25`) to mix analog and P25 in one
file. **Import Trunk Recorder config…** brings in `conventional` and
`conventionalP25` systems' `channels` lists. Squelch values aren't carried
over: Trunk Recorder's are absolute levels; here squelch is dB above the noise.

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
`.github/workflows/build.yml` (macOS universal, Linux x86-64 / ARM64 against
glibc 2.28, Windows).

## Replay captures

```bash
# Record from an rtl_sdr capture (unsigned 8-bit IQ):
rtl_sdr -f 858300000 -s 2400000 -g 38.6 -n 72000000 capture.cu8        # 30 s
./target/release/trunk-lite replay capture.cu8 --center 858300000 --rate 2400000 \
    --cc 857987500 --out calls/ [--talkgroups tg.csv] [--bandplan site.bandplan]

# Several dongles on one system (control channel on either):
./target/release/trunk-lite replay --source a.cu8,858300000,2400000 \
    --source b.cu8,860700000,2400000 --cc 857987500 --out calls/

# Conventional channels (with or without --cc); talkgroup = frequency in kHz:
./target/release/trunk-lite replay capture.cu8 --center 154500000 --rate 2400000 \
    --fm 154430000,155100000 --p25 154725000 [--squelch 8] --out calls/
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
   ├─ voice channels  ─► receiver bank ─► framer ─► LDUs  ─► soft FEC ─► IMBE ─► audio
   └─ conventional    ─► energy in the shared spectrum ─► (open with pre-roll) ─► NBFM or P25 voice
receiver bank = CQPSK + CQPSK with a T/2 CMA equaliser + C4FM, best of each frame
```

| Path | What |
|---|---|
| `crates/trunk-core` | Platform-independent core (native and WebAssembly): no I/O, one dependency (`rustfft`) |
| `…/dsp/channelizer.rs` | Overlap-save multi-head channelizer (after CyberEther's `filter_engine`), pre-roll, waterfall spectrum |
| `…/dsp/cqpsk.rs`, `c4fm.rs` | Streaming receivers with soft bits; optional CMA equaliser |
| `…/dsp/fm.rs` | Narrowband FM: channel filter / carrier meter, discriminator, de-emphasis, 8 kHz audio, CTCSS high-pass, squelch gate |
| `…/p25/frame.rs` | Framer with flywheel sync and NID recovery |
| `…/p25/tsbk.rs` | Viterbi (soft) trellis decoder + CRC — 98–99 % of TSBKs on simulcast, vs 62 % for op25's greedy decoder |
| `…/p25/fec.rs`, `voice.rs` | Golay / Hamming (hard and soft), Reed–Solomon, IMBE framing, LC / ES / HDU / TDULC |
| `…/p25/diversity.rs` | Receiver diversity: per-frame best of several receivers |
| `…/p25/phase2.rs` | Phase 2 TDMA: slot framer, scrambler, ISCH / DUID, AMBE codeword FEC, ESS, MAC PDUs |
| `…/mbe/` | IMBE and AMBE+2 vocoders (mbelib + Trunk Recorder's enhanced synthesis) |
| `…/trunk/` | TSBK parser (Trunk Recorder's `p25_parser.cc`), call manager (`monitor_systems.cc`), Phase 1 and TDMA voice trackers, conventional channels (energy detection, calls), engine (multi-source) |
| `crates/trunk-lite` | The app: `serve` (default; source threads, engine thread, web server + WebSocket), `replay`, `capture`, `devices`, `tool` |
| `…/src/sdr.rs` | RTL-SDR over USB via `rtlsdr-nusb` (pure Rust; no libusb / librtlsdr) |
| `…/src/radio/` | USRP (UHD's C API) and Airspy (libairspy), loaded at run time when installed |
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
- **USRP B200, live** (through UHD 4.9, DCFD's control channel and voice at
  858 MHz, 8 MSPS): exactly 8.000 MSPS with 0 samples dropped over 3 min,
  99.8 % of TSBKs (3183 / 6), clear calls recorded with audio matching their
  length, 3.6 % of one core.
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
6. ~~Release packaging~~ — done: macOS app (DMG), Linux tarballs with an
   installer, Windows zip, browser zip; CI builds, checksums and publishes on
   a version tag (not yet run on GitHub; Apple signing/notarization when the
   secrets are added)
7. ~~Optional USRP (UHD) and Airspy (libairspy) sources~~ — done: drivers
   loaded at run time when installed; float (`cf32` / `cs16`) captures;
   verified live on a USRP B200 (8 MSPS, 0 dropped, 99.8 % of control
   messages, 4 clear calls recorded in full, 3.6 % of a core); Airspy
   streaming not yet tested on hardware
8. ~~Conventional channels: analog NBFM and P25, energy-detected from the
   shared spectrum with pre-roll~~ — done (verified on synthetic air; live
   testing pending). Next: CTCSS / DCS tones and P25 NAC matching, so
   several users of one frequency can be told apart

## License

GPL-3.0-or-later (the P25 code derives from op25). The vocoder derives from
mbelib (ISC).
