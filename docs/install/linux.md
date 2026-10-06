# Linux

This page covers installing Trunk Recorder Pro on Linux, giving your user access to the dongles,
running it as a background service on a headless machine, and reaching its interface from another
computer. A Raspberry Pi has its own page, [Raspberry Pi](raspberry-pi.md), which builds on this
one.

## What you need

- A 64-bit PC or server: **x86-64** or **ARM64** (aarch64). There is no 32-bit build.
- A distribution from about 2019 on: the binary needs **glibc 2.28** or newer. Debian 10, Ubuntu
  20.04, RHEL/Rocky/Alma 8, Raspberry Pi OS (64-bit) and anything newer are fine. Distributions
  built on musl (Alpine) can't run it; use [Docker](docker.md) there.

Nothing else is needed for RTL-SDRs. The binary is not static only so that it can load the
optional USRP, Airspy and SoapySDR drivers if you install them ([Other radios](other-radios.md)).

## Install

Download the tarball for your machine from the
[releases page](https://github.com/TrunkRecorder/trunk-recorder-pro/releases):
`trunk-pro-<version>-linux-x86_64.tar.gz` for Intel/AMD, `trunk-pro-<version>-linux-aarch64.tar.gz`
for ARM. Then:

```bash
tar xzf trunk-pro-<version>-linux-x86_64.tar.gz
cd trunk-pro-<version>-linux-x86_64
sudo ./install.sh
```

`install.sh` does three things:

- copies `trunk-pro` to `/usr/local/bin/`;
- installs a udev rule, `/etc/udev/rules.d/60-trunk-pro-rtlsdr.rules`, so that you can open the
  dongles without root (see below), and reloads udev;
- adds a **Trunk Recorder Pro** entry to your desktop's application menu.

It prints the installed version when it's done. Unplug the dongles and plug them back in so the
new rule applies to them, then start it:

```bash
trunk-pro
```

On a desktop this opens `http://localhost:8080` in your browser. On a machine without a desktop,
see [Running it headless](#running-it-headless) below. Then go to
[Getting started](../getting-started.md).

To install somewhere other than `/usr/local`, set `PREFIX`: `sudo PREFIX=/opt/trunk-pro ./install.sh`
(the udev rule always goes in `/etc/udev/rules.d/`).

### Updating

Download the new tarball and run its `install.sh` the same way; it replaces the binary. Your
settings and recordings aren't touched. If it runs as a service, restart it afterwards
(`systemctl --user restart trunk-pro`).

### Uninstalling

```bash
sudo ./install.sh --uninstall
```

This removes the binary, the udev rule and the menu entry. Your settings (`~/.config/trunk-pro`)
and recordings are kept.

## Dongle access (udev)

On Linux a USB device belongs to root unless a udev rule says otherwise. The rule `install.sh`
adds gives the `plugdev` group, and whoever is logged in at the machine's screen, access to:

- RTL-SDRs (USB IDs `0bda:2832` and `0bda:2838`);
- Airspy R2 / Mini (`1d50:60a1`);
- USRP B200 / B210 / B200mini (Ettus `2500:0020`–`0022`, NI `3923:7813` and `7814`).

For a service or an SSH session (nobody "at the screen"), your user must be in `plugdev`.
`install.sh` takes care of it: it creates the `plugdev` group if your distribution doesn't have one
(Fedora and Arch don't) and adds the user who ran `sudo ./install.sh` to it. Log out and back in
afterwards for the group to take effect. To check, or to add another user:

```bash
groups                              # is plugdev listed?
sudo usermod -aG plugdev "$USER"    # if not; then log out and back in
```

**You don't need to blacklist the DVB driver.** Linux has a TV-tuner driver (`dvb_usb_rtl28xxu`)
that grabs RTL-SDR dongles when they are plugged in. Trunk Recorder Pro detaches it from the
dongle itself when it opens it. It won't take a dongle away from another program, though: if
SDR++, GQRX or `rtl_tcp` has it open, quit that first.

If **Setup → Radios** says a dongle can't be opened with "no permission to open it", the udev rule
isn't applying: check the group, and replug the dongle.

## Running it from a terminal

```bash
trunk-pro                       # start, open the interface in a browser
trunk-pro --no-open             # start without opening a browser
trunk-pro --start               # start recording at once with the saved settings
trunk-pro devices               # list the radios it can see, and which drivers it found
trunk-pro --help                # every command
```

The log is printed to the terminal (stderr). Ctrl-C stops it; calls in progress are saved before
it exits. [Command line](../command-line.md) lists every option.

## Running it headless

On a machine you don't sit at (a server, a Pi in a closet) you want Trunk Recorder Pro to start
at boot, restart if it stops, and resume recording by itself. The tarball includes
`trunk-pro.service` for that: a **systemd user service**, which runs as your user, so it uses
the same settings and recordings folder as when you run `trunk-pro` yourself.

1. Set it up once. Either run `trunk-pro` on the machine and configure it in the browser, or do
   step 4 first and configure it through the tunnel. Turn on **Start recording when the app
   starts** in **Setup → Recording**, so that recording resumes after every restart.

2. Install and start the service. From the folder you unpacked:

   ```bash
   mkdir -p ~/.config/systemd/user
   cp trunk-pro.service ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user enable --now trunk-pro
   ```

3. Keep it running when you're logged out. A user service normally starts when you log in and
   stops when you log out. "Lingering" makes systemd start your services at boot and keep them
   running without a login:

   ```bash
   sudo loginctl enable-linger "$USER"
   ```

4. Reach the interface from another computer: see [Remote access](#remote-access).

The service runs `/usr/local/bin/trunk-pro serve --no-open` and restarts it 5 seconds after it
exits with an error. Useful commands:

```bash
systemctl --user status trunk-pro         # running? since when?
journalctl --user -u trunk-pro -f         # follow the log
systemctl --user restart trunk-pro        # after an update or a hand edit of config.json
systemctl --user stop trunk-pro
```

The log goes to the journal, because the service's console output goes there. If you also want
log files, turn on **To files** in **Setup → Recording → Log**.

Stopping the service (or `sudo systemctl reboot`) sends SIGTERM, and Trunk Recorder Pro saves the
calls in progress before it exits.

If you prefer to start recording from the service file rather than the setting, add `--start` to
its `ExecStart` line. If you installed with a different `PREFIX`, change the path in `ExecStart`
to match.

## Remote access

By default the interface only listens on `localhost`, so only a browser on the same machine can
reach it. There are two ways to use it from another computer.

**An SSH tunnel** (recommended). Nothing on the server changes and nothing is exposed to the
network. From your own computer:

```bash
ssh -L 8080:localhost:8080 you@recorder-host
```

Leave that running and open `http://localhost:8080` in your browser.

**Listening on the network.** Start it with `--bind 0.0.0.0`, which accepts connections from any
address:

```bash
trunk-pro --no-open --bind 0.0.0.0
```

For the service, edit `~/.config/systemd/user/trunk-pro.service` so the line reads
`ExecStart=/usr/local/bin/trunk-pro serve --no-open --bind 0.0.0.0`, then
`systemctl --user daemon-reload && systemctl --user restart trunk-pro`. Then browse to
`http://<machine's address>:8080`.

> **The interface has no login.** Anyone who can reach the port can change your settings,
> browse folders on the machine, install plugins and stop the recorder. Only listen on a network
> you trust, and never forward the port from the internet. For access from outside, use the SSH
> tunnel or a VPN.

While it listens only on `localhost`, Trunk Recorder Pro also refuses requests that arrive with
any other host name (to block web pages that try to reach it through DNS tricks). A reverse proxy
in front of it that passes on its own host name is refused unless that origin is listed in
`server.allowedOrigins`; see [Server and log settings](../configuration/server-and-log.md).

To use a port other than 8080, add `--port 8081` (or set it in the config).

## Where things are kept

| What | Where |
|---|---|
| Settings (`config.json`) | `~/.config/trunk-pro/` (or `$XDG_CONFIG_HOME/trunk-pro/` if you set it) |
| Band plans, talker aliases, plugins, statistics | The same folder, beside `config.json` |
| Log files (when **To files** is on) | `~/.config/trunk-pro/logs/` |
| Recordings | `~/TrunkRecorderPro/` (change it in **Setup → Recording → Recordings folder**) |

`trunk-pro --config /path/to/config.json` uses another config file. Band plans, plugins and the
rest are then kept beside that file, so two recorders with their own config files never share
them.

## M4A files

Plugins that upload compressed audio (OpenMHz, Broadcastify and others) need M4A files, which
Trunk Recorder Pro makes with `ffmpeg` (or `fdkaac`) if one is installed:

```bash
sudo apt install ffmpeg        # Debian, Ubuntu, Raspberry Pi OS
```

Without one, calls are saved as WAV only and plugins are told so.

## Problems

- **"Port 8080 is in use by another program."** Another program has the port. Start with
  `--port 8081`.
- **The dongle isn't listed.** Run `trunk-pro devices`. If it shows nothing, check `lsusb` sees
  the dongle and that no other SDR program has it open; if it lists the dongle but Setup says
  "no permission to open it", see [Dongle access](#dongle-access-udev).
- **Recording stops after you log out of SSH.** Lingering isn't on: `sudo loginctl enable-linger
  "$USER"`.
- **The service doesn't start.** `journalctl --user -u trunk-pro -e` shows why.

More in [Troubleshooting](../troubleshooting.md).
