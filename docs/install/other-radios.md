# Other radios

This page covers using radios other than a current RTL-SDR: USRPs, Airspy R2 / Mini, HydraSDR RFOnes, bladeRFs, anything with
a SoapySDR module, and older RTL-SDR dongles. It explains which driver each needs, how Trunk
Recorder Pro finds it, and how to check it worked. These radios only work in the desktop app, not
in the [browser version](browser.md) or the [Docker image](docker.md). The exceptions are the
[HackRF One](#hackrf-one), the [Airspy R2 / Mini](#airspy) and the [HydraSDR RFOne](#hydrasdr), which need no driver: all work in Docker.

## How drivers are found

RTL-SDRs with an R820T, R828D or E4000 tuner, HackRF Ones, Airspy R2 / Minis and HydraSDR RFOnes need nothing: Trunk Recorder Pro has its
own driver for them. Every other radio uses its maker's driver library, which you install yourself. Trunk
Recorder Pro isn't built against these libraries. It looks for each one when it starts, so the
same download works with or without them:

| Radio | Driver library | Source type in Setup |
|---|---|---|
| USRP (Ettus / NI) | UHD (`libuhd`) | **USRP** |
| bladeRF 2.0 micro xA4 / xA9, original bladeRF x40 / x115 | libbladeRF 2.x | **bladeRF** |
| SDRplay, LimeSDR, PlutoSDR, Airspy HF+, older RTL-SDRs, ... | SoapySDR plus the device's module | **SoapySDR** |

The driver is looked for once, when Trunk Recorder Pro starts. **After installing a driver,
restart Trunk Recorder Pro.** If you choose a source type whose driver wasn't found, **Setup →
Radios** says what's missing and how to install it.

## Installing the drivers

| | macOS (Homebrew) | Debian / Ubuntu / Raspberry Pi OS | Windows |
|---|---|---|---|
| USRP | `brew install uhd` | `sudo apt install libuhd-dev uhd-host` | Ettus's UHD installer (puts `uhd.dll` on the `PATH`) |
| bladeRF | `brew install libbladerf` | `sudo apt install libbladerf2 bladerf-fpga-hostedxa4` (xA9: `bladerf-fpga-hostedxa9`; or Nuand's PPA `ppa:nuandllc/bladerf`) | Nuand's bladeRF installer (`bladeRF.dll`) |
| SoapySDR | `brew install soapysdr` and the device's module, e.g. `brew install soapyhackrf` | `sudo apt install libsoapysdr0.8` and the device's module, e.g. `soapysdr0.8-module-hackrf` (or `soapysdr0.8-module-all`) | PothosSDR or radioconda |

**USRPs also need UHD's FPGA images**, downloaded once after installing UHD:

```bash
uhd_images_downloader          # sudo uhd_images_downloader on Linux
```

On Linux, the udev rule from the [Linux](linux.md#dongle-access-udev) package already covers the
Airspy R2 / Mini, the HydraSDR RFOne and the USB USRPs (B200, B210, B200mini, B206mini); UHD's own package
usually adds rules for its radios as well. bladeRFs get theirs from the libbladeRF package
(`88-nuand-bladerf*.rules`). Other SoapySDR devices need the udev rules their own packages
install.

On macOS the app can load libraries installed by Homebrew (`/opt/homebrew/lib` or
`/usr/local/lib`) or MacPorts (`/opt/local/lib`).

### Checking what was found

```bash
trunk-pro devices              # RTL-SDRs, Airspys, HydraSDRs, bladeRFs, which drivers were found, SoapySDR modules and devices
trunk-pro devices --usrp       # also search for USRPs (it can take a few seconds)
```

On macOS, the command is inside the app:
`"/Applications/Trunk Recorder Pro.app/Contents/MacOS/trunk-pro" devices`.

The output says, for each driver, either its version (for example `USRP: UHD 4.6.0`) or why it
wasn't loaded: "not found" with the names it looked for, or "found but couldn't be loaded" with
the reason (the wrong architecture, or a library missing its own dependencies). For SoapySDR it
also lists each installed module and the devices they can open, or "no modules installed" with
the folders it looked in. Without a module SoapySDR can't open any radio.

### A driver in an unusual place

If you installed a driver somewhere Trunk Recorder Pro doesn't look, name the library file with
an environment variable:

| Variable | For |
|---|---|
| `TRUNK_PRO_UHD` | UHD, e.g. `/opt/uhd/lib/libuhd.so.4.6.0` |
| `TRUNK_PRO_BLADERF` | libbladeRF |
| `TRUNK_PRO_SOAPY` | SoapySDR |

```bash
TRUNK_PRO_UHD=/opt/uhd/lib/libuhd.so.4.6.0 trunk-pro devices --usrp
```

When the variable is set, only that file is tried. Use these, not `LD_LIBRARY_PATH`, if you gave
`trunk-pro` the real-time priority capability (`setcap`): such a program's loader ignores
`LD_LIBRARY_PATH` (see [Dropped samples](../troubleshooting.md#dropped-samples)). For the systemd service, add an
`Environment=TRUNK_PRO_UHD=...` line to the `[Service]` section of `trunk-pro.service`.

## Setting up each radio

Add the radio in **Setup → Radios** with **Add a source**, and pick its type at the top of the
card. The settings each type shares with the RTL-SDR (center frequency, sample rate, frequency
correction, AutoTune, guard band) are explained in [Radios](../guides/radios.md), and every key is
listed in [Sources](../configuration/sources.md). What's particular to each:

### USRP

- **USRP**: UHD device arguments. Leave it blank for the first USRP found, or use
  `serial=...`, `type=b200`, or `addr=192.168.10.2` for a networked one. **Find** searches and
  offers what it found.
- **Sample rate**: any rate the device's clock supports. Wider covers more spectrum but costs
  more CPU (8 MSPS covers about 7 MHz).
- **Gain, dB**: 0–76 on a B200 / B210. **AGC** uses the device's own AGC instead, on the B200 /
  B210 and E3xx only.
- **Antenna**: for example `RX2` or `TX/RX`. Blank uses the device's default.
- **Wire format**: sc16, sc12 (B2xx only) or sc8; blank chooses. A B2xx on a USB 2.0 link above
  8 MSPS gets sc12, and the log says which link and format it found. The buffer UHD keeps is
  asked for as `num_recv_frames=1024` (about 260 ms at 8 MSPS) unless the device arguments set it.
- A B2xx whose master clock UHD would set equal to the sample rate (sidebands 270–310 kHz either side of strong carriers) gets it doubled,
  unless the device arguments set `master_clock_rate`.
- The B206mini needs UHD 4.9 or newer (Debian trixie's package is 4.8); it uses the b205mini FPGA image.

### Airspy

An **Airspy** source is driven directly over USB, like an RTL-SDR: nothing to install. The Linux
udev rule covers it, and on Windows an Airspy installs its own WinUSB driver when it's plugged in
(no Zadig).

On Windows, an Airspy whose driver was replaced with Zadig (libusbK or libusb-win32) can't be opened: switch it
back to WinUSB. `trunk-pro devices` shows `Airspy: built in`. If an Airspy misbehaves,
[tell us](https://github.com/TrunkRecorder/trunk-recorder-pro/issues).

- **Airspy**: pick it from the list (blank is the first one found). Each is listed with its model
  ("Airspy R2 · SN …"), asked once per run. One another program has open is listed too, without
  its model.
- **Sample rate**: 10 or 2.5 MSPS on the R2, 6 or 3 MSPS on the Mini. The list offers what the
  Airspy picked runs at, and moves to one of its rates when you pick a different Airspy. A rate it
  can't do is flagged here, and the source won't start at it ("this Airspy R2 runs at 10 / 2.5
  MSPS, not 6" in the log).
- **Gain**: **Linearity** (best near transmitters) or **Sensitivity** (best for weak signals),
  each a single step from 0 to 21, or **Each stage** to set the LNA (0–14), mixer (0–15) and VGA
  (0–15) yourself. With **Each stage**, **AGC** hands the LNA and mixer to the Airspy's AGC.
- **Bias-T**: powers an LNA through the antenna cable. Leave it off unless you have one.

### HydraSDR

A **HydraSDR** source is driven directly over USB too, by the same built-in driver as an Airspy: nothing to install. The Linux udev rule covers it, and on Windows it uses the WinUSB driver it binds by itself (a Zadig libusbK or libusb-win32 driver can't be opened: switch it back to WinUSB). `trunk-pro devices` shows `HydraSDR: built in`. The RFOne support hasn't been tested on real hardware by us yet: if it misbehaves, please [open an issue](https://github.com/TrunkRecorder/trunk-recorder-pro/issues).

- **HydraSDR**: pick it from the list (blank is the first one found), listed as
  "HydraSDR RFOne · SN …". Either firmware works: the one RFOnes shipped with (v1.0.0-rc2)
  enumerates with the Airspy's USB ID, `1d50:60a1`, and updated firmware (v1.0.2) with HydraSDR's
  own, `38af:0001`. An RFOne is found by its own USB ids, serial and firmware, so an Airspy source never opens it.
- **Sample rate**: 10, 5 or 2.5 MSPS.
- **Gain**: as an Airspy's: **Linearity** or **Sensitivity** (one step, 0–21), or **Each stage**
  (LNA 0–14, mixer 0–15, VGA 0–15, with **AGC** for the LNA and mixer).
- **Input**: ANT, CABLE1 or CABLE2.
- **Bias-T**: powers an LNA through the ANT port (4.5 V, up to 300 mA). It's on ANT only: with
  another input picked, the source won't start until it's turned off.

### bladeRF

- **bladeRF**: pick it from the list by serial (blank is the first one found). Boards another
  program already has open are marked; name the serial whenever more than one is plugged in.
- **Sample rate**: any rate from 0.52 to 61.44 MSPS on a bladeRF 2.0, up to 40 on the original.
  Over about 40 MSPS needs USB 3 and a fast CPU. Once a source has opened the board, Setup shows
  its range and flags a rate outside it.
- **Gain, dB**: one overall RX gain (bladeRF 2.0: about −15 to 60), or **AGC** for the board's own.
- **Bandwidth**: the analog low-pass filter; blank follows the sample rate.
- **Bias-T**: powers an LNA through the antenna cable (bladeRF 2.0 only).

Samples lost over USB are counted from the board's sample timestamps and show as **Dropped** on
the RF page. A bladeRF that SoapySDR finds is pointed to the **bladeRF** source type instead.

### HackRF One

A **HackRF** source is driven directly over USB, like an RTL-SDR: no driver, no SoapySDR. It needs
firmware 2021.03 or newer; an older one is refused when opened, with a message to update it
(`hackrf_spiflash`). The Linux udev rule covers it.

- **Device**: the board's serial; blank opens the first one.
- **Sample rate**: 1 to 20 MSPS.
- **LNA, dB** (0–40, steps of 8, default 16) and **VGA, dB** (0–62, steps of 2, default 20).
- **RF amp (+14 dB)**: off by default; it overloads near strong signals.
- **Bias-T**: powers an LNA through the antenna port.

A SoapySDR source with `driver=hackrf` still works, for a rad1o or a HackRF behind SoapyRemote.

### Radios on the network: rtl_tcp and SpyServer

An RTL-SDR behind **rtl_tcp**, or a **SpyServer**, is a radio on another machine, reached over TCP by its address: no driver, no
SoapySDR, and nothing to find (a network can't be searched), so you type the **Server address** in Setup → Radios. Once it's saved,
the recorder asks the server what it is (its tuner or receiver, rates and gain steps) and Setup shows it, or *Not answering*.

- **rtl_tcp**: on the machine with the dongle, run `rtl_tcp -a 0.0.0.0` (port 1234; `-p` changes it), and open that port to this
  machine. In Setup it's an **RTL-SDR** source with **Connection** set to **rtl_tcp**. The address is just the host when the server is on port 1234 (`pi.local`), and `host:port` when it isn't
  (`pi.local:4321`). It serves one client at a time: a dongle another client has open doesn't answer a second, which shows as a source that
  connects but says nothing.
- **SpyServer**: on the machine with an Airspy (or an RTL-SDR), run Airspy's `spyserver` (port 5555). Any number of clients can
  listen, but only the first can tune the receiver and set its gain. If a source is refused as out of reach, another client (SDR#,
  SDR++) has the receiver on another frequency: close it, or put the source's center inside its window.

A source that loses its server is retried every few seconds and starts again by itself, so a dongle's machine can reboot without
restarting the recorder. Use a wired link for anything beyond 1 MSPS.

### SoapySDR

- **Device**: SoapySDR device arguments, such as `driver=sdrplay,serial=...` or `driver=sdrplay,serial=...`.
  **Find** searches and offers what it found. Blank opens the first device found.
- **Sample rate**: one the device supports.
- **Gain, dB** and **AGC**: an overall gain, or the device's AGC (on by default for SoapySDR).
- **Gain stages, dB**: set each of the device's gain stages by name, for example SDRplay IFGR / RFGR
  or a rad1o's LNA (0–40), VGA (0–62) and AMP (0 or 14). Not used with AGC on.
- **Antenna**: blank uses the device's default.
- **Device settings**: `key=value` settings, such as `biastee=true`. `SoapySDRUtil --probe` lists
  what your device accepts.

A radio on another machine works through **SoapyRemote** (`SoapySDRServer` there, SoapyRemote's module
here), with device arguments such as `driver=remote,remote=tcp://<server>:55132,remote:driver=airspy,remote:prot=tcp`.
At 10 MSPS that's about 40 MB/s on the network: use a wired link.

## Older RTL-SDR dongles

Trunk Recorder Pro's own RTL-SDR driver handles the **R820T** and **R828D** tuners, which are in
nearly every dongle sold today, including the RTL-SDR Blog V3, V4 and V4L, and the older **E4000**.
Dongles with an **FC0012**, **FC0013** or **FC2580** tuner show up in the list but fail to open
with a message that the tuner isn't supported natively.

Those work through SoapySDR instead:

1. Install SoapySDR's RTL-SDR module: `brew install soapyrtlsdr` on macOS,
   `sudo apt install soapysdr0.8-module-rtlsdr` on Debian / Ubuntu / Raspberry Pi OS.
2. Restart Trunk Recorder Pro.
3. Add a **SoapySDR** source with the device arguments `driver=rtlsdr` (or
   `driver=rtlsdr,serial=...` to pick one of several).

This is desktop-only; the browser version can't use these dongles.
