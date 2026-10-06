# Other radios

This page covers using radios other than a current RTL-SDR: USRPs, Airspy R2 / Mini, anything with
a SoapySDR module, and older RTL-SDR dongles. It explains which driver each needs, how Trunk
Recorder Pro finds it, and how to check it worked. These radios only work in the desktop app, not
in the [browser version](browser.md) or the [Docker image](docker.md).

## How drivers are found

RTL-SDRs with an R820T or R828D tuner need nothing: Trunk Recorder Pro has its own driver for
them. Every other radio uses its maker's driver library, which you install yourself. Trunk
Recorder Pro isn't built against these libraries. It looks for each one when it starts, so the
same download works with or without them:

| Radio | Driver library | Source type in Setup |
|---|---|---|
| USRP (Ettus / NI) | UHD (`libuhd`) | **USRP** |
| Airspy R2, Airspy Mini | libairspy | **Airspy** |
| HackRF, SDRplay, LimeSDR, PlutoSDR, BladeRF, Airspy HF+, older RTL-SDRs, ... | SoapySDR plus the device's module | **SoapySDR** |

The driver is looked for once, when Trunk Recorder Pro starts. **After installing a driver,
restart Trunk Recorder Pro.** If you choose a source type whose driver wasn't found, **Setup →
Radios** says what's missing and how to install it.

## Installing the drivers

| | macOS (Homebrew) | Debian / Ubuntu / Raspberry Pi OS | Windows |
|---|---|---|---|
| USRP | `brew install uhd` | `sudo apt install libuhd-dev uhd-host` | Ettus's UHD installer (puts `uhd.dll` on the `PATH`) |
| Airspy | `brew install airspy` | `sudo apt install libairspy0` | `airspy.dll` from the airspy-tools release, next to `trunk-pro.exe` |
| SoapySDR | `brew install soapysdr` and the device's module, e.g. `brew install soapyhackrf` | `sudo apt install libsoapysdr0.8` and the device's module, e.g. `soapysdr0.8-module-hackrf` (or `soapysdr0.8-module-all`) | PothosSDR or radioconda |

**USRPs also need UHD's FPGA images**, downloaded once after installing UHD:

```bash
uhd_images_downloader          # sudo uhd_images_downloader on Linux
```

On Linux, the udev rule from the [Linux](linux.md#dongle-access-udev) package already covers the
Airspy R2 / Mini and the USB USRPs (B200, B210, B200mini); UHD's and libairspy's own packages
usually add rules for them as well. Other SoapySDR devices need the udev rules their own packages
install.

On macOS the app can load libraries installed by Homebrew (`/opt/homebrew/lib` or
`/usr/local/lib`) or MacPorts (`/opt/local/lib`).

### Checking what was found

```bash
trunk-pro devices              # RTL-SDRs, Airspys, which drivers were found, SoapySDR modules and devices
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
| `TRUNK_PRO_AIRSPY` | libairspy |
| `TRUNK_PRO_SOAPY` | SoapySDR |

```bash
TRUNK_PRO_UHD=/opt/uhd/lib/libuhd.so.4.6.0 trunk-pro devices --usrp
```

When the variable is set, only that file is tried. For the systemd service, add an
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

### Airspy

- **Airspy**: pick it from the list (blank is the first one found).
- **Sample rate**: 10 or 2.5 MSPS on the R2, 6 or 3 MSPS on the Mini.
- **Gain**: **Linearity** (best near transmitters) or **Sensitivity** (best for weak signals),
  each a single step from 0 to 21, or **Each stage** to set the LNA (0–14), mixer (0–15) and VGA
  (0–15) yourself. With **Each stage**, **AGC** hands the LNA and mixer to the Airspy's AGC.
- **Bias-T**: powers an LNA through the antenna cable. Leave it off unless you have one.

### SoapySDR

- **Device**: SoapySDR device arguments, such as `driver=hackrf` or `driver=sdrplay,serial=...`.
  **Find** searches and offers what it found. Blank opens the first device found.
- **Sample rate**: one the device supports.
- **Gain, dB** and **AGC**: an overall gain, or the device's AGC (on by default for SoapySDR).
- **Gain stages, dB**: set each of the device's gain stages by name, for example HackRF LNA
  (0–40), VGA (0–62) and AMP (0 or 14), or SDRplay IFGR / RFGR. Not used with AGC on.
- **Antenna**: blank uses the device's default.
- **Device settings**: `key=value` settings, such as `biastee=true`. `SoapySDRUtil --probe` lists
  what your device accepts.

## Older RTL-SDR dongles

Trunk Recorder Pro's own RTL-SDR driver handles the **R820T** and **R828D** tuners, which are in
nearly every dongle sold today, including the RTL-SDR Blog V3 and V4. Older dongles with an
**E4000**, **FC0012**, **FC0013** or **FC2580** tuner show up in the list but fail to open with a
message that the tuner isn't supported natively.

Those work through SoapySDR instead:

1. Install SoapySDR's RTL-SDR module: `brew install soapyrtlsdr` on macOS,
   `sudo apt install soapysdr0.8-module-rtlsdr` on Debian / Ubuntu / Raspberry Pi OS.
2. Restart Trunk Recorder Pro.
3. Add a **SoapySDR** source with the device arguments `driver=rtlsdr` (or
   `driver=rtlsdr,serial=...` to pick one of several).

This is desktop-only; the browser version can't use these dongles.
