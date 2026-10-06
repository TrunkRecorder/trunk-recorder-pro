# Trunk Recorder Pro

A lightweight, self-contained trunked-radio recorder: point one or more
RTL-SDRs (or, optionally, USRPs and Airspys) at one or more **P25** systems (Phase 1 and Phase 2 TDMA voice),
**SmartNet**, **trunked DMR** or **NXDN** systems and it follows their control channels and records
every call it can hear as WAV + Trunk Recorder–compatible JSON. It also records
**conventional channels** — analog FM, P25, DMR and NXDN — alongside a trunked system or
on their own (see [Conventional channels](#conventional-channels)). It is written
in Rust, with no GNU Radio or OP25 dependency, and runs as a desktop app
(macOS, Linux, Windows) or in the browser, with the same browser-based
interface for both. [Plugins](#plugins) upload calls to OpenMHz, Broadcastify
and Rdio Scanner, stream them, or run a script of yours.

**Status:** the desktop app works end to end — live input from one or several
radios, decoding, recording, and the browser interface. It has been tested
live on macOS and on Linux (a Raspberry Pi 5), and builds for Windows. The
browser version runs the same engine as WebAssembly. The previous
TypeScript/browser implementation is in git history (removed in 7012637).

More documentation:
- [docs/configuration.md](docs/configuration.md): every setting in the
  config file.
- [docs/architecture.md](docs/architecture.md): how it works, from SDR to
  audio file.
- [docs/api](docs/api/README.md): building interfaces of your own.
- [docs/performance.md](docs/performance.md): CPU use against Trunk
  Recorder.

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
- **Docker** (Linux hosts, x86-64 or ARM64; Docker Desktop on macOS and
  Windows can't pass USB through): [`docker-compose.yml`](docker-compose.yml)
  runs [robotastic/trunk-recorder-pro](https://hub.docker.com/r/robotastic/trunk-recorder-pro)
  with the host's USB devices, the interface on port 8080 and settings,
  plugins and recordings in `./data`. RTL-SDRs only (the image has no UHD or
  libairspy).

  ```bash
  docker compose up -d
  ```
- **Browser**, no install: see [In the browser](#in-the-browser-no-install).

### USRP, Airspy and SoapySDR (optional)

RTL-SDRs work with nothing else installed. Other radios use their makers'
drivers, which you install yourself:

- USRPs (Ettus / NI), through UHD;
- Airspy R2 / Mini, through libairspy;
- anything with a SoapySDR module: HackRF, SDRplay, LimeSDR, older RTL-SDRs…

Trunk Recorder Pro finds the drivers when it starts, with no special build,
and offers **USRP**, **Airspy** and **SoapySDR** as source types in Setup.
Setup says what is missing if a driver isn't found.

| | macOS | Debian / Ubuntu / Raspberry Pi OS | Windows |
|---|---|---|---|
| USRP | `brew install uhd` | `sudo apt install libuhd-dev uhd-host` | Ettus's UHD installer (adds `uhd.dll` to PATH) |
| Airspy | `brew install airspy` | `sudo apt install libairspy0` | `airspy.dll` from airspy-tools, next to `trunk-pro.exe` |
| SoapySDR | `brew install soapysdr` and the device's module (e.g. `soapyhackrf`) | `sudo apt install libsoapysdr0.8` and the device's module (e.g. `soapysdr0.8-module-hackrf`) | PothosSDR |

USRPs also need UHD's FPGA images once: `uhd_images_downloader` (sudo on
Linux). `trunk-pro devices` shows which drivers were found; `trunk-pro
devices --usrp` also searches for USRPs. A driver installed somewhere unusual
can be named with `TRUNK_PRO_UHD=/path/to/libuhd…`, `TRUNK_PRO_AIRSPY=…` or
`TRUNK_PRO_SOAPY=…`.

Settings: a USRP takes UHD device arguments (blank = the first found,
`serial=…`, `addr=192.168.10.2`), any sample rate its clock supports (e.g. 8
MSPS covers ~7 MHz), a gain in dB and an antenna (e.g. `RX2`, `TX/RX`).
An Airspy runs at 10 or 2.5 MSPS (R2) or 6 or 3 MSPS (Mini), with a
linearity or sensitivity gain step of 0–21, or its LNA (0–14), mixer and VGA
(0–15) stages set by hand (the LNA and mixer optionally by its AGC), and an
optional bias-T. A SoapySDR source takes device arguments
(`driver=hackrf`, `driver=sdrplay,serial=…`), its gain stages one by one,
an antenna and device settings (`biastee=true`).

RTL-SDRs are driven natively only with an R820T / R828D tuner (nearly every
dongle sold today, RTL-SDR Blog V3 / V4 included). Older E4000, FC0012,
FC0013 and FC2580 dongles work through SoapySDR instead: install its RTL-SDR
module (`brew install soapyrtlsdr`, `sudo apt install
soapysdr0.8-module-rtlsdr`) and add a **SoapySDR** source with device
arguments `driver=rtlsdr` (`driver=rtlsdr,serial=…` to pick one). Desktop app
only; the browser build can't use them.

Every radio has an AGC switch and **AutoTune**, which
corrects for the frequency error its control channels show. Wider sources cost more
CPU: about 5 % of a Raspberry Pi 5 core for one RTL-SDR at 2.4 MSPS; on an
Apple M4 Pro about 12 % of a core for a USRP at 8 MSPS and 8 % for two RTL-SDRs
(see [docs/performance.md](docs/performance.md)).

## Run it

```bash
trunk-pro          # opens http://localhost:8080 — set up the system(s), press Start
```

Add a system (or let **Find my system** find it), set your dongle(s) in the browser, press **Start**.
Calls are written to the recordings folder (default `~/TrunkRecorderPro`) as
`<system>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>.wav|json`, Trunk
Recorder's layout and JSON fields (or as **Folders and file names** says,
with Trunk Recorder's `filenameFormat` tokens); the interface shows live status, a
waterfall per dongle, active calls (listen live) and recent recordings. The
config lives in `~/Library/Application Support/trunk-pro/` (macOS),
`%APPDATA%\trunk-pro\` (Windows) or `~/.config/trunk-pro/` (Linux).
`--config <file>` uses another one; what the recorder learns and installs
(band plans, talker aliases, plugins) is kept beside it. Every setting is
described in [docs/configuration.md](docs/configuration.md).

The log goes to stderr in Trunk Recorder's format (and, as Setup → Recording
→ Log says, to daily files and the system log); `--log-level debug` for
more. On a machine that records unattended, turn on **Start recording when the app
starts** (or run `trunk-pro --start`); Ctrl-C, SIGTERM and **Quit** all save
the calls in progress before exiting. `--bind 0.0.0.0` makes the interface
reachable from other machines — it has no login, so only on a network you
trust (or use `ssh -L 8080:localhost:8080`).

**Interfaces of your own** (a scanner page, a wall display, a dashboard)
can do everything the built-in one does: see [docs/api](docs/api/README.md)
(for developers) and [llms.txt](docs/api/llms.txt) (for an LLM building one
for you). The recorder serves them: `trunk-pro --ui <folder>` shows one at
`/`, and Setup → Recording → Interfaces adds any number at `/ui/<name>/`.
The built-in interface is always at `/builtin/`, and `/ui/` lists everything,
including runnable examples.

`trunk-pro devices` lists radios; `trunk-pro capture out.cu8 --freq Hz
--serial SN --seconds 30` records raw IQ from an RTL-SDR like `rtl_sdr`.
Capture files can be `cu8` (rtl_sdr), `cs16` or `cf32` (GNU Radio, UHD's
`rx_samples_to_file`). `trunk-pro --help` lists every command, including the
`tool` diagnostics (`cc`, `voice`, `frames`, `p2`, `snr`, `smartnet`, `dmr`,
`dmrscan`, `nxdn`, `nxdnscan`, `revoice`). SIGHUP reopens the log file (for logrotate).

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

One recorder can follow several systems at once (P25, SmartNet, DMR and NXDN, in
any mix), or several **sites** of one multi-site system. Each gets a card under **Systems** in Setup, with its
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
filter. Importing a Trunk Recorder config brings in all its P25, SmartNet and
DMR systems, and its conventional ones.

#### Calls heard on several sites

A call on a multi-site system is usually granted on every site with radios
on its talkgroup. It is saved once. Each site's copy is recorded, and when
the last one ends the best copy is kept: the one with the most cleanly
decoded audio. A site that fades mid-call, or whose voice channel no source
covers, doesn't cost you the call. Live listening plays one copy, and the
dashboard shows "also on …" under a call that other sites carry. **Save a
call heard on several sites once** under Recording switches this off (every
copy saved, each in its site's folder).

Sites are grouped into systems by what their control channels say: P25 by
WACN and System ID, SmartNet by System ID. Two systems following the same
site are not grouped. A **Site group** name on a system overrides that:

```json
{ "shortName": "capmax-north", "type": "dmr", "siteGroup": "capmax", … },
{ "shortName": "capmax-south", "type": "dmr", "siteGroup": "capmax", … }
```

Give DMR sites a group name (they don't announce a system identity). Give
two systems linked by ISSI the same name too. A name of its own keeps a
site out of its system's group.

To prefer a site for a talkgroup, add a **Preferred Site** column (the
site's short name) to the talkgroup CSV. Trunk Recorder's **Preferred NAC**
column also works: a NAC, or RFSS and site as `RRRRssss`. The preferred
site's copy is kept when it holds at least 90 % of the best copy's clean
audio. Importing a Trunk Recorder config turns `multiSiteSystemName` into
site groups.

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
  with absolute frequencies, need no learning. `lcnTableHz` (Trunk Recorder's `lcnTable`)
  (`{ "101": 452275000, … }`) sets channels by hand and wins over what is
  learned.

A site's colour code is the control channel's (or `colorCode`); anything on
another colour code, such as a neighbour on a listed frequency, is ignored.
Each slot of a carrier records its own call (file names end in `.0` / `.1`,
and the call JSON has the slot and `color_code`). Systems that key their
checksums (Motorola / Hytera restricted access, as Capacity Max often does)
are recognised, and their blocks are taken on their error correction alone.
Encrypted transmissions are marked (the privacy bit or header) and left out
of the audio; a call that was all encrypted is kept only when the config's
`recording.recordEncrypted` is `true` (there is no switch for it in the
interface yet).

```json
{ "shortName": "capplus", "type": "dmr", "controlChannelsHz": [463375000, 463750000, 464350000] }
{ "shortName": "capmax", "type": "dmr", "controlChannelsHz": [452175000], "dmrChannelsHz": [452275000, 452300000],
  "lcnTableHz": { "101": 452275000 } }
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

### NXDN

NXDN (Kenwood NEXEDGE, Icom IDAS) comes at two rates: **NXDN48** (4800 bps,
6.25 kHz channels) and **NXDN96** (9600 bps, 12.5 kHz). Both are decoded,
conventional and trunked; voice is AMBE+2, as on DMR. A system of `type`
`nxdn` is one site:

- **Type-C** (`"nxdnType": "typeC"`, the default) has a control channel that
  assigns calls to channel numbers. List the control channel (`nxdnRate`:
  its rate, `nxdn48` by default). A channel number's frequency comes from
  `lcnTableHz` (`{ "12": 451118750, … }`; RadioReference lists them as LCNs
  for many systems), from the site itself when it uses Direct Frequency
  Assignment (its grants then carry the frequency), or is learned: list the
  site's voice frequencies under `nxdnChannelsHz`, and a grant for an
  unknown channel, then that group's call on one of them a moment later,
  ties the two together — saved like a band plan. The dashboard lists the
  channel numbers granted but not known yet.
- **Type-D** (`"nxdnType": "typeD"`, IDAS distributed trunking) has no
  control channel: each repeater says in its signalling channel (SCCH) when
  it is idle and who is talking on it. List every repeater; each call is
  found on the repeater carrying it.

A site's RAN (Radio Access Number, NXDN's colour code) is the control
channel's (or `ran`); other RANs are ignored. Type-C sites announce a system
and site code, which the site lock (`expect`'s `sysId` and `site`) and
multi-site grouping use. Group and unit IDs are 16 bits; on Type-D the top 5
bits are the home repeater or prefix. The call JSON gets `"ran"`, and file
name formats `{ran}`. Encrypted transmissions (scrambler, DES or AES) are
marked and left out of the audio, as on P25 and DMR; full-rate (EFR) voice
isn't decoded.

```json
{ "shortName": "nexedge", "type": "nxdn", "controlChannelsHz": [451018750],
  "nxdnChannelsHz": [451118750, 452381250], "lcnTableHz": { "12": 451118750 } }
{ "shortName": "idas", "type": "nxdn", "nxdnType": "typeD", "nxdnRate": "nxdn48",
  "controlChannelsHz": [452012500, 452062500, 452112500] }
```

Conventional NXDN channels are `"mode": "nxdn48"` or `"nxdn96"`, with an
optional `RAN 5` (or `RAN 5 TG 201`) in the Tone column. **Find my system**
recognises NXDN control channels (with their RAN, system and site) and busy
NXDN carriers.

`trunk-pro tool nxdnscan <capture> --center Hz` lists every NXDN carrier in a
capture at both rates, with its RAN and what it carries; `tool nxdn … --freq
Hz --nxdn 48|96` decodes one (layer 3 messages, `--frames` for every frame,
`--audio` its voice). The receiver finds the polarity itself, so spectrally
inverted recordings decode too. There is no NXDN system near the
developer: it was tested on recordings (the sigidwiki NXDN48 / NXDN96 IQ
files) and on synthesized control and traffic channels — reports from
real systems are welcome ([research/nxdn.md](research/nxdn.md) says what to
capture).

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

Conventional channels are single frequencies — analog narrowband FM, P25 or
DMR — recorded whenever something transmits on them. They are grouped in
**conventional systems**, as in Trunk Recorder: each has its own short name
(the folder its calls go to), squelch, channel list or channel file, unit
names, upload settings and Recording override, and is switched on or off as
a whole. A frequency belongs to one system. To plugins each is a system of
its own (numbered 65535, 65534, …), so each can have its own upload keys.
There are three ways to keep a system's list:

- **In the browser**, on the **Conventional** tab (**Add a conventional
  system**, then in its panel): a table, a box to paste
  many frequencies, **Import CSV…** (replace the list or add to it) and
  **Export CSV**.
- **In a spreadsheet** (desktop app): under *Channel file*, enter a path such as
  `fire-channels.csv` and press **Use this file**; each system can have its own. A new file is created from the
  current list; from then on the channels are read from it — edit it in Excel,
  Numbers or LibreOffice, then press **Reload** (recording also re-reads it
  every time it starts). Relative paths are next to the config file.
  **Unlink** keeps the channels in the app's settings again.
- **In the config file**, as JSON:

```json
"conventional": [{
  "shortName": "county",
  "squelchDb": 8,
  "channels": [
    { "freqHz": 154430000, "mode": "fm",  "name": "County Fire Dispatch", "talkgroup": 1001, "group": "Fire" },
    { "freqHz": 155100000, "mode": "fm",  "name": "EMS Ops", "tone": "151.4" },
    { "freqHz": 154325000, "mode": "fm",  "name": "County A Fire", "tone": "D223N" },
    { "freqHz": 154325000, "mode": "fm",  "name": "County B Fire", "tone": "118.8" },
    { "freqHz": 154325000, "mode": "fm",  "name": "County other" },
    { "freqHz": 460125000, "mode": "p25", "name": "PD Tac 2", "squelchDb": 12, "tone": "NAC 293" },
    { "freqHz": 452100000, "mode": "dmr", "name": "Ops", "tone": "CC 1 TS 2 TG 201" },
    { "freqHz": 453000000, "mode": "fm",  "enabled": false }
  ]
}, {
  "shortName": "pd",
  "channelFile": "pd-channels.csv",
  "recording": { "minCallS": 2 }
}]
```

| Field | |
|---|---|
| `freqHz` | The channel frequency, in Hz (the interface takes MHz) |
| `mode` | `fm` (analog narrowband FM, 12.5 kHz), `p25` (P25 Phase 1, C4FM or CQPSK), `dmr` (both slots, each recording its own calls), `nxdn48` or `nxdn96`. Each channel has its own, so one list can mix them |
| `name`, `description`, `tag`, `group` | Written into the call JSON (Trunk Recorder's alpha tag, description, tag, category) |
| `talkgroup` | The number calls are filed under (file names, JSON, uploaders). Default: the frequency in kHz, e.g. 154430 — stable however you reorder the list; further rows on the same frequency get that with a digit added (1543251, 1543252 …). P25, DMR and NXDN channels use the talkgroup the radio sends, when it sends one |
| `tone` | Record only transmissions carrying this code: analog's CTCSS tone (`151.4`) or DCS code (`D023N`), P25's NAC (`NAC 293`), DMR's colour code, slot and talkgroup (`CC 1 TS 2 TG 201`), NXDN's RAN and group (`RAN 5 TG 201`). Empty: any. See [Tones](#tones-several-users-of-one-frequency) |
| `squelchDb` | How far above the noise floor a signal must be to open the channel, in dB. Per channel, or for all in the system (default 8). The noise floor is measured, so this doesn't depend on the dongle or gain the way Trunk Recorder's absolute squelch does |
| `enabled` | `false` keeps a channel (or, on the system, the whole system) without recording it |
| `shortName`, `name` (system) | Its folder and record name; what people call it |
| `channelFile` (system) | A CSV to read its channels from instead of `channels` (desktop) |
| `recording`, `unitNames`, `plugins` (system) | Its own recording rules, unit names and plugin settings |

The CSV has a header row, then one channel per row; columns in any order:

```csv
TG Number,Frequency,Tone,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Enable
1001,154.4300,,fm,County Fire Dispatch,,,Fire,,true
,155.1000,151.4,fm,EMS Ops,,,,,true
,460.1250,,p25,PD Tac 2,,,,12,true
,453.0000,,fm,,,,,,false
```

| Column | |
|---|---|
| `Frequency` | MHz with a decimal point (`154.4300`), or Hz (`154430000`) |
| `Mode` | `fm`, `p25`, `dmr`, `nxdn48` (or `nxdn`) or `nxdn96` (also `analog` / `digital`, `A` / `D`); empty = `fm` |
| `TG Number` | Empty = the frequency in kHz (further rows on that frequency: with a digit added) |
| `Tone` | The code the row records, as Trunk Recorder or RadioReference write it (`151.4 PL`, `023 DPL`, `293 NAC`, `CC1 TS2 TG201`); empty = any. With no `Mode`, a NAC means `p25`, a colour code `dmr` and a RAN `nxdn48` |
| `Alpha Tag`, `Description`, `Tag`, `Category` | Names for the call JSON |
| `Squelch dB` | dB above the noise floor, 3–40; empty = the default |
| `Enable` | `false` (or `no`, `0`) = off; empty = on |

Trunk Recorder's channel file reads as is, `Tone` included (its `Comment` and
`Signal Detector` columns are ignored; its `Squelch` column is an absolute
level and isn't used). Files saved by Excel in any locale work: commas, semicolons or
tabs, decimal commas, and the byte-order mark. Rows that can't be read are
reported by row number; a file that can't be read at all keeps the last good
list. `trunk-pro replay … --channels channels.csv` reads the same format.

A config can have trunked systems, conventional channels, or both; with no
system, it records conventional channels only. Every enabled channel must lie
inside a source's bandwidth. A source's center is placed automatically when
it is left on Auto and the channels fit. Each conventional system's calls go
to its own folder (its **Short name**).

### Tones: several users of one frequency

Analog radios often share a frequency, each agency keyed with its own
sub-audible CTCSS tone or DCS code so its radios hear only its own traffic.
Give an analog channel a **Tone** and it records only transmissions carrying
it. Type it as RadioReference or Trunk Recorder shows it: `151.4`,
`151.4 PL`, `D023N` or `023 DPL` all work, and it is tidied to `151.4` /
`D023N`. A mistake is pointed out (`151.5 Hz isn't a standard CTCSS tone —
151.4?`).

Digital channels have the same idea, in the same column:

| Mode | Code | Typed as | Shown as |
|---|---|---|---|
| Analog | CTCSS tone or DCS code | `151.4 PL`, `PL 151.4`, `023 DPL`, `D023` | `151.4`, `D023N` |
| P25 | NAC | `293`, `293 NAC`, `$293`, `0x293` (`F7E` = any) | `NAC 293` |
| DMR | colour code, and optionally slot and talkgroup | `CC1`, `1`, `CC1 TS2 TG201`, `CC 1 TG 201 SL 2` | `CC 1 TS 2 TG 201` |
| NXDN | RAN, and optionally group | `RAN 5`, `5`, `RAN5 TG 201` (`RAN 0` = any) | `RAN 5 TG 201` |

To split a shared frequency, list it once per code: **+ tone** (**+ NAC**,
**+ code**) on a row adds another on the same frequency, with its own
talkgroup number filled in.

| Rows on 154.325 MHz | A transmission with D223N | with 118.8 | with another tone, or none |
|---|---|---|---|
| one, no tone | that row | that row | that row |
| D223N, 118.8 | the D223N row | the 118.8 row | not recorded |
| D223N, 118.8, and one with no tone | the D223N row | the 118.8 row | the row with no tone |

So a channel listed with no code records everything, as before; only rows
with codes leave anything out.

- **Analog:** each transmission is held until its tone is known (a few
  tenths of a second; up to a second for one with none), so nothing is cut
  off, and two agencies keying one after the other make two calls. DCS codes
  that are the same signal on the air (D023N and D047I) count as one.
- **P25:** every frame carries the NAC, so nothing is held. A lone row with
  a NAC only filters: calls keep the talkgroup the radio sends. With several
  rows on the frequency, each row's calls are filed under its own talkgroup,
  because conventional radios mostly send one that says little (often 1).
- **DMR:** each slot on its own. The most specific row that fits wins (`CC1
  TS2 TG201` over `CC1`), and calls keep the talkgroup on the air; the row
  gives the names. A row whose code names a talkgroup is filed under it.
- **NXDN:** as DMR, with the RAN for the colour code (and no slots).

Every analog call's tone is identified whether or not one is set, and shown
in the call list. The call JSON gets Trunk Recorder's fields: `tone_mode`
(`ctcss` / `dcs` when the row has a tone, `search` when it hasn't),
`tone_detected` and `tone_confidence`.

**Not sure of a frequency's codes?** List it with none and let it record for
a while. Under its rows the channel table shows what it carried, most heard
first: *Heard: 151.4 Hz 42 calls [Add] · D023N 3 calls [Add] · no tone 5
calls*. **Add** makes a row for that code (with its own talkgroup). The list
counts transmissions that no row records too ("2 not recorded"), so you can
see what tones are leaving out. It is kept between runs (on the desktop in
`conventional.heard.json` next to the config; in the browser
version, in its own storage). Trunk Recorder
doesn't match NACs or colour codes; its channel file's `Tone` column (CTCSS
and, with its PR #1137, DCS) reads as is.

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
300 Hz, which removes CTCSS tones from what you hear. Each analog call's CTCSS
tone or DCS code is identified from the audio below that and written to the
call JSON (`tone_detected`, e.g. `151.4` or `D023N`, with `tone_confidence`)
and shown in the call list. A channel with a tone records only what carries
it; see [Tones](#tones-several-users-of-one-frequency).

**From Trunk Recorder.** **Import CSV…** (or a channel file) reads Trunk
Recorder's channel file; add a `Mode` column to mix analog and P25 in one
file. **Import Trunk Recorder config…** brings in each `conventional`,
`conventionalP25` and `conventionalDMR` system as a conventional system here,
with its short name, `channels` or channel file, rules and unit names. Squelch values aren't carried
over: Trunk Recorder's are absolute levels; here squelch is dB above the noise.

## Plugins

Plugins are programs of their own that the recorder runs while it records
and tells what happens (calls starting, ending and saved; radio activity;
live audio; status). They only watch: nothing a plugin does changes what is
recorded, a plugin that crashes is restarted, and one that falls behind
misses events rather than slowing the recorder. The published ones, from the
[plugin registry](https://github.com/TrunkRecorder/plugins):

| Plugin | |
|---|---|
| `openmhz` | Uploads calls to OpenMHz |
| `broadcastify` | Uploads calls to Broadcastify Calls |
| `rdioscanner` | Uploads calls to an Rdio Scanner server |
| `simplestream` | Streams calls' audio to other programs over UDP or TCP |
| `upload-script` | Runs a script of yours on each call, as Trunk Recorder's `uploadScript` |

**Plugins** in the interface installs, updates and removes them (each
download checked against the registry's SHA-256), turns them on and off, and
shows settings forms drawn from what each plugin describes: its own settings
and each system's (e.g. an upload key per system). Changes apply at once,
even while recording. Calls are encoded to M4A once for every plugin that
wants it (ffmpeg, macOS's afconvert or fdkaac, whichever is there).
Importing a Trunk Recorder config carries its OpenMHz, Broadcastify, Rdio
Scanner, simplestream and `uploadScript` settings over.

From the command line:

```bash
trunk-pro plugin search                 # what the registry has
trunk-pro plugin install openmhz        # install (or update) one
trunk-pro plugin list                   # what's installed, and what each is
trunk-pro plugin run openmhz ~/TrunkRecorderPro/dcfd   # try one on calls already recorded
```

Their settings live in the config's `plugins` and each system's `plugins`
([docs/configuration.md](docs/configuration.md#plugins)). To write one, start
from the [plugin template](https://github.com/TrunkRecorder/trunk-plugin-template):
the protocol is JSON lines over stdin / stdout, and the
[`trunk-recorder-plugin`](crates/trunk-recorder-plugin) crate is a Rust SDK
for it.

## Build

Needs Rust 1.95+ and Node 20+ (for the interface).

```bash
(cd web && npm ci && npm run build)     # the interface → web/dist, embedded in the binary
cargo build --release                   # target/release/trunk-pro
cargo test --release
cargo build --profile dist              # stripped, as released
```

Release packages: `packaging/package.sh <macos|linux-x86_64|linux-aarch64|windows-x86_64|browser> <version> <binary> <out-dir>`
(what CI runs; see the top of the script).

### Releasing

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

[docs/architecture.md](docs/architecture.md) explains the whole path, the
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
  Recorder's 8–23. Details, charts and method: [docs/performance.md](docs/performance.md).
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
   (x86-64, ARM), Windows~~ — done (verified live on macOS and on a Raspberry
   Pi 5; Windows builds, hardware testing there pending)
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
   testing pending), then CTCSS / DCS tones, P25 NAC and DMR colour code
   matching, so several users of one frequency can be told apart, and
   conventional DMR
9. ~~Several systems and sites at once, with site locks~~ — done (verified
   on captures), with duplicate calls across sites saved once (best copy
   kept)
10. ~~SmartNet~~ — done: 800 / 900 MHz and VHF / UHF (OBT) band plans, analog
    and digital voice, band plans found by the survey; verified live on a
    UHF OBT system
11. ~~Trunked DMR~~ — done: Capacity Plus (and Linked), Capacity Max,
    Connect Plus, Tier III; channel tables learned from the air
11a. NXDN — built: NXDN48 / NXDN96, conventional, Type-C (channel table,
    Direct Frequency Assignment or learned) and Type-D trunking; tested on
    recordings and synthesized signals, awaiting a live system
12. ~~Plugins~~ — done: a plugin protocol and Rust SDK, the registry and
    store, uploaders for OpenMHz, Broadcastify and Rdio Scanner, streaming,
    and upload scripts
13. ~~Trunk Recorder parity~~ — done: call rules per system, file name
    formats, the log format and options, unit names, Trunk Recorder config
    import

## License

GPL-3.0-or-later (the P25 code derives from op25). The vocoder derives from
mbelib (ISC).
