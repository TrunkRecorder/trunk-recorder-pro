# Installing Trunk Recorder Pro

This page helps you pick a way to run Trunk Recorder Pro and says what kind of computer and radio
you need. The platform pages have the steps.

## Ways to run it

Trunk Recorder Pro is one program, `trunk-pro`, with its web interface built in. There is nothing
else to install for RTL-SDR dongles: no GNU Radio, no OP25, no driver libraries. You start it, it
opens `http://localhost:8080` in your browser, and you set everything up from there.

| You have | Use | Page |
|---|---|---|
| A Mac (macOS 11 or newer, Apple silicon or Intel) | The app in a DMG | [macOS](macos.md) |
| A Windows 10/11 PC (64-bit, x86-64) | A zip with `trunk-pro.exe` | [Windows](windows.md) |
| A Linux PC or server (x86-64 or ARM64) | A tarball with an installer | [Linux](linux.md) |
| A Raspberry Pi | The Linux ARM64 tarball | [Raspberry Pi](raspberry-pi.md) |
| A Linux machine that runs everything in Docker | The Docker image | [Docker](docker.md) |
| Just a browser (Chrome or Edge) and a dongle | The browser version, no install | [In the browser](browser.md) |

All of the downloads are on the
[releases page](https://github.com/TrunkRecorder/trunk-recorder-pro/releases). Each release has:

| File | What it is |
|---|---|
| `trunk-pro-<version>-macos.dmg` | The macOS app (universal: Apple silicon and Intel) |
| `trunk-pro-<version>-macos-universal.tar.gz` | The same program as a bare command-line binary, for scripts and servers |
| `trunk-pro-<version>-linux-x86_64.tar.gz` | Linux on 64-bit Intel/AMD |
| `trunk-pro-<version>-linux-aarch64.tar.gz` | Linux on 64-bit ARM: Raspberry Pi with a 64-bit OS, ARM servers |
| `trunk-pro-<version>-windows-x86_64.zip` | Windows, 64-bit |
| `trunk-pro-<version>-browser.zip` | The browser (WebAssembly) version, for any static web server |
| `SHA256SUMS` | Checksums of all of the above |

Every package also carries `README.md`, `LICENSE` and `THIRD-PARTY-NOTICES.txt`.

To check a download against the checksums (Linux or macOS):

```bash
sha256sum -c SHA256SUMS --ignore-missing      # macOS: shasum -a 256 -c SHA256SUMS --ignore-missing
```

### Desktop app or browser?

The browser version runs the same decoding engine, compiled to WebAssembly, inside a browser tab.
It is a quick way to try Trunk Recorder Pro, but it only drives RTL-SDRs, keeps calls inside the
browser, has no plugins, and stops when the tab is closed or put to sleep. For anything you want
to leave running, or for more than one dongle, use the desktop app. [In the browser](browser.md)
lists the differences.

## What you need

### A radio

- **RTL-SDR dongles** work out of the box on every platform. Trunk Recorder Pro drives them
  itself (no `librtlsdr`), which works for dongles with an **R820T or R828D** tuner. That is
  nearly every dongle sold today, including the RTL-SDR Blog V3 and V4.
- **Older RTL-SDRs** with an E4000, FC0012, FC0013 or FC2580 tuner, **USRPs**, **Airspy R2 / Mini**
  and anything with a **SoapySDR** module (HackRF, SDRplay, LimeSDR, ...) work too, once you have
  installed their makers' drivers. See [Other radios](other-radios.md). These are desktop-only.

How many dongles you need depends on how spread out the system's frequencies are. A dongle sees
a slice of spectrum about as wide as its sample rate (2.4 MHz for a typical RTL-SDR at
2.4 MSPS), minus a little at each edge. If the control channel and every voice channel fit in
one slice, one dongle is enough. [Radios](../guides/radios.md) explains how to plan this.

### A computer

Trunk Recorder Pro is light. On a Raspberry Pi 5 recording a P25 system from one RTL-SDR it used
about 5 % of one CPU core and 56 MB of memory (see [Performance](../performance.md) for how that
was measured). Any reasonably recent computer will do; a Raspberry Pi 5 is plenty for a typical
system.

| Platform | Requirement |
|---|---|
| macOS | macOS 11 (Big Sur) or newer, Apple silicon or Intel |
| Windows | Windows 10 or 11, 64-bit x86. There is no ARM build for Windows. |
| Linux | x86-64 or ARM64 (64-bit only), glibc 2.28 or newer: Debian 10, Ubuntu 20.04, RHEL 8, Raspberry Pi OS (64-bit) or anything newer |
| Docker | A Linux host, x86-64 or ARM64. Docker Desktop on macOS and Windows can't pass USB devices into a container. |
| Browser | Chrome, Edge, Opera or Brave on Windows, macOS, Linux or ChromeOS; Chrome on Android with a USB-OTG adapter |

Recordings take disk space: a WAV file per call, plus a small JSON file. Decide where they go
before a long run; the default is a folder called `TrunkRecorderPro` in your home folder, and you
can change it in **Setup → Recording**.

USB matters more than the CPU. Each RTL-SDR at 2.4 MSPS streams about 4.8 MB/s, so plug several
dongles into ports or a powered hub that can carry them, and use good cables.

## After installing

Go to [Getting started](../getting-started.md): it walks through finding your system's
frequencies, adding the system (or letting **Find my system** find it), setting gain and
frequency correction, and pressing **Start**.

## Building from source

You only need this if you want to change the code or run a version that hasn't been released.
You need:

- **Rust 1.95 or newer** (the `rust-version` in `Cargo.toml`), from [rustup](https://rustup.rs).
- **Node.js 20 or newer** for the web interface (the release builds use Node 22).

```bash
git clone https://github.com/TrunkRecorder/trunk-recorder-pro.git
cd trunk-recorder-pro
(cd web && npm ci && npx vite build)     # the interface -> web/dist, embedded in the binary
cargo build --release                    # -> target/release/trunk-pro
target/release/trunk-pro
```

Build the interface first: the binary embeds `web/dist` when it is compiled, so rebuild the
binary after changing the interface. `cargo build --profile dist` makes a stripped binary like the
released ones.

`npm run build` does the same as `npx vite build` but also type-checks the browser version, which
fails until you have built its WebAssembly engine. To build the browser version, add the
WebAssembly target and `wasm-bindgen-cli` at the version pinned in `Cargo.lock` (0.2.129 at the
time of writing):

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
(cd web && npm run wasm && npm run build:web)   # -> web/dist-web
```

`packaging/package.sh` turns a built binary into the release packages; the comment at the top of
the script explains it.
