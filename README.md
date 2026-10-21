# Trunk Recorder Pro

Trunk Recorder Pro records the calls on trunked and conventional radio systems. Point one or more
SDRs at a system and it follows the control channel and records every call it can hear, as WAV
files with Trunk Recorder–compatible JSON beside them. Plugins upload the calls to OpenMHz,
Broadcastify Calls or Rdio Scanner, stream them, or hand them to a script of yours.

It is a rewrite of [Trunk Recorder](https://github.com/TrunkRecorder/trunk-recorder) in Rust, with
no GNU Radio or OP25 dependency. It is a single program of a few megabytes, with a browser
interface built in, and runs on macOS, Linux (including the Raspberry Pi), Windows and Docker.
The same engine also runs in the browser with nothing to install. On a Raspberry Pi 5 it uses
4–6× less CPU than Trunk Recorder on the same system ([Performance](docs/performance.md)).

**It records:**

- **P25**: Phase 1 and Phase 2 (TDMA) voice, C4FM and CQPSK / simulcast control channels
- **SmartNet / SmartZone**: 800 MHz, 900 MHz and VHF / UHF (OBT) band plans, analog and digital
  voice
- **Trunked DMR**: Capacity Plus (and Linked Capacity Plus), Capacity Max, Connect Plus, Tier III
- **NXDN**: NXDN48 and NXDN96, Type-C and Type-D. Tested on recordings and synthesized signals;
  not yet on a live trunked system.
- **EDACS**: Standard and Networked sites, analog and ProVoice voice. Analog calls tested live on
  a three-site networked system; ProVoice on synthesized signals.
- **Conventional channels**: analog FM and AM (airband), P25, DMR and NXDN, split by CTCSS / DCS tone, NAC, colour
  code or RAN
- Several systems and several sites at once; a call heard on more than one site is saved once,
  from the best copy

**With:** RTL-SDR dongles (nothing else to install), Ettus USRPs, Airspy R2 / Mini, and anything
with a SoapySDR module (HackRF, SDRplay, LimeSDR…).

## Install

Download the package for your system from the
[releases](https://github.com/TrunkRecorder/trunk-recorder-pro/releases).

| Platform | Package | Instructions |
|---|---|---|
| macOS 11+ (Apple silicon and Intel) | `trunk-pro-<version>-macos.dmg` | [Install on macOS](docs/install/macos.md) |
| Windows 10 / 11 (64-bit) | `trunk-pro-<version>-windows-x86_64.zip` | [Install on Windows](docs/install/windows.md) |
| Linux (x86-64, ARM64) | `trunk-pro-<version>-linux-<arch>.tar.gz` | [Install on Linux](docs/install/linux.md) |
| Raspberry Pi 4 / 5 (64-bit OS) | `trunk-pro-<version>-linux-aarch64.tar.gz` | [Install on a Raspberry Pi](docs/install/raspberry-pi.md) |
| Docker (Linux hosts) | [`robotastic/trunk-recorder-pro`](https://hub.docker.com/r/robotastic/trunk-recorder-pro) | [Run in Docker](docs/install/docker.md) |
| Chrome or Edge | `trunk-pro-<version>-browser.zip`, or [trunkrecorder.pro/app](https://trunkrecorder.pro/app/) | [Run in the browser](docs/install/browser.md) |

USRP, Airspy and SoapySDR radios need their makers' drivers: see
[Other radios](docs/install/other-radios.md).

## Quick start

```bash
trunk-pro          # opens http://localhost:8080
```

1. The setup guide opens the first time. Pick your radios (or import an existing Trunk Recorder
   `config.json` instead).
2. Let **Find my system** scan for a control channel, or enter the system by hand.
3. Press **Start**. Calls are saved to `~/TrunkRecorderPro/<system>/<year>/<month>/<day>/`.

[Getting started](docs/getting-started.md) walks through it step by step, including how to
research a system on RadioReference and set gain and frequency correction.

## Documentation

**Start here:** [docs/README.md](docs/README.md) is the full index.

| | |
|---|---|
| **Setting up** | [Getting started](docs/getting-started.md) · [Radios](docs/guides/radios.md) · [Find my system](docs/guides/find-my-system.md) |
| **Radio systems** | [P25](docs/guides/p25.md) · [SmartNet](docs/guides/smartnet.md) · [DMR](docs/guides/dmr.md) · [NXDN](docs/guides/nxdn.md) · [Several systems and sites](docs/guides/multi-site.md) · [Talkgroups and units](docs/guides/talkgroups-and-units.md) |
| **Conventional** | [Conventional channels](docs/guides/conventional.md) · [Tones, NACs and colour codes](docs/guides/tones.md) |
| **Using it** | [The interface](docs/guides/interface.md) · [The dashboard](docs/guides/dashboard.md) · [Recordings](docs/guides/recordings.md) · [Troubleshooting](docs/troubleshooting.md) |
| **Plugins** | [Using plugins](docs/plugins/README.md) · [OpenMHz](docs/plugins/openmhz.md) · [Broadcastify](docs/plugins/broadcastify.md) · [Rdio Scanner](docs/plugins/rdioscanner.md) · [simplestream](docs/plugins/simplestream.md) · [Upload script](docs/plugins/upload-script.md) |
| **Reference** | [Configuration](docs/configuration/README.md) · [Command line](docs/command-line.md) |
| **Coming from Trunk Recorder** | [Importing your config, and what's different](docs/migrating-from-trunk-recorder.md) |
| **Developers** | [Development](docs/development.md) (building, releasing, code layout) · [Architecture](docs/architecture.md) · [Interface API](docs/api/README.md) · [Writing a plugin](docs/plugins/writing-plugins.md) · [Performance](docs/performance.md) |

## Building from source

Rust 1.95+, Node 20+, the WebAssembly target and `wasm-bindgen-cli` at the version in
`Cargo.lock` (0.2.129). The interface runs the recorder's own config rules as WebAssembly, so that
is built first: `npm run wasm` makes `web/src/web/pkg`, which isn't in the repository.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
(cd web && npm ci && npm run wasm)      # the WebAssembly rules -> web/src/web/pkg
(cd web && npm run build)               # the interface -> web/dist, embedded in the binary
cargo build --release                   # target/release/trunk-pro
```

Build the interface before the binary, which embeds it when compiled. Without `npm run wasm` the
interface build stops at `Could not resolve "./web/pkg/trunk_web.js"`.
[Building from source](docs/install/README.md#building-from-source) has more detail;
[Development](docs/development.md) covers the browser build, packaging and making a release.

## Help

Questions, bug reports and reports from systems we haven't tested on (NXDN especially) are
welcome as [GitHub issues](https://github.com/TrunkRecorder/trunk-recorder-pro/issues).
[Troubleshooting](docs/troubleshooting.md) says what to include.

## License

GPL-3.0-or-later (the P25 code derives from op25). The vocoder derives from mbelib (ISC).
