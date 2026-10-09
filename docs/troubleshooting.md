# Troubleshooting

Ugh, why is it doing that? This page goes through the problems people run into, what the app
says when they happen, and what to try. At the end: where the log is, how to make a capture, and
what to put in a bug report.

## It won't start

### "not a config this version reads"

```text
/home/pi/.config/trunk-pro/config.json: not a config this version reads (expected `,` or `}` at line 41 column 5)
```

The config file isn't valid JSON, or a value has the wrong type (text where a number goes, say).
The part in brackets says what and where. This usually comes from a hand edit: a missing comma,
a trailing comma, or a stray quote. Fix that line, or move the file aside and start again (the
app starts with an empty config when there's no file).

A Trunk Recorder `config.json` is a different format. Don't point `--config` at one; bring it over
with **Import Trunk Recorder config…** instead (see
[Migrating from Trunk Recorder](migrating-from-trunk-recorder.md)).

The app reads the config when it starts and writes it whenever you change something in the
interface, so edit the file by hand only while the app is stopped.

Other things that stop it at start, each with its message:

| Message | Fix |
|---|---|
| `--log-level x: trace, debug, info, warning, error or fatal` | Use one of those |
| `--port x: a port number, 1–65535` | A number in that range |
| `bind address: …` | `--bind` or `server.bind` must be an IP address (`127.0.0.1`, `0.0.0.0`), not a host name |
| `--ui <folder>: no index.html there` | Point `--ui` at a folder with an `index.html` |

Started from Finder, Explorer or a desktop launcher, these show as a dialog (macOS) or a
notification (Linux) titled "Trunk Recorder Pro couldn't start".

### "Port 8080 is in use by another program"

Something else is using the port. Start on another one:

```bash
trunk-pro --port 8081
```

or set `server.port` in the config. If what's on the port is another copy of Trunk Recorder Pro,
you won't see this: the new copy opens your browser on the running one and exits, and the log
says `Trunk Recorder Pro is already running — http://localhost:8080`. To run two recorders at
once, give each its own config and port (`--config`, `--port`).

### macOS says it can't be opened

Until releases are signed with an Apple Developer ID, macOS blocks the first launch. Open the app
once, then go to **System Settings → Privacy & Security** and click **Open Anyway**. The app has no
Dock icon: it opens its interface in your browser.

### Windows warns about an unrecognised app

SmartScreen may warn the first time: click **More info → Run anyway**.

## Start is greyed out (or "Not started")

When the settings can't record, **Start** is greyed out and hovering over it says why. The same
check runs when recording starts from `--start` or **Start recording when the app starts**, and
the log says `Not started: <why>`. The reasons:

| Message | What to do |
|---|---|
| `Add a source (a dongle or a capture file).` | Setup → Radios → **Add a source** |
| `Choose the capture file to replay.` | A capture-file source has no file |
| `Add a system with a control channel, or a conventional channel.` | A trunked system only counts once it's enabled and has a control channel |
| `Every system needs a short name.` | Give it one in Setup |
| `Two systems are named "x" — each needs a short name of its own …` | Short names are folder names and how plugins tell systems apart, so they must differ (conventional systems included) |
| `Set a center frequency for source N — it couldn't be placed automatically …` | Automatic centring couldn't fit the channels. Set the centre yourself, or add a source. See [Radios](guides/radios.md). |
| `No control channel of x falls inside any source's bandwidth — move a center frequency or add a source.` | At least one control channel must be inside a source's usable band (its band minus the guard band at each edge) |
| `x: DMR (or NXDN) frequencies outside every source's bandwidth: … MHz` | Every control and voice frequency of a DMR or NXDN site has to be covered |
| `x: …` (a band plan problem) | A SmartNet system's band plan is incomplete; see [SmartNet](guides/smartnet.md) |
| `Conventional channel(s) outside every source's bandwidth: … MHz — move a center frequency or disable them.` | As it says |
| `Conventional channel … MHz is in both a and b …` | A frequency can belong to only one conventional system |
| `A conventional channel has no frequency yet.` | Fill it in or remove the row |
| `Conventional channel … MHz: …` | Its tone, NAC, colour code or RAN can't be read; see [Tones](guides/tones.md) |
| `At most 256 conventional systems.` | |

A linked channel file that can't be read also stops the start, with the file's problem as the
message.

## My dongle isn't found

Check what the app can see:

```bash
trunk-pro devices
```

Setup → Radios lists your dongles too, and says when one is `in use by another program` or the app
has `no permission to open it`. **Dongle not showing up?** on that tab has the short version of
what follows.

- **Plug it straight into the computer.** Skip hubs and extension cables; if one port fails, try
  another.
- **Only one program can use a dongle.** Quit SDR#, SDR++, GQRX, `rtl_tcp`, another recorder, and
  `trunk-pro capture`.
- **Linux: permission.** `install.sh` adds a udev rule for you. Without it, add one and replug the
  dongle:

  ```bash
  printf '%s\n' \
    'SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2838", MODE="0666"' \
    'SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2832", MODE="0666"' \
    | sudo tee /etc/udev/rules.d/20-rtlsdr.rules
  sudo udevadm control --reload-rules && sudo udevadm trigger
  ```

- **Linux: the TV-tuner driver.** Linux may claim an RTL-SDR as a TV tuner (`dvb_usb_rtl28xxu`).
  The app detaches it when it opens the dongle, but if it gets in the way, stop it for good:

  ```bash
  echo 'blacklist dvb_usb_rtl28xxu' | sudo tee /etc/modprobe.d/blacklist-rtlsdr.conf
  sudo rmmod dvb_usb_rtl28xxu
  ```

- **Windows: the USB driver.** Every RTL-SDR program on Windows needs the WinUSB driver, once. Get
  [Zadig](https://zadig.akeo.ie), choose **Options → List All Devices**, pick **Bulk-In,
  Interface (Interface 0)**, make sure **WinUSB** is selected, press **Replace Driver**, then
  replug the dongle.
- **macOS:** works as it is. If it's missing, look in **System Report → USB** for **RTL2838** or
  your radio's name; if it isn't there, try another cable or port.

### "its tuner isn't supported natively"

```text
open RTL-SDR (first): its tuner isn't supported natively (only R820T / R828D; not E4000, FC0012, FC0013 or FC2580) — add it as a SoapySDR source with device arguments driver=rtlsdr instead (needs SoapySDR's RTL-SDR module)
```

The built-in RTL-SDR driver handles the R820T and R828D tuners (nearly every current dongle). For
an older one, install SoapySDR and its RTL-SDR module and add the dongle as a SoapySDR source; see
[Other radios](install/other-radios.md).

### Source errors while recording

USB trouble shows as `Source 0: <error>` (sources counted from 0) in the log and on the RF page (`open RTL-SDR …`,
`USB: …`, `USB stream timed out`). The app reopens the dongle by itself after 2 seconds, so a
dongle that was unplugged and plugged back in carries on. If it keeps happening, suspect the cable,
the hub or the power supply (a Raspberry Pi with a weak supply is a common one).

## The control channel isn't decoding

A trunked system lives on its control channel: if that isn't decoding, nothing gets recorded.
Signs:

- The system's light is red and says `Not decoding its control channel — hunting for another.`
- The Overview's system card says `hunting` instead of a frequency.
- The log has errors like this every 10 seconds:

  ```text
  freq: 857.262500 MHz	Control Channel Message Decode Rate: 2.3/sec, count:  23
  ```

That line is logged as an error whenever a control channel decodes fewer messages a second than
**Decode rate warning** in Setup → Recording → Log (`log.controlWarnRatePerS`, 10 by default). Set
it to `-1` to log the rate every 10 seconds even when it's fine, which is handy while you tune.
Every 200 seconds the log also prints each system's average rate under `Control Channel Decode
Rates:`.

What to try, in order:

1. **The frequency.** Is it a current control channel for this site? Check RadioReference, or let
   [Find my system](guides/find-my-system.md) find it. Add the alternates too: the recorder moves
   to another when one stops.
2. **Inside the band.** The control channel should be well inside a source's band, not near its
   edge. The RF page's **The band** shows where it falls.
3. **Gain.** Look at the RF page: **Headroom** near 0 or clipping means too much; a channel only a
   few dB above the floor means too little (or the antenna). Change it a few dB at a time.
4. **Frequency error.** A radio that's several ppm off can miss the channel entirely. Turn on
   AutoTune, or set the ppm that **Frequency error** reports. See [Radios](guides/radios.md).
5. **Decode page numbers.** **Eye opening** under 4 or **Phase error** over 20° means the signal is
   weak or distorted where you are: a better antenna or location helps more than settings. On a
   simulcast system, try another site's control channel.

If it decodes but fewer messages than usual with more lost ones, the Decode page says so in
amber: that points at reception (multipath, a marginal signal), not at traffic.

## "The control channel is another system's"

```text
Control channel 857.26250 MHz is not this system: NAC 2A3 (expected 2A4)
```

The system has a site lock (`expect` in the config, set by Find my system or by hand) and the
control channel it found belongs to a different system or site. Its grants aren't followed and the
recorder keeps hunting. Either the frequency belongs to another site (fix the control channel list),
or the expected identity is wrong (fix or clear it). See [Multiple sites](guides/multi-site.md).

## Calls are empty, or missing

- **Encrypted calls.** They show as `encrypted — not recorded`. Encrypted audio can't be decoded,
  so by default they aren't recorded. `recordEncrypted` in the config records them anyway (there's
  no switch for it in the interface); they'll have no usable audio.
- **"Call not saved - no audio, or shorter than the minimum".** Calls with no decoded audio are
  dropped unless **Calls with no audio** is on in Call rules, and calls shorter than **Shortest
  call** are dropped too. See [Recordings](guides/recordings.md).
- **Garbled or broken-up audio.** Look at **Voice lost** on the Decode page. Over 8% is bad. The
  frequency table there shows whether it's one frequency (interference), the frequencies near a
  band edge (move the centre), or all of them (gain, antenna, ppm).
- **Calls you know happened aren't there.** The Radio system page's **What happened to calls**
  shows where they went: **No recorder** (raise **Recorders**), **Out of band** (a voice channel no
  source covers), **Not in the file** (unknown talkgroups are off), **Ignored**.

## Nothing is recorded on a conventional channel

Conventional channels have no control channel to say when someone talks, so the recorder opens a
channel when its signal rises far enough above the noise floor: the squelch, 8 dB by default
(`squelchDb` for a system, **Squelch dB** for a channel).

- **Nothing at all:** lower the squelch a few dB, and check on the RF page that the channel is
  inside a source's band and the frequency is right.
- **Recordings of noise:** raise it.
- **A tone, NAC, colour code or RAN on the channel:** only transmissions that match are recorded.
  If it's wrong, nothing is. The **Heard:** row under the channel in Setup → Conventional shows the
  codes actually on the air; see [Tones](guides/tones.md).

More in [Conventional](guides/conventional.md).

## Dropped samples

The source's light goes red with `Dropping samples: the computer or USB bus can't keep up. Calls on
it lose audio.`, the RF page shows **Dropped** above zero, and the event feed says
`<source>: dropping samples`. `trunk-pro capture` prints how many it dropped too.

The radio delivers samples at a fixed rate and the driver has only a small buffer: if the app
doesn't collect them in time, they're lost. Causes, most likely first:

- **CPU.** Check the Platform page and **Decoding load** on the Overview. A lower sample rate,
  fewer sources, or a faster computer helps.
- **USB.** Several dongles on one hub or one bus, a long or poor cable, or a Raspberry Pi with a weak
  power supply. Spread dongles across ports.
- **Something else on the computer** hogging it for a moment (a backup, a screen-sharing session).

**USRPs** have a very small buffer by default, so the app asks UHD for a bigger one
(`num_recv_frames=256`, about 65 ms) unless your device arguments set `num_recv_frames`
themselves. Other USRP messages: `USRP can't run at … MSPS (it offers …); set that sample rate`
(pick a rate the device supports) and `USRP: no samples for 3 s`.

## Clipping or "Peaks reach full scale"

The gain is too high, or there's a strong transmitter nearby. Lower the gain a few dB. On the RF
page, **Headroom** should stay a few dB above zero. More gain isn't better: past a point it
raises the noise and overloads the radio. See [Radios](guides/radios.md).

## No M4A files

```text
No M4A encoder found (install ffmpeg): calls are kept as WAV only
```

**Also save an M4A** needs an encoder: `ffmpeg` (any system), `afconvert` (built into macOS) or
`fdkaac`, found on your `PATH`. Install ffmpeg (`brew install ffmpeg`, `sudo apt install ffmpeg`).
`trunk-pro plugin list` says which encoder it would use. `M4A encoding is off: calls are kept as
WAV only` means the encoder is set to `none`. See [Recordings](guides/recordings.md).

## A plugin keeps failing

The Plugins page's **Health** tab shows each plugin's state and last failure.

- `[<id>] exited (1); restarting in 4 s`: the plugin crashed. It's restarted automatically, waiting
  a little longer each time (up to a minute). **Restarted N×** counts these. Open **Last 24 h and
  log ▾** for what it said before it died.
- A plugin that stops because of its settings says why in its status and isn't restarted until
  the next Start. Fix its settings in Setup → Plugins.
- `[<id>] is falling behind: N event(s) dropped so far`: the plugin is too slow to keep up, and
  events it couldn't take were dropped. **Fell behind N events** shows it on its card.
- A service shown as down: the plugin failed 3 times in a row, or hasn't succeeded in 5 minutes.
  Check your internet (Platform page) and the plugin's key or URL.

To see exactly what a plugin does with a call, run it by hand against calls already on disk:

```bash
trunk-pro plugin run openmhz ~/TrunkRecorderPro/dcfd --limit 3
```

See [Plugins](plugins/README.md).

## The RAM spool

`No RAM spool (…): calls go to the recordings folder` means the spool couldn't be made; recording
carries on, writing to disk. Spool warnings in the event feed (filling, full) mean the plugins
aren't uploading as fast as calls arrive. Nothing is lost when it's full: calls go to the
recordings folder instead. See [Recordings](guides/recordings.md).

## Spotlight

On macOS, the Platform page may say **Spotlight indexes the recordings folder**. Spotlight reads
every call you save into its index: disk writes for files nobody searches. Apps can't change
Spotlight's list, so do it yourself: **System Settings → Spotlight → Search Privacy**, click **+**,
and choose the recordings folder. The app checks again once a day. If it can't tell
(`Spotlight may be indexing…`), click **I've done it** once you have.

## A browser or page is "Refused"

```text
Refused a page from http://192.168.1.50:3000: add it to server.allowedOrigins in the config to let it use the interface.
```

The app only lets its own pages drive it, so other web pages can't. Add the page's origin to
**Other pages allowed to use the API** in Setup → Recording → Interfaces. See
[Remote access](guides/interface.md#remote-access).

## Where is the log?

The log always goes to the terminal (stderr) unless you turn that off. Set it up in Setup →
Recording → **Log**, or in the config's `log` section:

| Setting | What it does |
|---|---|
| **Level** | `info` by default. `debug` adds calls that weren't recorded; `trace` adds every control message. `--log-level` on the command line overrides it for one run. |
| **To files** | Off by default. Files go in `logs/` beside the config file (or **Folder**), named like Trunk Recorder's (`10-06-2026_1415_00.log`), a new one each day and at 100 MB. |
| **One file, for logrotate** | One `trunk-pro.log`, appended to and never rotated by the app. Send `SIGHUP` after logrotate moves it. |
| **To the system log** | syslog, on Linux and macOS |

The folder beside the config file is your app's config folder unless you used `--config`:
`~/Library/Application Support/trunk-pro` on macOS, `%APPDATA%\trunk-pro` on Windows,
`~/.config/trunk-pro` on Linux. Running as the systemd user service, the terminal output goes to
the journal:

```bash
journalctl --user -u trunk-pro -f
```

The Live page's **Control channel log** is a live view of control messages, not this log.

## Making a capture

A capture is a recording of the raw radio signal. It lets someone else replay exactly what your
radio heard, which is the most useful thing you can attach to a decoding problem. Stop recording
first (the dongle can only be open once), then:

```bash
trunk-pro capture problem.cu8 --freq 858500000 --rate 2400000 --gain 25 --seconds 30
```

Use the same centre, rate and gain as your source in Setup. Thirty seconds at 2.4 MS/s is about
145 MB, so compress it before sharing it. You can replay it yourself to try settings:

```bash
trunk-pro replay problem.cu8 --center 858500000 --rate 2400000 --cc 857262500
```

See [Command line](command-line.md) for all of `capture` and `replay`.

## Reporting a bug

Open an issue at
[github.com/TrunkRecorder/trunk-recorder-pro/issues](https://github.com/TrunkRecorder/trunk-recorder-pro/issues).
Include:

- the version (`trunk-pro --version`) and your system (macOS, Windows, Linux or Raspberry Pi, and
  which radio);
- what you did, what you expected, and what happened;
- the log around the problem, at `debug` level if you can;
- your config file. It can hold plugin keys and passwords: remove them first;
- for a decoding problem, a short capture (see above) and the system's frequencies, or its
  RadioReference page;
- for a dashboard problem, a screenshot.
