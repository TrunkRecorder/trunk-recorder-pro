# Development

How to build Trunk Recorder Pro from source, make a release, and find your way around the code.
For how the recorder works inside, see [Architecture](architecture.md); for the interface API,
[api/README.md](api/README.md); for plugins, [Writing a plugin](plugins/writing-plugins.md).

## Building

Needs Rust 1.95+ and Node 20+ (for the interface).

```bash
(cd web && npm ci && npx vite build)    # the interface → web/dist, embedded in the binary
cargo build --release                   # target/release/trunk-pro
cargo test --release
cargo build --profile dist              # stripped, as released
```

Release packages: `packaging/package.sh <macos|linux-x86_64|linux-aarch64|windows-x86_64|browser> <version> <binary> <out-dir>`
(what CI runs; see the top of the script).

## Releasing

A release is a commit that bumps the version, and a tag on it. With
everything for the release already pushed to `main`:

1. **`Cargo.toml`**: `[workspace.package] version` (`0.1.3` → `0.1.4`).
   `trunk-core`, `trunk-app`, `trunk-pro` and `trunk-web` take it from
   there; the plugin SDK (`trunk-recorder-plugin`) has its own version
   (below).
2. **`Cargo.lock`**: refresh it with `cargo check`. Four entries change
   (the four crates above).
3. **`CHANGELOG.md`**: rename `## [Unreleased]` to `## [<version>] — <date>`,
   or add that section under it, and leave an empty `## [Unreleased]` on
   top. The release notes are this section, copied as it is, so write it
   for users: what's new, what changed, and anything they must change
   (config keys, say) first, in bold.
4. Commit as `v<version>`, tag, push both:

   ```bash
   git commit -am "v0.1.4"
   git tag -a v0.1.4 -m v0.1.4
   git push origin main v0.1.4
   ```

