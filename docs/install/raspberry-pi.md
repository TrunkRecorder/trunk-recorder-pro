# Raspberry Pi

This page covers setting up a Raspberry Pi as a dedicated recorder: which Pi and OS to use, power
and heat, installing Trunk Recorder Pro, and running it headless. Once it's installed, the
[Linux](linux.md) page applies too.

## Which Pi

Trunk Recorder Pro is light enough that a Pi is a good home for it. It has been tested on a
Raspberry Pi Compute Module 5 (the same processor as a Pi 5) recording a P25 system from one
RTL-SDR: about 5 % of one core with a call being recorded, each extra call adding under a point,
and 56 MB of memory ([Performance](../performance.md) has the details).

- **Raspberry Pi 5** is the best choice and the one that has been measured.
- **Raspberry Pi 4** runs the same build. It is slower and hasn't been benchmarked; watch the
  **Platform** page's CPU figures when you first set it up.
- **Older Pis and the Pi Zero** aren't worth the trouble. They need a 64-bit OS too (below), and
  their USB is weak for SDRs.

## Power and heat

Get a good power supply. A Pi that doesn't get enough power slows itself down, and dongles draw
from the same supply: for a Pi 5, use the official 27 W USB-C supply. If you run more than one or
two dongles, put them on a powered USB hub.

Also pay attention to heat. If the Pi gets too hot, it slows down. A case with a heatsink or a
fan keeps it running at full speed. (The test Pi peaked at 71 °C during the measurements and
never throttled.)

Trunk Recorder Pro watches for both: when the Pi's firmware reports under-voltage or heat
throttling, the dashboard warns "The Pi is throttled (under-voltage or heat)", and the
**Platform** page shows the temperature with "throttled now" under it.

## Install the OS

**You need a 64-bit OS.** Trunk Recorder Pro is released for ARM64 (`aarch64`) Linux only; there
is no 32-bit ARM build. Use **Raspberry Pi OS Lite (64-bit)**. Lite has no desktop, which you
don't need: you'll use the interface from your own computer's browser.

The easiest way to set up a Pi without a monitor or keyboard is
[Raspberry Pi Imager](https://www.raspberrypi.com/software/):

1. Choose your Pi model, then **Raspberry Pi OS (other) → Raspberry Pi OS Lite (64-bit)**, then
   your SD card.
2. When it asks about OS customisation, choose **Edit settings** and set:
   - a hostname (this page uses `recorder`);
   - a username and password;
   - your Wi-Fi network and country, if you won't use Ethernet (Ethernet is more reliable);
   - your time zone, so call times and file names are local;
   - under **Services**, **Enable SSH** (with a password, or better, your public key).
3. Write the card, put it in the Pi and power it up. The first boot takes a minute or two.

From your own computer, connect to it:

```bash
ping recorder.local
ssh you@recorder.local
```

Then bring it up to date:

```bash
sudo apt update && sudo apt full-upgrade -y
uname -m                    # should print aarch64
```

If `uname -m` prints `armv7l`, the OS is 32-bit and Trunk Recorder Pro won't run on it. Write a
64-bit image instead.

## Install Trunk Recorder Pro

Download the **`linux-aarch64`** package on the Pi. Replace `<version>` with the latest version from
the [releases page](https://github.com/TrunkRecorder/trunk-recorder-pro/releases) (for example
`0.1.4`):

```bash
curl -LO https://github.com/TrunkRecorder/trunk-recorder-pro/releases/download/v<version>/trunk-pro-<version>-linux-aarch64.tar.gz
tar xzf trunk-pro-<version>-linux-aarch64.tar.gz
cd trunk-pro-<version>-linux-aarch64
sudo ./install.sh
```

The installer puts `trunk-pro` in `/usr/local/bin` and adds the udev rule that lets your user open
the dongles. The first user Raspberry Pi OS creates is already in the `plugdev` group the rule
uses; check with `groups`. Plug in (or replug) the dongles now.

You don't need to blacklist the DVB TV driver as older guides tell you to: Trunk Recorder Pro
detaches it from the dongle itself.

To make M4A files for plugins that upload compressed audio, also install ffmpeg:

```bash
sudo apt install -y ffmpeg
```

Check that the dongles are seen:

```bash
trunk-pro devices
```

## Set it up

The Pi has no browser, so reach the interface from your computer through an SSH tunnel. On your
computer:

```bash
ssh -L 8080:localhost:8080 you@recorder.local
```

and in that SSH session, on the Pi:

```bash
trunk-pro --no-open
```

Open `http://localhost:8080` in your computer's browser and follow
[Getting started](../getting-started.md). Before you leave it, turn on **Start recording when the
app starts** in **Setup → Recording**. Press Ctrl-C on the Pi when you're done; the settings are
saved.

## Run it as a service

To have it start at boot and keep recording without you logged in, install the systemd user
service that came in the package (from the folder you unpacked):

```bash
mkdir -p ~/.config/systemd/user
cp trunk-pro.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now trunk-pro
sudo loginctl enable-linger "$USER"
```

The last line ("lingering") is what lets it run at boot and after you log out. Check on it with
`systemctl --user status trunk-pro` and follow the log with `journalctl --user -u trunk-pro -f`.

From now on, use the SSH tunnel whenever you want the interface:
`ssh -L 8080:localhost:8080 you@recorder.local`, then `http://localhost:8080`. To reach it
directly from your network instead, and the warning that goes with that, see
[Remote access](linux.md#remote-access).

## Saving the SD card

SD cards wear out with constant writing, and a recorder writes every call. Some ways to make the
card last:

- Keep recordings on a USB SSD or a network share instead: set **Setup → Recording → Recordings
  folder** to a folder on it.
- If calls are only uploaded (by a plugin) and not kept, turn on **Keep calls waiting to upload in
  memory** in **Setup → Recording**: calls wait for their upload on a RAM disk instead of the card.
  See [Recordings](../guides/recordings.md).
- Boot from an SSD (a Pi 5 can boot from NVMe or USB).

## Where things are kept

As on any Linux machine: settings in `~/.config/trunk-pro/`, recordings in `~/TrunkRecorderPro/`.
See [Linux: where things are kept](linux.md#where-things-are-kept).

## Updating

Download the new `linux-aarch64` tarball as above, run its `sudo ./install.sh`, then
`systemctl --user restart trunk-pro`.
