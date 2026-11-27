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
| [`tune`](#tune) | Find a radio's best gain on a recorder that is recording |
| [`tool …`](#tool) | Diagnostics: one channel's decode, scans, re-vocoding; [developer tools](developer/tools.md) too |
| [`plugin …`](#plugin) | Find, install, update, remove and test plugins |
| [`interface …`](#interface) | Find, install, update and remove interfaces, or add a folder of your own |
| [`access …`](#access) | Signing in: users and their access tokens, and turning it on or off |

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
| `--port n` | `server.port` (8080) | The built-in interface's port, for this run only. |
| `--bind address` | `server.bind` (`127.0.0.1`) | The address to listen on, for this run only. `0.0.0.0` makes the interface reachable from other machines. Signing in is off unless you turn it on ([`access`](#access)): see [Remote access and signing in](guides/remote-access.md). |
| `--no-open` | | Don't open a browser (headless machines, services). |
| `--start` | `server.autoStart` | Start recording at once with the saved settings. If the settings can't start, the reason is logged as `Not started: …` and the app keeps running. |
| `--ui folder` | | Serve a folder with an `index.html` as an interface for this run, with control access, on the first free port above the built-in interface's, and open it instead of the built-in one. Nothing is saved. See [Interfaces](interfaces/README.md). |
| `--log-level level` | `log.level` (`info`) | `trace`, `debug`, `info`, `warning` (or `warn`), `error` or `fatal`. Overrides the config's level for this run. |

Where the default config lives:

| System | Default config file |
|---|---|
| macOS | `~/Library/Application Support/trunk-pro/config.json` |
| Windows | `%APPDATA%\trunk-pro\config.json` |
| Linux | `$XDG_CONFIG_HOME/trunk-pro/config.json`, or `~/.config/trunk-pro/config.json` |

If the file doesn't exist, the app starts with the default config (one RTL-SDR source, no
systems) and the interface offers the setup guide. If it exists but can't be read, the app stops with
`<path>: not a config this version reads (…)`: a wrong type, or a choice the app doesn't list (a
system `type` of `P25x`). See [Troubleshooting](troubleshooting.md).

At start the log says where everything is:

```text
Trunk Recorder Pro 0.4.0 — open http://localhost:8080
Using Config file: /home/pi/.config/trunk-pro/config.json
Capture Directory: /home/pi/TrunkRecorderPro
Call store: sqlite /home/pi/.config/trunk-pro/trunk-pro.sqlite
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

Lists the RTL-SDRs it can see (model and serial number; one the app is recording from is shown as `recording`, not opened), the HackRFs, the Airspys and the HydraSDRs (the Airspy and HydraSDR drivers are built in: `Airspy: built in`, `HydraSDR: built in`), then whether the optional bladeRF (libbladeRF), USRP (UHD) and SoapySDR drivers are installed. For Airspy,
HydraSDR and bladeRF it lists the devices: Airspys with their model (R2 or Mini) and the sample rates they run
at, HydraSDRs with theirs, bladeRFs with their full serials; for
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
                  [--seconds 10] [--buflen n] [--buffers n]
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
| `--buflen n` `--buffers n` | 0 (the driver's) | USB transfer size in bytes and transfers in flight, as a source's [`bufLen` and `buffers`](configuration/sources.md#type-rtlsdr). `survey` and `rolloff` take them too |

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
| `<capture>` | | One capture file (the first argument that isn't an option), or a SigMF capture: its folder, `.sigmf-collection` or `.sigmf-meta`. A SigMF capture says its centre, rate and format; [one made by the recorder](guides/captures.md#capture-what-trp-receives) also says its systems, so it needs no other options. |
| `--center Hz` | | Its centre frequency |
| `--rate Hz` | 2400000 | Its sample rate |
| `--source file,center,rate[,format]` | | A capture with its own centre and rate; repeat for several radios. They're fed in step, as they would be live. |
| `--format cu8\|cs16\|cf32` | from the extension | Sample format: `cu8` (rtl_sdr), `cs16` (`.cs16`, `.sc16`), `cf32` (`.cf32`, `.cfile`, `.fc32`). Anything else is refused. |
| `--guard Hz` | 75000 | Left unused at each edge of a source's band (the guard band) |
| `--auto-tune` | off | Correct the sources' frequency error as measured on the control channels and calls. The error is reported at the end either way. |
| `--threads` | off | Feed each source from a thread of its own, as live, none far ahead of the slowest, instead of in step on one thread. |

### A trunked system

| Option | Default | What it does |
|---|---|---|
| `--cc Hz[,Hz…]` | | The system's control channels. P25 unless `--smartnet`, `--dmr-trunk` or `--nxdn-trunk` says otherwise. |
| `--short-name name` | `replay` | The `--cc` system's short name |
| `--talkgroups file.csv` | | A talkgroup file, in the usual [format](configuration/systems.md#talkgroup-file). Every system in the replay uses it, the conventional one too |
| `--bandplan file` | | Read a learned band plan from this file before, and save it after. With several sites, one file each: `<file>.<shortName>`, or `<file>.<shortName>.<site>` for a site with a name of its own. |
| `--system name[/site]:Hz[,Hz…][:keys]` | | Another system, or with `/site` a site of one (`county/south` is site `south` of system `county`); repeatable. A call heard on several sites of a system is saved once. The keys lock it to one site: `nac`, `sysid`, `wacn` (hex), `rfss`, `site` (decimal). A control channel that disagrees isn't followed. |

With more than one system, each one's calls go to `<out>/<shortName>/`.

```bash
# Two sites of one system from one capture, each locked to its own site number
trunk-pro replay p25.cu8 --center 858500000 --rate 2400000 \
  --system county/north:857262500:site=1 --system county/south:858237500:site=3
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
| `--am Hz,…` | | Analog AM channels (airband) |
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
| `--keys file` | none | Decrypt P25 voice with these [keys](configuration/systems.md#encryption-keys-keys) (every system): a JSON file, or a [KFDtool container](configuration/systems.md#keys-in-a-kfdtool-container) with `--keys-password-file` |
| `--keys-password-file file` | none | The file holding the password of a key container given to `--keys` |
| `--keep-silent` | off | Keep calls with no audio |
| `--min-call s` | 0 | Drop calls shorter than this |
| `--max-call s` | 0 (no limit) | Split calls longer than this |
| `--transmissions` | off | Conversation mode off: each transmission is a call of its own (`conversationMode`) |
| `--min-transmission s` | 0 | Leave out transmissions shorter than this |
| `--tapes dir` | | Also write each channel as a SigMF tape in `dir`: its IQ and what its decoder needs, for [`tool channel`](developer/tools.md#tool-channel) |
| `--taps` | none | Signal taps to save beside the calls, by id (`fm.discriminator,voice.air,voice.frames`): see [`taps`](configuration/recording.md#taps) |
| `--messages` | off | Print every control channel message |
| `--summary file` | | Also write each site's control channel blocks, good and bad, as JSON: over all of it and, for one of our captures, over the part its live figures (`trp:live`) cover |
| `--quiet` | off | Don't print control channels, calls and files as they happen |

At the end it prints how fast it ran, how many calls it wrote, and for each system the control
channel's good and bad message counts, modulation and identity (NAC, WACN, SysID, RFSS, site).
A system whose control channel belonged to another site says `NOT FOLLOWED` and why. For one of our
captures, each site's blocks over the part live counted ("As live counted, 0.9–20.0 s"), to set beside
its `trp:live`. Each source's measured frequency error is printed too.

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
                 [--rate 2400000] [--buflen n] [--buffers n]
trunk-pro survey <capture> --center Hz [--rate Hz] [--format cu8|cs16|cf32]
trunk-pro survey [--radio '{"type":"bladerf",…}'] --freqs 851.0125,852.5
                 [--type p25|smartnet|edacs|dmr|nxdn] [--listen s]
    any of these: [--identify lat,lon[,km]] [--api URL]
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
| `--bands list` | `800,700,900,uhf,vhf` | Any of `800`, `700`, `900`, `uhf`, `vhf`, and also `uhf-fed` (380–420 MHz), `uhf-420` (420–450 MHz), `biz-uhf`, `biz-vhf` (business bands, where most DMR is) and `t-band` (470–512 MHz), which are only scanned when you name them. An id it doesn't know stops the scan with *Not a band*. |
| `--ranges list` | | Ranges of your own, `lo-hi`, commas between, each end in MHz (under 10 000) or Hz: `--ranges 420-450,433.05-434.79`. Scanned after the bands; given alone, just them. |
| `--gain dB` | AGC, then a gain search | A fixed gain; turns off the gain search |
| `--no-gain` | | Don't search for the best gain |
| `--ppm n` | 0 | Frequency correction while scanning, in whole ppm |
| `--seconds n` | 30 | How long to listen to the best control channel |
| `--identify lat,lon[,km]` | | Then look up the FCC licenses within `km` (50) of there for what was heard, from trunkrecorder.pro: a last JSON line, `{"licenses": [...]}` |
| `--api URL` | https://trunkrecorder.pro | The API `--identify` asks |
| `--rate Hz` | 2400000 | Sample rate |
| `--buflen n` `--buffers n` | 0 (the driver's) | The RTL-SDR's USB transfer size and transfers in flight, as for [`capture`](#capture) |
| `--radio json` | | Survey this radio instead of an RTL-SDR: a source as in the config (`'{"type":"bladerf",…}'`) |
| `--freqs MHz,…` | | Don't scan: listen to just these frequencies, each probed, and say which of a system's sites are heard here |
| `--type t` | | With `--freqs`: look for only this protocol (`p25`, `smartnet`, `edacs`, `dmr` or `nxdn`), with its receivers only |
| `--listen s` | by `--type` | With `--freqs`: how many seconds to listen to each |
| `<capture>` `--center` `--format` | format from the extension | Look at a capture instead of scanning with a dongle. `--center` is required, and `--rate` is the capture's. An unknown `--format` is refused |

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

## tune

```bash
trunk-pro tune [--url http://127.0.0.1:8080] [--source N] [--systems a,b] [--slice 15] [--rounds 3]
               [--settle 4] [--max-steps 12] [--margin 1] [--mode manual|linearity|sensitivity]
               [--protect 5] [--try stage=value,…] [--yes] [--apply]
```

Asks a recorder that is **recording** to find the gain at which one of its radios decodes its
systems best. The recorder at `--url` does the tuning (it can be another computer's; it is the
[interface API](api/README.md)'s `tuneGain`, which needs control access); this command asks for it
and shows how it goes. Each gain to try goes on the radio as a **trial**: the radio takes it at once,
as a gain edited in Setup, but nothing is saved and the config isn't changed. The recorder scores
what the radio's systems decode while it is set:

- **The control channel first.** P25's coverage (the share of the control channel's air decoded),
  else the share of messages decoded while it was locked. Without the control channel nothing is
  recorded.
- **Then voice frames lost**, when a system had at least 5 seconds of voice. A system's score is
  70 % control channel and 30 % voice then, else its control channel alone.
- **A gain that clips** (more than 0.5 % of samples at full scale, as the dashboard warns) is
  refused, whatever it decoded.

The setting's score (0 to 100) is the systems' average with the weakest counted twice, so a gain
that helps the strong systems a little at a weak one's cost isn't chosen. The systems scored are
those whose control channels the recorder receives on that radio, or only `--systems`. The radio's
other systems are still watched then, and shown (the scored ones starred), and protected: a
candidate that costs one of them more than `--protect` points (5 by default) on average over the
rounds is refused, and one that costs one of them more than twice that in its first round is
refused at once, so it isn't kept on the air for the rest. On one 800 MHz Airspy, the LNA that
suited a weak simulcast system best took a neighbouring system's control channel from 74 % to 45 %:
that is what this stops.

**Settings are compared, not scored once.** How well a control channel decodes drifts from minute
to minute (traffic, a simulcast site's fading) by more than a step of gain usually changes it: on
one 800 MHz Airspy the same gain scored 92 and then 84 ten minutes apart. So each candidate is
compared with the best so far by turns, `--slice` seconds each after `--settle` seconds to settle,
for `--rounds` rounds (best first in one round, the candidate first in the next), and it is moved to
only when it scored better on average by more than `--margin` **and** by more than twice the
standard error of the rounds' differences.

**A control channel already decoded in full is compared by its SNR.** Once it decodes 99.8 % at
one gain it can't score better at another, however close to the noise it is: on one RTL-SDR at
490 MHz, 16.6 dB and 25.4 dB both decoded 99.8 %, but the control channel's SNR went from 13.6 to
20.3 dB. So a candidate that decodes no worse (by less than `--margin`) is also better when the
weakest scored system's control channel SNR is higher by more than 2 dB and by more than twice the
standard error of the rounds' differences: that margin is what a fade takes. A candidate takes `rounds × 2 × (settle + slice)`
seconds: 114 by default.

**Each kind of radio has a plan**, from how it is built. The front of the chain (the LNA) sets the
noise figure and is what strong signals overload, so it moves first; the stages after it only bring
the level into the ADC's range, so they move last. Where the radio's maker drew a path through its
stages (an Airspy's linearity and sensitivity tables, an R820T's gain table, the gain table behind an AD9361's one number),
the search goes along that path: each step on it is a setting the maker measured. A move goes one
way while each step is better; refused at its first step, it tries the other way. When any move
was kept, the moves are walked once more (one stage moved can change what suits another), never
trying a setting twice.

| Radio | Plan |
|---|---|
| Airspy, HydraSDR | **Linearity** or **sensitivity** (the mode it is in, or `--mode`): the table's step (0–21), by 3, then 1. Linearity gives up the LNA and mixer before the VGA, what wins against strong signals. **Manual**: the LNA (0–14), then the mixer (0–15), by 3 then 1, and the VGA last, down first, kept to 12 (higher with the LNA up saturates the ADC); AGC off |
| HackRF | The LNA first (0–40 dB, by 8), then the VGA (by 6 dB, then 2, to 40 dB), the RF amp last, and tried on only with the LNA at 24 dB or under: it adds little and puts strong signals at the mixer (more than −5 dBm at the input damages a HackRF) |
| RTL-SDR | The tuner's gain table (0–49.6 dB; the LNA and mixer together, the VGA fixed), about 5 dB at a time, then one entry; AGC off |
| bladeRF, USRP, SoapySDR | The overall gain, which the driver shares over the stages front first: by 6 dB, then 1. Its AGC must be off (that takes a Stop and Start), and a SoapySDR radio needs an overall gain set |

At the end the best is compared with the starting setting once more, and recommended only if it is
still clearly better.

**Recording and uploading go on while it runs**, for every system, so a poor setting costs the
radio's systems while it is tried: their calls in those seconds are recorded (and uploaded) as that
setting hears them. Other radios aren't touched. A setting under which a control channel that was
decoding stops for 10 seconds is given up at once, and one that clips heavily after 5. However it
ends (done, an error, Ctrl-C), the radio goes back as it was, unless `--apply` keeps the best, which
is then saved in the config as an edit in Setup would be. Since the trials are never saved, a
tuning cut short (the recorder stopped, this command killed) leaves the config as it was; stopping
recording, or editing that radio in Setup, stops the tuning.

Before anything changes, it shows the plan, how long it will take ("This will take up to about 24
minutes") and which systems may record worse meanwhile, and waits for Enter (in a terminal; `--yes`
doesn't ask). At the end a **summary** says what it recommends and what that does, system by system,
from the final comparison of the new gain against the current one:

```text
Summary (21 min, 9 settings compared)
  Recommended: gain LNA 9 · MIX 10 · VGA 10 (now LNA 10 · MIX 10 · VGA 10).
  With it, measured against the current one by turns (3 rounds): score +4.2 ± 1.3
     Balch            control channel 74.3% → 79.0% (+4.7)
     MetroC           control channel 75.6% → 80.7% (+5.1) · voice lost 13.3% → 6.5%
```

The frequency correction is Setup's (its **Set correction to**), not this command's.

| Option | Default | What it does |
|---|---|---|
| `--url` | `http://127.0.0.1:8080` | The recorder's built-in interface |
| `--source N` | the only one | The radio, by its number in Setup's list (from 1). Needed when there are several |
| `--systems a,b` | every system on the radio | Score only these (short names); the others are still watched and guarded |
| `--protect points` | 5 | With `--systems`: the most one of the radio's other systems may lose (0–100 scale). `off` or `0` allows any loss |
| `--try stage=value,…` | | Compare this one setting with the one the radio has, and nothing else: `lna=11,mixer=10,vga=9` (Airspy manual), `step=16` (Airspy linearity or sensitivity), `lna=32,vga=24,amp=on` (HackRF), `gain=33.8` (RTL-SDR, bladeRF, USRP, SoapySDR). With `--apply`, kept if it is clearly better |
| `--slice s` | 15 | How long each side of a round is measured (at least 5) |
| `--rounds n` | 3 | Rounds per comparison (2 to 20). More rounds tell smaller differences apart |
| `--settle s` | 4 | How long a gain runs before it is measured |
| `--max-steps n` | 12 | At most this many candidates compared |
| `--margin` | 1 | A move is kept only when it scores this much better (0–100 scale) |
| `--mode` | the radio's | Airspy, HydraSDR: search `manual`, `linearity` or `sensitivity` instead |
| `--yes` | | Don't ask before starting |
| `--apply` | | Keep the best gain, and save it |

```bash
trunk-pro tune --url http://192.168.1.20:8080 --source 2
trunk-pro tune --source 1 --systems county,city --mode sensitivity --apply
trunk-pro tune --source 2 --try lna=11,vga=9          # before applying what a --systems run found
```

## tool

Diagnostic tools for chasing a decoding problem down to one channel, and for looking at saved
calls. They read raw `cu8` captures (from `trunk-pro capture` or `rtl_sdr`) and print JSON lines on
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
| `tool revoice` | Run a call's saved vocoder frames through the vocoder again |
| `tool sdr` | A saved `.sdr` file's metadata, or its frames |
| `tool tones` | The in-band tones in saved calls, or the patterns they make up |

The tools for testing and measuring the decoders (`p25synth`, `dmrsynth`, `nxdnsynth`, `perf`,
`corpus`, `submission`, `channel`, `snr`) are in [Developer tools](developer/tools.md).

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
| `--detect 0` | on | With both receivers (`--demod auto`), run both always instead of finding which the channel needs |
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

### tool nxdnscan and nxdn

```bash
trunk-pro tool nxdnscan <capture.cu8> --center Hz [--rate Hz] [--seconds N] [--step 6250] [--guard 75000]
trunk-pro tool nxdn <capture.cu8> --center Hz --freq Hz [--rate Hz] [--nxdn 48|96] [--frames]
                    [--audio out.f32] [--variant …]
```

`nxdnscan` runs every channel through both NXDN receivers and lists those with NXDN on them, with
rate and RAN. `nxdn` prints one channel's messages (`--frames`: every frame). To make a synthetic
NXDN capture, see [`tool nxdnsynth`](developer/tools.md#tool-nxdnsynth).

### tool revoice

```bash
trunk-pro tool revoice <frames.sdr | frames.jsonl> <out.wav> [--profile enhanced|fixed|mbelib]
                       [--seed 1] [--hard-fec] [--s16]
```

Runs a transmission's saved vocoder frames through the vocoder again, for example to hear it with another
vocoder (`enhanced` unless `--profile` says otherwise; recording's own default is `fixed`). The frames are those the
[`voice.frames` signal tap](configuration/recording.md#taps) saves (Setup → Recording → Advanced → Signal taps, or
`replay --taps voice.frames`): its `.sdr` or its `.jsonl`. `--hard-fec` uses the standard's repeat rule; `--s16` writes raw
16-bit samples instead of a WAV.

### tool sdr

```bash
trunk-pro tool sdr <frames.sdr> [--frames]
```

Prints a saved `.sdr` file's metadata as JSON, as MimoSDR reads it, or with `--frames` its vocoder
frames as JSON lines (one frame per line: `codec`, `bits` in hex, `e0`, `errs`, `erased`, and `out`,
what the vocoder made of it).

### tool tones

```bash
trunk-pro tool tones <frames.sdr | frames.jsonl | call.wav>… [--patterns]
```

Finds the in-band tones in saved calls as a call's are found while recording: DTMF, KNOX, call
progress and single tones, from a digital call's vocoder frames or an analog call's audio. With
`--patterns`, the pages, alert tones and digit strings they make up instead, as the call JSON's
`tone_patterns` (JSON lines). See [Signals](guides/signals.md).

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

## interface

```bash
trunk-pro interface search | install | update | add | uninstall | list …
```

Interfaces besides the built-in one, each served on a port of its own (see
[Interfaces](interfaces/README.md)). Installed ones are kept beside the config, in `interfaces/`;
each is listed in the config's `server.interfaces`. Every subcommand takes `--config file.json`. A
running recorder picks up what these write when it restarts; its Interfaces page does the same
at once.

| Subcommand | What it does |
|---|---|
| `interface search [text]` | The interfaces in the store (github.com/TrunkRecorder/interfaces), or those whose id, name or description contains `text`; which you have, whether an update is out, and which ask for control. |
| `interface install <id \| owner/repo> [--tag v1.2.0] [--port n]` | Install an interface from the store, or from a GitHub repository's release (its latest, or `--tag`). It's served on the first free port, or `--port`. One from outside the store gets listen access whatever it asks for. |
| `interface update [id…]` | Install newer versions of installed interfaces (all, or these). |
| `interface add <folder> [--port n]` | Serve a folder of your own. Its `interface.json`, if any, gives its id and the access it gets; otherwise the folder's name is its id. |
| `interface uninstall <id>` | Stop serving it and forget it (`remove` works too). An installed copy is deleted; a folder of your own is left alone. |
| `interface list` | Every interface: its port, access, address or why it isn't served, and its folder. |

```bash
trunk-pro interface search player
trunk-pro interface install trunkamp
trunk-pro interface add ~/wall-display --port 8090
```

## access

```bash
trunk-pro access list | add <name> [--access listen|control] | remove <name> | signout <name>
                   | passkey remove <name> <passkey> | on | off
```

Signing in: who may, each user with a role and an access token (see
[Signing in](guides/remote-access.md#signing-in)), for a recorder with no browser nearby, and the
way back in when you've lost your token. Every subcommand takes `--config file.json`, for a
recorder that runs with a config other than the default (run it as the account the recorder runs
as). They write the config file (`access signout`, `access-state.json` beside it); a running
recorder picks the change up when it restarts, so restart it right away (a setting saved from the
browser before then would write over the change).
Setup → **Access** does the same at once.

| Subcommand | What it does |
|---|---|
| `access list` | Whether signing in is on, what `anonymous` allows, the [proxy in front](guides/remote-access.md#behind-a-proxy-that-signs-people-in) if set (its header, addresses, what others get and its names mapped to users), and each user: their name, role, the day their token was made, the day they were last signed out everywhere, and their passkeys (name and day). |
| `access add <name> [--access listen\|control]` | Add a user (control unless `--access listen`) with an access token. Its secret is printed once, on stdout (the rest on stderr), so `trunk-pro access add kiosk --access listen > kiosk.token` keeps it in a file. |
| `access remove <name>` | Remove a user: whoever signed in as them is signed out. Refused if they're the last one with control access while signing in is on. |
| `access signout <name>` | Sign a user out everywhere: every browser signed in as them until now signs in again. Their access token still works for a program that sends it with each request: remove the token, or the user, for that. `access list` shows the day. |
| `access passkey remove <name> <passkey>` | Remove one of a user's passkeys, by its name as `access list` shows it: whoever signed in with it is signed out. Refused if it's the last way in for the last control user while signing in is on. (Passkeys are added from a browser: Setup → **Access**.) |
| `access on` | Turn signing in on. Refused until there's a user with control access. |
| `access off` | Turn signing in off: the recovery when you're locked out. |

```bash
trunk-pro access add luke
trunk-pro access on
systemctl --user restart trunk-pro    # or however the recorder runs
```

## Environment variables

| Variable | What it does |
|---|---|
| `TRUNK_PRO_UHD` | The UHD library file to load for USRPs, instead of searching for it |
| `TRUNK_PRO_BLADERF` | The libbladeRF library file to load, instead of searching |
| `TRUNK_PRO_SOAPY` | The SoapySDR library file to load, instead of searching |
| `TRP_CORPUS_CACHE` | Where [`tool corpus`](developer/tools.md#tool-corpus) keeps its files, when `--cache` isn't given |
| `NO_COLOR` | When set (and `log.color` is empty), the log isn't coloured |
| `XDG_CONFIG_HOME` | Linux: where the default config folder goes (`$XDG_CONFIG_HOME/trunk-pro`) |
| `HOME` (or `USERPROFILE`) | Your home folder: the default config folder (macOS, Linux) and recordings folder (`~/TrunkRecorderPro`), and `~/` in paths |
| `APPDATA` | Windows: where the default config folder goes (`%APPDATA%\trunk-pro`) |
| `PATH` | Searched for an M4A encoder (`ffmpeg`, `afconvert`, `fdkaac`) |

The Docker image sets `HOME=/data` and `XDG_CONFIG_HOME=/data/config`, so everything lands in the
`/data` volume. See [Docker](install/docker.md).
