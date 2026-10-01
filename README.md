# Trunk Recorder Pro

A lightweight, self-contained trunked-radio recorder: point one or more
RTL-SDRs (or, optionally, USRPs and Airspys) at one or more **P25** systems (Phase 1 and Phase 2 TDMA voice),
**SmartNet** or **trunked DMR** systems and it follows their control channels and records
every call it can hear as WAV + Trunk Recorder–compatible JSON. It also records
**conventional channels** — analog FM, P25 and DMR — alongside a trunked system or
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
[releases](https://github.com/TrunkRecorder/trunk-recorder-pro/releases)
(each is a single ~3–7 MB program with the interface built in; nothing else to
install). `SHA256SUMS` lists their checksums.

- **macOS** (11+, Apple silicon and Intel): open `trunk-pro-<version>-macos.dmg`
  and drag **Trunk Recorder Pro** to Applications. Until releases are signed
  with an Apple Developer ID, macOS blocks the first launch: open it once,
  then **System Settings → Privacy & Security → Open Anyway**. The app has no
  Dock icon; it opens its interface in your browser. Open the app again to get
  back to it, and use **Quit** in the interface to stop it.
- **Windows** (10/11, 64-bit): unzip `trunk-pro-<version>-windows-x86_64.zip`
  and run `trunk-pro.exe`. SmartScreen may warn about an unrecognised app:
  **More info → Run anyway**. The console window is the app; closing it quits.
  Dongles need the WinUSB driver once, as for every RTL-SDR program: run
  [Zadig](https://zadig.akeo.ie), pick “Bulk-In, Interface (Interface 0)”,
  install WinUSB.
- **Linux** (x86-64 or ARM64, e.g. a Raspberry Pi 4/5 with a 64-bit OS; any
  distribution from about 2019 on — glibc 2.28 or newer, e.g. Debian 10,
  Ubuntu 20.04, RHEL 8, Raspberry Pi OS bullseye):

  ```bash
  tar xzf trunk-pro-<version>-linux-x86_64.tar.gz
  cd trunk-pro-<version>-linux-x86_64 && sudo ./install.sh
  trunk-pro
  ```

  `install.sh` puts `trunk-pro` in `/usr/local/bin`, adds a udev rule so
  your user can open RTL-SDRs, Airspys and USB USRPs (the kernel's DVB driver is detached
  automatically) and a menu entry. `trunk-pro.service` in the package runs it
  headless as a systemd user service.
- **Browser**, no install: see [In the browser](#in-the-browser-no-install).

### USRP and Airspy (optional)

RTL-SDRs work with nothing else installed. USRPs (Ettus / NI, through UHD)
and Airspy R2 / Mini (through libairspy) use their makers' drivers, which you
install yourself; Trunk Recorder Pro finds them when it starts — no special
build — and offers **USRP** and **Airspy** as source types in Setup (which
says what is missing if a driver isn't found).

| | macOS | Debian / Ubuntu / Raspberry Pi OS | Windows |
|---|---|---|---|
| USRP | `brew install uhd` | `sudo apt install libuhd-dev uhd-host` | Ettus's UHD installer (adds `uhd.dll` to PATH) |
| Airspy | `brew install airspy` | `sudo apt install libairspy0` | `airspy.dll` from airspy-tools, next to `trunk-pro.exe` |

USRPs also need UHD's FPGA images once: `uhd_images_downloader` (sudo on
Linux). `trunk-pro devices` shows which drivers were found; `trunk-pro
devices --usrp` also searches for USRPs. A driver installed somewhere unusual
can be named with `TRUNK_PRO_UHD=/path/to/libuhd…` / `TRUNK_PRO_AIRSPY=…`.

Settings: a USRP takes UHD device arguments (blank = the first found,
`serial=…`, `addr=192.168.10.2`), any sample rate its clock supports (e.g. 8
MSPS covers ~7 MHz), a gain in dB and an antenna (e.g. `RX2`, `TX/RX`).
An Airspy runs at 10 or 2.5 MSPS (R2) or 6 or 3 MSPS (Mini), with a
linearity gain step of 0–21 and an optional bias-T. Wider sources cost more
CPU: about 1–2 % of a core per 2.4 MSPS.

## Run it

```bash
trunk-pro          # opens http://localhost:8080 — set up the system(s), press Start
```

Add a system (or let **Find my system** find it), set your dongle(s) in the browser, press **Start**.
Calls are written to the recordings folder (default `~/TrunkRecorderPro`) as
`<system>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>.wav|json`, Trunk
Recorder's layout and JSON fields; the interface shows live status, a
waterfall per dongle, active calls (listen live) and recent recordings. The
config lives in `~/Library/Application Support/trunk-pro/` (macOS),
`%APPDATA%\trunk-pro\` (Windows) or `~/.config/trunk-pro/` (Linux).

On a machine that records unattended, turn on **Start recording when the app
starts** (or run `trunk-pro --start`); Ctrl-C, SIGTERM and **Quit** all save
the calls in progress before exiting. `--bind 0.0.0.0` makes the interface
reachable from other machines — it has no login, so only on a network you
trust (or use `ssh -L 8080:localhost:8080`).

`trunk-pro devices` lists radios; `trunk-pro capture out.cu8 --freq Hz
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

### Several systems and sites

One recorder can follow several P25 systems at once — or several **sites** of
one multi-site system. Each gets a card under **Systems** in Setup, with its
own short name (its recordings folder and band plan), control channels,
modulation and talkgroup CSV; **Record** switches one off without deleting
it. The systems share the sources and the recorder pool: each control channel
runs on whichever source covers it, each call on whichever source covers its
voice channel. A source left on **Auto** is centered over a system the
sources before it don't cover, and each source's card shows which systems'
control (tall ticks) and voice channels (short ticks) fall inside it.

A **site lock** (NAC, WACN, System ID, RFSS, site) keeps a system on the
right control channel: a control channel that announces a different identity
is not followed — its grants are ignored and the recorder hunts on to the
next one listed — and the dashboard says why. Grants wait until every locked
field has been heard (the site comes in the RFSS status broadcast, every few
seconds). For a multi-site system, add each site as its own system locked to
its site number; the survey fills the lock in, and the neighbouring sites a
control channel announces can be added with one click (in the survey, and on
the dashboard while recording). A site added next to one of the same system
gets its talkgroups.

The dashboard lists every system with its site, control channel, decode rate
and calls, grouping sites of the same WACN / System ID; active calls, recent
calls and the log can be filtered by system, and live listening follows the
filter. A call heard on two sites is recorded by both (duplicate detection
across sites is planned). Importing a Trunk Recorder config brings in all its
P25 systems.

### Trunked DMR

A system of `type` `dmr` is a DMR site: Motorola Capacity Plus (and Linked
Capacity Plus), Capacity Max, Connect Plus, or ETSI Tier III. Which one is
found by listening. Every frequency listed is watched at once (a receiver
costs a fraction of a percent of a core):

- **Capacity Plus** has no grants: radios idle on a rest channel that moves
  from repeater to repeater. List every repeater of the site as a control
  channel; each call is found by its own link control (talkgroup, radio),
  on whichever repeater and slot it is.
- **Capacity Max, Connect Plus, Tier III** grant calls to logical channel
  numbers. List the control channel, and the site's voice frequencies under
  **Voice frequencies** (Trunk Recorder's `channels`). A channel's frequency
  is learned from the air: a grant for an unknown channel, then that
  talkgroup's link control on one of the voice frequencies a moment later,
  ties the two together — saved like a band plan, so it is known from the
  start next time. Tier III systems that announce their channels, or grant
  with absolute frequencies, need no learning. Trunk Recorder's `lcnTable`
  (`{ "101": 452275000, … }`) sets channels by hand and wins over what is
  learned.

A site's colour code is the control channel's (or `colorCode`); anything on
another colour code, such as a neighbour on a listed frequency, is ignored.
Each slot of a carrier records its own call (file names end in `.0` / `.1`,
and the call JSON has the slot and `color_code`). Systems that key their
checksums (Motorola / Hytera restricted access, as Capacity Max often does)
are recognised, and their blocks are taken on their error correction alone.
Encrypted calls are marked (the privacy bit or header) and not recorded,
unless **Record encrypted calls** is on.

```json
{ "shortName": "capplus", "type": "dmr", "controlChannels": [463375000, 463750000, 464350000] }
{ "shortName": "capmax", "type": "dmr", "controlChannels": [452175000], "channels": [452275000, 452300000],
  "lcnTable": { "101": 452275000 } }
```

**Find my system** finds trunked DMR sites too (pick the Business UHF / VHF
bands to look where most are) and adds one with its colour code. The
dashboard shows a DMR site's kind, colour code and rest channel, each
watched frequency with the call on each slot, and the channel table
(configured or learned).

Conventional DMR channels also record radios talking to each other directly
(simplex / talkaround): one slot's bursts with nothing on the air between
them.

`trunk-pro tool dmrscan <capture> --center Hz --rate Hz` lists every DMR
carrier in a capture with its colour code; `tool dmr … --freq Hz` decodes
one (link control, CSBKs, `--bursts` for every burst, `--audio` a slot).

### Find my system

Don't know the frequencies? Under **Find my system** in Setup, press **Scan**
with a dongle connected. The recorder steps through the P25 bands (800, 700
and 900 MHz, UHF 450–470, VHF 136–174 by default; UHF federal 380–420 and
T-band 470–512 on request), about 1.3 s per 2 MHz step. In each step it looks
for carriers that stay on (a control channel never keys down) and checks each
one with the P25 and SmartNet receivers. Then it listens to the control
channel with the best signal and shows what that channel announces:

- the system's WACN, System ID, NAC, RFSS and site, and its band plan;
- its alternate control channels and neighbouring sites;
- the voice channels it grants calls on;
- the **frequency correction** (ppm) your radio needs. The channel announces
  its own frequency, and base stations are GPS- or rubidium-locked, so how far
  off the recorder hears it is the radio's error;
- on an RTL-SDR, the **gain**: it tries gains from 19.7 to 49.6 dB and keeps
  the lowest one within 1 dB of the best signal that doesn't clip.

A **SmartNet** control channel names channels only by number, and on VHF /
UHF (OBT) systems nothing on the air says what frequency a number is. So the
survey watches: while a channel number is being granted, one carrier in the
spectrum comes up that is down otherwise. A few such channels, and the
number the control channel broadcasts for itself, give the band plan
(Trunk Recorder's `400_custom` base / spacing / offset, or an 800 / 900 MHz
plan when the channels land on one) — and with it the alternate control
channels and every voice channel granted, even ones outside the dongle's
view. On WMATA (UHF OBT) it takes a minute or so of normal traffic.

**Add this system** adds it — control channels, a site lock with the
identity it announced, the voice channels seen — and sets the ppm, the gain
and a center frequency that covers the most voice channels seen (unless
another system needs that source where it is). Pick **replacing …** instead
to update a system already set up. It also says when the system spans more
than one dongle can cover. Any other control channel the scan found can be
picked with **Listen**, or added straight away with **Add** (with its site's
other control channels); neighbouring sites can be added too. Everything can
still be typed in by hand.

From the command line (JSON lines of what's found, then the system):

```bash
trunk-pro survey --serial 200 --bands 800,700 --seconds 30
trunk-pro survey --serial 91 --bands t-band --seconds 120                # e.g. a UHF SmartNet system
trunk-pro survey capture.cu8 --center 858300000 --rate 2400000   # a capture: one look
```

## Conventional channels

Conventional channels are single frequencies — analog narrowband FM or P25 —
recorded whenever something transmits on them. There are three ways to keep
the list:

- **In the browser**, under **Conventional channels**: a table, a box to paste
  many frequencies, **Import CSV…** (replace the list or add to it) and
  **Export CSV**.
- **In a spreadsheet** (desktop app): under *Channel file*, enter a path such as
  `channels.csv` and press **Use this file**. A new file is created from the
  current list; from then on the channels are read from it — edit it in Excel,
  Numbers or LibreOffice, then press **Reload** (recording also re-reads it
  every time it starts). Relative paths are next to the config file.
  **Unlink** keeps the channels in the app's settings again.
- **In the config file**, as JSON:

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
| `mode` | `fm` (analog narrowband FM, 12.5 kHz), `p25` (P25 Phase 1, C4FM or CQPSK) or `dmr` (both slots, each recording its own calls). Each channel has its own, so one list can mix them |
| `name`, `description`, `tag`, `group` | Written into the call JSON (Trunk Recorder's alpha tag, description, tag, category) |
| `talkgroup` | The number calls are filed under (file names, JSON, uploaders). Default: the frequency in kHz, e.g. 154430 — stable however you reorder the list. P25 and DMR channels use the talkgroup the radio sends, when it sends one |
| `squelchDb` | How far above the noise floor a signal must be to open the channel, in dB. Per channel, or for all in the section (default 8). The noise floor is measured, so this doesn't depend on the dongle or gain the way Trunk Recorder's absolute squelch does |
| `enabled` | `false` keeps a channel in the list without recording it |
| `channelFile` (section) | A CSV to read the channels from instead of `channels` (desktop) |

The CSV has a header row, then one channel per row; columns in any order:

```csv
TG Number,Frequency,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Enable
1001,154.4300,fm,County Fire Dispatch,,,Fire,,true
,155.1000,fm,EMS Ops,,,,,true
,460.1250,p25,PD Tac 2,,,,12,true
,453.0000,fm,,,,,,false
```

| Column | |
|---|---|
| `Frequency` | MHz with a decimal point (`154.4300`), or Hz (`154430000`) |
| `Mode` | `fm`, `p25` or `dmr` (also `analog` / `digital`, `A` / `D`); empty = `fm` |
| `TG Number` | Empty = the frequency in kHz |
| `Alpha Tag`, `Description`, `Tag`, `Category` | Names for the call JSON |
| `Squelch dB` | dB above the noise floor, 3–40; empty = the default |
| `Enable` | `false` (or `no`, `0`) = off; empty = on |

Trunk Recorder's channel file reads as is (its `Tone`, `Comment` and `Signal
Detector` columns are ignored; its `Squelch` column is an absolute level and
isn't used). Files saved by Excel in any locale work: commas, semicolons or
tabs, decimal commas, and the byte-order mark. Rows that can't be read are
reported by row number; a file that can't be read at all keeps the last good
list. `trunk-pro replay … --channels channels.csv` reads the same format.

A config can have trunked systems, conventional channels, or both; with no
system, it records conventional channels only. Every enabled channel must lie
inside a source's bandwidth. A source's center is placed automatically when
it is left on Auto and the channels fit. Conventional calls go to their own
folder (**Short name** in the panel, `conv` by default).

Analog calls (conventional FM channels, and SmartNet analog grants) pick up
the unit ID that MDC1200 and FleetSync radios send as a data burst when they
key up or unkey. It goes in the call's `srcList` like a digital radio's:
MDC1200 as its 16-bit ID (as Trunk Recorder writes it), FleetSync as
fleet × 10000 + unit (fleet 101, unit 1234 → 1011234). An MDC1200 emergency
marks the call as an emergency. There is nothing to turn on; Trunk Recorder's
`decodeMDC` and `decodeFSync` aren't needed.

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

**From Trunk Recorder.** **Import CSV…** (or a channel file) reads Trunk
Recorder's channel file; add a `Mode` column to mix analog and P25 in one
file. **Import Trunk Recorder config…** brings in `conventional` and
`conventionalP25` systems' `channels` lists. Squelch values aren't carried
over: Trunk Recorder's are absolute levels; here squelch is dB above the noise.

## Build

Needs Rust 1.82+ and Node 20+ (for the interface).

```bash
(cd web && npm ci && npm run build)     # the interface → web/dist, embedded in the binary
cargo build --release                   # target/release/trunk-pro
cargo test --release
cargo build --profile dist              # stripped, as released
```

Release packages: `packaging/package.sh <macos|linux-x86_64|linux-aarch64|windows-x86_64|browser> <version> <binary> <out-dir>`
(what CI runs; see the top of the script). Tagging `v<version>` (the
version in `Cargo.toml`, with a matching section in `CHANGELOG.md`) makes CI
publish a release.

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
 Releases are built by
`.github/workflows/build.yml` (macOS universal, Linux x86-64 / ARM64 against
glibc 2.28, Windows).

## Replay captures

```bash
# Record from an rtl_sdr capture (unsigned 8-bit IQ):
rtl_sdr -f 858300000 -s 2400000 -g 25.4 -n 72000000 capture.cu8        # 30 s
./target/release/trunk-pro replay capture.cu8 --center 858300000 --rate 2400000 \
    --cc 857987500 --out calls/ [--talkgroups tg.csv] [--bandplan site.bandplan]

# Several dongles on one system (control channel on either):
./target/release/trunk-pro replay --source a.cu8,858300000,2400000 \
    --source b.cu8,860700000,2400000 --cc 857987500 --out calls/

# Several systems or sites (each to <out>/<name>/), with optional site locks
# (NAC / System ID / WACN in hex): a control channel that disagrees isn't followed.
./target/release/trunk-pro replay capture.cu8 --center 858300000 --rate 2400000 \
    --system east:857987500:nac=443,site=3 --system west:858987500:site=4 --out calls/

# Conventional channels (with or without --cc); talkgroup = frequency in kHz:
./target/release/trunk-pro replay capture.cu8 --center 154500000 --rate 2400000 \
    --fm 154430000,155100000 --p25 154725000 [--squelch 8] --out calls/
#   or --channels channels.csv (the channel-file format above)
```

Calls are written as `<talkgroup>-<epoch>_<freq>.wav|json` with Trunk
Recorder's JSON fields (Phase 2 TDMA calls as `…_<freq>.<slot>.wav`; the
scrambler seed — WACN, System ID, NAC — comes from the control channel, no
setup needed). `--bandplan` keeps the system's IDEN tables between
runs, so a grant heard before the next IDEN broadcast can be followed at once
(with several systems, one file each: `<file>.<name>`).

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
| `crates/trunk-pro` | The app: `serve` (default; source threads, engine thread, web server + WebSocket), `replay`, `capture`, `devices`, `tool` |
| `…/src/sdr.rs` | RTL-SDR over USB via `rtlsdr-nusb` (pure Rust; no libusb / librtlsdr) |
| `…/src/radio/` | USRP (UHD's C API) and Airspy (libairspy), loaded at run time when installed |
| `crates/trunk-app` | The app layer shared by desktop and browser: config, the conventional channel CSV (`channels.rs`), and a recording `Session` (status, spectrum, log, calls, files) |
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
9. ~~Several systems and sites at once, with site locks~~ — done (verified
   on captures). Next: duplicate-call detection across the sites of a
   multi-site system

## License

GPL-3.0-or-later (the P25 code derives from op25). The vocoder derives from
mbelib (ISC).