Then `.github/workflows/build.yml` takes over. It stops if the tag doesn't
match `Cargo.toml`. It builds the packages (macOS DMG and CLI, Linux x86-64 /
ARM64, Windows, browser) and publishes the GitHub release with `SHA256SUMS`
and the changelog section. It then tells
[trunkrecorder.pro](https://trunkrecorder.pro) to rebuild its download links
and `/app/` (`SITE_DEPLOY_TOKEN`; without the token the site still finds the
release within the hour). A tag with a suffix (`v0.1.4-rc.1`) makes a
prerelease and leaves the site alone. Watch it with `gh run watch`.

The same tag starts `.github/workflows/docker.yml`, which builds the Docker
image for linux/amd64 and linux/arm64 and pushes it to Docker Hub as
`robotastic/trunk-recorder-pro` with the tags `<version>`, `<major.minor>` and
`latest` (a prerelease gets only its own tag). Every push to `main` also
publishes `edge`. It needs the `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN`
repository secrets; without them the image is built but not pushed.

If CI fails, fix it on `main` and move the tag:
`git tag -fa v0.1.4 -m v0.1.4 && git push -f origin v0.1.4`. Do this only
before anyone has downloaded the release.

The plugin SDK is released apart from the app, only when its API changes:
bump `crates/trunk-recorder-plugin/Cargo.toml`'s `version`, refresh
`Cargo.lock`, commit, and `cargo publish -p trunk-recorder-plugin`. Plugins
pick it up from crates.io. `API_VERSION`
(`crates/trunk-recorder-plugin/src/protocol.rs`) changes only when a plugin
built for the old one would break.

`web/`: `npm run dev` serves the interface with hot reload on :5173, talking to
a running `trunk-pro` on :8080.

The browser version additionally needs the `wasm32-unknown-unknown` target and
`wasm-bindgen-cli` at the version in `Cargo.lock` (0.2.129):

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
(cd web && npm run wasm && npm run build:web)   # → web/dist-web
(cd web && npm run dev:web)                     # hot reload, engine in the page
```

`npm run build` type-checks the browser version too, so it fails until the
WebAssembly engine (`web/src/web/pkg`) has been built; `npx vite build` builds
the interface alone.

## Code layout

```
radio (RTL-SDR, USRP, Airspy, SoapySDR, capture file) ─► IQ
  ─► Channelizer: one shared FFT per radio, a "head" per channel, 1 s of pre-roll history
      ├─ control channels ─► receivers ─► framer ─► TSBK (P25) / OSW (SmartNet) / CSBK (DMR) / CAC (NXDN)
      │                                         ─► Message ─► call manager (grants, timeouts)
      ├─ voice channels, opened per grant with pre-roll
      │     ─► receivers ─► framer ─► voice tracker ─► IMBE / AMBE+2 vocoder ─► 8 kHz audio
      └─ conventional channels: energy in the shared spectrum ─► open with pre-roll ─► FM / P25 / DMR / NXDN voice
  ─► call ends ─► best copy across sites ─► WAV + Trunk Recorder JSON ─► M4A, plugins
P25 receiver bank = CQPSK + CQPSK with a T/2 CMA equaliser + C4FM, best of each frame
```

[Architecture](architecture.md) explains the whole path, the
threads and how the pieces connect.

| Path | What |
|---|---|
| `crates/trunk-core` | Platform-independent core (native and WebAssembly): no I/O; depends on `rustfft`, `num-complex` and `regex-lite` only |
| `…/dsp/channelizer.rs` | Overlap-save multi-head channelizer (after CyberEther's `filter_engine`), pre-roll, waterfall spectrum |
| `…/dsp/cqpsk.rs`, `c4fm.rs`, `msd.rs` | Streaming receivers with soft bits; optional CMA equaliser; multi-symbol detector |
| `…/dsp/fm.rs` | Narrowband FM: channel filter / carrier meter, discriminator, de-emphasis, 8 kHz audio, CTCSS high-pass, squelch gate |
| `…/dsp/tones.rs`, `signalling.rs` | Analog calls: which CTCSS tone / DCS code they carry; MDC1200 / FleetSync unit IDs |
| `…/p25/frame.rs` | Framer with flywheel sync and NID recovery |
| `…/p25/tsbk.rs` | Viterbi (soft) trellis decoder + CRC — 98–99 % of TSBKs on simulcast, vs 62 % for op25's greedy decoder |
| `…/p25/fec.rs`, `voice.rs` | Golay / Hamming (hard and soft), Reed–Solomon, IMBE framing, LC / ES / HDU / TDULC |
| `…/p25/diversity.rs` | Receiver diversity: per-frame best of several receivers |
| `…/p25/phase2.rs` | Phase 2 TDMA: slot framer, scrambler, ISCH / DUID, AMBE codeword FEC, ESS, MAC PDUs |
| `…/smartnet/` | SmartNet: 3600 baud receiver, OSW framing and decoding, band plans |
| `…/dmr/` | DMR: burst framing, slots, FEC, link control, CSBKs, trunking (Capacity Plus / Max, Connect Plus, Tier III) |
| `…/nxdn/` | NXDN: frame sync and scrambler, LICH, the convolutional / CRC channel coding (SACCH, FACCH1, UDCH, CAC, SCCH), layer 3 messages, voice, Type-C and Type-D trunking, a synthesizer for tests |
| `…/mbe/` | IMBE and AMBE+2 vocoders (mbelib + Trunk Recorder's enhanced synthesis) |
| `…/trunk/` | Control-channel messages (Trunk Recorder's `p25_parser.cc`), call manager (`monitor_systems.cc`), Phase 1, TDMA and DMR voice tracking, conventional channels, multi-site dedupe, the engine (several radios and systems) |
| `…/survey.rs` | **Find my system**: band scan, control-channel check, band plan and ppm |
| `crates/trunk-app` | The app layer shared by desktop and browser: config, the conventional channel CSV (`channels.rs`), file names, and a recording `Session` (status, spectrum, log, calls, files) |
| `crates/trunk-pro` | The desktop app: `serve` (default; radio threads, engine thread, web server + WebSocket), `replay`, `capture`, `devices`, `survey`, `tool`, `plugin` |
| `…/src/sdr.rs` | RTL-SDR over USB via `rtlsdr-nusb` (pure Rust; no libusb / librtlsdr) |
| `…/src/radio/` | USRP (UHD's C API), Airspy (libairspy) and SoapySDR, loaded at run time when installed |
| `…/src/plugins/` | The plugin host, store and command line |
| `crates/trunk-recorder-plugin` | The plugin protocol, and a Rust SDK for writing plugins (on crates.io) |
| `crates/trunk-web` | The browser build: `Session` and the RTL-SDR driver (WebUSB) exported to JavaScript with `wasm-bindgen` |
| `web/` | The browser interface (React + Vite), embedded in the binary; `src/web/` runs the engine in a worker for the browser version |
| `research/native-bench` | Benchmarks, the C++ prototype, synthetic simulcast ground truth, comparison scripts — see its `RESULTS.md` |

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
- **CPU, live against Trunk Recorder** (same radios, one at a time, 30 min
  each): on a Raspberry Pi 5 (Linux, P25 on one RTL-SDR) 4–6× less while
  recording (5.0 % of a core vs 20.3 % with one call, 6.2 % vs 35.6 % with
  three); on macOS 17–20× less (P25 on a USRP at 8 MSPS 11.7 % vs 201 %,
  SmartNet on two RTL-SDRs 7.6 % vs 149 %), where Trunk Recorder's thread
  wake-ups cost far more. Each call adds under 1 point against Trunk
  Recorder's 8–23. Details, charts and method: [Performance](performance.md).
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
