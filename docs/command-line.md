# Command line

This page lists every subcommand and option `trunk-pro` understands: running the app, checking
your radios, making and replaying captures, finding a system, and the diagnostic and plugin
tools. Most people only ever run `trunk-pro` with no arguments and do the rest in the browser.

## How options work

- Options are `--name value` or `--name=value`. A few are on/off switches (flags) that never take
  a value; for those, `--name=0` turns them off.
- An option that takes a value takes the next argument unless it starts with `--`. So put the
  capture file first, before the options, or use the `--name=value` form:

  ```bash
  trunk-pro replay p25.cu8 --center 858500000 --rate 2400000 --cc 857262500
  ```

- Frequencies and sample rates are in Hz.
- If an option is given twice, the last one wins (except `--source` and `--system`, which you
  can repeat).
- `trunk-pro --version` (or `-V`, `version`) prints the version. `trunk-pro --help` (or `-h`,
  `help`) prints a short summary. An unknown subcommand prints the summary and exits with
  status 2.

| Subcommand | What it does |
|---|---|
| [`serve`](#running-the-app-serve) (the default) | The recorder and its web interface |
| [`devices`](#devices) | List radios and optional drivers |
| [`capture`](#capture) | Record raw IQ from an RTL-SDR to a file |
| [`replay`](#replay) | Record calls from capture files instead of radios |
| [`survey`](#survey) | Find a system: scan the bands, then listen to the best control channel |
| [`rolloff`](#rolloff) | Measure how a radio's band edges sag, and the guard band to use |
| [`tool …`](#tool) | Diagnostics: one channel's decode, scans, weak-signal curves, re-vocoding |
| [`plugin …`](#plugin) | Find, install, update, remove and test plugins |

## Running the app (`serve`)

```bash
trunk-pro [serve] [--config file.json] [--port 8080] [--bind 127.0.0.1] [--no-open] [--start]
          [--ui folder] [--log-level info]
```

With no subcommand, or with only options (`trunk-pro --port 9000`), `trunk-pro` runs `serve`: it
loads the config, starts the web interface and opens it in your browser. Recording starts when
you press **Start** in the interface, or right away with `--start` or the config's
`server.autoStart`.

| Option | Default | What it does |
|---|---|---|
| `--config file.json` | the app's config folder | The config file to use. Everything the app learns and installs (band plans, unit names, plugins, stats history, logs) is kept in the folder this file is in, so two recorders with their own configs never share them. |
| `--port n` | `server.port` (8080) | The web interface's port, for this run only. |
| `--bind address` | `server.bind` (`127.0.0.1`) | The address to listen on, for this run only. `0.0.0.0` makes the interface reachable from other machines. There is no login: see [Remote access](guides/interface.md#remote-access). |
| `--no-open` | | Don't open a browser (headless machines, services). |
| `--start` | `server.autoStart` | Start recording at once with the saved settings. If the settings can't start, the reason is logged as `Not started: …` and the app keeps running. |
| `--ui folder` | | Serve an interface of your own (a folder with an `index.html`) at `/` for this run. The built-in interface stays at `/builtin/`. See [Interfaces of your own](guides/interface.md#interfaces-of-your-own). |
| `--log-level level` | `log.level` (`info`) | `trace`, `debug`, `info`, `warning` (or `warn`), `error` or `fatal`. Overrides the config's level for this run. |

Where the default config lives:

| System | Default config file |
|---|---|
| macOS | `~/Library/Application Support/trunk-pro/config.json` |
| Windows | `%APPDATA%\trunk-pro\config.json` |
| Linux | `$XDG_CONFIG_HOME/trunk-pro/config.json`, or `~/.config/trunk-pro/config.json` |

If the file doesn't exist, the app starts with the default config (one RTL-SDR source, no
systems) and the interface offers the setup guide. If it exists but can't be read, the app stops with
`<path>: not a config this version reads (…)`. See [Troubleshooting](troubleshooting.md).

At start the log says where everything is:

```text
Trunk Recorder Pro 0.1.4 — open http://localhost:8080
Using Config file: /home/pi/.config/trunk-pro/config.json
Capture Directory: /home/pi/TrunkRecorderPro
Log Level: info · Log to File: false · System log: false
```

**Already running?** If the port is taken by another copy of Trunk Recorder Pro (you
double-clicked the app twice), the new one opens the browser on the running one and exits. If
something else has the port, it stops with `Port 8080 is in use by another program. Start with
--port <another port>.`

**Started from Finder, Explorer or a desktop launcher**, a startup failure also shows a dialog
(macOS) or a desktop notification (Linux, via `notify-send`), since there's no terminal to read.

### Stopping, and signals

| You do | It does |
|---|---|
| **Quit** in the interface, Ctrl-C, or `SIGTERM` | Stops recording first (calls in progress are written out), tells open browsers it has quit, then exits. The log says `Caught an Exit Signal...` (for Ctrl-C and SIGTERM) and `Cleaning up & Exiting...`. |
| `SIGHUP` (Linux, macOS) | Reopens the log file, for logrotate: `Received SIGHUP signal - log file reopened`. Recording carries on. |

On Windows, closing the console window, Ctrl-Break, signing out and shutting down all stop the app the same way, saving calls in progress first.

### Examples

```bash
# The usual: open the interface on this machine
trunk-pro

# A headless Raspberry Pi: no browser, record at once, reachable on the LAN
trunk-pro serve --no-open --start --bind 0.0.0.0

# A second recorder with its own config, folder and port
trunk-pro --config ~/wmata/config.json --port 8081

# More detail in the log for this run
trunk-pro --log-level debug
```

## devices

```bash
trunk-pro devices [--usrp [args]]
```

Lists the RTL-SDRs it can see (model and serial number), then whether the optional Airspy
(libairspy), USRP (UHD) and SoapySDR drivers are installed. For Airspy it lists the devices; for
SoapySDR it lists the installed modules (and any that failed to load, with why) and the devices
found. USRPs are only searched for with `--usrp`, optionally with UHD device arguments
(`--usrp type=b200`), because a search can be slow.

```bash
trunk-pro devices
trunk-pro devices --usrp
```

## capture

```bash
trunk-pro capture <out.cu8> --freq Hz [--rate 2400000] [--gain dB] [--ppm 0] [--serial S]
                  [--seconds 10]
```

Records raw IQ from an RTL-SDR to a file, like `rtl_sdr` does: unsigned 8-bit I/Q pairs (`.cu8`).
Captures are how you test settings offline with [`replay`](#replay), and the best thing to attach
to a bug report.

| Option | Default | What it does |
|---|---|---|
| `--freq Hz` | | Centre frequency |
| `--rate Hz` | 2400000 | Sample rate |
| `--gain dB` | tuner AGC | Fixed gain |
| `--ppm n` | 0 | Frequency correction |
| `--serial S` | first dongle | Which dongle |
| `--seconds n` | 10 | How long |

When it's done it prints how many samples it got and how many the driver dropped. Stop the
recorder first if it is using the same dongle: only one program can open a dongle at a time.

```bash
trunk-pro capture p25.cu8 --freq 858500000 --rate 2400000 --gain 25 --seconds 60
```

A capture at 2.4 MS/s is 4.8 MB a second (about 290 MB a minute).

## replay

Records calls from capture files instead of radios, writing `<tg>-<epoch>_<freq>.wav` and `.json`
files like the live recorder does. It's for trying settings, comparing against Trunk Recorder on
the same air, and reproducing a problem. Replay runs as fast as the computer can go and prints
each call as it goes.

```bash
trunk-pro replay <capture> --center Hz --rate Hz --cc Hz[,Hz…] [options]
trunk-pro replay --source cap1.cu8,center,rate[,format] [--source …] --cc Hz[,Hz…] [options]
```

### The captures

| Option | Default | What it does |
|---|---|---|
| `<capture>` | | One capture file (the first argument that isn't an option) |
| `--center Hz` | | Its centre frequency |
| `--rate Hz` | 2400000 | Its sample rate |
| `--source file,center,rate[,format]` | | A capture with its own centre and rate; repeat for several radios. They're fed in step, as they would be live. |
| `--format cu8\|cs16\|cf32` | from the extension | Sample format: `cu8` (rtl_sdr), `cs16` (`.cs16`, `.sc16`), `cf32` (`.cf32`, `.cfile`, `.fc32`). Anything else is refused. |
| `--guard Hz` | 75000 | Left unused at each edge of a source's band (the guard band) |
| `--auto-tune` | off | Correct the sources' frequency error as measured on the control channels. The error is reported at the end either way. |

### A trunked system

| Option | Default | What it does |
|---|---|---|
| `--cc Hz[,Hz…]` | | The system's control channels. P25 unless `--smartnet`, `--dmr-trunk` or `--nxdn-trunk` says otherwise. |
| `--short-name name` | `replay` | The `--cc` system's short name |
| `--talkgroups file.csv` | | A talkgroup file, in the usual [format](configuration/systems.md) |
| `--bandplan file` | | Read a learned band plan from this file before, and save it after. With several systems, one file each: `<file>.<shortName>`. |
| `--system name:Hz[,Hz…][:keys]` | | Another system (or site); repeatable. The keys lock it to one site: `nac`, `sysid`, `wacn` (hex), `rfss`, `site` (decimal), and `group=name` for its site group. A control channel that disagrees isn't followed. |

With more than one system, each one's calls go to `<out>/<shortName>/`.

```bash
# Two sites of one system from one capture, each locked to its own site number
trunk-pro replay p25.cu8 --center 858500000 --rate 2400000 \
  --system north:857262500:site=1,group=county --system south:858237500:site=3,group=county
```

### SmartNet

| Option | What it does |
|---|---|
| `--smartnet plan` | The `--cc` system is SmartNet with this band plan: `800_standard`, `800_reband`, `800_splinter`, `900` or `400_custom`. Alone (`--smartnet` followed by another option) it means `800_standard`. |
| `--bp-base Hz` `--bp-spacing Hz` `--bp-offset n` `--bp-high Hz` | The `400_custom` (OBT) band plan's numbers |
| `--analog-default` | Talkgroups the control channel never marks as digital are recorded as analog FM |

### DMR and NXDN trunking

| Option | What it does |
|---|---|
| `--dmr-trunk` | The `--cc` frequencies are a trunked DMR site's |
| `--dmr-channels Hz,…` | More DMR voice frequencies to watch |
| `--color-code n` | The site's colour code |
| `--nxdn-trunk typeC\|typeD` | The `--cc` frequencies are an NXDN site's (Type-C unless `typeD` or `d`) |
| `--nxdn-rate 48\|96` | NXDN48 or NXDN96 (default 48) |
| `--nxdn-channels Hz,…` | More NXDN frequencies to watch |
| `--lcn n=Hz,…` | The site's channel table: logical channel number = frequency (DMR and NXDN) |
| `--ran n` | The NXDN site's RAN |

### Conventional channels

These work with or without `--cc`. All conventional channels in a replay belong to one system
called `conv`.

| Option | Default | What it does |
|---|---|---|
| `--fm Hz,…` | | Analog FM channels |
| `--p25 Hz,…` | | Conventional P25 channels |
| `--dmr Hz,…` | | Conventional DMR channels |
| `--nxdn48 Hz,…` `--nxdn96 Hz,…` | | Conventional NXDN channels |
| `--channels file.csv` | | Channels from a [channel file](configuration/conventional.md) |
| `--squelch dB` | 8 | How far above the noise floor a channel must rise to open |

### Calls and output

| Option | Default | What it does |
|---|---|---|
| `--out folder` | `calls` | Where calls go (created if needed) |
| `--recorders n` | 32 | Calls recorded at once |
| `--preroll s` | 1 | Audio kept from before each grant |
| `--timeout s` | 3 | Silence that ends a call |
| `--epoch s` | 0 | The Unix time the capture started, for call times and file names |
| `--no-unknown` | off | Don't record talkgroups that aren't in the talkgroup file |
| `--record-encrypted` | off | Record (and keep) encrypted calls |
| `--keep-silent` | off | Keep calls with no audio |
| `--min-call s` | 0 | Drop calls shorter than this |
| `--max-call s` | 0 (no limit) | Split calls longer than this |
| `--min-transmission s` | 0 | Leave out transmissions shorter than this |
| `--capture-frames` | off | Also write each digital call's vocoder frames as `<call>.sdr` (NXDN: `<call>.frames.jsonl`), for `tool revoice` |
| `--messages` | off | Print every control channel message |
| `--quiet` | off | Don't print control channels, calls and files as they happen |

At the end it prints how fast it ran, how many calls it wrote, and for each system the control
channel's good and bad message counts, modulation and identity (NAC, WACN, SysID, RFSS, site).
A system whose control channel belonged to another site says `NOT FOLLOWED` and why. Each source's
measured frequency error is printed too.

```bash
# A UHF SmartNet OBT system: the band plan numbers come from your system's listing
trunk-pro replay obt.cu8 --center <Hz> --rate 2400000 --cc <Hz> \
  --smartnet 400_custom --bp-base <Hz> --bp-spacing 25000 --bp-offset <n> --bp-high <Hz>

# Conventional FM channels with a stricter squelch
trunk-pro replay vhf.cu8 --center 154500000 --rate 2400000 --fm 154130000,154415000 --squelch 12
```

## survey

```bash
trunk-pro survey [--serial S] [--bands 800,700,…] [--gain dB | --no-gain] [--ppm 0] [--seconds 30]
                 [--rate 2400000]
trunk-pro survey <capture> --center Hz [--rate Hz] [--format cu8|cs16|cf32]
```

The command-line version of **Find my system**: scans the bands for control channels (P25,
SmartNet, trunked DMR and NXDN), then listens to the best one and learns the system from it. See
[Find my system](guides/find-my-system.md) for what it finds and how.

It prints a JSON line for each control channel candidate found, progress on stderr
(`survey: <stage>`), and at the end a JSON line with what it learned (`monitor`), suggested
settings (`suggest`) and how long it took.

| Option | Default | What it does |
|---|---|---|
| `--serial S` | first dongle | Which RTL-SDR |
| `--bands list` | `800,700,900,uhf,vhf` | Any of `800`, `700`, `900`, `uhf`, `vhf`, and also `uhf-fed` (380–420 MHz), `biz-uhf`, `biz-vhf` (business bands, where most DMR is) and `t-band` (470–512 MHz), which are only scanned when you name them. |
| `--gain dB` | AGC, then a gain search | A fixed gain; turns off the gain search |
| `--no-gain` | | Don't search for the best gain |
| `--ppm n` | 0 | Frequency correction while scanning |
| `--seconds n` | 30 | How long to listen to the best control channel |
| `--rate Hz` | 2400000 | Sample rate |
| `<capture>` `--center` `--format` | | Look at a capture instead of scanning with a dongle. An unknown `--format` is refused |

```bash
trunk-pro survey --bands 800,700
```

## rolloff

```bash
trunk-pro rolloff [--serial S] --center Hz [--rate 2400000] [--gain dB] [--full]
trunk-pro rolloff <capture> --center Hz --rate Hz [--format cu8|cs16|cf32]
```

Measures how far in from each edge of the band a radio's noise floor sags, and suggests the guard
band to leave there; the same measurement as **Profile roll-off** in Setup. It prints one line of
JSON. `--full` adds the spectrum, floor and waterfall rows. Without `--gain` it uses 25.4 dB. See
[Radios](guides/radios.md) for what the guard band is and how to use the result.

```bash
trunk-pro rolloff --center 858500000
```

## tool

Diagnostic tools, mostly for developers and for chasing a decoding problem down to one channel.
They read raw `cu8` captures (from `trunk-pro capture` or `rtl_sdr`) and print JSON lines on
stdout with a summary on stderr. Where a tool takes `--rate`, `--fs` is accepted as well.

| Tool | What it does |
|---|---|
| `tool cc` | One P25 control channel's TSBKs |
| `tool voice` | One P25 voice channel's frames, link control and IMBE codewords |
| `tool frames` | Counts of P25 frames by type on one channel (the summary only) |
| `tool p2` | One P25 Phase 2 TDMA channel's slots, bursts and MAC messages |
| `tool smartnet` | One SmartNet control channel's messages |
| `tool dmrscan` | Every DMR carrier in a capture |
| `tool dmr` | One DMR channel's link control, CSBKs and calls |
| `tool nxdnscan` | Every NXDN carrier in a capture |
| `tool nxdn` | One NXDN channel's messages |
| `tool nxdnsynth` | Make a synthetic NXDN capture |
| `tool snr` | Weak-signal curves: decode a strong channel again with noise added |
| `tool revoice` | Run a call's saved vocoder frames through the vocoder again |

### tool cc, voice, frames

```bash
trunk-pro tool cc <capture.cu8> --center Hz --rate Hz --cc Hz [options]
trunk-pro tool voice|frames <capture.cu8> --center Hz --rate Hz --freq Hz [options]
```

| Option | Default | What it does |
|---|---|---|
| `--demod cqpsk\|c4fm\|auto` | `auto` | Which receivers run (`auto`: both) |
| `--diversity` | off | Also run an equalised CQPSK receiver and pick the best frame |
| `--eq n` `--mu x` | 9, 0.02 | The equaliser's taps and step size |
| `--trellis greedy` | Viterbi | `cc`: the trellis decoder |
| `--soft none` | soft | Hard decisions only |
| `--softfec 0` | on | `voice`: hard-decision voice FEC |
| `--flywheel 0` `--nidrecover 0` | on | Turn off the framer's flywheel or NID recovery |
| `--audio out.f32` | | `voice`: write the decoded audio (8 kHz, 32-bit float) |
| `--profile mbelib` | enhanced | `voice`: the vocoder |
| `--iq out.cf32` | | Write the channel's IQ |

### tool p2

```bash
trunk-pro tool p2 <capture.cu8> --center Hz --rate Hz --freq Hz --nac N --sysid N --wacn N
                  [--slot 0|1] [--audio out.f32] [--eq 0] [--soft none]
```

`--nac`, `--sysid` and `--wacn` give the scrambling seed; they're decimal unless written with
`0x`. `--audio` vocodes the voice of `--slot`.

### tool smartnet

```bash
trunk-pro tool smartnet <capture.cu8> --center Hz --rate Hz --cc Hz [--bandplan 800_standard]
                        [--bp-base Hz --bp-spacing Hz --bp-offset N --bp-high Hz] [--osw]
```

Prints the control channel's messages; `--osw` also prints every OSW (and every bad one). The
summary gives good and lost OSWs a second, the carrier offset, the deviation and the system ID.

### tool dmrscan and dmr

```bash
trunk-pro tool dmrscan <capture.cu8> --center Hz --rate Hz [--seconds N] [--guard 75000]
trunk-pro tool dmr <capture.cu8> --center Hz --rate Hz --freq Hz [--bursts] [--audio out.f32 --slot 0|1]
```

`dmrscan` checks every 6.25 kHz channel and lists those with DMR on them, with sync counts, colour
codes and what they carry. `dmr` prints one channel's messages; `--bursts` prints every burst, and
`--audio` writes a slot's voice (8 kHz, 32-bit float).

### tool nxdnscan, nxdn and nxdnsynth

```bash
trunk-pro tool nxdnscan <capture.cu8> --center Hz [--rate Hz] [--seconds N] [--step 6250] [--guard 75000]
trunk-pro tool nxdn <capture.cu8> --center Hz --freq Hz [--rate Hz] [--nxdn 48|96] [--frames]
                    [--audio out.f32] [--variant …]
trunk-pro tool nxdnsynth <out.cu8> [--center 451000000] [--kind conv|typeC|typeD] [--nxdn 48|96]
                         [--rate 2400000]
```

`nxdnscan` runs every channel through both NXDN receivers and lists those with NXDN on them, with
rate and RAN. `nxdn` prints one channel's messages (`--frames`: every frame). `nxdnsynth` writes a
synthetic capture, handy for checking a setup when there's no NXDN on the air near you:

- `conv`: a conventional call at centre + 50 kHz
- `typeC` (the default): a control channel at centre + 18.75 kHz granting group 3001 on
  channel 12, whose call is at centre + 68.75 kHz
- `typeD`: repeaters at centre + 12.5 and + 62.5 kHz, the call on the second

### tool snr

```bash
trunk-pro tool snr <capture.cu8> --center Hz --rate Hz --freq Hz --kind dmr|p25|smartnet|p2
                   [--seconds N] [--snr 40,20,16,14,12,10,8,6] [--variant base,…] [--cutoff Hz]
                   [--trials 1] [--nac --sysid --wacn]
```

Takes one strong channel from a capture and decodes it again and again with noise added at each
SNR, counting how much of what the clean run decoded still comes through. It's how receiver
changes are compared. `--kind p2` needs `--nac`, `--sysid` and `--wacn` (hex). `--quality`
(with `--kind p2`) reports a real channel as it is, and `--separation` the C4FM receiver's level
separation at each SNR.

### tool revoice

```bash
trunk-pro tool revoice <call.sdr | call.frames.jsonl> <out.wav> [--profile enhanced|fixed|mbelib]
                       [--seed 1] [--hard-fec] [--s16]
```

Runs a call's saved vocoder frames through the vocoder again, for example to hear it with another
vocoder. The frames are saved when **Save vocoder frames** is on in Setup → Recording (or with
`replay --capture-frames`). `--hard-fec` uses the standard's repeat thresholds; `--s16` writes raw
16-bit samples instead of a WAV.

### tool sdr

```bash
trunk-pro tool sdr <call.sdr> [--frames]
```

Prints a saved `.sdr` call's metadata as JSON, as MimoSDR reads it, or with `--frames` its vocoder
frames as JSON lines (one frame per line: `codec`, `bits` in hex, `e0`, `errs`, `erased`, and `out`,
what the vocoder made of it).

## plugin

```bash
trunk-pro plugin search | install | update | uninstall | list | describe | run …
```

Plugins are kept beside the config, in `plugins/` (their data in `plugin-data/<id>/`). Every
`plugin` subcommand takes `--config file.json` to work on another recorder's. See
[Plugins](plugins/README.md) for using them and [Writing plugins](plugins/writing-plugins.md) for
building one.

| Subcommand | What it does |
|---|---|
| `plugin search [text]` | The plugins in the registry (github.com/TrunkRecorder/plugins), or those whose id, name or description contains `text`. Shows which you have and whether an update is out. If the registry can't be reached it says so and uses the last list fetched, or the one built in. |
| `plugin install <id \| owner/repo> [--tag v1.2.0]` | Install a plugin from the registry by id, or from a GitHub repository's release (its latest, or `--tag`). A plugin from outside the registry gets a warning that nobody has reviewed it. Installed plugins are off: turn them on on the Plugins page. |
| `plugin update [id…]` | Install newer versions of installed plugins (all, or these). Builds of your own (a `path` in the config) are left alone. |
| `plugin uninstall <id>` | Remove a plugin and forget its settings. Its data folder is kept. |
| `plugin list` | The installed plugins and those the config names: on or off, where the executable is, its name, version and what it subscribes to. Also says which M4A encoder would be used. |
| `plugin describe <executable>` | A plugin's manifest, checked |
| `plugin run <executable \| id> [<call.json \| folder>…]` | Run a plugin against calls already on disk, print what it says, then stop it. See below. |

`plugin run` is a plugin author's test bench. It sends each call to the plugin as a
`call.concluded` event; a folder means its newest calls. An id runs an installed plugin with its
settings from the config.

| Option | Default | What it does |
|---|---|---|
| `--settings file.json` | the config's | The plugin's settings: `{"config": {…}, "systems": {"<shortName>": {…}}}` |
| `--capture-dir dir` | the config's | The calls' recordings folder |
| `--limit n` | 10 | For a folder: how many of its newest calls |
| `--encoder auto\|ffmpeg\|afconvert\|fdkaac\|none` | `auto` | The M4A encoder |
| `--grace s` | 30 | How long the plugin gets to finish after the last call |
| `--data-dir dir` | a folder in the temp folder | The plugin's data folder. Not the installed plugin's, so a test run doesn't leave work for it. |

```bash
trunk-pro plugin search upload
trunk-pro plugin install openmhz
trunk-pro plugin run openmhz ~/TrunkRecorderPro/dcfd --limit 3
```

## Environment variables

| Variable | What it does |
|---|---|
| `TRUNK_PRO_UHD` | The UHD library file to load for USRPs, instead of searching for it |
| `TRUNK_PRO_AIRSPY` | The libairspy library file to load, instead of searching |
| `TRUNK_PRO_SOAPY` | The SoapySDR library file to load, instead of searching |
| `NO_COLOR` | When set (and `log.color` is empty), the log isn't coloured |
| `XDG_CONFIG_HOME` | Linux: where the default config folder goes (`$XDG_CONFIG_HOME/trunk-pro`) |
| `HOME` (or `USERPROFILE`) | Your home folder: the default config folder (macOS, Linux) and recordings folder (`~/TrunkRecorderPro`), and `~/` in paths |
| `APPDATA` | Windows: where the default config folder goes (`%APPDATA%\trunk-pro`) |
| `PATH` | Searched for an M4A encoder (`ffmpeg`, `afconvert`, `fdkaac`) |

The Docker image sets `HOME=/data` and `XDG_CONFIG_HOME=/data/config`, so everything lands in the
`/data` volume. See [Docker](install/docker.md).
